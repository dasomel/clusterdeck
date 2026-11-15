# ADR-0014: Reveal service login credentials for discovered endpoints

Status: Proposed

## Context

ClusterDeck discovers ingress/gateway endpoints (`k8s_endpoints.rs`, `DiscoveredEndpoint { host, ip, source, resource_name: "<ns>/<name>" }`) so a user can open ArgoCD, Grafana, Keycloak, Harbor, Gitea, Superset and similar services from a freshly recreated cluster. The next thing that user needs is the service's initial admin login, which today means running `kubectl get secret … -o jsonpath | base64 -d` by hand for every service on every recreate.

Measured on two local clusters (names only, values not read):

| Service | Secret (namespace varies) | Key(s) |
|---|---|---|
| Argo CD | `argocd-initial-admin-secret` | `password` (user `admin`) |
| Harbor | `harbor-secrets` / chart `<release>-core` | `HARBOR_ADMIN_PASSWORD` (user `admin`) |
| Gitea | `gitea-admin` | `admin-password` (+ username key or chart value) |
| Keycloak | `keycloak-initial-admin` / bitnami `<release>` | `password` / `admin-password` (+ `username`) |
| Superset | `superset-credential` | `admin-password` (user `admin`) |
| Grafana (chart) | `<release>-grafana` | `admin-user`, `admin-password` |

Secret names and keys are deployment-specific, so detection must be a curated registry with fallbacks, not a guess.

This is a security-relevant change: AGENTS.md forbids exposing private-key contents to the frontend and logging secrets, and treats widening Tauri commands and filesystem/process access as design changes. A password shown in the UI is a deliberate, narrow exception that this ADR defines. It also stays inside the product boundary: ClusterDeck still does not list, edit or create accounts; it only reads an existing well-known login for an endpoint it already discovered.

## Decision

**D1 — Permission: the user's own credentials, checked twice.** Every call uses the profile's ClusterDeck-owned kubeconfig with `--kubeconfig <path> --context <ctx>` pinned for the whole operation (no ambient `KUBECONFIG`; the only fallback is the transport-level curl retry amended below). The kubeconfig is read once per action; those exact bytes are checked and copied to an exclusively created, owner-only (0600) temporary file under the ClusterDeck directory, shared by all kubectl calls and removed on exit. A kubeconfig whose selected user sets `as`, `as-groups` or `as-user-extra` (impersonation) is refused. `kubectl auth can-i get secret/<name> -n <ns>` runs first and only enables the action in the UI; the authoritative check is the GET itself, so a permission change between the two still fails closed. ClusterDeck never escalates or uses another credential: if the user could not run `kubectl get secret` themselves, ClusterDeck will not show it. (kubeconfig `exec` credential plugins run exactly as they already do for discovery; this ADR adds no new plugin trust.)

**D2 — Curated registry bound to the backend Service, one secret per endpoint.** Discovery records each endpoint's backend Service (`<ns>/<service>`) next to `resource_name`. A static Rust table maps a *known backend Service name* (e.g. `argocd-server`, `<release>-grafana`, `keycloak`, `harbor-core`, `gitea-http`, `superset`) to exactly one `(secret name, username key | fixed username, password key)` in the backend Service's namespace. Hostnames, Ingress names and annotations are never used to choose a secret, so a look-alike Ingress cannot borrow another service's password; the UI also labels the login with the Service name, not the hostname. The candidate is resolved before any API call, so each click performs at most one `can-i` and one Secret GET, plus bounded backend validation reads. Unknown services show no login action. Each registry entry is unit-tested.

**D2 amendment (2026-11-16).** APISIX upstream Domain nodes must identify one distinct Service using exactly `<service>.<namespace>.svc` or `<service>.<namespace>.svc.cluster.local`, with DNS-1123 labels. Cache `<route namespace>/<upstream name>` alongside that backend; the Secret belongs to the target Service's namespace. Both permission check and reveal re-read that upstream and require the same exact-form target, then GET the Service and reject missing (`not_found`) or ExternalName (`backend_mismatch`) targets. Changed or unresolved bindings return `backend_mismatch`; legacy cross-namespace caches without provenance require a rescan. All reads are bounded and share the pinned kubeconfig snapshot/context. Ingress and APISIX `serviceName` remain namespace-local and need only the live Service check. Measured aliases `harbor` and `keycloak-service` share existing conventions.

**D3 — Read on explicit action only; never persisted; fixed errors.** Nothing is fetched during Scan/Connect. The command takes `(profile_id, endpoint_id)` where `endpoint_id` refers to an endpoint the backend itself stored for that profile; the backend re-derives namespace, Service and secret from its own record and re-validates profile id, namespace and secret name with a new Kubernetes DNS-name validator in `validate.rs` before building a fixed argv. The value is decoded in Rust, returned once, and never written to `profiles.yaml`, `state.json`, `endpoints.json`, local-DNS files, evidence or logs. Every failure maps to a fixed code (`forbidden_caller`, `forbidden`, `not_found`, `backend_mismatch`, `unsupported_service`, `impersonation_refused`, `timeout`, `kubectl_failed`) and never carries `kubectl` stdout/stderr. The secret type is not `Debug`/`Display`-printable. Calls are bounded (`run_bounded`, 10 s, 64 KiB).

**D4 — UI handling.** Password masked by default with *Reveal* and *Copy*; username in clear. Each request carries a generation number and a response arriving after navigation, profile switch or a newer request is discarded. The value clears from component state when the card collapses, the user leaves the screen, or after 60 s. Copy writes to the clipboard only on click and, after 60 s, clears it only if it still holds that value; the UI notes that clipboard managers may keep a copy. No toast, banner or log line ever contains the value. The command is exposed only to the app's own main window.

