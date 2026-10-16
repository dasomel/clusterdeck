# ADR-0011: Built-in local DNS with per-zone `/etc/resolver` entries instead of `/etc/hosts`

- Status: Proposed (blocked on the verification spike below)
- Date: 2026-10-16
- Issue: #86
- Relates to: [ADR-0004](0004-hosts-file-toctou-mitigation.md), [ADR-0009](0009-bastion-api-tunnel.md), [ADR-0010](0010-profile-gateway-address.md)

## Context

ClusterDeck makes node aliases (`<host>.<profile>.clusterdeck.local`) and discovered ingress/gateway hostnames reachable by writing a managed block to `/etc/hosts` (ARCHITECTURE §7.1, ADR-0004). PR #85 flushes the resolver cache after each write.

On a user's Mac running macOS 27.0.1, the two system resolution paths disagreed:

| Path | Used by | Honors `/etc/hosts` | Honors `/etc/resolver/<domain>` |
|---|---|---|---|
| libinfo `getaddrinfo` | Chrome, Firefox, curl, kubectl, `dscacheutil` | yes | **yes** |
| Network.framework → mDNSResponder | Safari, `URLSession`, `dns-sd` | **no** (even the stock `broadcasthost` returns "No Such Record") | **yes** |

What was ruled out:
- Ruled out: `/etc/hosts` content, the resolver cache, Tailscale (tested disconnected), Avast Real Site.
- Not excluded: the Avast system extension, because it could not be fully disabled.

The following were measured on the same Mac with a prototype: `dnsmasq` on `127.0.0.1:5354` answering `*.local.example.internal`, plus the suffix file `/etc/resolver/local.example.internal` (`nameserver 127.0.0.1` / `port 5354`).

1. **Both paths resolve through it without `/etc/hosts`.** A name under the zone that is *absent* from `/etc/hosts` returned the gateway address via `getaddrinfo` (0.04 s), `dscacheutil` and `URLSession`. URLSession reached the gateway over TLS with the pinned CA; the gateway answered 404 for the unknown host.
2. **Existing names also resolve:** `URLSession` returned HTTP 200 for the cluster's ingress hostnames.
3. **A resolver file whose responder is down is harmful.** `getaddrinfo` stalled **30 s** before falling back to `/etc/hosts`, and `curl` timed out at 10 s. Resolver files must never outlive their responder.

Every privileged write (one admin prompt each) exists today only because `/etc/hosts` must change whenever records change. A local authoritative DNS is a PowerDNS-like zone server, minus everything ClusterDeck doesn't need. It moves record changes into a user-owned file. Root is needed only when the set of *zones* changes.

## Decision

### D1 — Local DNS is the primary mapping; `/etc/hosts` becomes legacy

`manage_hosts_file: bool` becomes `host_mapping: off | local_dns | hosts`.

- **Migration:** the old bool maps `false → off` and `true → hosts`, so existing behaviour is unchanged. New profiles default to `off`. The UI recommends `local_dns`.
- **`local_dns`:** writes **no** `/etc/hosts` entries. Discovered endpoints, including `gateway_address` (ADR-0010), are served by the local DNS.
- **`hosts`:** today's block, kept for users who don't want a background component.

### D2 — Built-in authoritative responder, always on, outside the app

ClusterDeck does not bundle PowerDNS or dnsmasq, which are external daemons with a Homebrew or package dependency. It ships a minimal **authoritative** responder in Rust (a `hickory-server`-based handler or equivalent). It runs as a ClusterDeck-owned **user** LaunchAgent (`KeepAlive`, no root): the same signed binary in a headless responder mode.

