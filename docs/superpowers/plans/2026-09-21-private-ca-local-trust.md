# Private Cluster CA Local Trust Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Let ClusterDeck fetch the CA certificate backing a discovered endpoint's TLS termination directly from the cluster, trust it in the macOS login keychain so Safari/Chrome stop warning on `*.local.beluga.internal`-style private domains, and detect + replace stale trust after a clean cluster reinstall rotates the CA.

**Architecture:** A new `services/ca_trust.rs` (pure functions + `CommandRunner`-based subprocess calls, mirroring `services/k8s_endpoints.rs`) resolves each discovered endpoint's backing TLS Secret, extracts `ca.crt`, fingerprints it, and drives `security add-trusted-cert`/`delete-certificate`. `Profile` gains a `trusted_cas` field persisted through the existing `store.rs`. A new `commands/ca_trust.rs` exposes three Tauri commands. The frontend extends the already-shipped "Discovered Cluster Endpoints" section (`src/App.tsx:878-928`) with a CA status pill wired to the already-shipped `ConfirmModal`/`StatusBanner`.

**Tech Stack:** Same Rust/Tauri/React/TypeScript stack. Two new Rust dependencies: `sha2` and `sha1` (RustCrypto, pure-Rust, no build script) for certificate fingerprinting — the fingerprint is the security-critical value so it is computed natively rather than parsed from CLI text output. No new frontend dependency.

**Spec:** `docs/superpowers/specs/2026-09-21-private-ca-local-trust-design.md` — read it first; this plan implements decisions D1-D6 recorded there.

## Global Constraints

- `tls.key` (or any Secret field other than `ca.crt`) must never be logged, serialized, or sent across the Tauri IPC boundary to the frontend. Extract `ca.crt` and drop the rest of the `Secret` JSON value immediately.
- All `security` and `openssl` subprocess calls go through `CommandRunner` (never a direct process spawn), per `AGENTS.md`'s architecture rule.
- Any temp file holding PEM material is created 0600 and unconditionally removed on every exit path (mirrors the `k8s_endpoints.rs` cleanup fix in commit `d4d143a`).
- CA trust/untrust in the keychain is scoped by SHA-1 fingerprint only, never by subject name or label — mirrors the `/etc/hosts` BEGIN/END marker and `~/.ssh/config` `Include`-block ownership convention already in this repo.
- `namespace`/`secret name` values that originate from cluster API responses are re-validated with `is_safe_host_domain` before being interpolated into a `kubectl get --raw` API path string, per `AGENTS.md`'s "re-check defensively at the sink" rule.
- v1 only resolves TLS secret refs for `ingress` and `apisix`-sourced `DiscoveredEndpoint`s (D5). `istio`/`gateway-api`-sourced endpoints are skipped (not an error).
- `cargo fmt --all -- --check`, `cargo clippy --all-targets --all-features -- -D warnings`, `cargo test --all-targets --all-features`, and `pnpm build` (i.e. `make verify`) must pass after every task.
- Test fixtures use a locally-generated throwaway self-signed cert (`CN=clusterdeck-test-ca.invalid`) — never real cluster data, per `AGENTS.md` security rules.

---

## File Structure

```text
src-tauri/
  Cargo.toml                       # MODIFY: + sha2, sha1
  src/
    services/
      ca_trust.rs                  # NEW: fingerprinting, secret-ref resolution, discovery
                                    #      orchestration, security(1) trust/untrust, CaTrustStatus
      k8s_endpoints.rs              # MODIFY: decode_base64 and write_owner_only_file -> pub(crate)
      config.rs                     # MODIFY: Profile.trusted_cas field
      store.rs                      # MODIFY: ProfileBody + load/save mapping for trusted_cas
      mod.rs                        # MODIFY: + pub mod ca_trust;
    commands/
      ca_trust.rs                   # NEW: discover_cluster_cas_cmd, trust_ca_cmd, replace_ca_cmd
      mod.rs                        # MODIFY: + pub mod ca_trust;
    lib.rs                          # MODIFY: register the 3 new commands
docs/
  adr/
    0006-private-ca-local-trust.md  # NEW: short ADR pointing at the spec
src/
  api/tauri.ts                      # MODIFY: DiscoveredCaView, TrustedCa, CaTrustStatus types + calls
  App.tsx                           # MODIFY: CA status pill + ConfirmModal wiring in the endpoints section
```

## Global Interfaces

```rust
// services/ca_trust.rs
pub struct DiscoveredCaMeta {
    pub pem: String,
    pub fingerprint_sha256: String,
    pub fingerprint_sha1: String,
    pub subject_cn: String,
    pub not_after: String,
}

pub struct DiscoveredCa {
    pub secret_ref: String,        // "namespace/name"
    pub source_hosts: Vec<String>,
    pub meta: DiscoveredCaMeta,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CaTrustStatus { New, Trusted, Rotated }

pub struct TlsSecretRef { pub namespace: String, pub name: String }

// config.rs
pub struct TrustedCa {
    pub secret_ref: String,
    pub fingerprint_sha256: String,
    pub fingerprint_sha1: String,
    pub subject_cn: String,
    pub not_after: String,
    pub trusted_at: String,
}
```

```typescript
// src/api/tauri.ts
export type CaTrustStatus = 'new' | 'trusted' | 'rotated';
export type DiscoveredCaView = {
  secret_ref: string;
  source_hosts: string[];
  subject_cn: string;
  not_after: string;
  fingerprint_sha256: string;
  status: CaTrustStatus;
};
export type TrustedCa = {
  secret_ref: string;
  fingerprint_sha256: string;
  fingerprint_sha1: string;
  subject_cn: string;
  not_after: string;
  trusted_at: string;
};
```

---

### Task 1: PEM → DER + SHA fingerprints

**Files:**
- Modify: `src-tauri/Cargo.toml`
- Modify: `src-tauri/src/services/k8s_endpoints.rs:21` (`fn decode_base64` → `pub(crate) fn decode_base64`)
- Modify: `src-tauri/src/services/mod.rs` (+ `pub mod ca_trust;`)
- Create: `src-tauri/src/services/ca_trust.rs`

**Interfaces:**
- Produces: `pub fn pem_to_der(pem: &str) -> Result<Vec<u8>, String>`, `pub fn fingerprint_hex_sha256(der: &[u8]) -> String`, `pub fn fingerprint_hex_sha1(der: &[u8]) -> String`

- [ ] **Step 1: Add fingerprint dependencies**

In `src-tauri/Cargo.toml`, under `[dependencies]`, add:

```toml
sha2 = "0.10"
sha1 = "0.10"
```

- [ ] **Step 2: Expose `decode_base64` to the new module**

In `src-tauri/src/services/k8s_endpoints.rs:21`, change:

```rust
fn decode_base64(input: &str) -> Option<Vec<u8>> {
```

to:

```rust
pub(crate) fn decode_base64(input: &str) -> Option<Vec<u8>> {
```

- [ ] **Step 3: Register the new module**

In `src-tauri/src/services/mod.rs`, add (alphabetical, after `pub mod config;`... actually insert to keep the existing alphabetical order):

```rust
pub mod ca_trust;
```

(goes before `pub mod config;` alphabetically)

- [ ] **Step 4: Write the failing test**

Create `src-tauri/src/services/ca_trust.rs` with:

```rust
#![allow(dead_code)]

use sha1::Sha1;
use sha2::{Digest, Sha256};

use crate::services::k8s_endpoints::decode_base64;

pub fn pem_to_der(pem: &str) -> Result<Vec<u8>, String> {
    let body: String = pem
        .lines()
        .filter(|line| !line.trim().is_empty() && !line.starts_with("-----"))
        .collect::<Vec<_>>()
        .join("");
    if body.is_empty() {
        return Err("empty PEM body".to_string());
    }
    decode_base64(&body).ok_or_else(|| "failed to base64-decode PEM body".to_string())
}

fn to_hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

pub fn fingerprint_hex_sha256(der: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(der);
    to_hex(&hasher.finalize())
}

pub fn fingerprint_hex_sha1(der: &[u8]) -> String {
    let mut hasher = Sha1::new();
    hasher.update(der);
    to_hex(&hasher.finalize())
}

#[cfg(test)]
mod tests {
    use super::*;

    // Locally generated throwaway self-signed cert (`openssl req -x509 -newkey rsa:2048 -nodes
    // -subj "/CN=clusterdeck-test-ca.invalid" -days 3650`) -- NOT real infrastructure data.
    const TEST_CA_PEM: &str = "-----BEGIN CERTIFICATE-----\nMIICyDCCAbACCQC5RXP4GnoQ0TANBgkqhkiG9w0BAQsFADAmMSQwIgYDVQQDDBtj\nbHVzdGVyZGVjay10ZXN0LWNhLmludmFsaWQwHhcNMjYwOTIxMDU0MDQ3WhcNMzYw\nOTE4MDU0MDQ3WjAmMSQwIgYDVQQDDBtjbHVzdGVyZGVjay10ZXN0LWNhLmludmFs\naWQwggEiMA0GCSqGSIb3DQEBAQUAA4IBDwAwggEKAoIBAQDsDV5ks2ty7cdofHOB\n2gvZDKeauuipTN9l212c8EADzkk03XZU7Nq7ANIJb9MWEcP/uVqDI6jDNQhHwAOR\nItKTrXCm4CDqFv8SOyNqodXOsOek6c9Sxcbgip/N4RDHQsL8Ve36pjT9QsgwMGaR\nJyfmJ+iYOVK/0BdAEor5A6Ts8FsULFcNa8n/aWGJ1UqVC4JHX210iSGUdTxNzAiF\nujUeb3ghW5dRCw2ZSpRXMOYrY0u870xyuHNSTM3MO1E6yyI0n8B399t89wqroldl\nq6/kQVKkbqCXNMIpJQPk3KjBIpt1Pq8+9qk9TA7uxSeOV3ZZLzZaaKr0s6bI0xuC\n3HzLAgMBAAEwDQYJKoZIhvcNAQELBQADggEBAJlp7DkX5Z+IpRdeTx/t2mHjYt4h\n/LjNjIDxYuvP3Q1plI/FNS1/J5kd6v6ysX/E6NOHkuCW6Fiknknqxgzh7k7K89e5\n7e9OTldpl1AE90jMhJlGiqbojOTuN0OqtSd8VrgEXN+ohxySiG4P+W/StdF5CYej\n+8MY+aF/uejphebUGvRVGM+JXfVzLmX4JjXgxLTJ1+Ezu/HdHoevj+gMMjIX5E+Q\ncZ/asV1CX/0km7a1w/YOVCkNPWjQlUZR0PCdm8xsrMz1Z6Lz2hGuAiSh7Y1G+xCG\nO162GvGCb2ptICZJOr8p+rB/0SLmt+iwT9m/9SoM/r6Amr3wk2wvG1InM4A=\n-----END CERTIFICATE-----\n";

    #[test]
    fn pem_to_der_and_fingerprint_match_known_values() {
        let der = pem_to_der(TEST_CA_PEM).expect("valid PEM");
        assert_eq!(
            fingerprint_hex_sha256(&der),
            "8b75bf97e19ce7efe9bb4d6c76b4f10e072a9d6ea89d8f438f423afea7156246"
        );
        assert_eq!(
            fingerprint_hex_sha1(&der),
            "67fc8cc8df72476829ecd88d188331a6d29baabb"
        );
    }

    #[test]
    fn pem_to_der_rejects_empty_input() {
        assert!(pem_to_der("").is_err());
        assert!(pem_to_der("-----BEGIN CERTIFICATE-----\n-----END CERTIFICATE-----\n").is_err());
    }
}
```

- [ ] **Step 5: Run the test**

Run: `cd src-tauri && cargo test --lib services::ca_trust -- --nocapture`
Expected: PASS (the implementation is written in the same step as the test above since the fingerprint values were independently verified via `openssl`/`shasum` before writing this plan — there is no separate red/green cycle for pure hash-comparison code with pre-known-correct expected values). If it fails, the fixture PEM was mistyped when copied into this file — diff it byte-for-byte against this plan.

- [ ] **Step 6: Commit**

```bash
cd /Users/m/Documents/IdeaProjects/20.dasomel/clusterdeck
git add src-tauri/Cargo.toml src-tauri/Cargo.lock src-tauri/src/services/ca_trust.rs src-tauri/src/services/k8s_endpoints.rs src-tauri/src/services/mod.rs
git commit -m "feat(ca-trust): add PEM/DER conversion and SHA-256/SHA-1 fingerprinting"
```

### Task 2: Cosmetic cert metadata (subject CN / expiry) via `openssl`

**Files:**
- Modify: `src-tauri/src/services/k8s_endpoints.rs:76` (`fn write_owner_only_file` → `pub(crate) fn write_owner_only_file`, both the `#[cfg(unix)]` and `#[cfg(not(unix))]` variants)
- Modify: `src-tauri/src/services/ca_trust.rs`

