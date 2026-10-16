# ADR-0011: Resolve managed hostnames in Safari via per-host `/etc/resolver` entries and an always-on local DNS responder

- Status: Proposed (blocked on the verification spike below)
- Date: 2026-10-16
- Issue: #86
- Relates to: [ADR-0004](0004-hosts-file-toctou-mitigation.md), [ADR-0009](0009-bastion-api-tunnel.md), [ADR-0010](0010-profile-gateway-address.md)

## Context

ClusterDeck makes node aliases (`<host>.<profile>.clusterdeck.local`) and discovered ingress/gateway hostnames reachable by writing a managed block to `/etc/hosts` (ARCHITECTURE §7.1, ADR-0004). PR #85 flushes the resolver cache after each write.

On a user's Mac running macOS 27.0.1, the two system resolution paths disagreed:

| Path | Used by | Honors `/etc/hosts` | Honors `/etc/resolver/<domain>` |
|---|---|---|---|
| libinfo `getaddrinfo` | Chrome, Firefox, curl, kubectl, `dscacheutil` | yes | yes |
| Network.framework → mDNSResponder | Safari, `URLSession`, `dns-sd` | **no** (even the stock `broadcasthost` returns "No Such Record") | yes |

Ruled out on that machine:
- `/etc/hosts` content and the resolver cache.
- Tailscale (tested disconnected) and Avast Real Site.
- The Avast system extension could not be fully disabled, so it is not excluded.

A manual prototype proved the direction. It used `dnsmasq` on `127.0.0.1:5354` answering `*.local.example.internal`, plus the suffix file `/etc/resolver/local.example.internal` (`nameserver 127.0.0.1` / `port 5354`). Safari and `URLSession` then returned HTTP 200 with the pinned cluster CA verified.

**Measured on the same Mac:** with the prototype's responder stopped but its resolver file still present, `getaddrinfo` for a name that is also in `/etc/hosts` took **30 s** before returning the hosts address, and `curl` timed out at 10 s. A resolver file whose nameserver is down therefore breaks the getaddrinfo path as well. It does not fall back promptly to `/etc/hosts`. **Resolver files must never outlive their responder.**

## Decision

### D1 — One per-profile setting

`manage_hosts_file: bool` becomes `host_mapping: off | hosts | hosts_and_resolver`. This is a domain enum; the bool migrates as `false → off` and `true → hosts`, and the default stays `off`. `hosts_and_resolver` adds D2–D7 on top of the `/etc/hosts` block. It cannot be combined with a disabled hosts block, so no undefined combination exists.

### D2 — Always-on responder, outside the app

The responder runs as a ClusterDeck-owned **user** LaunchAgent (`KeepAlive`, no root), not inside the app window process.

- It is the same signed binary started in a headless responder mode, so no third-party daemon is needed.
- It serves records from `~/.clusterdeck/dns/records.json`, which the app writes atomically and the agent reloads on change.
- It binds `127.0.0.1:<port>` only (default `15353`). On `EADDRINUSE` it exits non-zero and the app reports the failure; it never retries silently on another port.
- This removes the app-closed failure mode that the measurement above exposed.

### D3 — Ownership before any file points at the port

Before writing or keeping any resolver file, the app checks two things:

- the agent is loaded (`launchctl print`);
- the listener on `<port>` is the agent's pid (`lsof`, same technique as the ADR-0009 tunnel ownership check).

If either check fails, the app does not write resolver files. It removes the existing ones in the next privileged action and reports the reason.

### D4 — Exact-host resolver files

For each **eligible** managed hostname, the app writes `/etc/resolver/<exact-hostname>` containing a first line `# managed by ClusterDeck (profile: <id>)`, then `nameserver 127.0.0.1` and `port <port>`.

- Exact-host files avoid capturing sibling names under a shared suffix that ClusterDeck does not manage.
- Exact-host files are expected to take precedence over a user's suffix file; the spike must confirm this.

### D5 — Eligibility and scope (anti-hijack)

Discovered ingress hosts are controlled by cluster tenants, so a name such as `github.com` must never become a resolver file. A hostname is eligible only if all of the following hold:

