# Private Cluster CA Local Trust — Design

**Status:** Approved 2026-09-21 (user decisions recorded inline below). Implementation plan to follow via `superpowers:writing-plans`.

## Problem

`Discovered Cluster Endpoints` (`src/App.tsx:878-928`, backed by `discover_cluster_endpoints` /
`src-tauri/src/services/k8s_endpoints.rs`) surfaces private-domain HTTPS services (e.g.
`argocd.local.beluga.internal`, `sso.local.beluga.internal`) fronted by a cluster-internal CA
(typically a cert-manager self-signed-bootstrap `ClusterIssuer`). Opening any of these in a real
browser today shows a certificate warning because the issuing CA is not trusted by the local
machine. There is currently no "Open in browser" path that works cleanly — the existing button
(`App.tsx:916-924`) even opens `http://`, not `https://`.

Additionally, these clusters are frequently torn down and recreated from scratch (ClusterDeck's
core use case — see `AGENTS.md` Product Boundary). A clean reinstall regenerates the self-signed
bootstrap CA with a new keypair, so a previously-trusted CA becomes stale and the new one is
untrusted, silently reintroducing the warning.

## Goals

- Fetch the CA certificate backing a discovered endpoint's TLS termination, directly from the
  cluster (no manual copy/paste of PEM files).
- Trust that CA locally so Safari/Chrome/curl stop warning when the user opens the domain.
- Detect when a cluster's CA has changed (clean reinstall) and let the user replace the stale
  trust entry with the new one, scoped strictly to what ClusterDeck itself added.

## Non-Goals (v1)

- `istio` / `gateway-api` sourced endpoints (per `DiscoveredEndpoint.source`) — resolving their
  TLS secret ref requires an extra `Gateway` object query not currently wired into
  `k8s_endpoints.rs`. Deferred; see Decisions.
- CA export/download UI.
- Auto-untrusting a CA when its Profile is deleted (leaves the keychain entry in place —
  destructive cleanup on profile delete was not asked for).
- System-wide (System keychain / `sudo`) trust. Login keychain only — see D2.

## Architecture

### New service boundary: `services/ca_trust.rs`

Follows the existing `services/k8s_endpoints.rs` / `services/kube_import.rs` pattern (pure
functions over `&dyn CommandRunner`, unit-testable with `FakeRunner`).

```rust
pub struct DiscoveredCa {
    pub secret_ref: String,       // "namespace/name"
    pub source_hosts: Vec<String>, // discovered endpoint hosts backed by this secret
    pub fingerprint_sha256: String,
    pub fingerprint_sha1: String, // used for security(1) -Z lookups
    pub subject_cn: String,
    pub not_after: String,
}

pub enum CaTrustStatus { New, Trusted, Rotated { old_fingerprint_sha1: String } }

// Resolves each endpoint's backing TLS secret (ingress: spec.tls[].secretName directly;
// apisix: matching ApisixTls CRD), fetches the Secret, extracts `ca.crt` only, computes
// fingerprints, and dedupes by fingerprint_sha256 (one cluster CA commonly backs many hosts).
pub async fn discover_cluster_cas(
    runner: &dyn CommandRunner,
    kubeconfig_path: &Path,
    endpoints: &[DiscoveredEndpoint],
) -> Result<Vec<DiscoveredCa>, String>;

// Writes `pem` to a 0600 temp file, runs `security add-trusted-cert -r trustRoot -k
// <login-keychain-path>`, unconditionally removes the temp file, returns the sha1 fingerprint.
pub async fn trust_ca(runner: &dyn CommandRunner, pem: &str) -> Result<String, String>;

// `security delete-certificate -Z <fingerprint_sha1> -t -k <login-keychain-path>`
pub async fn untrust_ca(runner: &dyn CommandRunner, fingerprint_sha1: &str) -> Result<(), String>;
```

### Storage: extend `Profile`, reuse `store.rs`

```rust
// added to the existing Profile struct (src-tauri/src/.../store.rs + src/api/tauri.ts)
pub struct TrustedCa {
    pub secret_ref: String,
    pub fingerprint_sha256: String,
    pub fingerprint_sha1: String,
    pub subject_cn: String,
    pub not_after: String,
    pub trusted_at: String,
}
// Profile.trusted_cas: Vec<TrustedCa>
```

No new storage mechanism — persisted through the existing `store::upsert_profile` path used by
every other `Profile` field.

### Commands: `commands/ca_trust.rs`

- `discover_cluster_cas(profile_id) -> Vec<DiscoveredCaView>` — computes `CaTrustStatus` by
  comparing freshly-fetched fingerprints against `Profile.trusted_cas`. Returns only
  metadata (`secret_ref`, `source_hosts`, `fingerprint_sha256`, `subject_cn`, `not_after`,
  `status`) — **never the PEM** (not needed by the frontend for the trust flow itself; keeping it
  backend-only avoids a reason to ever pass certificate material over the Tauri IPC boundary at
  all, even though `ca.crt` itself is public, non-secret data).
- `trust_ca(profile_id, secret_ref) -> TrustedCa` — calls `services::ca_trust::trust_ca`,
  appends/updates `Profile.trusted_cas`, persists.
- `replace_ca(profile_id, secret_ref) -> TrustedCa` — rotation path: `untrust_ca(old)` then
  `trust_ca(new)`, updates the stored record. Exposed as a distinct command (not silently
  folded into `trust_ca`) so the frontend confirmation copy can say "replace" instead of "trust".

### UI: extend the existing endpoints section (`App.tsx:878-928`)

- Section header gets a CA status pill next to the existing endpoint-count pill: "N CA(s) need
  trust" / "CA trusted" / "CA changed — update available".
- Clicking it opens the existing `ConfirmModal` component (`src/components/ConfirmModal.tsx`,
  already shipped) showing subject CN + expiry + fingerprint, backed by `trust_ca` or
  `replace_ca`.
- Outcome reported through the existing `StatusBanner` (already renders on this view per the
  fix in commit `d4d143a`).
- Fingerprint comparison itself runs automatically whenever endpoints are (re)discovered —
  silent, no OS prompt. The OS Keychain authorization dialog (Touch ID/password) only appears
  after the user confirms in our own `ConfirmModal`, never unprompted.

## Data Flow

```
Discover Endpoints (existing)
  → discover_cluster_cas(profile_id)
      → resolve TLS secret ref per endpoint (ingress: direct; apisix: ApisixTls CRD)
      → kubectl get secret <ref> -o json   [full Secret incl. tls.key arrives in Rust memory —
                                             unavoidable, the K8s API has no field-level select]
      → extract `ca.crt` ONLY, drop the rest immediately, never log/serialize further
      → sha256 + sha1 fingerprint
      → dedupe by fingerprint_sha256
      → compare vs Profile.trusted_cas → New | Trusted | Rotated
  → UI shows status pill
  → user clicks → ConfirmModal → trust_ca / replace_ca
      → security add-trusted-cert (+ delete-certificate for replace) via CommandRunner
      → Profile.trusted_cas updated, persisted
