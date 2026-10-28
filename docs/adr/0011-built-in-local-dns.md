# ADR-0011: Built-in local DNS with per-zone `/etc/resolver` entries instead of `/etc/hosts`

- Status: Accepted (verification spike passed 2026-10-16)
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
| Anything outside active zones, including `_dns.resolver.arpa` DDR probes | `REFUSED` |

The responder is never recursive. Answers are generated from validated records (`is_safe_host_domain`, `is_safe_ip_address`) and never from the query. Because the server is authoritative, real DNS names under an active zone are hidden while the zone is active. This is why zones must be dedicated private suffixes and require approval (D3).

### D6 — Node aliases

The spike showed that `/etc/resolver/<zone>` works for `.clusterdeck.local` zones on both paths. Node aliases (`<host>.<profile>.clusterdeck.local`) are therefore served by the same responder: each `local_dns` profile gets the zone `<profile>.clusterdeck.local`, which needs no user approval because ClusterDeck owns the `clusterdeck.local` namespace. Aliases keep their names, so there is no user-visible rename.

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

## Verification spike (passed 2026-10-16)

The spike ran on macOS 27.0.1 with a std-only Rust prototype responder (four instances behind four test resolver files) and the RFC 5737 test addresses. Each item was checked on both paths, `getaddrinfo`/`dscacheutil` and `URLSession` (the Safari stack), and cross-checked against the responder's query log:

| # | Claim | Result |
|---|---|---|
| 1 | The `port` directive is honored, including TCP fallback | Pass: a UDP answer with TC=1 made the system retry over TCP on the same port |
| 2 | A more specific zone file takes precedence over a suffix file | Pass: queries under the specific zone reached only its responder |
| 3 | `.clusterdeck.local` names resolve under a resolver file | Pass, on both paths (D6) |
| 4 | Negative and no-data answers do not stall | Pass: an unknown in-zone name failed with `NSURLErrorCannotFindHost` in 0.01 s; AAAA and HTTPS (type 65) queries got NOERROR/NODATA |
| 5 | Agent ownership check and file cleanup | Deferred to the implementation tests (app logic, not OS behaviour) |

Findings that change the plan:

- **`dns-sd -G` is not a valid oracle.** It reported "No Such Record" for names that `URLSession` resolved through the same resolver files. Verification uses `URLSession` and the responder log instead.
- **DDR probes.** mDNSResponder sends `_dns.resolver.arpa` (type 64, SVCB) to each configured nameserver. The responder must answer `REFUSED`, which signals no encrypted-DNS upgrade; the prototype did this without side effects.
- **Earlier measurements (Context 1–3).** Zone resolution without `/etc/hosts` and the 30 s `getaddrinfo` stall when the responder is down stand as recorded.

## Alternatives considered

- **Keep `/etc/hosts` only:** unreliable on the Safari path, and every record change costs an admin prompt.
- **Hosts plus per-host resolver files** (the first draft of this ADR): still edits `/etc/hosts`, and prompts on every record change.
- **Responder inside the app process:** a resolver file pointing at a stopped responder stalls even `getaddrinfo` for 30 s.
- **Bundle PowerDNS or dnsmasq:** external daemons, package coupling, and far more surface than an authoritative-only responder for a few zones.
- **Static answers in resolver files:** not possible, because `/etc/resolver` supports only `nameserver`, `port`, `domain`, `search`, `options` and similar directives.
- **Port 53:** needs root, and `port` makes it unnecessary.
- **Network Extension DNS proxy:** needs entitlements and a system extension, too heavy for this problem.

## Amendment 2026-10-28: node alias domain moves to `clusterdeck.internal` (D6 amended, #119)

Spike item 4 ("negative and no-data answers do not stall") passed for the stock `*.example.internal` zone but was never run against a `.local` zone. On a real Mac a negative lookup under `*.clusterdeck.local` stalls: mDNSResponder waits on multicast DNS after the unicast NXDOMAIN, because `.local` is an mDNS domain. The Option-3 mitigation (item 3: add an SOA to the NXDOMAIN authority section) was implemented and measured on the live agent: no effect, even for repeated lookups of the same name. It was reverted and must not be re-added.

Measured with `getaddrinfo` on the same Mac and the same responder:

| Zone | Negative lookup |
|---|---|
| `*.clusterdeck.local` (mDNS `.local`) | 5003-5043 ms |
| `*.clusterdeck.local` with SOA in authority | 5003-5043 ms (no change) |
| `*.internal` served by the same responder | 1-2 ms |

Decision:

- **D6 (amended):** node aliases are `<host>.<profile>.clusterdeck.internal` and the implicit zone is `<label>.clusterdeck.internal`. `.internal` is reserved by ICANN for private use. The constant `NODE_ALIAS_DOMAIN` in `services/validate.rs` is the single source of truth (the frontend mirrors it in `src/lib/nodeAlias.ts`).
- **Reservation:** both `clusterdeck.internal` and the legacy `clusterdeck.local` (and everything under them) are reserved, so a user cannot approve either as a zone.
- **Migration:** the hosts block is emitted with the new names only (no dual emission). Legacy `.clusterdeck.local` lines in an existing block are dropped, not preserved as endpoints. `plan_changes_for_port` already removes marker-owned resolver files whose zone is no longer desired, so the old `/etc/resolver/<label>.clusterdeck.local` files go on the next apply (one admin prompt); unowned files are never touched.
- **Breaking:** bookmarks, SSH configs and scripts using the old names must be updated. Downgrading below this version recreates the `.local` zones.