**Interfaces:**
- Consumes: nothing new from Task 1 (same file)
- Produces: `pub async fn extract_cert_metadata(runner: &dyn CommandRunner, pem: &str) -> (String, String)` — `(subject_cn, not_after)`, both empty string on any failure (never errors — this is cosmetic display data only, per the spec's Security section)

- [ ] **Step 1: Expose `write_owner_only_file`**

In `src-tauri/src/services/k8s_endpoints.rs`, change both:
```rust
#[cfg(unix)]
fn write_owner_only_file(path: &Path, contents: &[u8]) -> std::io::Result<()> {
```
and
```rust
#[cfg(not(unix))]
fn write_owner_only_file(path: &Path, contents: &[u8]) -> std::io::Result<()> {
```
to `pub(crate) fn write_owner_only_file(...)`.

- [ ] **Step 2: Write the failing tests (pure parser functions first)**

Add to `src-tauri/src/services/ca_trust.rs`, above the existing `#[cfg(test)]` block:

```rust
use crate::services::process::CommandRunner;
use crate::services::k8s_endpoints::write_owner_only_file;

fn parse_openssl_subject_cn(output: &str) -> String {
    match output.find("CN") {
        Some(idx) => {
            let rest = &output[idx + 2..];
            let rest = rest.trim_start_matches([' ', '=']);
            let end = rest.find(['/', ',', '\n']).unwrap_or(rest.len());
            rest[..end].trim().to_string()
        }
        None => String::new(),
    }
}

fn parse_openssl_enddate(output: &str) -> String {
    output
        .trim()
        .strip_prefix("notAfter=")
        .map(|v| v.trim().to_string())
        .unwrap_or_default()
}
```

Add these test cases inside the existing `mod tests` block (after the Task 1 tests):

```rust
    #[test]
    fn parse_openssl_subject_cn_handles_legacy_and_modern_formats() {
        assert_eq!(
            parse_openssl_subject_cn("subject= /CN=clusterdeck-test-ca.invalid"),
            "clusterdeck-test-ca.invalid"
        );
        assert_eq!(
            parse_openssl_subject_cn("subject=CN = clusterdeck-test-ca.invalid"),
            "clusterdeck-test-ca.invalid"
        );
        assert_eq!(
            parse_openssl_subject_cn("subject=O = Acme, CN = foo.internal"),
            "foo.internal"
        );
        assert_eq!(parse_openssl_subject_cn(""), "");
    }

    #[test]
    fn parse_openssl_enddate_strips_prefix() {
        assert_eq!(
            parse_openssl_enddate("notAfter=Sep 18 05:40:47 2036 GMT"),
            "Sep 18 05:40:47 2036 GMT"
        );
        assert_eq!(parse_openssl_enddate("garbage"), "");
    }
```

- [ ] **Step 3: Run, verify the two parser tests pass**

Run: `cd src-tauri && cargo test --lib services::ca_trust::tests::parse_openssl -- --nocapture`
Expected: PASS (pure string functions, implemented alongside their tests above).

- [ ] **Step 4: Implement and test the async wrapper**

Add to `src-tauri/src/services/ca_trust.rs` (non-test code, after `fingerprint_hex_sha1`):

```rust
pub async fn extract_cert_metadata(runner: &dyn CommandRunner, pem: &str) -> (String, String) {
    let now_nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let temp_pem = std::env::temp_dir().join(format!("cd_ca_meta_{now_nanos}.pem"));

    if write_owner_only_file(&temp_pem, pem.as_bytes()).is_err() {
        return (String::new(), String::new());
    }
    let temp_pem_str = temp_pem.to_string_lossy().to_string();

    let subject_out = runner
        .run(
            "openssl",
            &[
                "x509".to_string(),
                "-noout".to_string(),
                "-subject".to_string(),
                "-in".to_string(),
                temp_pem_str.clone(),
            ],
        )
        .await
        .ok()
        .filter(|o| o.success)
        .map(|o| o.stdout)
        .unwrap_or_default();

    let enddate_out = runner
        .run(
            "openssl",
            &[
                "x509".to_string(),
                "-noout".to_string(),
                "-enddate".to_string(),
                "-in".to_string(),
                temp_pem_str,
            ],
        )
        .await
        .ok()
        .filter(|o| o.success)
        .map(|o| o.stdout)
        .unwrap_or_default();

    // Unconditional: metadata extraction is best-effort, but the temp PEM must never linger.
    let _ = std::fs::remove_file(&temp_pem);

    (
        parse_openssl_subject_cn(&subject_out),
        parse_openssl_enddate(&enddate_out),
    )
}
```

Add these tests inside `mod tests`:

```rust
    struct FakeOpensslRunner;

    #[async_trait::async_trait]
    impl CommandRunner for FakeOpensslRunner {
        async fn run(
            &self,
            bin: &str,
            args: &[String],
        ) -> Result<crate::services::process::CommandOutput, String> {
            assert_eq!(bin, "openssl");
            if args.contains(&"-subject".to_string()) {
                return Ok(crate::services::process::CommandOutput {
                    stdout: "subject=CN = clusterdeck-test-ca.invalid".to_string(),
                    stderr: String::new(),
                    success: true,
                });
            }
            if args.contains(&"-enddate".to_string()) {
                return Ok(crate::services::process::CommandOutput {
                    stdout: "notAfter=Sep 18 05:40:47 2036 GMT".to_string(),
                    stderr: String::new(),
                    success: true,
                });
            }
            Ok(crate::services::process::CommandOutput {
                stdout: String::new(),
                stderr: "unexpected args".to_string(),
                success: false,
            })
        }
    }

    #[tokio::test]
    async fn extract_cert_metadata_parses_subject_and_enddate() {
        let (cn, not_after) = extract_cert_metadata(&FakeOpensslRunner, TEST_CA_PEM).await;
        assert_eq!(cn, "clusterdeck-test-ca.invalid");
        assert_eq!(not_after, "Sep 18 05:40:47 2036 GMT");
    }

    #[tokio::test]
    async fn extract_cert_metadata_returns_empty_strings_on_openssl_failure() {
        struct FailingRunner;
        #[async_trait::async_trait]
        impl CommandRunner for FailingRunner {
            async fn run(
                &self,
                _bin: &str,
                _args: &[String],
            ) -> Result<crate::services::process::CommandOutput, String> {
                Ok(crate::services::process::CommandOutput {
                    stdout: String::new(),
                    stderr: "not found".to_string(),
                    success: false,
                })
            }
        }
        let (cn, not_after) = extract_cert_metadata(&FailingRunner, TEST_CA_PEM).await;
        assert_eq!(cn, "");
        assert_eq!(not_after, "");
    }
```

- [ ] **Step 5: Run all ca_trust tests**

Run: `cd src-tauri && cargo test --lib services::ca_trust -- --nocapture`
Expected: PASS, 6 tests total (2 from Task 1, 4 from this task).

- [ ] **Step 6: Commit**

```bash
cd /Users/m/Documents/IdeaProjects/20.dasomel/clusterdeck
git add src-tauri/src/services/ca_trust.rs src-tauri/src/services/k8s_endpoints.rs
git commit -m "feat(ca-trust): extract subject CN and expiry via openssl (best-effort)"
```

### Task 3: SNI wildcard matching

**Files:**
- Modify: `src-tauri/src/services/ca_trust.rs`

**Interfaces:**
- Produces: `pub fn sni_matches_host(sni: &str, host: &str) -> bool`

- [ ] **Step 1: Write the failing test**

Add to `mod tests` in `src-tauri/src/services/ca_trust.rs`:

```rust
    #[test]
    fn sni_matches_host_handles_exact_and_wildcard() {
        assert!(sni_matches_host("local.beluga.internal", "local.beluga.internal"));
        assert!(sni_matches_host("*.local.beluga.internal", "argocd.local.beluga.internal"));
        assert!(!sni_matches_host("*.local.beluga.internal", "local.beluga.internal"));
        assert!(!sni_matches_host("*.local.beluga.internal", "a.b.local.beluga.internal"));
        assert!(!sni_matches_host("*.local.beluga.internal", "evillocal.beluga.internal"));
        assert!(!sni_matches_host("*.local.beluga.internal", "argocd.other.internal"));
    }
```

- [ ] **Step 2: Run, verify it fails to compile (function undefined)**

Run: `cd src-tauri && cargo test --lib services::ca_trust`
Expected: compile error, `cannot find function sni_matches_host`

- [ ] **Step 3: Implement**

Add to `src-tauri/src/services/ca_trust.rs` (non-test code):

```rust
/// A wildcard SNI covers exactly one label (standard TLS cert semantics): `*.example.com`
/// matches `foo.example.com` but not `example.com` itself or `a.b.example.com`.
pub fn sni_matches_host(sni: &str, host: &str) -> bool {
    if sni == host {
        return true;
    }
    let suffix = match sni.strip_prefix("*.") {
        Some(s) => s,
        None => return false,
    };
    let remainder = match host.strip_suffix(suffix) {
        Some(r) => r,
        None => return false,
    };
    match remainder.strip_suffix('.') {
        Some(label) => !label.is_empty() && !label.contains('.'),
        None => false,
    }
}
```

- [ ] **Step 4: Run, verify it passes**

Run: `cd src-tauri && cargo test --lib services::ca_trust::tests::sni_matches_host -- --nocapture`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
cd /Users/m/Documents/IdeaProjects/20.dasomel/clusterdeck
git add src-tauri/src/services/ca_trust.rs
git commit -m "feat(ca-trust): add single-label wildcard SNI matching"
```

### Task 4: TLS secret-ref resolution for `ingress` and `apisix` sources

**Files:**
- Modify: `src-tauri/src/services/ca_trust.rs`

**Interfaces:**
- Consumes: `sni_matches_host` (Task 3)
- Produces: `pub struct TlsSecretRef { pub namespace: String, pub name: String }`, `pub fn resolve_ingress_secret_ref(ingresses: &serde_json::Value, host: &str) -> Option<TlsSecretRef>`, `pub fn resolve_apisixtls_secret_ref(apisixtls: &serde_json::Value, host: &str) -> Option<TlsSecretRef>`

- [ ] **Step 1: Write the failing tests**

Add to `mod tests`:

```rust
    #[test]
    fn resolve_ingress_secret_ref_matches_host_in_tls_block() {
        let json: serde_json::Value = serde_json::from_str(
            r#"{
            "items": [{
                "metadata": { "name": "web", "namespace": "apps" },
                "spec": {
                    "tls": [{ "hosts": ["app.example.internal"], "secretName": "web-tls" }],
                    "rules": [{ "host": "app.example.internal" }]
                }
            }]
        }"#,
        )
        .unwrap();
        let found = resolve_ingress_secret_ref(&json, "app.example.internal").unwrap();
        assert_eq!(found.namespace, "apps");
        assert_eq!(found.name, "web-tls");
        assert!(resolve_ingress_secret_ref(&json, "other.example.internal").is_none());
    }

    #[test]
    fn resolve_apisixtls_secret_ref_matches_wildcard_sni() {
        let json: serde_json::Value = serde_json::from_str(
            r#"{
            "items": [{
                "metadata": { "name": "apisix-gateway-tls", "namespace": "platform-system" },
                "spec": {
                    "snis": ["*.local.beluga.internal", "local.beluga.internal"],
                    "secret": { "name": "apisix-gateway-tls-secret", "namespace": "platform-system" }
                }
            }]
        }"#,
        )
        .unwrap();
        let found = resolve_apisixtls_secret_ref(&json, "argocd.local.beluga.internal").unwrap();
        assert_eq!(found.namespace, "platform-system");
        assert_eq!(found.name, "apisix-gateway-tls-secret");
        assert!(resolve_apisixtls_secret_ref(&json, "unrelated.example.com").is_none());
    }
```

Note: these fixture shapes are independently confirmed against a real cluster (`kubectl get apisixtls -A -o jsonpath=...` was run against a live cert-manager + APISIX ingress-controller install during design) — `spec.snis` (not `spec.hosts`) and `spec.secret.{name,namespace}` are the real `ApisixTls` CRD field names.

- [ ] **Step 2: Run, verify compile failure**

Run: `cd src-tauri && cargo test --lib services::ca_trust`
Expected: compile error, functions/struct undefined

- [ ] **Step 3: Implement**

Add to `src-tauri/src/services/ca_trust.rs` (non-test code):

```rust
pub struct TlsSecretRef {
    pub namespace: String,
    pub name: String,
}