```

## Security

- **tls.key never leaves backend memory.** The K8s API returns the whole `Secret.data` map for
  any `get secret` call (no server-side field selection exists); `ca_trust.rs` must extract
  `ca.crt` and drop the rest before returning anything to the command layer. Never log the raw
  Secret object at any log level.
- **Never send certificate/key material to the frontend** beyond the metadata listed above.
- **Temp PEM file** for `security add-trusted-cert`'s file argument: 0600, ClusterDeck-owned temp
  dir, unconditional cleanup on every exit path (same pattern as the `k8s_endpoints.rs` temp
  cert/key cleanup fixed in commit `d4d143a`).
- **`security` calls go through `CommandRunner`**, never a direct process spawn, per
  `AGENTS.md` architecture rule.
- **Deletion is scoped by SHA-1 fingerprint**, never by subject name — mirrors the
  `/etc/hosts` BEGIN/END marker and `~/.ssh/config` `Include`-block convention already in this
  repo: only ever touch what ClusterDeck itself added.
- Any `profile.id`-derived path/state key goes through `services/validate.rs` first (defense in
  depth, per the two prior CRITICAL findings already on record in `AGENTS.md`).

## Testing

- `FakeRunner` unit tests: fingerprint computation, dedupe, `CaTrustStatus` transitions
  (new/trusted/rotated), secret-ref resolution for `ingress` and `apisix` sources.
- One `#[ignore]`d real-exec test against a **throwaway test keychain** (never
  `login.keychain-db`) exercising actual `add-trusted-cert` / `delete-certificate` argv, per the
  existing project convention that `FakeRunner` cannot validate real external-command behavior
  (same rationale as the SSH `StrictHostKeyChecking` regression noted in `AGENTS.md`).
- Manual acceptance check in the running Tauri app: Trust CA → Keychain Access.app shows the
  entry under login keychain with "Always Trust" → open a discovered `https://` domain in
  Safari/Chrome with no warning. This is the actual user-stated acceptance criterion.

## Decisions

- **D1 — CA source: fetch from the K8s Secret backing the endpoint's TLS, not by hunting
  `ClusterIssuer`s.** Reason: cert-manager copies `ca.crt` into every leaf secret it issues from
  a CA-type issuer, confirmed against the live `vagrant-beluga` cluster
  (`beluga-internal-ca-issuer` → `apisix-gateway-tls-secret` both carry the same `ca.crt`). This
  is more portable than assuming cert-manager naming conventions and ties directly into
  discovery's existing resource references. Escape hatch: TLS-handshake chain extraction as a
  fallback when RBAC denies `get secrets`.
- **D2 — Trust target: macOS login keychain, not System keychain.** Reason (user-confirmed): only
  requirement is that browsers trust it; login keychain covers Safari/Chrome without requiring
  `sudo`/System-keychain admin password handling in the app. Escape hatch: none needed unless a
  future multi-user-machine requirement appears.
- **D3 — Rotation detection is automatic/silent; only the keychain mutation is
  user-confirmed.** Reason: fingerprint comparison is cheap and side-effect-free; an unprompted
  OS Touch ID/password dialog during routine "Discover Endpoints" would be surprising. Escape
  hatch: n/a.
- **D4 — Deletion/replacement scoped by SHA-1 fingerprint, never by CN/label.** Reason: mirrors
  the existing owned-block convention for `/etc/hosts` and `~/.ssh/config`; guarantees ClusterDeck
  never touches a cert it didn't add itself.
- **D5 — v1 skips `istio`/`gateway-api` discovery sources.** Reason: YAGNI — the only cluster
  exercised so far uses `apisix` exclusively; resolving TLS secret refs for those two sources
  needs an additional `Gateway` object query not currently in `k8s_endpoints.rs`. Escape hatch:
  add when a real cluster needs it; discovery already tags `source` per endpoint so this is an
  additive change, not a rework.
- **D6 — Storage reuses `Profile`/`store.rs`.** Reason: `trusted_cas` is per-cluster state,
  `Profile` is already the per-cluster record; no new storage mechanism needed.

## Open Question (unresolved, not blocking v1)

None outstanding — all forks above were confirmed with the user during brainstorming
(2026-09-21).