- **Records** come from `~/.clusterdeck/dns/zones.json`. The app writes the file atomically and the agent reloads it on change. Changing an IP or adding a host needs **no** admin prompt.
- **Binding** is `127.0.0.1:<port>` only, default `15353`. On `EADDRINUSE` the responder exits non-zero and the app reports it. It never silently switches port.
- **Lifetime:** the responder outlives the app window, which avoids the measured stall (#3).

### D3 — Zones, approval and conflicts

A **zone** is a dedicated private suffix that a profile serves authoritatively, e.g. `local.example.internal`.

- **Approval:** zones are proposed from the discovered endpoints' parent domains and become active only after the user approves them in the UI.
- **Eligibility:** a zone must pass `is_safe_host_domain`, have at least three labels, and is never a public suffix. A tenant-controlled ingress host such as `github.com` can never become a zone.
- **Hosts outside approved zones** are not served. They are listed in the UI.
- **One owner per zone.** Two profiles claiming the same zone, such as a local VM cluster and a cloud cluster deployed from the same platform code, is a conflict. The user picks the active one, an explicit switch that replaces today's "first /etc/hosts entry wins". The #82 approval dialog is reused for the switch.
- **Unowned resolver files:** an existing `/etc/resolver/<zone>` without the ClusterDeck marker, e.g. one left by a VM setup script, is never modified. It is reported as a conflict with instructions.

### D4 — Resolver files per approved zone, written only when the zone set changes

For each active zone the app writes `/etc/resolver/<zone>`. The file has a first line `# managed by ClusterDeck (profile: <id>)`, then `nameserver 127.0.0.1` and `port <port>`.

- **Timing:** files are written or removed only when the active zone set changes, in **one** `do shell script … with administrator privileges` call together with the cache flush. This preserves ADR-0004's single-prompt rule.
- **Ownership gate:** before writing, and on app start, the app verifies that the agent is loaded (`launchctl print`). It also checks that the port's listener is the agent's pid (`lsof`), the same ownership technique as ADR-0009.
- **Failed gate:** if the agent is missing or not the owner, the app removes its resolver files in the next privileged action, so they cannot stall lookups (#3). It then reports the problem.
- **Removal** sweeps `/etc/resolver` for files carrying this profile's marker; it does not trust state. Lost state or a crash therefore cannot orphan files.

### D5 — DNS semantics (authoritative, never recursive)

| Query | Response |
|---|---|
| A for a served name | the address, TTL 30 s |
| AAAA (IPv4-only record), HTTPS (65), SVCB, other types for a served name | `NOERROR` with no data |
| Unknown name inside an active zone | `NXDOMAIN`, authoritative |
| Anything outside active zones | `REFUSED` |

The responder is never recursive. Answers are generated from validated records (`is_safe_host_domain`, `is_safe_ip_address`) and never from the query. Because the server is authoritative, real DNS names under an active zone are hidden while the zone is active. This is why zones must be dedicated private suffixes and require approval (D3).

### D6 — Node aliases

Node aliases live under `.clusterdeck.local`, and mDNSResponder treats `.local` specially. Until the spike proves resolver files work there, `local_dns` profiles keep serving node aliases through the hosts block. Those aliases are used by SSH, which goes through `getaddrinfo`. If the spike fails for `.local`, a follow-up ADR may move the aliases to a non-`.local` zone, e.g. `<profile>.clusterdeck.internal`. That would be a user-visible rename.

## Consequences

- Cluster hostnames resolve in Safari and every other client, and IP or endpoint changes stop costing an admin prompt.
- The #79 and #82 `/etc/hosts` conflict machinery becomes zone-ownership switching for `local_dns` profiles.
- **New background component:** a user LaunchAgent with a read-only record file. It is installed when the first profile selects `local_dns`. When none do, it is unloaded and the resolver files are swept.
- **Security:**
  - While the agent is down, its loopback port could be squatted. D4's ownership gate then refuses to point resolver files at it, or removes them.
  - TLS against the pinned CA still protects HTTPS clients.
  - Plain-HTTP endpoints remain spoofable, as on any shared LAN.
  - Unknown queries are refused or answered NXDOMAIN, never forwarded.
- **Scope:** ClusterDeck still does not become a general DNS server. It serves only the zones the user approved for its own profiles.

## Verification spike (must pass before Accepted)

Each check below must be run on macOS 27 with `dscacheutil -q host`, `dns-sd -G v4`, `scutil --dns` and `URLSession`, not `dig`, which bypasses mDNSResponder.

**Already shown by the prototype:** context items 1–3, i.e. zone resolution without `/etc/hosts` on both paths, and the 30 s stall when the responder is down.

**Still open:**
1. The `port` directive is honored for TCP fallback as well as UDP.
2. A zone file takes precedence over a less specific unowned suffix file, and an equally specific unowned file is detected and reported.
3. Behaviour of `.clusterdeck.local` names under a resolver file (D6).
4. `NXDOMAIN` and `NOERROR`/no-data answers do not stall Safari.
5. The agent's ownership check (`launchctl` + `lsof`) and the removal of files when the agent is gone.

## Alternatives considered

- **Keep `/etc/hosts` only:** unreliable on the Safari path, and every record change costs an admin prompt.
- **Hosts plus per-host resolver files** (the first draft of this ADR): still edits `/etc/hosts`, and prompts on every record change.
- **Responder inside the app process:** a resolver file pointing at a stopped responder stalls even `getaddrinfo` for 30 s.
- **Bundle PowerDNS or dnsmasq:** external daemons, package coupling, and far more surface than an authoritative-only responder for a few zones.
- **Static answers in resolver files:** not possible, because `/etc/resolver` supports only `nameserver`, `port`, `domain`, `search`, `options` and similar directives.
- **Port 53:** needs root, and `port` makes it unnecessary.
- **Network Extension DNS proxy:** needs entitlements and a system extension, too heavy for this problem.