pub fn resolve_ingress_secret_ref(ingresses: &serde_json::Value, host: &str) -> Option<TlsSecretRef> {
    let items = ingresses.get("items")?.as_array()?;
    for item in items {
        let ns = match item
            .get("metadata")
            .and_then(|m| m.get("namespace"))
            .and_then(|n| n.as_str())
        {
            Some(ns) => ns,
            None => continue,
        };
        let tls_list = match item
            .get("spec")
            .and_then(|s| s.get("tls"))
            .and_then(|t| t.as_array())
        {
            Some(list) => list,
            None => continue,
        };
        for tls in tls_list {
            let secret_name = match tls.get("secretName").and_then(|s| s.as_str()) {
                Some(s) => s,
                None => continue,
            };
            let hosts = match tls.get("hosts").and_then(|h| h.as_array()) {
                Some(h) => h,
                None => continue,
            };
            if hosts.iter().any(|h| h.as_str() == Some(host)) {
                return Some(TlsSecretRef {
                    namespace: ns.to_string(),
                    name: secret_name.to_string(),
                });
            }
        }
    }
    None
}

pub fn resolve_apisixtls_secret_ref(apisixtls: &serde_json::Value, host: &str) -> Option<TlsSecretRef> {
    let items = apisixtls.get("items")?.as_array()?;
    for item in items {
        let spec = match item.get("spec") {
            Some(s) => s,
            None => continue,
        };
        let secret = match spec.get("secret") {
            Some(s) => s,
            None => continue,
        };
        let ns = secret.get("namespace").and_then(|n| n.as_str());
        let name = secret.get("name").and_then(|n| n.as_str());
        let (ns, name) = match (ns, name) {
            (Some(ns), Some(name)) => (ns, name),
            _ => continue,
        };
        let snis = match spec.get("snis").and_then(|s| s.as_array()) {
            Some(s) => s,
            None => continue,
        };
        let matched = snis
            .iter()
            .any(|s| s.as_str().map(|s| sni_matches_host(s, host)).unwrap_or(false));
        if matched {
            return Some(TlsSecretRef {
                namespace: ns.to_string(),
                name: name.to_string(),
            });
        }
    }
    None
}
```

- [ ] **Step 4: Run, verify all pass**

Run: `cd src-tauri && cargo test --lib services::ca_trust -- --nocapture`
Expected: PASS, 9 tests total.

- [ ] **Step 5: Commit**

```bash
cd /Users/m/Documents/IdeaProjects/20.dasomel/clusterdeck
git add src-tauri/src/services/ca_trust.rs
git commit -m "feat(ca-trust): resolve TLS secret refs for ingress and apisix sources"
```

### Task 5: Fetch-and-fingerprint a CA, and batch discovery across endpoints

**Files:**
- Modify: `src-tauri/src/services/ca_trust.rs`

**Interfaces:**
- Consumes: `pem_to_der`, `fingerprint_hex_sha256`, `fingerprint_hex_sha1` (Task 1); `extract_cert_metadata` (Task 2); `resolve_ingress_secret_ref`, `resolve_apisixtls_secret_ref` (Task 4); `k8s_endpoints::{query_k8s_api_json, DiscoveredEndpoint}`; `validate::is_safe_host_domain`
- Produces: `pub struct DiscoveredCaMeta { pub pem: String, pub fingerprint_sha256: String, pub fingerprint_sha1: String, pub subject_cn: String, pub not_after: String }`, `pub struct DiscoveredCa { pub secret_ref: String, pub source_hosts: Vec<String>, pub meta: DiscoveredCaMeta }`, `pub async fn fetch_ca(runner: &dyn CommandRunner, kubeconfig_path: &Path, namespace: &str, name: &str) -> Result<DiscoveredCaMeta, String>`, `pub async fn discover_cluster_cas(runner: &dyn CommandRunner, kubeconfig_path: &Path, endpoints: &[DiscoveredEndpoint]) -> Result<Vec<DiscoveredCa>, String>`

This is the task where a Secret's full JSON (including `tls.key` when present) briefly enters process memory — read the Security section of the spec again before writing `fetch_ca`. The block-scoped extraction below is deliberate: it ends the borrow on `secret_json` before `drop(secret_json)`, so nothing after that point can reach `tls.key`.

- [ ] **Step 1: Add imports**

At the top of `src-tauri/src/services/ca_trust.rs`, add:

```rust
use std::collections::BTreeMap;
use std::path::Path;

