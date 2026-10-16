# ADR-0011: Resolve managed hostnames in Safari via per-host `/etc/resolver` entries and an in-app DNS responder

- Status: Proposed
- Date: 2026-10-16
- Issue: #86
- Relates to: [ADR-0004](0004-hosts-file-toctou-mitigation.md), [ADR-0009](0009-bastion-api-tunnel.md), [ADR-0010](0010-profile-gateway-address.md)

## Context

ClusterDeck makes node aliases (`<host>.<profile>.clusterdeck.local`) and discovered ingress/gateway hostnames reachable by writing a managed block to `/etc/hosts` (ARCHITECTURE §7.1, ADR-0004). PR #85 additionally flushes the resolver cache after each write.

On a user's Mac running macOS 27.0.1, the two system resolution paths disagreed:

| Path | Used by | Honors `/etc/hosts` | Honors `/etc/resolver/<domain>` |
|---|---|---|---|
| libinfo `getaddrinfo` | Chrome, Firefox, curl, kubectl, `dscacheutil` | yes | yes |
| Network.framework → mDNSResponder | Safari, `URLSession`, `dns-sd` | **no** (even the stock `broadcasthost` returns "No Such Record") | yes |

Ruled out on that machine:
- `/etc/hosts` content, the resolver cache, and Tailscale (tested disconnected).
- Avast Real Site.
- The Avast system extension could not be fully disabled, so it is not excluded.

Regardless of the trigger, the observable rule is that **`/etc/resolver` is honored on both paths, and `/etc/hosts` is not reliable on the Safari path.** Consequences of relying on `/etc/hosts` alone:
- Safari shows "server not found".
- Where another `/etc/resolver/<suffix>` exists (one was left by a Vagrant setup script), Safari silently reaches a different cluster and shows a certificate warning.

A manual workaround proved the approach: `dnsmasq` on `127.0.0.1:5354` answering `*.local.example.internal`, plus `/etc/resolver/local.example.internal` (`nameserver 127.0.0.1` / `port 5354`). Safari and `URLSession` then returned HTTP 200 with the pinned cluster CA verified.

## Decision

- **D1 — Opt-in, additive.** A per-profile setting `dns_resolution: hosts | hosts_and_resolver` (domain enum; default `hosts`, i.e. today's behaviour). `hosts_and_resolver` keeps the `/etc/hosts` block (the getaddrinfo path still uses it, and it keeps working while the app is closed) and adds D2–D4.
- **D2 — In-app DNS responder.** A minimal UDP+TCP DNS responder runs inside the app, bound to `127.0.0.1` only, on a fixed configurable port (default `15353`; it must not collide with user tools such as a dnsmasq on 5354).
  - It answers **only** the exact hostnames ClusterDeck manages, built from the same entry list as the hosts block (node aliases, bastion alias, discovered endpoints, `gateway_address` per ADR-0010).
  - It returns A/AAAA records with a short TTL (30 s). Every other name gets `REFUSED`, so it is never an open or recursive resolver.
  - It is implemented in Rust (a small `hickory-proto`-based handler, or equivalent) rather than by bundling or depending on dnsmasq: ClusterDeck must not require external daemons, and an in-process responder stays testable behind a trait like `CommandRunner`.
- **D3 — Per-host `/etc/resolver` files, not suffix files.** For each managed hostname, ClusterDeck writes `/etc/resolver/<exact-hostname>` (`nameserver 127.0.0.1`, `port <P>`, and a first-line marker `# managed by ClusterDeck (profile: <id>)`).
  - Exact-host files never capture sibling names under a shared suffix that ClusterDeck does not manage.
  - They are more specific than any user suffix file, so the system resolver prefers them.
  - All files of one sync are written and removed in a **single** privileged call (one admin prompt, ADR-0004 discipline), followed by the cache flush from #85. State tracks the set written per profile so removal is exact.
- **D4 — Never touch unowned resolver files.**
  - An existing `/etc/resolver/<name>` without the ClusterDeck marker is left untouched and reported as a conflict, extending the #79 conflict model with `kind: resolver`.
  - A user suffix file such as `local.example.internal` is reported as "may shadow" information only, because per-host files take precedence.
  - Removal deletes only files whose marker names this profile.
- **D5 — Validation at the sink.** Hostnames pass `is_safe_host_domain` and addresses pass `is_safe_ip_address` before reaching a resolver file name, its content, or a DNS answer. The resolver file content is generated, never interpolated from free-form profile text.
- **D6 — Lifecycle.**
  - The responder starts with the app and serves the current profiles' entries from the same source the hosts sync uses.
  - Profile edit, Connect, Sync, Clear and delete update its in-memory table without a restart.
  - Resolver files are written on Sync and removed on Clear or profile delete, never on app quit: leaving them in place costs nothing while the app is closed, because the getaddrinfo path still falls back to `/etc/hosts`.

## Consequences

- Managed hostnames resolve in Safari as well as in every other client, without user-installed DNS tools.
- **While the app is not running, Safari lookups for these names fail** ("server not found"), because the resolver files point at a stopped responder; other browsers and CLIs keep working via `/etc/hosts`. This matches the ADR-0009 model (the API tunnel also lives only while the app runs). A later option is a ClusterDeck-owned LaunchAgent running the responder headless; it is deferred because it adds a privileged-adjacent background component.
- **Security.** Another local process (or user) could bind the responder port while the app is closed and answer these names. As with ADR-0009, TLS verification against the pinned cluster CA is the control: an impostor cannot complete the handshake. Plain-HTTP endpoints would remain spoofable, as they already are on a shared LAN. The responder never answers names it does not manage.
- **Cost.** The managed file count grows with the number of discovered endpoints (10–50 is typical). Each sync rewrites the set atomically from a temp directory in one privileged call.
- **UX.** The conflict banner (#79) and the resolve dialog (#82) gain resolver conflicts. Their copy must distinguish "ClusterDeck file of another profile" (resolvable) from "unowned file" (manual).

## Alternatives considered

- **Keep `/etc/hosts` only.** It is unreliable on the Safari path, as observed above.
- **Suffix resolver files (`/etc/resolver/local.example.internal`).** These capture every name under the suffix, including ones the cluster does not serve, and collide with user or Vagrant suffix files. Rejected in favour of exact-host files.
- **Bundle or require dnsmasq.** This adds an external daemon, Homebrew coupling, and a lifecycle outside the app. The manual workaround used it only to validate the approach.
- **Run the responder on port 53.** That requires root for the whole process. `/etc/resolver` supports a `port` directive, so an unprivileged high port suffices.
- **A system DNS proxy (Network Extension).** This requires entitlements and a system extension, which is far heavier than the problem warrants.

## Verification plan

- **Unit tests:** responder answers (managed name → A/AAAA, unmanaged → `REFUSED`); resolver file rendering, including the marker and validation rejections; conflict detection for unowned files.
- **Real-path check (AGENTS.md: real binary, not only fakes):**
  - with the responder up, `dig @127.0.0.1 -p <P> <name>` answers;
  - after Sync, `URLSession`/Safari loads a managed HTTPS endpoint with the pinned CA;
  - after Clear, the per-host files are gone and no unowned file was modified.