- it passes `is_safe_host_domain`;
- it has **at least three labels**;
- it falls under a per-profile **domain allowlist**.

The allowlist is seeded from the discovered endpoints' parent domains and confirmed by the user once in the UI. Names outside the allowlist are listed and need explicit approval.

Node aliases under `.clusterdeck.local` are excluded by default, because mDNSResponder treats `.local` specially; the spike decides whether to include them. They serve SSH, which uses the getaddrinfo path anyway. Addresses must pass `is_safe_ip_address`.

### D6 — DNS semantics

For a managed name, the responder handles query types as follows:

| Query | Answer |
|---|---|
| A | the address, TTL 30 s |
| AAAA (IPv4-only entry), HTTPS (65), SVCB and other types | `NOERROR` with no data |
| A name *below* a managed name, which the per-host file also routes to the responder | `NXDOMAIN` |
| Any other name | `REFUSED` |

The responder is never recursive, and every answer is generated from validated records, never from the query.

### D7 — One privileged action, exact cleanup

The hosts-block copy, all resolver-file writes and removals, and the cache flush run in **one** `do shell script … with administrator privileges` call. This extends `write_hosts_file`, so ADR-0004's single-prompt rule holds.

- When the resulting `/etc/hosts` content and the resolver file set are unchanged, the prompt is skipped entirely.
- The ADR-0004 TOCTOU recheck is extended to the resolver directory listing.
- Removal on Clear, profile delete, or a failed D3 check deletes files whose marker names the profile, found by **sweeping `/etc/resolver` for the marker**. It does not rely on state, so lost state or a crash cannot orphan files. A sweep also runs on app start when the agent is missing.
- Unowned files with no marker are never modified. They are reported as conflicts, extending #79 and #82 with `kind: resolver`.
- The write is not atomic across files. A partial failure is reported, and the next action converges the set by sweep.

## Consequences

- Managed hostnames resolve in Safari as well as in every other client, without user-installed DNS tools, including while the app window is closed.
- **New background component.** A user LaunchAgent with a small, read-only record set. It must be installed and removed with the feature. Turning `host_mapping` away from `hosts_and_resolver` on every profile unloads it and sweeps the files.
- **Security.** The loopback port could be squatted while the agent is down. D3 then refuses to point files at it, and TLS against the pinned CA still protects HTTPS clients. Plain-HTTP endpoints remain spoofable, as on any shared LAN. The responder never answers unmanaged names (`REFUSED`).
- **Cost.** One resolver file per eligible hostname, typically 10–50.

## Verification spike (must pass before Accepted)

These claims are unverified. Each must be shown on macOS 27 using `dscacheutil -q host`, `dns-sd -G v4`, `scutil --dns` and `URLSession`, not `dig`, which bypasses mDNSResponder:

1. An exact-hostname file name (`/etc/resolver/app.local.example.internal`) is honored on both resolution paths.
2. The `port` directive is used for UDP and for TCP fallback.
3. An exact-host file takes precedence over a user suffix file for the same name.
4. Behaviour of `.clusterdeck.local` names under a resolver file.
5. With the responder stopped and the file present, the measured 30 s getaddrinfo stall reproduces. This confirms why D2/D3 exist.
6. Unmanaged sub-names receive `NXDOMAIN` without stalling Safari.

## Alternatives considered

- **Keep `/etc/hosts` only.** It is unreliable on the Safari path, as observed above.
- **Responder inside the app process.** A resolver file pointing at a stopped responder stalls even getaddrinfo by 30 s. The responder must therefore outlive the app.
- **Suffix resolver files.** They capture every name under the suffix and collide with user or Vagrant suffix files.
- **Static answers inside the resolver file.** Not possible: `/etc/resolver` supports only `nameserver`, `port`, `domain`, `search`, `options`, and similar directives.
- **Bundle or require dnsmasq.** This adds an external daemon and couples the feature to Homebrew. The prototype used dnsmasq only to validate the approach.
- **Port 53.** It needs root, and `port` makes it unnecessary.
- **Network Extension DNS proxy.** It needs entitlements and a system extension, which is too heavy for this problem.