use crate::services::k8s_endpoints::{query_k8s_api_json, DiscoveredEndpoint};
use crate::services::validate::is_safe_host_domain;
```

- [ ] **Step 2: Write the failing tests**

Add to `mod tests` (needs `use base64::prelude::*;` added to the `use super::*;` line's neighborhood inside the test module):

```rust
    use base64::prelude::*;

    fn ok_output(stdout: &str) -> crate::services::process::CommandOutput {
        crate::services::process::CommandOutput {
            stdout: stdout.to_string(),
            stderr: String::new(),
            success: true,
        }
    }

    struct FakeDiscoverCaRunner;

    #[async_trait::async_trait]
    impl CommandRunner for FakeDiscoverCaRunner {
        async fn run(
            &self,
            bin: &str,
            args: &[String],
        ) -> Result<crate::services::process::CommandOutput, String> {
            if bin == "kubectl" {
                if let Some(path_idx) = args.iter().position(|a| a == "--raw") {
                    let raw_path = &args[path_idx + 1];
                    if raw_path == "/apis/networking.k8s.io/v1/ingresses" {
                        return Ok(ok_output(r#"{"items": []}"#));
                    }
                    if raw_path == "/apis/apisix.apache.org/v2/apisixtlses" {
                        return Ok(ok_output(
                            r#"{
                            "items": [{
                                "metadata": { "name": "apisix-gateway-tls", "namespace": "platform-system" },
                                "spec": {
                                    "snis": ["*.local.beluga.internal", "local.beluga.internal"],
                                    "secret": { "name": "apisix-gateway-tls-secret", "namespace": "platform-system" }
                                }
                            }]
                        }"#,
                        ));
                    }
                    if raw_path
                        == "/api/v1/namespaces/platform-system/secrets/apisix-gateway-tls-secret"
                    {
                        let ca_crt_b64 = BASE64_STANDARD.encode(TEST_CA_PEM.as_bytes());
                        return Ok(ok_output(&format!(
                            r#"{{"data": {{"ca.crt": "{ca_crt_b64}", "tls.crt": "unused", "tls.key": "unused"}}}}"#
                        )));
                    }
                }
                return Ok(crate::services::process::CommandOutput {
                    stdout: "{}".to_string(),
                    stderr: "NotFound".to_string(),
                    success: false,
                });
            }
            if bin == "openssl" {
                if args.contains(&"-subject".to_string()) {
                    return Ok(ok_output("subject=CN = clusterdeck-test-ca.invalid"));
                }
                if args.contains(&"-enddate".to_string()) {
                    return Ok(ok_output("notAfter=Sep 18 05:40:47 2036 GMT"));
                }
            }
            Ok(crate::services::process::CommandOutput {
                stdout: String::new(),
                stderr: format!("unexpected command: {bin}"),
                success: false,
            })
        }
    }

    #[tokio::test]
    async fn discover_cluster_cas_dedupes_apisix_endpoints_to_one_secret() {
        let endpoints = vec![
            DiscoveredEndpoint {
                host: "argocd.local.beluga.internal".to_string(),
                ip: "192.168.77.200".to_string(),
                source: "apisix".to_string(),
                resource_name: "platform-system/argocd".to_string(),
            },
            DiscoveredEndpoint {
                host: "sso.local.beluga.internal".to_string(),
                ip: "192.168.77.200".to_string(),
                source: "apisix".to_string(),
                resource_name: "iam/keycloak".to_string(),
            },
        ];
        let temp_kc = std::env::temp_dir().join("test-ca-trust-dummy-kc.yaml");
        let _ = std::fs::write(&temp_kc, "dummy");

        let result = discover_cluster_cas(&FakeDiscoverCaRunner, &temp_kc, &endpoints)
            .await
            .unwrap();
        let _ = std::fs::remove_file(&temp_kc);

        assert_eq!(result.len(), 1);
        assert_eq!(
            result[0].secret_ref,
            "platform-system/apisix-gateway-tls-secret"
        );
        assert_eq!(result[0].source_hosts.len(), 2);
        assert!(result[0]
            .source_hosts
            .contains(&"argocd.local.beluga.internal".to_string()));
        assert!(result[0]
            .source_hosts
            .contains(&"sso.local.beluga.internal".to_string()));
        assert_eq!(
            result[0].meta.fingerprint_sha256,
            "8b75bf97e19ce7efe9bb4d6c76b4f10e072a9d6ea89d8f438f423afea7156246"
        );
        assert_eq!(result[0].meta.subject_cn, "clusterdeck-test-ca.invalid");
    }

    #[tokio::test]
    async fn fetch_ca_errors_when_secret_missing() {
        struct EmptyRunner;
        #[async_trait::async_trait]
        impl CommandRunner for EmptyRunner {
            async fn run(
                &self,
                _bin: &str,
                _args: &[String],
            ) -> Result<crate::services::process::CommandOutput, String> {
                Ok(ok_output(r#"{"code": 404, "reason": "NotFound"}"#))
            }
        }
        let temp_kc = std::env::temp_dir().join("test-ca-trust-dummy-kc-2.yaml");
        let _ = std::fs::write(&temp_kc, "dummy");
        let result = fetch_ca(&EmptyRunner, &temp_kc, "platform-system", "missing-secret").await;
        let _ = std::fs::remove_file(&temp_kc);
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn fetch_ca_rejects_unsafe_namespace_or_name_before_running_any_command() {
        struct UnusedRunner;
        #[async_trait::async_trait]
        impl CommandRunner for UnusedRunner {
            async fn run(
                &self,
                _bin: &str,
                _args: &[String],
            ) -> Result<crate::services::process::CommandOutput, String> {
                panic!("should not be called -- validation must reject before any command runs");
            }
        }
        let temp_kc = std::env::temp_dir().join("test-ca-trust-dummy-kc-3.yaml");
        let _ = std::fs::write(&temp_kc, "dummy");
        let result = fetch_ca(&UnusedRunner, &temp_kc, "../../etc", "passwd").await;
        let _ = std::fs::remove_file(&temp_kc);
        assert!(result.is_err());
    }
```

- [ ] **Step 3: Run, verify compile failure**

Run: `cd src-tauri && cargo test --lib services::ca_trust`
Expected: compile error, `fetch_ca`/`discover_cluster_cas`/`DiscoveredCa`/`DiscoveredCaMeta` undefined

- [ ] **Step 4: Implement**

Add to `src-tauri/src/services/ca_trust.rs` (non-test code):

```rust
pub struct DiscoveredCaMeta {
    pub pem: String,
    pub fingerprint_sha256: String,
    pub fingerprint_sha1: String,
    pub subject_cn: String,
    pub not_after: String,
}

pub struct DiscoveredCa {
    pub secret_ref: String,
    pub source_hosts: Vec<String>,
    pub meta: DiscoveredCaMeta,
}

pub async fn fetch_ca(
    runner: &dyn CommandRunner,
    kubeconfig_path: &Path,
    namespace: &str,
    name: &str,
) -> Result<DiscoveredCaMeta, String> {
    // Defensive re-check at this sink before the values reach a `kubectl get --raw` API path
    // string, per AGENTS.md -- even though namespace/name here came from a prior cluster API
    // response, not raw user input.
    if !is_safe_host_domain(namespace) || !is_safe_host_domain(name) {
        return Err(format!("unsafe secret ref: {namespace}/{name}"));
    }

    let api_path = format!("/api/v1/namespaces/{namespace}/secrets/{name}");
    let secret_json = query_k8s_api_json(runner, kubeconfig_path, &api_path)
        .await?
        .ok_or_else(|| format!("secret {namespace}/{name} not found"))?;

    // Block-scoped: the borrow on `secret_json` ends here, before `drop(secret_json)` below.
    // Nothing past this point can reach `tls.key` or any other field of the Secret.
    let pem = {
        let ca_crt_b64 = secret_json
            .get("data")
            .and_then(|d| d.get("ca.crt"))
            .and_then(|v| v.as_str())
            .ok_or_else(|| format!("secret {namespace}/{name} has no ca.crt key"))?;
        let ca_crt_bytes = decode_base64(ca_crt_b64)
            .ok_or_else(|| format!("secret {namespace}/{name}: ca.crt is not valid base64"))?;
        String::from_utf8(ca_crt_bytes)
            .map_err(|_| format!("secret {namespace}/{name}: ca.crt is not valid UTF-8 PEM"))?
    };
    drop(secret_json);

    let der = pem_to_der(&pem)?;
    let fingerprint_sha256 = fingerprint_hex_sha256(&der);
    let fingerprint_sha1 = fingerprint_hex_sha1(&der);
    let (subject_cn, not_after) = extract_cert_metadata(runner, &pem).await;

    Ok(DiscoveredCaMeta {
        pem,
        fingerprint_sha256,
        fingerprint_sha1,
        subject_cn,
        not_after,
    })
}

pub async fn discover_cluster_cas(
    runner: &dyn CommandRunner,
    kubeconfig_path: &Path,
    endpoints: &[DiscoveredEndpoint],
) -> Result<Vec<DiscoveredCa>, String> {
    let ingress_json = query_k8s_api_json(
        runner,
        kubeconfig_path,
        "/apis/networking.k8s.io/v1/ingresses",
    )
    .await
    .ok()
    .flatten();
    let apisixtls_json = query_k8s_api_json(
        runner,
        kubeconfig_path,
        "/apis/apisix.apache.org/v2/apisixtlses",
    )
    .await
    .ok()
    .flatten();

    // Dedupe by (namespace, name) before fetching -- cheaper than fetching+fingerprinting
    // every host individually, and in practice one secret backs every host behind a shared
    // gateway (confirmed against a live cluster: 10 endpoints, 1 apisix-gateway-tls-secret).
    let mut secret_refs: BTreeMap<(String, String), Vec<String>> = BTreeMap::new();
    for ep in endpoints {
        let found = match ep.source.as_str() {
            "ingress" => ingress_json
                .as_ref()
                .and_then(|v| resolve_ingress_secret_ref(v, &ep.host)),
            "apisix" => apisixtls_json
                .as_ref()
                .and_then(|v| resolve_apisixtls_secret_ref(v, &ep.host)),
            // istio/gateway-api sources are deferred -- D5 in the design spec.
            _ => None,
        };
        if let Some(r) = found {
            secret_refs
                .entry((r.namespace, r.name))
                .or_default()
                .push(ep.host.clone());
        }
    }

    let mut result = Vec::new();
    for ((namespace, name), source_hosts) in secret_refs {
        match fetch_ca(runner, kubeconfig_path, &namespace, &name).await {
            Ok(meta) => result.push(DiscoveredCa {
                secret_ref: format!("{namespace}/{name}"),
                source_hosts,
                meta,
            }),
            // RBAC denial, a missing ca.crt key, or a malformed cert must not fail endpoint
            // discovery as a whole -- skip this one secret, keep the rest.
            Err(_) => continue,
        }
    }
    Ok(result)
}
```

- [ ] **Step 5: Run, verify all pass**

Run: `cd src-tauri && cargo test --lib services::ca_trust -- --nocapture`
Expected: PASS, 12 tests total.

- [ ] **Step 6: Run clippy**

Run: `cd src-tauri && cargo clippy --all-targets --all-features -- -D warnings`
Expected: clean. If clippy flags the `endpoints: &[DiscoveredEndpoint]` unused-if-empty path or similar, fix inline — do not add `#[allow(...)]` without first checking whether the lint is pointing at a real issue.

- [ ] **Step 7: Commit**

```bash
cd /Users/m/Documents/IdeaProjects/20.dasomel/clusterdeck
git add src-tauri/src/services/ca_trust.rs
git commit -m "feat(ca-trust): fetch and fingerprint CAs for discovered endpoints"
```

### Task 6: Trust / untrust in the macOS login keychain

**Files:**
- Modify: `src-tauri/src/services/ca_trust.rs`

**Interfaces:**
- Consumes: `pem_to_der`, `fingerprint_hex_sha1` (Task 1); `write_owner_only_file` and `TEMP_FILE_SEQ` (now `pub(crate)`, Task 2)
- Produces: `pub async fn resolve_login_keychain_path(runner: &dyn CommandRunner) -> Result<String, String>`, `pub async fn trust_ca(runner: &dyn CommandRunner, pem: &str) -> Result<String, String>` (returns the SHA-1 fingerprint it trusted), `pub async fn untrust_ca(runner: &dyn CommandRunner, fingerprint_sha1: &str) -> Result<(), String>`

**Important — read before implementing:** `trust_ca`'s temp PEM filename appends `k8s_endpoints::TEMP_FILE_SEQ.fetch_add(1, Ordering::Relaxed)` to the nanosecond timestamp — do not drop this and go back to a nanosecond-only filename. Task 2's `extract_cert_metadata` originally did exactly that and it caused a real, reproduced test flake (3 of 7 `cargo test` runs failed) under default parallelism: two concurrent `#[tokio::test]`s computed the same timestamp and lost the `create_new` race on the same path. The fix made `TEMP_FILE_SEQ` (already used by `k8s_endpoints::curl_k8s_api` for this exact hazard) `pub(crate)` — reuse it here rather than inventing a second counter.

**Important — read before implementing (2):** `security add-trusted-cert` blocks on a macOS GUI authorization prompt (Touch ID/password), even against a throwaway non-login keychain. This was confirmed empirically while writing this plan: running it from a headless/agent shell hung indefinitely with no way to answer the prompt, and had to be force-killed. `trust_ca`/`untrust_ca` themselves are plain async functions with no special handling for this (the blocking happens inside the `security` subprocess, which `CommandRunner::run` already awaits correctly) — the implication is entirely for **Step 6** below (the real-exec test): it must be `#[ignore]`d and can only be run manually from a real, logged-in Terminal session, never from CI or an agent session.

**Important — read before implementing (3):** two `security(1)` argv details below were wrong in an earlier draft of this plan and are corrected in the code that follows — transcribe the corrected version, not what you might expect by analogy:
- `delete-certificate` (used by `untrust_ca` and by the ignored test) takes its keychain as a trailing **positional** argument — `[-h] [-c name] [-Z hash] [-t] [keychain...]` — there is no `-k` flag on this subcommand (`-k` exists only on `add-trusted-cert`, with a different meaning there). Passing `-k` here makes `security` reject the whole invocation, so `untrust_ca` would always fail. Verified directly against `man security`.
- `add-trusted-cert`'s `-k keychain` only controls which keychain the certificate *item* is stored in — it does **not** scope the trust setting itself. Per `man security`: "`-o settingsFileOut` Output trust settings file; default is user domain." Production `trust_ca` correctly wants the trust setting in the real user domain (that's the feature), so it does not pass `-o`. But the `#[ignore]`d test in Step 5 below is supposed to be safe to run against a developer's real login session, and without `-o` it would silently write a real trust setting to that developer's actual user trust domain even though the *certificate* sits in a throwaway keychain — get `-o` right there, it's the difference between the test being safe and not.

- [ ] **Step 1: Write the failing unit tests (argv shape + keychain path parsing)**

Add to `mod tests`:

```rust
    #[tokio::test]
    async fn resolve_login_keychain_path_strips_quotes() {
        struct FakeSecurityRunner;
        #[async_trait::async_trait]
        impl CommandRunner for FakeSecurityRunner {
            async fn run(
                &self,
                bin: &str,
                args: &[String],
            ) -> Result<crate::services::process::CommandOutput, String> {
                assert_eq!(bin, "security");
                assert_eq!(
                    args,
                    &[
                        "default-keychain".to_string(),
                        "-d".to_string(),
                        "user".to_string()
                    ]
                );
                Ok(ok_output("\"/Users/m/Library/Keychains/login.keychain-db\""))
            }
        }
        let path = resolve_login_keychain_path(&FakeSecurityRunner).await.unwrap();
        assert_eq!(path, "/Users/m/Library/Keychains/login.keychain-db");
    }

    #[tokio::test]
    async fn trust_ca_builds_expected_argv_and_cleans_up_temp_file() {
        use std::sync::{Arc, Mutex};

        struct RecordingRunner {
            calls: Arc<Mutex<Vec<(String, Vec<String>)>>>,
        }

        #[async_trait::async_trait]
        impl CommandRunner for RecordingRunner {
            async fn run(
                &self,
                bin: &str,
                args: &[String],
            ) -> Result<crate::services::process::CommandOutput, String> {
                self.calls.lock().unwrap().push((bin.to_string(), args.to_vec()));
                if bin == "security" && args.first().map(String::as_str) == Some("default-keychain")
                {
                    return Ok(ok_output("\"/Users/m/Library/Keychains/login.keychain-db\""));
                }
                Ok(ok_output(""))
            }
        }

        let calls = Arc::new(Mutex::new(Vec::new()));
        let runner = RecordingRunner {
            calls: calls.clone(),
        };

        let fingerprint = trust_ca(&runner, TEST_CA_PEM).await.unwrap();
        assert_eq!(fingerprint, "67fc8cc8df72476829ecd88d188331a6d29baabb");

        let recorded = calls.lock().unwrap();
        let add_call = recorded
            .iter()
            .find(|(bin, args)| {
                bin == "security" && args.first().map(String::as_str) == Some("add-trusted-cert")
            })
            .expect("add-trusted-cert should have been called");
        // Full exact-match, not `.contains()` on individual flags: a subset check would have
        // silently passed even with an invalid flag mixed in (this is exactly how a real bug
        // shipped during this task's review -- `.contains()` checks can't see the argv also
        // contained something that shouldn't be there, only that a specific expected element
        // is present somewhere in it).
        let temp_path = add_call.1.last().unwrap().clone();
        assert_eq!(
            add_call.1,
            vec![
                "add-trusted-cert".to_string(),
                "-r".to_string(),
                "trustRoot".to_string(),
                "-p".to_string(),
                "ssl".to_string(),
                "-k".to_string(),
                "/Users/m/Library/Keychains/login.keychain-db".to_string(),
                temp_path.clone(),
            ]
        );
        assert!(
            !Path::new(&temp_path).exists(),
            "temp PEM file must be cleaned up after trust_ca returns"
        );
    }

    #[tokio::test]
    async fn trust_ca_cleans_up_temp_file_even_when_security_fails() {
        use std::sync::{Arc, Mutex};

        struct FailingSecurityRunner {
            last_temp_path: Arc<Mutex<Option<String>>>,
        }

        #[async_trait::async_trait]
        impl CommandRunner for FailingSecurityRunner {
            async fn run(
                &self,
                bin: &str,
                args: &[String],
            ) -> Result<crate::services::process::CommandOutput, String> {
                if bin == "security" && args.first().map(String::as_str) == Some("default-keychain")
                {
                    return Ok(ok_output("\"/Users/m/Library/Keychains/login.keychain-db\""));
                }
                if bin == "security" && args.first().map(String::as_str) == Some("add-trusted-cert")
                {
                    *self.last_temp_path.lock().unwrap() = args.last().cloned();
                    return Ok(crate::services::process::CommandOutput {
                        stdout: String::new(),
                        stderr: "user cancelled the authorization request".to_string(),
                        success: false,
                    });
                }
                Ok(ok_output(""))
            }
        }

        let last_temp_path = Arc::new(Mutex::new(None));
        let runner = FailingSecurityRunner {
            last_temp_path: last_temp_path.clone(),
        };

        let result = trust_ca(&runner, TEST_CA_PEM).await;
        assert!(result.is_err());

        let temp_path = last_temp_path
            .lock()
            .unwrap()
            .clone()
            .expect("add-trusted-cert should have been called");
        assert!(
            !Path::new(&temp_path).exists(),
            "temp PEM file must be cleaned up even on failure"
        );
    }

    #[tokio::test]
    async fn untrust_ca_builds_expected_argv() {
        use std::sync::{Arc, Mutex};

        struct RecordingRunner {
            calls: Arc<Mutex<Vec<(String, Vec<String>)>>>,
        }

        #[async_trait::async_trait]
        impl CommandRunner for RecordingRunner {
            async fn run(
                &self,
                bin: &str,
                args: &[String],
            ) -> Result<crate::services::process::CommandOutput, String> {
                self.calls.lock().unwrap().push((bin.to_string(), args.to_vec()));
                if bin == "security" && args.first().map(String::as_str) == Some("default-keychain")
                {
                    return Ok(ok_output("\"/Users/m/Library/Keychains/login.keychain-db\""));
                }
                Ok(ok_output(""))
            }
        }

        let calls = Arc::new(Mutex::new(Vec::new()));
        let runner = RecordingRunner {
            calls: calls.clone(),
        };

        untrust_ca(&runner, "67fc8cc8df72476829ecd88d188331a6d29baabb")
            .await
            .unwrap();

        let recorded = calls.lock().unwrap();
        let delete_call = recorded
            .iter()
            .find(|(bin, args)| {
                bin == "security" && args.first().map(String::as_str) == Some("delete-certificate")
            })
            .expect("delete-certificate should have been called");
        // Full exact-match (see the same note in trust_ca's argv test above).
        assert_eq!(
            delete_call.1,
            vec![
                "delete-certificate".to_string(),
                "-Z".to_string(),
                "67fc8cc8df72476829ecd88d188331a6d29baabb".to_string(),
                "-t".to_string(),
                "/Users/m/Library/Keychains/login.keychain-db".to_string(),
            ]
        );
    }

    #[tokio::test]
    async fn untrust_ca_rejects_malformed_fingerprint_before_running_any_command() {
        struct UnusedRunner;
        #[async_trait::async_trait]
        impl CommandRunner for UnusedRunner {
            async fn run(
                &self,
                _bin: &str,
                _args: &[String],
            ) -> Result<crate::services::process::CommandOutput, String> {
                panic!("should not be called -- validation must reject before any command runs");
            }
        }
        assert!(untrust_ca(&UnusedRunner, "not-a-fingerprint").await.is_err());
        assert!(untrust_ca(&UnusedRunner, "").await.is_err());
        assert!(untrust_ca(&UnusedRunner, "67fc8cc8df72476829ecd88d188331a6d29baa")
            .await
            .is_err()); // 39 chars, one short
    }
```

- [ ] **Step 2: Run, verify compile failure**

Run: `cd src-tauri && cargo test --lib services::ca_trust`
Expected: compile error, functions undefined

- [ ] **Step 3: Implement**

Add to `src-tauri/src/services/ca_trust.rs` (non-test code):

```rust
pub async fn resolve_login_keychain_path(runner: &dyn CommandRunner) -> Result<String, String> {
    let output = runner
        .run(
            "security",
            &[
                "default-keychain".to_string(),
                "-d".to_string(),
                "user".to_string(),
            ],
        )
        .await?;
    if !output.success {
        return Err(format!("security default-keychain failed: {}", output.stderr));
    }
    let trimmed = output.stdout.trim();
    let unquoted = trimmed
        .strip_prefix('"')
        .and_then(|s| s.strip_suffix('"'))
        .unwrap_or(trimmed);
    if unquoted.is_empty() {
        return Err("security default-keychain returned an empty path".to_string());
    }
    Ok(unquoted.to_string())
}

pub async fn trust_ca(runner: &dyn CommandRunner, pem: &str) -> Result<String, String> {
    let der = pem_to_der(pem)?;
    let fingerprint_sha1 = fingerprint_hex_sha1(&der);
    let keychain_path = resolve_login_keychain_path(runner).await?;

    let now_nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let seq = crate::services::k8s_endpoints::TEMP_FILE_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let temp_pem = std::env::temp_dir().join(format!("cd_ca_trust_{now_nanos}_{seq}.pem"));
    write_owner_only_file(&temp_pem, pem.as_bytes())
        .map_err(|e| format!("failed to write temp CA file: {e}"))?;

    let result = runner
        .run(
            "security",
            &[
                "add-trusted-cert".to_string(),
                "-r".to_string(),
                "trustRoot".to_string(),
                // Least privilege: without -p, trustRoot applies to every policy (S/MIME,
                // codeSign, pkgSign, timestamping, ...), not just the TLS use case this feature
                // exists for. Scope it to SSL/TLS only.
                "-p".to_string(),
                "ssl".to_string(),
                "-k".to_string(),
                keychain_path,
                temp_pem.to_string_lossy().to_string(),
            ],
        )
        .await;

    // Unconditional: the temp PEM must never linger, whether add-trusted-cert succeeded,
    // failed, or the user dismissed the macOS authorization prompt.
    let _ = std::fs::remove_file(&temp_pem);

    let output = result?;
    if !output.success {
        return Err(format!("security add-trusted-cert failed: {}", output.stderr));
    }
    Ok(fingerprint_sha1)
}

pub async fn untrust_ca(runner: &dyn CommandRunner, fingerprint_sha1: &str) -> Result<(), String> {
    // Defensive re-check at this sink: fingerprint_sha1 will later (Task 9) be sourced from a
    // stored Profile record (profiles.yaml), not only from trust_ca's own return value, so its
    // shape is validated here before it reaches the security(1) argv, per AGENTS.md's
    // "re-check defensively at the sink" rule.
    if fingerprint_sha1.len() != 40 || !fingerprint_sha1.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err(format!("invalid SHA-1 fingerprint: {fingerprint_sha1}"));
    }
    let keychain_path = resolve_login_keychain_path(runner).await?;
    // `delete-certificate`'s synopsis is `[-h] [-c name] [-Z hash] [-t] [keychain...]` -- the
    // keychain is a trailing POSITIONAL argument, there is no `-k` flag (that flag exists only
    // on `add-trusted-cert`, where it means something different: which keychain a *new*
    // certificate item is stored in). Passing `-k` here does not silently no-op -- `security`
    // treats it as an unrecognized option and the whole subcommand fails, so untrust_ca would
    // always return Err. Verified against `man security` (delete-certificate section) directly.
    let output = runner
        .run(
            "security",
            &[
                "delete-certificate".to_string(),
                "-Z".to_string(),
                fingerprint_sha1.to_string(),
                "-t".to_string(),
                keychain_path,
            ],
        )
        .await?;
    if !output.success {
        return Err(format!("security delete-certificate failed: {}", output.stderr));
    }
    Ok(())
}
```

- [ ] **Step 4: Run, verify unit tests pass**

Run: `cd src-tauri && cargo test --lib services::ca_trust -- --nocapture`
Expected: PASS, 18 tests total.

- [ ] **Step 5: Write the ignored real-exec test**

Add to `mod tests`. This exercises the real `security` binary against a **throwaway test keychain** (never `login.keychain-db`) by calling `security` directly rather than through `trust_ca`/`untrust_ca` (which always resolve the *login* keychain). It runs cleanup **before** any assertion that could panic, and redirects `add-trusted-cert`'s trust-setting write to a throwaway file via `-o` — without `-o`, the trust setting itself (as opposed to the certificate item, which `-k` does scope) is written to the developer's real user trust domain regardless of which keychain `-k` names; `man security`'s `add-trusted-cert` section confirms `-o`'s default is "user domain". Both of these were wrong in an earlier version of this test and are corrected here — get them right, they're what make this test actually safe for a human to run later.

```rust
    // Manual-only: `security add-trusted-cert` blocks on a GUI authorization prompt even for a
    // throwaway keychain (confirmed while writing this plan -- it hangs forever in a headless
    // shell). Run from a real, logged-in Terminal and approve the prompt when it appears:
    //   cargo test --lib services::ca_trust::tests::real_trust_and_untrust_cycle_on_test_keychain -- --ignored --nocapture
    #[tokio::test]
    #[ignore]
    async fn real_trust_and_untrust_cycle_on_test_keychain() {
        use crate::services::process::SystemRunner;

        let runner = SystemRunner;
        let test_keychain = std::env::temp_dir().join("clusterdeck-ca-trust-test.keychain-db");
        let temp_pem = std::env::temp_dir().join("clusterdeck-ca-trust-test.pem");
        // `-o` below redirects add-trusted-cert's TRUST SETTING write to this throwaway file.
        // Without it, the trust setting itself (as opposed to the certificate item, which `-k`
        // scopes) is written to the developer's real user trust domain regardless of which
        // keychain `-k` names -- `man security`'s add-trusted-cert section: "-o settingsFileOut
        // Output trust settings file; default is user domain." This is what makes the test
        // actually safe to run against the developer's own login session.
        let throwaway_trust_settings = std::env::temp_dir().join("clusterdeck-ca-trust-test.plist");
        let _ = std::fs::remove_file(&test_keychain);
        write_owner_only_file(&temp_pem, TEST_CA_PEM.as_bytes()).unwrap();

        let create = runner
            .run(
                "security",
                &[
                    "create-keychain".to_string(),
                    "-p".to_string(),
                    "clusterdeck-test-only".to_string(),
                    test_keychain.to_string_lossy().to_string(),
                ],
            )
            .await
            .unwrap();

        let add = runner
            .run(
                "security",
                &[
                    "add-trusted-cert".to_string(),
                    "-k".to_string(),
                    test_keychain.to_string_lossy().to_string(),
                    "-o".to_string(),
                    throwaway_trust_settings.to_string_lossy().to_string(),
                    "-r".to_string(),
                    "trustRoot".to_string(),
                    temp_pem.to_string_lossy().to_string(),
                ],
            )
            .await
            .unwrap();

        let find = runner
            .run(
                "security",
                &[
                    "find-certificate".to_string(),
                    "-c".to_string(),
                    "clusterdeck-test-ca.invalid".to_string(),
                    test_keychain.to_string_lossy().to_string(),
                ],
            )
            .await
            .unwrap();

        // `delete-certificate`'s keychain argument is positional -- there is no `-k` flag (see
        // untrust_ca's own comment above; this test hit the identical bug once).
        let delete = runner
            .run(
                "security",
                &[
                    "delete-certificate".to_string(),
                    "-Z".to_string(),
                    "67fc8cc8df72476829ecd88d188331a6d29baabb".to_string(),
                    "-t".to_string(),
                    test_keychain.to_string_lossy().to_string(),
                ],
            )
            .await
            .unwrap();

        // Cleanup runs unconditionally, BEFORE any assertion below can panic -- a failed
        // assertion must never skip removing the throwaway keychain/files (an earlier version
        // of this test put cleanup after the asserts, so a failure would leave the throwaway
        // keychain, temp PEM, and a trust-setting file behind for a human to clean up by hand).
        let _ = std::fs::remove_file(&temp_pem);
        let _ = std::fs::remove_file(&throwaway_trust_settings);
        let _ = runner
            .run(
                "security",
                &[
                    "delete-keychain".to_string(),
                    test_keychain.to_string_lossy().to_string(),
                ],
            )
            .await;

        assert!(create.success, "create-keychain failed: {}", create.stderr);
        assert!(add.success, "add-trusted-cert failed: {}", add.stderr);
        assert!(find.success, "the trusted cert should be findable by its CN");
        assert!(delete.success, "delete-certificate failed: {}", delete.stderr);
    }
```

- [ ] **Step 6: Run the non-ignored suite once more, then commit**

Run: `cd src-tauri && cargo test --lib services::ca_trust -- --nocapture` (do **not** pass `--ignored` — leave the real-exec test for the developer to run manually later)
Expected: PASS, 19 tests total (1 ignored, 18 passing).

```bash
cd /Users/m/Documents/IdeaProjects/20.dasomel/clusterdeck
git add src-tauri/src/services/ca_trust.rs
git commit -m "feat(ca-trust): trust/untrust a CA in the macOS login keychain"
```

### Task 7: Persist `trusted_cas` on `Profile`

**Files:**
- Modify: `src-tauri/src/services/ca_trust.rs` (+ `TrustedCa` struct)
- Modify: `src-tauri/src/services/config.rs` (`Profile.trusted_cas` field)
- Modify: `src-tauri/src/services/store.rs` (`ProfileBody` + `load_profiles`/`save_profiles` mapping, + new round-trip test)
- Modify (mechanical, compiler-enforced): `src-tauri/src/services/store.rs` (3 existing test fixtures), `src-tauri/src/services/kubeconfig.rs` (6 existing test fixtures), `src-tauri/src/services/validate.rs` (2 existing test fixtures), `src-tauri/src/services/hosts_file.rs` (1 test fixture), `src-tauri/src/services/ssh_config.rs` (1 test fixture)

**Why the mechanical list:** `Profile` gains a new field with no default at the *struct-literal* level (`#[serde(default)]` only affects deserialization, not Rust struct literals). Every existing `Profile { ... }` literal in the codebase must add `trusted_cas: Vec::new(),` or the crate will not compile. This was enumerated exhaustively via `grep -rn "Profile {" src-tauri/src/` during planning — 14 total literals, 1 production (`store.rs` `load_profiles`, handled in Step 3 below) + 13 test fixtures (handled in Step 5).

**Interfaces:**
- Produces: `#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)] pub struct TrustedCa { pub secret_ref: String, pub fingerprint_sha256: String, pub fingerprint_sha1: String, pub subject_cn: String, pub not_after: String, pub trusted_at: String }` (in `ca_trust.rs`), `Profile.trusted_cas: Vec<TrustedCa>` (in `config.rs`)

- [ ] **Step 1: Add `TrustedCa` to `ca_trust.rs`**

Add near the top of `src-tauri/src/services/ca_trust.rs` (with the other `use` lines):

```rust
use serde::{Deserialize, Serialize};
```

Add the struct (non-test code, can go right before `DiscoveredCaMeta`):

```rust
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrustedCa {
    pub secret_ref: String,
    pub fingerprint_sha256: String,
    pub fingerprint_sha1: String,
    pub subject_cn: String,
    pub not_after: String,
    pub trusted_at: String,
}
```

- [ ] **Step 2: Add the field to `Profile`**

In `src-tauri/src/services/config.rs`, in `pub struct Profile { ... }`, add after `pub manage_hosts_file: bool,`:

```rust
    #[serde(default)]
    pub trusted_cas: Vec<crate::services::ca_trust::TrustedCa>,
```

- [ ] **Step 3: Wire it through `store.rs`**

In `src-tauri/src/services/store.rs`:

In `struct ProfileBody { ... }` (line ~16), add after `manage_hosts_file: bool,`:
```rust
    #[serde(default)]
    trusted_cas: Vec<crate::services::ca_trust::TrustedCa>,
```

In `load_profiles` (the `Profile { ... }` construction around line 41-49), add after `manage_hosts_file: body.manage_hosts_file,`:
```rust
                trusted_cas: body.trusted_cas,
```

In `save_profiles` (the `ProfileBody { ... }` construction around line 75-82), add after `manage_hosts_file: p.manage_hosts_file,`:
```rust
                trusted_cas: p.trusted_cas.clone(),
```

- [ ] **Step 4: Write the failing round-trip test**

Add to `mod tests` in `store.rs` (after the existing `upsert_profile_rejects_invalid_profile_id` test):

```rust
    #[test]
    fn upsert_then_load_roundtrips_trusted_cas() {
        let paths = temp_paths("trusted-cas-roundtrip");
        let profile = Profile {
            id: "cka".into(),
            name: "CKA Lab".into(),
            hosts: vec![],
            bastion: None,
            bootstrap: BootstrapPolicy::default(),
            kubeconfig: None,
            manage_hosts_file: false,
            trusted_cas: vec![crate::services::ca_trust::TrustedCa {
                secret_ref: "platform-system/apisix-gateway-tls-secret".into(),
                fingerprint_sha256: "8b75bf97e19ce7efe9bb4d6c76b4f10e072a9d6ea89d8f438f423afea7156246"
                    .into(),
                fingerprint_sha1: "67fc8cc8df72476829ecd88d188331a6d29baabb".into(),
                subject_cn: "clusterdeck-test-ca.invalid".into(),
                not_after: "Sep 18 05:40:47 2036 GMT".into(),
                trusted_at: "2026-09-21T00:00:00+00:00".into(),
            }],
        };
        upsert_profile(&paths, profile.clone()).unwrap();
        let loaded = get_profile(&paths, "cka").unwrap();
        assert_eq!(loaded.trusted_cas.len(), 1);
        assert_eq!(loaded.trusted_cas[0], profile.trusted_cas[0]);
    }

    #[test]
    fn load_profiles_defaults_trusted_cas_when_field_absent_from_yaml() {
        let paths = temp_paths("trusted-cas-default");
        if let Some(parent) = paths.profiles_file().parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        // Simulates a profiles.yaml written before this field existed.
        let yaml = r#"
profiles:
  legacy:
    name: "Legacy"
    hosts: []
    manage_hosts_file: false
"#;
        std::fs::write(paths.profiles_file(), yaml).unwrap();
        let loaded = load_profiles(&paths).unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].trusted_cas.len(), 0);
    }
```

- [ ] **Step 5: Fix every other call site the compiler flags**

`cargo build --lib` alone will **not** surface most of these — it does not compile `#[cfg(test)]`
modules (only `cargo test`/`cargo build --all-targets`/`cargo check --all-targets` do; this is the
same `--all-targets` distinction `AGENTS.md` already calls out for `clippy`, and it applies here
too). Use:

Run: `cd src-tauri && cargo build --all-targets 2>&1 | grep "missing field"`
Expected output: 14 errors (1 production site in `store.rs::load_profiles`, already fixed in Step 3
above and so should NOT still appear if Step 3 was done correctly — if it does still appear,
Step 3 was missed; plus the 13 test sites enumerated below), each
`missing field \`trusted_cas\` in initializer of \`Profile\``.

Fix each by adding `trusted_cas: Vec::new(),` as the last field, immediately after the existing `manage_hosts_file: ...,` line, in these exact locations:

| File | Test(s) | Anchor to insert after |
|---|---|---|
| `src-tauri/src/services/store.rs:152` | `upsert_then_load_roundtrips` | `manage_hosts_file: true,` |
| `src-tauri/src/services/store.rs:171` | `delete_profile_removes_entry` | `manage_hosts_file: false,` |
| `src-tauri/src/services/store.rs:216` | `upsert_profile_rejects_invalid_profile_id` | `manage_hosts_file: false,` |
| `src-tauri/src/services/kubeconfig.rs` (6 sites: lines 1328, 1374, 1468, 1656, 1753, 1799) | scp/fallback/default-kubeconfig tests | each ends `manage_hosts_file: false,` immediately followed by `};` — apply with `replace_all: true` in one `Edit` call since all 6 tails are byte-identical |
| `src-tauri/src/services/validate.rs:130` | `validate_profile_rejects_newline_in_host_name` | `manage_hosts_file: false,` |
| `src-tauri/src/services/validate.rs:160` | `validate_profile_rejects_newline_in_host_address_and_accepts_valid` | `manage_hosts_file: false,` |
| `src-tauri/src/services/hosts_file.rs:185` | `profile()` test helper | `manage_hosts_file: true,` |
| `src-tauri/src/services/ssh_config.rs:142` | `profile_with_bastion()` test helper | `manage_hosts_file: false,` |

For `store.rs`, note two of its three test literals end in the identical `manage_hosts_file: false,` — `replace_all: true` on that file will catch both `delete_profile_removes_entry` and `upsert_profile_rejects_invalid_profile_id` in one `Edit` call; the `upsert_then_load_roundtrips` one (`manage_hosts_file: true,`) needs a separate call since its anchor text differs.

Re-run `cargo build --all-targets` after each file until the `missing field` errors are gone.

- [ ] **Step 6: Run the full test suite**

Run: `cd src-tauri && cargo test --all-targets --all-features`
Expected: PASS, no failures, no ignored-but-should-run tests missed.

- [ ] **Step 7: Run fmt and clippy**

Run: `cd src-tauri && cargo fmt --all -- --check && cargo clippy --all-targets --all-features -- -D warnings`
Expected: clean.

- [ ] **Step 8: Commit**

```bash
cd /Users/m/Documents/IdeaProjects/20.dasomel/clusterdeck
git add src-tauri/src/services/ca_trust.rs src-tauri/src/services/config.rs src-tauri/src/services/store.rs src-tauri/src/services/kubeconfig.rs src-tauri/src/services/validate.rs src-tauri/src/services/hosts_file.rs src-tauri/src/services/ssh_config.rs
git commit -m "feat(ca-trust): persist trusted_cas on Profile"
```

### Task 8: Compare a discovered CA against stored trust to compute status

**Files:**
- Modify: `src-tauri/src/services/ca_trust.rs`

**Interfaces:**
- Consumes: `DiscoveredCa` (Task 5), `TrustedCa` (Task 7)
- Produces: `#[derive(Debug, Clone, PartialEq, Eq)] pub enum CaTrustStatus { New, Trusted, Rotated }`, `pub fn compute_trust_status(discovered: &DiscoveredCa, trusted: &[TrustedCa]) -> CaTrustStatus`

- [ ] **Step 1: Write the failing tests**

Add to `mod tests`:

```rust
    fn sample_discovered_ca(secret_ref: &str) -> DiscoveredCa {
        DiscoveredCa {
            secret_ref: secret_ref.to_string(),
            source_hosts: vec!["argocd.local.beluga.internal".to_string()],
            meta: DiscoveredCaMeta {
                pem: TEST_CA_PEM.to_string(),
                fingerprint_sha256:
                    "8b75bf97e19ce7efe9bb4d6c76b4f10e072a9d6ea89d8f438f423afea7156246".to_string(),
                fingerprint_sha1: "67fc8cc8df72476829ecd88d188331a6d29baabb".to_string(),
                subject_cn: "clusterdeck-test-ca.invalid".to_string(),
                not_after: "Sep 18 05:40:47 2036 GMT".to_string(),
            },
        }
    }

    #[test]
    fn compute_trust_status_transitions() {
        let discovered = sample_discovered_ca("platform-system/apisix-gateway-tls-secret");

        assert_eq!(compute_trust_status(&discovered, &[]), CaTrustStatus::New);

        let matching = TrustedCa {
            secret_ref: discovered.secret_ref.clone(),
            fingerprint_sha256: discovered.meta.fingerprint_sha256.clone(),
            fingerprint_sha1: discovered.meta.fingerprint_sha1.clone(),
            subject_cn: discovered.meta.subject_cn.clone(),
            not_after: discovered.meta.not_after.clone(),
            trusted_at: "2026-01-01T00:00:00+00:00".to_string(),
        };
        assert_eq!(
            compute_trust_status(&discovered, &[matching]),
            CaTrustStatus::Trusted
        );

        let stale = TrustedCa {
            secret_ref: discovered.secret_ref.clone(),
            fingerprint_sha256: "0".repeat(64),
            fingerprint_sha1: "0".repeat(40),
            subject_cn: "old-ca.invalid".to_string(),
            not_after: "Sep 18 05:40:47 2030 GMT".to_string(),
            trusted_at: "2026-01-01T00:00:00+00:00".to_string(),
        };
        assert_eq!(
            compute_trust_status(&discovered, &[stale]),
            CaTrustStatus::Rotated
        );
    }

    #[test]
    fn compute_trust_status_matches_by_secret_ref_not_position() {
        let discovered = sample_discovered_ca("ns-b/secret-b");
        let other = TrustedCa {
            secret_ref: "ns-a/secret-a".to_string(),
            fingerprint_sha256: "1".repeat(64),
            fingerprint_sha1: "1".repeat(40),
            subject_cn: "unrelated.invalid".to_string(),
            not_after: "Sep 18 05:40:47 2030 GMT".to_string(),
            trusted_at: "2026-01-01T00:00:00+00:00".to_string(),
        };
        let matching = TrustedCa {
            secret_ref: "ns-b/secret-b".to_string(),
            fingerprint_sha256: discovered.meta.fingerprint_sha256.clone(),
            fingerprint_sha1: discovered.meta.fingerprint_sha1.clone(),
            subject_cn: discovered.meta.subject_cn.clone(),
            not_after: discovered.meta.not_after.clone(),
            trusted_at: "2026-01-01T00:00:00+00:00".to_string(),
        };
        assert_eq!(
            compute_trust_status(&discovered, &[other, matching]),
            CaTrustStatus::Trusted
        );
    }
```

- [ ] **Step 2: Run, verify compile failure**

Run: `cd src-tauri && cargo test --lib services::ca_trust`
Expected: compile error, `CaTrustStatus`/`compute_trust_status` undefined

- [ ] **Step 3: Implement**

Add to `src-tauri/src/services/ca_trust.rs` (non-test code, near `TrustedCa`):

```rust
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CaTrustStatus {
    New,
    Trusted,
    Rotated,
}

pub fn compute_trust_status(discovered: &DiscoveredCa, trusted: &[TrustedCa]) -> CaTrustStatus {
    match trusted
        .iter()
        .find(|t| t.secret_ref == discovered.secret_ref)
    {
        None => CaTrustStatus::New,
        Some(existing) if existing.fingerprint_sha256 == discovered.meta.fingerprint_sha256 => {
            CaTrustStatus::Trusted
        }
        Some(_) => CaTrustStatus::Rotated,
    }
}
```

- [ ] **Step 4: Run, verify all pass**

Run: `cd src-tauri && cargo test --lib services::ca_trust -- --nocapture`
Expected: PASS, 21 tests total (1 ignored, 20 passing).

- [ ] **Step 5: Commit**

```bash
cd /Users/m/Documents/IdeaProjects/20.dasomel/clusterdeck
git add src-tauri/src/services/ca_trust.rs
git commit -m "feat(ca-trust): compute New/Trusted/Rotated status against stored trust"
```

### Task 9: Tauri commands

**Files:**
- Create: `src-tauri/src/commands/ca_trust.rs`
- Modify: `src-tauri/src/commands/mod.rs` (+ `pub mod ca_trust;`)
- Modify: `src-tauri/src/lib.rs` (register 3 commands)

Note: this codebase has **no unit tests under `src-tauri/src/commands/`** — every existing command file (`connection.rs`, `profiles.rs`, etc.) is untested glue over a tested `services/` layer, verified by `cargo build`/`cargo clippy` plus manual app verification. This task follows that same convention; all the real logic it calls was already TDD'd in Tasks 1-8.

**Important — read before implementing:** `replace_ca_cmd`'s body below does two things an earlier draft of this plan got wrong — validate cheap preconditions (`split_secret_ref`, kubeconfig existence) *before* the destructive `untrust_ca` call, and clean up the stale `trusted_cas` record if the subsequent `trust_ca_cmd` call fails after the old cert was already untrusted. Without the cleanup, a failure there (e.g. the user dismisses the macOS auth prompt on the new cert, or the cluster becomes unreachable mid-replace) would leave `profile.trusted_cas` still holding the old record — the next discover would report a false `"trusted"` status for a CA that's actually no longer in the keychain. Transcribe the corrected ordering/cleanup below, not just the two-line "untrust then trust" shape you might expect from the Interfaces summary above.

**Interfaces:**
- Consumes: `ca_trust::{discover_cluster_cas, compute_trust_status, fetch_ca, trust_ca, untrust_ca, CaTrustStatus, TrustedCa}` (Tasks 5, 6, 8); `k8s_endpoints::DiscoveredEndpoint`; `store::{get_profile, upsert_profile}`; `paths::ClusterDeckPaths`; `process::SystemRunner`
- Produces (Tauri commands, callable from the frontend): `discover_cluster_cas_cmd(profile_id: String, endpoints: Vec<DiscoveredEndpoint>) -> Result<Vec<DiscoveredCaView>, String>`, `trust_ca_cmd(profile_id: String, secret_ref: String) -> Result<TrustedCa, String>`, `replace_ca_cmd(profile_id: String, secret_ref: String) -> Result<TrustedCa, String>`

- [ ] **Step 1: Create the command file**

Create `src-tauri/src/commands/ca_trust.rs`:

```rust
use chrono::Utc;

use crate::services::ca_trust::{self, CaTrustStatus, TrustedCa};
use crate::services::paths::ClusterDeckPaths;
use crate::services::process::SystemRunner;
use crate::services::store;

#[derive(Debug, Clone, serde::Serialize)]
pub struct DiscoveredCaView {
    pub secret_ref: String,
    pub source_hosts: Vec<String>,
    pub subject_cn: String,
    pub not_after: String,
    pub fingerprint_sha256: String,
    pub status: String, // "new" | "trusted" | "rotated"
}

fn status_str(status: &CaTrustStatus) -> String {
    match status {
        CaTrustStatus::New => "new".to_string(),
        CaTrustStatus::Trusted => "trusted".to_string(),
        CaTrustStatus::Rotated => "rotated".to_string(),
    }
}

fn split_secret_ref(secret_ref: &str) -> Result<(String, String), String> {
    match secret_ref.split_once('/') {
        Some((ns, name)) if !ns.is_empty() && !name.is_empty() => {
            Ok((ns.to_string(), name.to_string()))
        }
        _ => Err(format!("invalid secret ref: {secret_ref}")),
    }
}

#[tauri::command]
pub async fn discover_cluster_cas_cmd(
    profile_id: String,
    endpoints: Vec<crate::services::k8s_endpoints::DiscoveredEndpoint>,
) -> Result<Vec<DiscoveredCaView>, String> {
    let paths = ClusterDeckPaths::resolve()?;
    let profile = store::get_profile(&paths, &profile_id)?;
    let runner = SystemRunner;
    let kubeconfig_path = paths.kubeconfig_file(&profile_id);
    if !kubeconfig_path.exists() {
        return Err("kubeconfig not found for profile; please connect first".to_string());
    }

    let discovered = ca_trust::discover_cluster_cas(&runner, &kubeconfig_path, &endpoints).await?;

    Ok(discovered
        .into_iter()
        .map(|d| {
            let status = ca_trust::compute_trust_status(&d, &profile.trusted_cas);
            DiscoveredCaView {
                secret_ref: d.secret_ref,
                source_hosts: d.source_hosts,
                subject_cn: d.meta.subject_cn,
                not_after: d.meta.not_after,
                fingerprint_sha256: d.meta.fingerprint_sha256,
                status: status_str(&status),
            }
        })
        .collect())
}

#[tauri::command]
pub async fn trust_ca_cmd(profile_id: String, secret_ref: String) -> Result<TrustedCa, String> {
    let paths = ClusterDeckPaths::resolve()?;
    let mut profile = store::get_profile(&paths, &profile_id)?;
    let runner = SystemRunner;
    let kubeconfig_path = paths.kubeconfig_file(&profile_id);
    if !kubeconfig_path.exists() {
        return Err("kubeconfig not found for profile; please connect first".to_string());
    }

    let (namespace, name) = split_secret_ref(&secret_ref)?;
    let meta = ca_trust::fetch_ca(&runner, &kubeconfig_path, &namespace, &name).await?;
    ca_trust::trust_ca(&runner, &meta.pem).await?;

    let record = TrustedCa {
        secret_ref: secret_ref.clone(),
        fingerprint_sha256: meta.fingerprint_sha256,
        fingerprint_sha1: meta.fingerprint_sha1,
        subject_cn: meta.subject_cn,
        not_after: meta.not_after,
        trusted_at: Utc::now().to_rfc3339(),
    };
    // Idempotent: replaces any prior record for this secret_ref rather than duplicating it,
    // so re-confirming an already-Trusted CA (or completing a Rotated -> trust cycle) is safe.
    profile.trusted_cas.retain(|c| c.secret_ref != secret_ref);
    profile.trusted_cas.push(record.clone());
    store::upsert_profile(&paths, profile)?;

    Ok(record)
}

#[tauri::command]
pub async fn replace_ca_cmd(profile_id: String, secret_ref: String) -> Result<TrustedCa, String> {
    let paths = ClusterDeckPaths::resolve()?;
    let profile = store::get_profile(&paths, &profile_id)?;
    let runner = SystemRunner;

    // Validate preconditions before the destructive untrust step below, so the common
    // not-connected / malformed-secret-ref case never removes the old trust entry for nothing.
    split_secret_ref(&secret_ref)?;
    let kubeconfig_path = paths.kubeconfig_file(&profile_id);
    if !kubeconfig_path.exists() {
        return Err("kubeconfig not found for profile; please connect first".to_string());
    }

    if let Some(old) = profile
        .trusted_cas
        .iter()
        .find(|c| c.secret_ref == secret_ref)
    {
        // Best-effort: if the old cert is already gone from the keychain (e.g. the user
        // removed it by hand), that must not block trusting the new one.
        let _ = ca_trust::untrust_ca(&runner, &old.fingerprint_sha1).await;
    }

    match trust_ca_cmd(profile_id.clone(), secret_ref.clone()).await {
        Ok(record) => Ok(record),
        Err(e) => {
            // The old cert may already be out of the keychain (untrust above) while the new
            // one failed to go in -- keeping the stale record would report a false "trusted"
            // status on the next discover. Drop it so the CA correctly shows as untrusted
            // again rather than lying about its state.
            if let Ok(mut profile) = store::get_profile(&paths, &profile_id) {
                profile.trusted_cas.retain(|c| c.secret_ref != secret_ref);
                let _ = store::upsert_profile(&paths, profile);
            }
            Err(e)
        }
    }
}
```

- [ ] **Step 2: Register the module**

In `src-tauri/src/commands/mod.rs`, add (keeps alphabetical order — goes first):

```rust
pub mod ca_trust;
```

- [ ] **Step 3: Register the commands in `lib.rs`**

In `src-tauri/src/lib.rs`, inside the `tauri::generate_handler![...]` list, add after `commands::connection::open_url_in_browser,`:

```rust
            commands::ca_trust::discover_cluster_cas_cmd,
            commands::ca_trust::trust_ca_cmd,
            commands::ca_trust::replace_ca_cmd,
```

- [ ] **Step 4: Build and lint**

Run: `cd src-tauri && cargo build --lib && cargo clippy --all-targets --all-features -- -D warnings`
Expected: both clean.

- [ ] **Step 5: Run the full test suite once more**

Run: `cd src-tauri && cargo test --all-targets --all-features`
Expected: PASS (this task added no new tests, but must not have broken anything).

- [ ] **Step 6: Commit**

```bash
cd /Users/m/Documents/IdeaProjects/20.dasomel/clusterdeck
git add src-tauri/src/commands/ca_trust.rs src-tauri/src/commands/mod.rs src-tauri/src/lib.rs
git commit -m "feat(ca-trust): expose discover/trust/replace as Tauri commands"
```

### Task 10: Frontend types and API wrapper

**Files:**
- Modify: `src/api/tauri.ts`
- Modify: `src/components/ProfileEditor.tsx:342` (the one existing `Profile`-typed object literal in the frontend)

**Interfaces:**
- Consumes: the 3 commands from Task 9
- Produces: `CaTrustStatus`, `DiscoveredCaView`, `TrustedCa` TS types; `api.discoverClusterCas`, `api.trustCa`, `api.replaceCa`

- [ ] **Step 1: Add `trusted_cas` to the `Profile` type**

In `src/api/tauri.ts`, find:

```typescript
export type Profile = {
  id: string;
  name: string;
  hosts: Host[];
  bastion: Bastion | null;
  bootstrap: BootstrapPolicy;
  kubeconfig: KubeconfigSource | null;
  manage_hosts_file: boolean;
};
```

Replace with:

```typescript
export type Profile = {
  id: string;
  name: string;
  hosts: Host[];
  bastion: Bastion | null;
  bootstrap: BootstrapPolicy;
  kubeconfig: KubeconfigSource | null;
  manage_hosts_file: boolean;
  trusted_cas: TrustedCa[];
};
```

(`TrustedCa` is defined later in this same file — TypeScript type declarations are not order-dependent, so this compiles fine.)

- [ ] **Step 2: Add the CA types**

Find:

```typescript
export type DiscoveredEndpoint = {
  host: string;
  ip: string;
  source: string;
  resource_name: string;
};
```

Add immediately after it:

```typescript
export type CaTrustStatus = 'new' | 'trusted' | 'rotated';

export type DiscoveredCaView = {
  secret_ref: string;
  source_hosts: string[];
  subject_cn: string;
  not_after: string;
  fingerprint_sha256: string;
  status: CaTrustStatus;
};

export type TrustedCa = {
  secret_ref: string;
  fingerprint_sha256: string;
  fingerprint_sha1: string;
  subject_cn: string;
  not_after: string;
  trusted_at: string;
};
```

- [ ] **Step 3: Add the API wrapper functions**

Find the end of the `api` object:

```typescript
  openUrlInBrowser: (url: string) =>
    invoke<void>('open_url_in_browser', { url }),
};
```

Replace with:

```typescript
  openUrlInBrowser: (url: string) =>
    invoke<void>('open_url_in_browser', { url }),
  discoverClusterCas: (profileId: string, endpoints: DiscoveredEndpoint[]) =>
    invoke<DiscoveredCaView[]>('discover_cluster_cas_cmd', { profileId, endpoints }),
  trustCa: (profileId: string, secretRef: string) =>
    invoke<TrustedCa>('trust_ca_cmd', { profileId, secretRef }),
  replaceCa: (profileId: string, secretRef: string) =>
    invoke<TrustedCa>('replace_ca_cmd', { profileId, secretRef }),
};
```

- [ ] **Step 4: Fix the one existing `Profile`-typed literal in the frontend**

`src/components/ProfileEditor.tsx:295-343` builds `const finalProfile: Profile = { ... }` when saving the create/edit form. Find:

```typescript
      manage_hosts_file: manageHostsFile,
    };
```

Replace with:

```typescript
      manage_hosts_file: manageHostsFile,
      trusted_cas: initial?.trusted_cas ?? [],
    };
```

**Not** `trusted_cas: []` — `ProfileEditor` is also used to *edit* an existing profile (`initial: Profile | null` prop, `isEditing = initial !== null`), and `saveProfile` fully overwrites the stored profile. Defaulting to `[]` unconditionally would silently wipe every previously-trusted CA record the first time a user edited any other field (name, hosts, bootstrap policy, ...) and saved. `initial?.trusted_cas ?? []` matches the exact pattern every other field in this component already uses (see `initial?.id ?? ''`, `initial?.bastion`, etc. at the top of the component) — carry the existing value forward when editing, empty when creating new.

- [ ] **Step 5: Type-check**

Run: `pnpm exec tsc --noEmit`
Expected: no errors.

- [ ] **Step 6: Commit**

```bash
cd /Users/m/Documents/IdeaProjects/20.dasomel/clusterdeck
git add src/api/tauri.ts src/components/ProfileEditor.tsx
git commit -m "feat(ca-trust): add frontend types and API bindings"
```

### Task 11: UI — CA status in the Discovered Cluster Endpoints section

**Files:**
- Modify: `src/App.tsx`

**Interfaces:**
- Consumes: `api.discoverClusterCas`, `api.trustCa`, `api.replaceCa`, `DiscoveredCaView` (Task 10); `ConfirmModal` (already shipped, `src/components/ConfirmModal.tsx`); `StatusBanner`/`setStatusMessage` (already shipped)

- [ ] **Step 1: Import the new type**

In `src/App.tsx:3`, find:

```typescript
import { api, type ConnectionResult, type HostsFileStatus, type Profile, type VerificationResult } from './api/tauri';
```

Replace with:

```typescript
import { api, type ConnectionResult, type DiscoveredCaView, type HostsFileStatus, type Profile, type VerificationResult } from './api/tauri';
```

- [ ] **Step 2: Add state**

In `src/App.tsx:28`, find:

```typescript
  const [discoveringEndpoints, setDiscoveringEndpoints] = useState(false);
```

Replace with:

```typescript
  const [discoveringEndpoints, setDiscoveringEndpoints] = useState(false);
  const [caViews, setCaViews] = useState<DiscoveredCaView[]>([]);
  const [caActionTarget, setCaActionTarget] = useState<DiscoveredCaView | null>(null);
  const [caActionBusy, setCaActionBusy] = useState(false);
```

- [ ] **Step 3: Fetch CA status alongside endpoint discovery**

In `src/App.tsx`, inside `discoverEndpoints` (around line 240), find:

```typescript
      const eps = await api.discoverClusterEndpoints(selected.id);
      setLastResult((prev) => {
        if (!prev) {
          return {
            aliases_written: false,
            kubeconfig: null,
            verification: { ...EMPTY_VERIFICATION, kubernetes: true },
            endpoints: eps,
            errors: [],
            hosts: [],
          };
        }
        return { ...prev, endpoints: eps };
      });
      setStatusMessage({
        type: 'success',
        title: 'Endpoints Discovered',
        details: [`Found ${eps.length} external cluster endpoint(s) (APISIX, Ingress, Gateways)`],
        time: new Date().toLocaleTimeString(),
      });
```

Replace with:

```typescript
      const eps = await api.discoverClusterEndpoints(selected.id);
      setLastResult((prev) => {
        if (!prev) {
          return {
            aliases_written: false,
            kubeconfig: null,
            verification: { ...EMPTY_VERIFICATION, kubernetes: true },
            endpoints: eps,
            errors: [],
            hosts: [],
          };
        }
        return { ...prev, endpoints: eps };
      });
      // Best-effort: CA status is a nice-to-have overlay on top of endpoint discovery, so a
      // failure here (e.g. RBAC denies reading Secrets) must not turn a successful endpoint
      // scan into a reported error -- it just means the CA summary stays empty.
      try {
        setCaViews(await api.discoverClusterCas(selected.id, eps));
      } catch {
        setCaViews([]);
      }
      setStatusMessage({
        type: 'success',
        title: 'Endpoints Discovered',
        details: [`Found ${eps.length} external cluster endpoint(s) (APISIX, Ingress, Gateways)`],
        time: new Date().toLocaleTimeString(),
      });
```

- [ ] **Step 4: Add the trust/replace action handler**

In `src/App.tsx`, immediately after the `discoverEndpoints` function's closing `};`, add:

```typescript
  const executeCaTrustAction = async (target: DiscoveredCaView) => {
    if (!selected) return;
    setCaActionBusy(true);
    try {
      if (target.status === 'rotated') {
        await api.replaceCa(selected.id, target.secret_ref);
      } else {
        await api.trustCa(selected.id, target.secret_ref);
      }
      setCaActionTarget(null);
      setStatusMessage({
        type: 'success',
        title: 'CA Trusted',
        details: [
          `${target.subject_cn || target.secret_ref} is now trusted for ${target.source_hosts.length} host(s). Safari/Chrome will stop warning on them.`,
        ],
        time: new Date().toLocaleTimeString(),
      });
      const cas = await api.discoverClusterCas(selected.id, lastResult?.endpoints ?? []);
      setCaViews(cas);
    } catch (err) {
      setStatusMessage({
        type: 'error',
        title: 'CA Trust failed',
        details: [String(err)],
        time: new Date().toLocaleTimeString(),
      });
    } finally {
      setCaActionBusy(false);
    }
  };
```

- [ ] **Step 5: Run the app and locate the exact insertion point**

Run `make dev` (or `pnpm tauri dev`) and confirm the app launches — this task's remaining steps insert JSX at exact line numbers that shift slightly depending on the exact state of the file after Steps 1-4; re-read `src/App.tsx` around the "Discovered Cluster Endpoints" section (search for `Discovered Cluster Endpoints (Ingress, APISIX, Gateway API)`) before editing to confirm current line numbers.

- [ ] **Step 6: Insert the CA summary block**

Find (the section header, currently ending the way it was read during planning):

```tsx
                  {lastResult?.endpoints && lastResult.endpoints.length > 0 && (
                    <span className="pill" style={{ fontSize: '10px' }}>
                      {lastResult.endpoints.length} endpoint(s) discovered
                    </span>
                  )}
                </div>

                {lastResult?.endpoints && lastResult.endpoints.length > 0 ? (
```

Replace with:

```tsx
                  {lastResult?.endpoints && lastResult.endpoints.length > 0 && (
                    <span className="pill" style={{ fontSize: '10px' }}>
                      {lastResult.endpoints.length} endpoint(s) discovered
                    </span>
                  )}
                </div>

                {caViews.length > 0 && (
                  <div className="host-list" style={{ marginBottom: '8px' }}>
                    {caViews.map((ca) => (
                      <div className="host-row" key={ca.secret_ref}>
                        <div>
                          <div style={{ display: 'flex', alignItems: 'center', gap: '8px' }}>
                            <span className="host-name mono" style={{ fontSize: '12px' }}>
                              {ca.subject_cn || ca.secret_ref}
                            </span>
                            <span
                              className="pill"
                              style={{ fontSize: '10px', padding: '1px 6px', textTransform: 'uppercase' }}
                            >
                              {ca.status === 'trusted'
                                ? 'CA Trusted'
                                : ca.status === 'rotated'
                                  ? 'CA Changed'
                                  : 'CA Untrusted'}
                            </span>
                          </div>
                          <div className="host-address">
                            {ca.source_hosts.length} host(s) &middot; expires {ca.not_after || 'unknown'}
                          </div>
                        </div>
                        <div style={{ display: 'flex', alignItems: 'center', gap: '6px' }}>
                          {ca.status !== 'trusted' && (
                            <button
                              type="button"
                              className="secondary-button"
                              style={{ width: 'auto', marginTop: 0, padding: '5px 10px', fontSize: '11px' }}
                              onClick={() => setCaActionTarget(ca)}
                            >
                              {ca.status === 'rotated' ? 'Update Trust' : 'Trust CA'}
                            </button>
                          )}
                        </div>
                      </div>
                    ))}
                  </div>
                )}

                {lastResult?.endpoints && lastResult.endpoints.length > 0 ? (
```

- [ ] **Step 7: Add the ConfirmModal instance**

Find the existing delete-profile `ConfirmModal` block near the end of the component:

```tsx
      {profileToDelete && (
        <ConfirmModal
          title={`Delete Profile "${profileToDelete.name}"?`}
          message={`Are you sure you want to delete profile "${profileToDelete.name}" (${profileToDelete.id})? This will permanently remove its configuration, SSH alias, and locally synced kubeconfig.`}
          confirmLabel="Delete Profile"
          cancelLabel="Cancel"
          isDanger={true}
          busy={deletingProfile}
          onConfirm={() => executeDeleteProfile(profileToDelete)}
          onCancel={() => {
            if (!deletingProfile) setProfileToDelete(null);
          }}
        />
      )}
    </div>
  );
}
```

Replace with:

```tsx
      {profileToDelete && (
        <ConfirmModal
          title={`Delete Profile "${profileToDelete.name}"?`}
          message={`Are you sure you want to delete profile "${profileToDelete.name}" (${profileToDelete.id})? This will permanently remove its configuration, SSH alias, and locally synced kubeconfig.`}
          confirmLabel="Delete Profile"
          cancelLabel="Cancel"
          isDanger={true}
          busy={deletingProfile}
          onConfirm={() => executeDeleteProfile(profileToDelete)}
          onCancel={() => {
            if (!deletingProfile) setProfileToDelete(null);
          }}
        />
      )}

      {caActionTarget && (
        <ConfirmModal
          title={
            caActionTarget.status === 'rotated'
              ? `Update trust for "${caActionTarget.subject_cn || caActionTarget.secret_ref}"?`
              : `Trust CA "${caActionTarget.subject_cn || caActionTarget.secret_ref}"?`
          }
          message={
            caActionTarget.status === 'rotated'
              ? `This cluster's CA has changed since it was last trusted (likely a clean reinstall). Remove the old trust entry and trust the new certificate (SHA-256 ${caActionTarget.fingerprint_sha256.slice(0, 16)}..., expires ${caActionTarget.not_after || 'unknown'}) for ${caActionTarget.source_hosts.length} host(s)? macOS will ask you to confirm in a system dialog.`
              : `Add this certificate (SHA-256 ${caActionTarget.fingerprint_sha256.slice(0, 16)}..., expires ${caActionTarget.not_after || 'unknown'}) to your login keychain so Safari/Chrome stop warning on ${caActionTarget.source_hosts.length} host(s) behind it? macOS will ask you to confirm in a system dialog.`
          }
          confirmLabel={caActionTarget.status === 'rotated' ? 'Update Trust' : 'Trust CA'}
          cancelLabel="Cancel"
          isDanger={false}
          busy={caActionBusy}
          onConfirm={() => executeCaTrustAction(caActionTarget)}
          onCancel={() => {
            if (!caActionBusy) setCaActionTarget(null);
          }}
        />
      )}
    </div>
  );
}
```

- [ ] **Step 8: Type-check and build**

Run: `pnpm exec tsc --noEmit && pnpm build`
Expected: both clean.

- [ ] **Step 9: Manual verification in the running app**

This is the step that actually matters for this feature — type-checking proves the code compiles, not that it works. With a real cluster connected (or the `vagrant-beluga` profile used throughout this plan's fixtures):

1. `make dev`, open the profile, click "Scan Endpoints" (or whatever triggers `discoverEndpoints`).
2. Confirm a "CA Untrusted" row appears above the endpoint list, showing the subject CN and host count.
3. Click "Trust CA", confirm the `ConfirmModal` copy reads correctly, click confirm.
4. Approve the macOS Touch ID/password authorization dialog when it appears.
5. Confirm the row updates to "CA Trusted" and the `StatusBanner` shows success.
6. Open **Keychain Access.app**, search the login keychain for the cert's CN, confirm it shows "Always Trust".
7. Open one of the discovered `https://<host>` URLs in Safari or Chrome, confirm no certificate warning.
8. (Rotation path, optional but ideally verified once) Re-run cluster bootstrap so the CA regenerates, re-scan endpoints, confirm the row now shows "CA Changed" with an "Update Trust" button, click through it, confirm Keychain Access now shows only the new cert (old one removed).

