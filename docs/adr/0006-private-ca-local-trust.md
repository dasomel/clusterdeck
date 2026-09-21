# ADR-0006: Private cluster CA local trust

- Status: Accepted
- Date: 2026-09-21
- Design spec: [docs/superpowers/specs/2026-09-21-private-ca-local-trust-design.md](../superpowers/specs/2026-09-21-private-ca-local-trust-design.md)

## Context

Clusters ClusterDeck connects to commonly front their discovered ingress/APISIX/Gateway API
endpoints (e.g. `argocd.local.beluga.internal`) with a cluster-internal CA (typically a
cert-manager self-signed-bootstrap `ClusterIssuer`). Opening these in a real browser showed a
certificate warning, since the issuing CA was not locally trusted. These clusters are also
frequently torn down and recreated (`AGENTS.md`'s Product Boundary), which regenerates a
self-signed bootstrap CA with a new keypair — a previously-trusted CA silently goes stale.

## Decision

Fetch the CA from the Kubernetes `Secret` backing a discovered endpoint's TLS termination
(resolved via the `Ingress`/`ApisixTls` object referencing it — not by assuming cert-manager
`ClusterIssuer` naming conventions), fingerprint it locally (SHA-256/SHA-1, computed natively via
`sha2`/`sha1` rather than parsed from CLI text output), and trust it in the macOS **login**
keychain (not the System keychain — covers Safari/Chrome without requiring `sudo`/admin-password
handling in the app). A per-profile `trusted_cas` record tracks what ClusterDeck itself has
trusted, keyed by the Secret it came from; on each endpoint discovery, the freshly-fetched CA's
fingerprint is silently compared against that record, and a mismatch (cluster CA rotated) is
surfaced as an "Update Trust" action rather than auto-replaced. Trust/untrust in the keychain is
always scoped by SHA-1 fingerprint, mirroring this repo's existing `/etc/hosts`
BEGIN/END-marker and `~/.ssh/config` `Include`-block convention of only ever touching what
ClusterDeck itself added.

`tls.key` (and any other Secret field besides `ca.crt`) is extracted-and-discarded server-side
immediately after fetch and never crosses the Tauri IPC boundary to the frontend — see the design
spec's Security section for the full data-flow accounting (this was informed by a real reviewed
mistake during design: an early manual `kubectl get secret -o jsonpath='{.data}'` investigation
step briefly pulled the full secret, including `tls.key`, into a conversation transcript before
the extraction boundary was tightened to `ca.crt` only).

v1 resolves TLS secret refs only for `ingress`- and `apisix`-sourced `DiscoveredEndpoint`s
(`istio`/`gateway-api` deferred — see spec D5); this is an additive gap, not a design constraint,
since `DiscoveredEndpoint.source` already tags which resolver path an endpoint needs.

## Consequences

- Trusting or replacing a CA requires the user to approve a macOS GUI authorization dialog
  (Touch ID/password) — by design (D3 in the spec): the action is never silent or automatic.
- A cluster whose RBAC denies `get` on `secrets` cannot use this feature (v1 has no TLS-handshake
  fallback implemented, though the spec allows adding one later without changing this decision).
- `istio`/`gateway-api`-sourced endpoints show no CA status until a future change adds their
  secret-ref resolution.