**D5 — Scope.** Read-only, one secret per click, only for endpoints discovered for the selected profile, only namespaces of their resolved backend Services. No listing of secrets, no write/rotate, no bulk export.

**D1 amendment — curl retry on kubectl transport failure.** Measured in the bundled macOS app: Local Network Privacy blocks third-party binaries spawned by the app (Homebrew `kubectl`) from reaching LAN API servers, while Apple's `/usr/bin/curl` is exempt; discovery and verify already use curl for this reason. Only when `kubectl` fails at the transport level (spawn error, timeout, or non-success without an authoritative API answer such as `Forbidden`/`NotFound`/`can-i` `no`), the same call is retried once through system curl against the *same* pinned snapshot and the *same* selected context's cluster and user (the snapshot is reduced to that context in memory; TLS is verified against its CA, never `-k`). Reads are `GET` of the Service/ApisixUpstream/Secret; `can-i` becomes a `SelfSubjectAccessReview` `POST` (no secret in the body). Every curl call is bounded like kubectl (10 s, 64 KiB); temp cert/key files are 0600 and removed; a `kind: Status` body maps 403 to `forbidden` and 404 to `not_found`, anything else to the kubectl error code (fixed codes only, no output echoed). Impersonation is still refused before any call; users relying on `exec`/`auth-provider` plugins or lacking a certificate/token cannot be served by curl, so kubectl's own error code stands. Cost: a second request on transport failure and a curl-capable credential subset. Escape hatch: remove the curl path in `service_login/transport.rs` to return to kubectl-only.

**D1 amendment 2 — curl hardening (security review).** The shared curl helper (`k8s_endpoints.rs`) now: starts with `-q` so `~/.curlrc` cannot inject options; passes the bearer token through an owner-only (0600, exclusive) header file (`-H @file`) removed on every return path instead of argv, where `ps` shows it; resolves `curl` to `/usr/bin/curl` first (Homebrew curl is not Local-Network-Privacy exempt); fails closed when a kubeconfig CA, client certificate or key cannot be decoded or written instead of falling back to the system trust store or going anonymous; reads `tokenFile` in preference to `token` (client-go precedence, so curl and kubectl present the same identity); and pins the kubeconfig's `current-context` rather than `clusters[0]`/`users[0]`. kubectl `Unauthorized` is authoritative and never retried, like `Forbidden`/`NotFound`; the ApisixUpstream lookup retries via curl only on transport failure. Cost: a few more refusals on malformed kubeconfigs; escape hatch: none wanted for the security properties, the curl path as a whole stays removable per the first amendment.

**D4 amendment — clipboard clearing via change count.** WKWebView denies `navigator.clipboard.readText()` (measured: the value was still on the clipboard after 62 s), so "clear only if unchanged" cannot be decided in the webview. Rust exposes `clipboard_change_count` and `clear_clipboard_if_change_count(expected)` (main window only), implemented with `osascript` + AppleScriptObjC over `NSPasteboard.generalPasteboard`; the script is fixed text and the only variable is an `i64` formatted by Rust, and the secret never reaches Rust. The UI records the count right after `writeText` and clears after 60 s only if it still matches; if the count cannot be read the note says the clipboard will not auto-clear. Cost: an `osascript` spawn per call and a tiny window between write and count read; escape hatch: remove the two commands and the UI note falls back to "will not auto-clear".

## Accepted residual risks

- The curl path cannot present a client certificate and a bearer token together; when a kubeconfig user carries both, the token is omitted and the request fails (no credential exposure, kubectl's error stands).
- The clipboard change-count read and clear have millisecond race windows (between `writeText` and the count read, and between the check and the clear). Another writer landing inside a window could be cleared or leave the value un-cleared; accepted given the 60 s auto-clear is best-effort.

## Verification

- Unit: registry entries; impersonation refusal; forged `endpoint_id`; `can-i` no ⇒ no GET; GET forbidden after `can-i` yes ⇒ `forbidden`; every failure path returns only fixed codes (assert no stderr/stdout content leaks); late-response discard in the UI hook.
- Transport fallback (FakeRunner): kubectl `no route to host` reveals via pinned curl (CA, no `-k`, selected context only); curl Status 403/404/garbage map to fixed codes; authoritative kubectl `Forbidden` is never retried; SelfSubjectAccessReview allowed/denied; exec-plugin users keep kubectl's code and never reach curl; discovery's ApisixUpstream list falls back to curl.
- Real cluster: an admin kubeconfig reveals each registered service; a namespace-scoped ServiceAccount kubeconfig without `get secrets` gets `forbidden` and no value.

## Consequences

- Removes the manual `kubectl get secret | base64 -d` step after every recreate.
- New Tauri command (`reveal_endpoint_login(profile_id, endpoint)`) and new `kubectl` invocations, both through `CommandRunner` with argv built from validated namespace/name (`validate.rs`, Kubernetes DNS-label rules).
- Registry maintenance: a renamed chart secret silently shows no login until the registry is updated.
- The value transits the Tauri IPC boundary and the WebView's memory; mitigated by D3/D4, not eliminated.

**Escape hatch:** the command can be removed or gated behind a per-profile `allow_credential_reveal: false` without data migration because nothing is persisted.

## Alternatives considered

- *Show a copy-able `kubectl` command instead of the value* — zero secret transit, but keeps the manual step the feature exists to remove.
- *Match by hostname or Ingress name* — rejected: spoofable by anyone who can create an Ingress in the namespace (D2).
- *Gate on cluster-admin only* — simpler but wrong both ways: namespace admins are excluded, and cluster-admin is not required to read one secret. Per-secret `can-i` matches what the user can already do.
- *Read every known secret during Scan* — convenient, but fetches secrets nobody asked for and widens exposure; rejected by D3.