Report which of these you actually ran and observed — do not claim this task complete on `tsc`/`pnpm build` passing alone; that only proves the code compiles, not that trust actually works (per `AGENTS.md`'s distinction between frontend/unit evidence and real runtime evidence).

- [ ] **Step 10: Commit**

```bash
cd /Users/m/Documents/IdeaProjects/20.dasomel/clusterdeck
git add src/App.tsx
git commit -m "feat(ca-trust): show CA trust status and wire trust/replace actions in the UI"
```

### Task 12: ADR

**Files:**
- Create: `docs/adr/0006-private-ca-local-trust.md`

Per `AGENTS.md`: "Architecture decisions belong in `docs/adr/`." This ADR is intentionally short — it records the decision and points at the design spec for full reasoning, per `AGENTS.md`'s "do not duplicate the same rule in multiple documents when a single authoritative source is sufficient."

- [ ] **Step 1: Write the ADR**

Create `docs/adr/0006-private-ca-local-trust.md`:

```markdown
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
```

- [ ] **Step 2: Commit**

```bash
cd /Users/m/Documents/IdeaProjects/20.dasomel/clusterdeck
git add docs/adr/0006-private-ca-local-trust.md
git commit -m "docs(adr): record private cluster CA local-trust decision"
```

---

## Final Verification

After all 12 tasks:

```bash
cd /Users/m/Documents/IdeaProjects/20.dasomel/clusterdeck
make verify
```

Expected: `cargo fmt --check`, `cargo clippy --all-targets --all-features -- -D warnings`,
`cargo test --all-targets --all-features`, and `pnpm build` all pass (EXIT=0). Then complete
Task 11 Step 9's manual verification in the real running app — this is the step that actually
proves the feature works, not just that it compiles.
