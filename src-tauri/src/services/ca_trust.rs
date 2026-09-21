#![allow(dead_code)]

use std::collections::BTreeMap;
use std::path::Path;

use serde::{Deserialize, Serialize};
use sha1::Sha1;
use sha2::{Digest, Sha256};

use crate::services::k8s_endpoints::decode_base64;
use crate::services::k8s_endpoints::{query_k8s_api_json, DiscoveredEndpoint};
use crate::services::validate::is_safe_host_domain;

pub fn pem_to_der(pem: &str) -> Result<Vec<u8>, String> {
    let cert_count = pem.matches("-----BEGIN CERTIFICATE-----").count();
    if cert_count > 1 {
        return Err(format!(
            "PEM contains {cert_count} certificates; expected exactly one (multi-certificate \
             bundles are not supported -- concatenating them would silently fingerprint a \
             value that is not any single certificate)"
        ));
    }
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

use crate::services::k8s_endpoints::{write_owner_only_file, TEMP_FILE_SEQ};
use crate::services::process::CommandRunner;

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

/// Returns the extension's body text following a `<header>` line in `openssl x509 -noout -text`
/// output, up to (but excluding) the next `X509v3 ...` extension header or the trailing
/// `Signature Algorithm:` line that always follows the extensions block. `None` means the header
/// itself was not found (the extension is absent); `Some("")` means it was found with an empty
/// body. Matching only a header *prefix* (not the full line) tolerates the `: critical` suffix
/// OpenSSL/LibreSSL appends to some extension headers (e.g. Basic Constraints, Key Usage).
fn extract_openssl_section(text: &str, header: &str) -> Option<String> {
    let mut lines = text.lines();
    lines
        .by_ref()
        .find(|line| line.trim_start().starts_with(header))?;
    let mut body = Vec::new();
    for line in lines {
        let trimmed = line.trim_start();
        if trimmed.starts_with("X509v3 ") || trimmed.starts_with("Signature Algorithm") {
            break;
        }
        body.push(trimmed);
    }
    Some(body.join(" "))
}

fn has_server_auth_eku(text: &str) -> bool {
    extract_openssl_section(text, "X509v3 Extended Key Usage")
        .is_some_and(|body| body.contains("TLS Web Server Authentication"))
}

fn has_ca_true_basic_constraint(text: &str) -> bool {
    extract_openssl_section(text, "X509v3 Basic Constraints")
        .is_some_and(|body| body.contains("CA:TRUE"))
}

fn has_cert_sign_key_usage(text: &str) -> bool {
    extract_openssl_section(text, "X509v3 Key Usage")
        .is_some_and(|body| body.contains("Certificate Sign"))
}

/// Parses `DNS:` SAN entries out of the `X509v3 Subject Alternative Name` section. Empty when
/// the extension is absent or has no `DNS:` entries (e.g. IP-only SANs).
fn parse_openssl_sans(text: &str) -> Vec<String> {
    extract_openssl_section(text, "X509v3 Subject Alternative Name")
        .map(|body| {
            body.split(',')
                .filter_map(|entry| entry.trim().strip_prefix("DNS:"))
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect()
        })
        .unwrap_or_default()
}

pub async fn extract_cert_metadata(runner: &dyn CommandRunner, pem: &str) -> (String, String) {
    let now_nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let seq = TEMP_FILE_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let temp_pem = std::env::temp_dir().join(format!("cd_ca_meta_{now_nanos}_{seq}.pem"));

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

pub struct TlsSecretRef {
    pub namespace: String,
    pub name: String,
}

pub fn resolve_ingress_secret_ref(
    ingresses: &serde_json::Value,
    host: &str,
) -> Option<TlsSecretRef> {
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

pub fn resolve_apisixtls_secret_ref(
    apisixtls: &serde_json::Value,
    host: &str,
) -> Option<TlsSecretRef> {
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
        // The ApisixTls CRD's field is `spec.hosts` (confirmed against a live cluster's raw API
        // response), not `spec.snis` -- `kubectl get apisixtls`'s table view labels this column
        // "SNIS" (a CRD-defined additionalPrinterColumns display name), which does not reflect
        // the actual JSON field name. An earlier version of this function read the wrong key
        // and always found zero matches; discover_cluster_cas silently returned 0 CAs on every
        // real cluster as a result -- no test caught it because the test fixtures were written
        // against the same wrong assumption.
        let hosts = match spec.get("hosts").and_then(|s| s.as_array()) {
            Some(s) => s,
            None => continue,
        };
        let matched = hosts.iter().any(|s| {
            s.as_str()
                .map(|s| sni_matches_host(s, host))
                .unwrap_or(false)
        });
        if matched {
            return Some(TlsSecretRef {
                namespace: ns.to_string(),
                name: name.to_string(),
            });
        }
    }
    None
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrustedCa {
    pub secret_ref: String,
    pub fingerprint_sha256: String,
    pub fingerprint_sha1: String,
    pub subject_cn: String,
    pub not_after: String,
    pub trusted_at: String,
}

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

pub struct DiscoveredCaMeta {
    pub pem: String,
    pub fingerprint_sha256: String,
    pub fingerprint_sha1: String,
    pub subject_cn: String,
    pub not_after: String,
    pub warnings: Vec<String>,
    pub leaf_sans: Vec<String>,
}

pub struct DiscoveredCa {
    pub secret_ref: String,
    pub source_hosts: Vec<String>,
    pub meta: DiscoveredCaMeta,
}

/// Returns `None` when openssl could not be run or exited non-zero -- callers must treat that as
/// "no signal" (skip the checks that read this text), never as "the text says the extension is
/// absent". Conflating the two would turn a tooling hiccup into a false health warning.
async fn run_openssl_text(runner: &dyn CommandRunner, pem_path: &str) -> Option<String> {
    runner
        .run(
            "openssl",
            &[
                "x509".to_string(),
                "-noout".to_string(),
                "-text".to_string(),
                "-in".to_string(),
                pem_path.to_string(),
            ],
        )
        .await
        .ok()
        .filter(|o| o.success)
        .map(|o| o.stdout)
}

/// Returns `Some(true)` when the cert is still valid at least `seconds` from now (exit 0),
/// `Some(false)` when it will genuinely expire by then, or `None` when openssl could not run, or
/// ran but couldn't evaluate the cert at all -- best-effort, same as every other check here: a
/// tooling/parse failure must never be misreported as a confident "will expire" claim.
///
/// A bare non-zero exit is NOT by itself "will expire": verified directly against a real LibreSSL
/// `openssl x509 -noout -checkend` (macOS system openssl -- see AGENTS.md) that a genuine
/// checkend result, true or false, is always silent on both stdout and stderr -- only the exit
/// code carries the answer. Every load/parse failure tried (non-PEM input, truncated PEM, missing
/// file, an out-of-range `-checkend` argument) instead exits 1 *and* prints `"unable to load
/// certificate"` (or, for a bad argument, `"checkend unusable: ..."`) to stderr. So exit-1-with-
/// output means "openssl couldn't evaluate this", not "it will expire" -- treat it as `None`.
async fn openssl_checkend(
    runner: &dyn CommandRunner,
    pem_path: &str,
    seconds: &str,
) -> Option<bool> {
    let output = runner
        .run(
            "openssl",
            &[
                "x509".to_string(),
                "-noout".to_string(),
                "-checkend".to_string(),
                seconds.to_string(),
                "-in".to_string(),
                pem_path.to_string(),
            ],
        )
        .await
        .ok()?;
    if output.success {
        Some(true)
    } else if output.stderr.is_empty() {
        Some(false)
    } else {
        None
    }
}

/// Leaf-cert health checks (EKU + expiry) plus its parsed SAN list, all from one `openssl -text`
/// shell-out against a single temp file. Best-effort throughout: any openssl failure yields no
/// warning for that check rather than a hard error, matching `extract_cert_metadata`.
async fn leaf_health_warnings(
    runner: &dyn CommandRunner,
    leaf_pem: &str,
) -> (Vec<String>, Vec<String>) {
    let now_nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let seq = TEMP_FILE_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let temp_pem = std::env::temp_dir().join(format!("cd_leaf_health_{now_nanos}_{seq}.pem"));

    if write_owner_only_file(&temp_pem, leaf_pem.as_bytes()).is_err() {
        return (Vec::new(), Vec::new());
    }
    let temp_pem_str = temp_pem.to_string_lossy().to_string();

    let mut warnings = Vec::new();
    let mut leaf_sans = Vec::new();

    let text_out = run_openssl_text(runner, &temp_pem_str).await;
    // Checkend is nested inside this `if let`, not run unconditionally: if `-text` couldn't even
    // parse the cert, openssl can't evaluate its expiry either, so there's no signal for any
    // check here -- matching how the EKU check below already skips silently in that case.
    if let Some(text) = text_out.as_deref() {
        leaf_sans = parse_openssl_sans(text);
        if !has_server_auth_eku(text) {
            warnings.push(
                "이 인증서엔 server auth 용도가 없어 CA를 신뢰해도 브라우저 경고가 계속될 수 있음"
                    .to_string(),
            );
        }

        let leaf_not_expired = openssl_checkend(runner, &temp_pem_str, "0").await;
        if leaf_not_expired == Some(false) {
            warnings.push("leaf 인증서가 이미 만료됨".to_string());
        } else if leaf_not_expired == Some(true)
            && openssl_checkend(runner, &temp_pem_str, "1209600").await == Some(false)
        {
            warnings.push("leaf 인증서가 14일 이내에 만료 예정".to_string());
        }
    }

    // Unconditional: these checks are best-effort, but the temp PEM must never linger.
    let _ = std::fs::remove_file(&temp_pem);

    (warnings, leaf_sans)
}

/// CA-cert health checks (structural validity + expiry), from one `openssl -text` shell-out
/// against a single temp file. Best-effort throughout, same rationale as `leaf_health_warnings`.
async fn ca_health_warnings(runner: &dyn CommandRunner, ca_pem: &str) -> Vec<String> {
    let now_nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let seq = TEMP_FILE_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let temp_pem = std::env::temp_dir().join(format!("cd_ca_health_{now_nanos}_{seq}.pem"));

    if write_owner_only_file(&temp_pem, ca_pem.as_bytes()).is_err() {
        return Vec::new();
    }
    let temp_pem_str = temp_pem.to_string_lossy().to_string();

    let mut warnings = Vec::new();

    let text_out = run_openssl_text(runner, &temp_pem_str).await;
    // Checkend is nested inside this `if let`, same rationale as leaf_health_warnings: no parsed
    // text means no signal for the expiry check either.
    if let Some(text) = text_out.as_deref() {
        if !has_ca_true_basic_constraint(text) || !has_cert_sign_key_usage(text) {
            warnings.push(
                "CA 인증서가 구조적으로 올바른 CA가 아님(CA:TRUE 또는 Certificate Sign 누락) — 신뢰해도 정상 동작하지 않을 수 있음"
                    .to_string(),
            );
        }

        // Already-expired first, same as leaf: a CA that's already expired is more severe than
        // one merely approaching expiry (it affects every host behind it) and deserves its own
        // message, not the milder "30 days" wording.
        let ca_not_expired = openssl_checkend(runner, &temp_pem_str, "0").await;
        if ca_not_expired == Some(false) {
            warnings.push("CA 인증서가 이미 만료됨".to_string());
        } else if ca_not_expired == Some(true)
            && openssl_checkend(runner, &temp_pem_str, "2592000").await == Some(false)
        {
            warnings.push("CA 인증서가 30일 이내에 만료 예정".to_string());
        }
    }

    let _ = std::fs::remove_file(&temp_pem);

    warnings
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
    // Nothing past this point can reach `tls.key` or any other field of the Secret -- only the
    // two public certificate fields, `ca.crt` and `tls.crt`, are ever extracted.
    let (pem, leaf_pem) = {
        let ca_crt_b64 = secret_json
            .get("data")
            .and_then(|d| d.get("ca.crt"))
            .and_then(|v| v.as_str())
            .ok_or_else(|| format!("secret {namespace}/{name} has no ca.crt key"))?;
        let ca_crt_bytes = decode_base64(ca_crt_b64)
            .ok_or_else(|| format!("secret {namespace}/{name}: ca.crt is not valid base64"))?;
        let pem = String::from_utf8(ca_crt_bytes)
            .map_err(|_| format!("secret {namespace}/{name}: ca.crt is not valid UTF-8 PEM"))?;

        // tls.crt (the leaf/server cert) is best-effort: unlike ca.crt, its absence must not
        // fail CA discovery -- it only means the leaf-specific health checks below are skipped.
        let leaf_pem = secret_json
            .get("data")
            .and_then(|d| d.get("tls.crt"))
            .and_then(|v| v.as_str())
            .and_then(decode_base64)
            .and_then(|bytes| String::from_utf8(bytes).ok());

        (pem, leaf_pem)
    };
    drop(secret_json);

    let der = pem_to_der(&pem)?;
    let fingerprint_sha256 = fingerprint_hex_sha256(&der);
    let fingerprint_sha1 = fingerprint_hex_sha1(&der);
    let (subject_cn, not_after) = extract_cert_metadata(runner, &pem).await;

    let mut warnings = ca_health_warnings(runner, &pem).await;
    let mut leaf_sans = Vec::new();
    if let Some(leaf_pem) = leaf_pem.as_deref() {
        let (leaf_warnings, sans) = leaf_health_warnings(runner, leaf_pem).await;
        warnings.extend(leaf_warnings);
        leaf_sans = sans;
    }

    Ok(DiscoveredCaMeta {
        pem,
        fingerprint_sha256,
        fingerprint_sha1,
        subject_cn,
        not_after,
        warnings,
        leaf_sans,
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
            Ok(mut meta) => {
                // Check 3 (SAN host coverage) needs source_hosts, which fetch_ca doesn't know --
                // only available here. Skip when leaf_sans is empty (tls.crt missing/unparseable)
                // rather than flagging every host as uncovered, which would be misleading: we
                // genuinely don't know, not "the leaf doesn't cover this host".
                if !meta.leaf_sans.is_empty() {
                    let unmatched: Vec<&str> = source_hosts
                        .iter()
                        .filter(|h| !meta.leaf_sans.iter().any(|s| sni_matches_host(s, h)))
                        .map(|s| s.as_str())
                        .collect();
                    if !unmatched.is_empty() {
                        meta.warnings.push(format!(
                            "다음 호스트는 이 인증서 SAN에 없음: {}",
                            unmatched.join(", ")
                        ));
                    }
                }
                result.push(DiscoveredCa {
                    secret_ref: format!("{namespace}/{name}"),
                    source_hosts,
                    meta,
                });
            }
            // RBAC denial, a missing ca.crt key, or a malformed cert must not fail endpoint
            // discovery as a whole -- skip this one secret, keep the rest.
            Err(_) => continue,
        }
    }

    // Secret-ref dedup above only avoids redundant fetches; it doesn't avoid redundant *rows*
    // when two different secrets happen to hold byte-identical certs (e.g. the same
    // cert-manager ClusterIssuer backing both an Ingress and an ApisixTls). Collapse those into
    // one row here, merging their source_hosts, per the spec's fingerprint-based dedup.
    let mut by_fingerprint: BTreeMap<String, DiscoveredCa> = BTreeMap::new();
    for ca in result {
        match by_fingerprint.get_mut(&ca.meta.fingerprint_sha256) {
            Some(existing) => {
                existing.source_hosts.extend(ca.source_hosts);
                // Two secrets sharing a CA fingerprint share identical CA-level warnings (they're
                // a function of the CA cert bytes alone, which the fingerprint match guarantees
                // are identical) but can carry *different* leaf certs -- e.g. two Ingresses using
                // distinct per-host TLS secrets issued by the same shared CA. Merge rather than
                // drop the second secret's leaf-derived warnings (EKU/expiry/SAN), deduped so an
                // identical message from both sides doesn't show up twice.
                for warning in ca.meta.warnings {
                    if !existing.meta.warnings.contains(&warning) {
                        existing.meta.warnings.push(warning);
                    }
                }
            }
            None => {
                by_fingerprint.insert(ca.meta.fingerprint_sha256.clone(), ca);
            }
        }
    }
    Ok(by_fingerprint.into_values().collect())
}

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
        return Err(format!(
            "security default-keychain failed: {}",
            output.stderr
        ));
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
    let seq = crate::services::k8s_endpoints::TEMP_FILE_SEQ
        .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
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
        return Err(format!(
            "security add-trusted-cert failed: {}",
            output.stderr
        ));
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
        return Err(format!(
            "security delete-certificate failed: {}",
            output.stderr
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::prelude::*;

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

    #[test]
    fn pem_to_der_rejects_multi_certificate_bundle() {
        let bundle = format!("{TEST_CA_PEM}{TEST_CA_PEM}");
        let result = pem_to_der(&bundle);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains('2'));
    }

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

    #[test]
    fn has_server_auth_eku_detects_presence_and_absence() {
        let with_eku = "Certificate:\n    Data:\n        X509v3 extensions:\n            \
             X509v3 Extended Key Usage: \n                TLS Web Server Authentication, TLS \
             Web Client Authentication\n            X509v3 Subject Alternative Name: \n         \
             DNS:example.internal\n    Signature Algorithm: sha256WithRSAEncryption";
        assert!(has_server_auth_eku(with_eku));

        // Reproduces the real incident this feature exists to catch: cert-manager's Certificate
        // had no `usages` field, so the leaf defaulted to `digital signature, key encipherment`
        // only -- no Extended Key Usage extension is emitted at all in that case.
        let without_eku = "Certificate:\n    Data:\n        X509v3 extensions:\n            \
             X509v3 Key Usage: critical\n                Digital Signature, Key \
             Encipherment\n            X509v3 Subject Alternative Name: \n                \
             DNS:example.internal\n    Signature Algorithm: sha256WithRSAEncryption";
        assert!(!has_server_auth_eku(without_eku));

        let eku_present_without_server_auth =
            "            X509v3 Extended Key Usage: \n                TLS Web Client \
             Authentication\n    Signature Algorithm: sha256WithRSAEncryption";
        assert!(!has_server_auth_eku(eku_present_without_server_auth));

        assert!(!has_server_auth_eku("no extensions at all"));
    }

    #[test]
    fn ca_structural_checks_detect_valid_and_invalid_ca() {
        let valid_ca = "            X509v3 Basic Constraints: critical\n                \
             CA:TRUE\n            X509v3 Key Usage: critical\n                Digital Signature, \
             Certificate Sign, CRL Sign\n    Signature Algorithm: sha256WithRSAEncryption";
        assert!(has_ca_true_basic_constraint(valid_ca));
        assert!(has_cert_sign_key_usage(valid_ca));

        let not_a_ca = "            X509v3 Basic Constraints: critical\n                \
             CA:FALSE\n            X509v3 Key Usage: critical\n                Digital \
             Signature, Key Encipherment\n    Signature Algorithm: sha256WithRSAEncryption";
        assert!(!has_ca_true_basic_constraint(not_a_ca));
        assert!(!has_cert_sign_key_usage(not_a_ca));

        assert!(!has_ca_true_basic_constraint("no extensions at all"));
        assert!(!has_cert_sign_key_usage("no extensions at all"));
    }

    #[test]
    fn parse_openssl_sans_extracts_dns_entries() {
        let text = "            X509v3 Subject Alternative Name: \n                \
             DNS:argocd.local.beluga.internal, DNS:*.local.beluga.internal\n    Signature \
             Algorithm: sha256WithRSAEncryption";
        assert_eq!(
            parse_openssl_sans(text),
            vec![
                "argocd.local.beluga.internal".to_string(),
                "*.local.beluga.internal".to_string(),
            ]
        );

        assert_eq!(
            parse_openssl_sans("no extensions here"),
            Vec::<String>::new()
        );
    }

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

    /// Serves canned `openssl x509 -noout -text` and `-checkend <seconds>` responses for
    /// `leaf_health_warnings`/`ca_health_warnings` tests, independent of the actual temp PEM
    /// content (these functions only care about argv shape, matching how the rest of this test
    /// file's FakeRunners mock openssl).
    struct FakeCertHealthRunner {
        text_output: String,
        /// `-checkend <seconds>` succeeds (exit 0, cert still valid) iff the requested window is
        /// strictly less than this -- mirrors real openssl semantics for a cert with exactly
        /// this many seconds left before expiry. Negative values simulate an already-expired cert.
        seconds_until_expiry: i64,
    }

    #[async_trait::async_trait]
    impl CommandRunner for FakeCertHealthRunner {
        async fn run(
            &self,
            bin: &str,
            args: &[String],
        ) -> Result<crate::services::process::CommandOutput, String> {
            assert_eq!(bin, "openssl");
            if args.contains(&"-text".to_string()) {
                return Ok(ok_output(&self.text_output));
            }
            if let Some(idx) = args.iter().position(|a| a == "-checkend") {
                let seconds: i64 = args[idx + 1].parse().expect("checkend seconds is numeric");
                return Ok(crate::services::process::CommandOutput {
                    stdout: String::new(),
                    stderr: String::new(),
                    success: seconds < self.seconds_until_expiry,
                });
            }
            panic!("unexpected openssl args: {args:?}");
        }
    }

    const HEALTHY_LEAF_TEXT: &str = "            X509v3 Extended Key Usage: \n                \
         TLS Web Server Authentication\n            X509v3 Subject Alternative Name: \n         \
         DNS:argocd.local.beluga.internal\n    Signature Algorithm: sha256WithRSAEncryption";

    const MISSING_EKU_LEAF_TEXT: &str = "            X509v3 Key Usage: critical\n                \
         Digital Signature, Key Encipherment\n            X509v3 Subject Alternative Name: \n    \
         DNS:argocd.local.beluga.internal\n    Signature Algorithm: sha256WithRSAEncryption";

    #[tokio::test]
    async fn leaf_health_warnings_is_empty_for_a_healthy_leaf_far_from_expiry() {
        let runner = FakeCertHealthRunner {
            text_output: HEALTHY_LEAF_TEXT.to_string(),
            seconds_until_expiry: 365 * 24 * 3600,
        };
        let (warnings, sans) = leaf_health_warnings(&runner, TEST_CA_PEM).await;
        assert!(
            warnings.is_empty(),
            "expected no warnings, got {warnings:?}"
        );
        assert_eq!(sans, vec!["argocd.local.beluga.internal".to_string()]);
    }

    #[tokio::test]
    async fn leaf_health_warnings_flags_missing_server_auth_eku() {
        let runner = FakeCertHealthRunner {
            text_output: MISSING_EKU_LEAF_TEXT.to_string(),
            seconds_until_expiry: 365 * 24 * 3600,
        };
        let (warnings, _) = leaf_health_warnings(&runner, TEST_CA_PEM).await;
        assert_eq!(
            warnings,
            vec![
                "이 인증서엔 server auth 용도가 없어 CA를 신뢰해도 브라우저 경고가 계속될 수 있음"
                    .to_string()
            ]
        );
    }

    #[tokio::test]
    async fn leaf_health_warnings_flags_already_expired_and_suppresses_the_14_day_message() {
        let runner = FakeCertHealthRunner {
            text_output: HEALTHY_LEAF_TEXT.to_string(),
            seconds_until_expiry: -3600, // expired 1 hour ago
        };
        let (warnings, _) = leaf_health_warnings(&runner, TEST_CA_PEM).await;
        assert_eq!(warnings, vec!["leaf 인증서가 이미 만료됨".to_string()]);
    }

    #[tokio::test]
    async fn leaf_health_warnings_flags_expiring_within_14_days() {
        let runner = FakeCertHealthRunner {
            text_output: HEALTHY_LEAF_TEXT.to_string(),
            seconds_until_expiry: 3600, // 1 hour left: within 14 days, not yet expired
        };
        let (warnings, _) = leaf_health_warnings(&runner, TEST_CA_PEM).await;
        assert_eq!(
            warnings,
            vec!["leaf 인증서가 14일 이내에 만료 예정".to_string()]
        );
    }

    const VALID_CA_TEXT: &str = "            X509v3 Basic Constraints: critical\n                \
         CA:TRUE\n            X509v3 Key Usage: critical\n                Digital Signature, \
         Certificate Sign, CRL Sign\n    Signature Algorithm: sha256WithRSAEncryption";

    const NOT_A_CA_TEXT: &str = "            X509v3 Basic Constraints: critical\n                \
         CA:FALSE\n            X509v3 Key Usage: critical\n                Digital Signature, \
         Key Encipherment\n    Signature Algorithm: sha256WithRSAEncryption";

    #[tokio::test]
    async fn ca_health_warnings_is_empty_for_a_structurally_valid_ca_far_from_expiry() {
        let runner = FakeCertHealthRunner {
            text_output: VALID_CA_TEXT.to_string(),
            seconds_until_expiry: 365 * 24 * 3600,
        };
        let warnings = ca_health_warnings(&runner, TEST_CA_PEM).await;
        assert!(
            warnings.is_empty(),
            "expected no warnings, got {warnings:?}"
        );
    }

    #[tokio::test]
    async fn ca_health_warnings_flags_structurally_invalid_ca() {
        let runner = FakeCertHealthRunner {
            text_output: NOT_A_CA_TEXT.to_string(),
            seconds_until_expiry: 365 * 24 * 3600,
        };
        let warnings = ca_health_warnings(&runner, TEST_CA_PEM).await;
        assert_eq!(
            warnings,
            vec![
                "CA 인증서가 구조적으로 올바른 CA가 아님(CA:TRUE 또는 Certificate Sign 누락) — \
                  신뢰해도 정상 동작하지 않을 수 있음"
                    .to_string()
            ]
        );
    }

    #[tokio::test]
    async fn ca_health_warnings_flags_expiring_within_30_days() {
        let runner = FakeCertHealthRunner {
            text_output: VALID_CA_TEXT.to_string(),
            seconds_until_expiry: 3600,
        };
        let warnings = ca_health_warnings(&runner, TEST_CA_PEM).await;
        assert_eq!(
            warnings,
            vec!["CA 인증서가 30일 이내에 만료 예정".to_string()]
        );
    }

    /// MEDIUM fix (independent review of a0045c0): an already-expired CA must get its own
    /// message, not the milder "expiring within 30 days" wording -- mirrors
    /// `leaf_health_warnings_flags_already_expired_and_suppresses_the_14_day_message`.
    #[tokio::test]
    async fn ca_health_warnings_flags_already_expired_and_suppresses_the_30_day_message() {
        let runner = FakeCertHealthRunner {
            text_output: VALID_CA_TEXT.to_string(),
            seconds_until_expiry: -3600, // expired 1 hour ago
        };
        let warnings = ca_health_warnings(&runner, TEST_CA_PEM).await;
        assert_eq!(warnings, vec!["CA 인증서가 이미 만료됨".to_string()]);
    }

    #[tokio::test]
    async fn leaf_and_ca_health_checks_produce_no_warnings_when_openssl_cannot_run() {
        struct UnavailableOpensslRunner;
        #[async_trait::async_trait]
        impl CommandRunner for UnavailableOpensslRunner {
            async fn run(
                &self,
                _bin: &str,
                _args: &[String],
            ) -> Result<crate::services::process::CommandOutput, String> {
                Err("'openssl' executable not found".to_string())
            }
        }
        let (warnings, sans) = leaf_health_warnings(&UnavailableOpensslRunner, TEST_CA_PEM).await;
        assert!(warnings.is_empty());
        assert!(sans.is_empty());

        let warnings = ca_health_warnings(&UnavailableOpensslRunner, TEST_CA_PEM).await;
        assert!(warnings.is_empty());
    }

    /// HIGH regression (independent review of a0045c0): `openssl_checkend` must not treat a bare
    /// non-zero exit as "genuinely expiring" -- verified directly against a real LibreSSL
    /// `openssl x509 -noout -checkend` (see the function's doc comment for the exact evidence)
    /// that a genuine result is always silent on stderr, while a load/parse failure always prints
    /// to it. This is the function-level version of that contract; the two tests below cover it
    /// at the `leaf_health_warnings`/`ca_health_warnings` level and through `discover_cluster_cas`
    /// (see the added assertion in
    /// `discover_cluster_cas_collapses_different_secrets_sharing_one_fingerprint`).
    #[tokio::test]
    async fn openssl_checkend_distinguishes_genuine_expiry_from_a_load_failure() {
        struct FakeCheckendRunner {
            success: bool,
            stderr: &'static str,
        }
        #[async_trait::async_trait]
        impl CommandRunner for FakeCheckendRunner {
            async fn run(
                &self,
                bin: &str,
                args: &[String],
            ) -> Result<crate::services::process::CommandOutput, String> {
                assert_eq!(bin, "openssl");
                assert!(args.contains(&"-checkend".to_string()));
                Ok(crate::services::process::CommandOutput {
                    stdout: String::new(),
                    stderr: self.stderr.to_string(),
                    success: self.success,
                })
            }
        }

        // Genuine "not expiring": exit 0, silent.
        let genuinely_valid = FakeCheckendRunner {
            success: true,
            stderr: "",
        };
        assert_eq!(
            openssl_checkend(&genuinely_valid, "/tmp/x.pem", "0").await,
            Some(true)
        );

        // Genuine "will expire": exit 1, still silent -- this is the case the HIGH bug got wrong.
        let genuinely_expiring = FakeCheckendRunner {
            success: false,
            stderr: "",
        };
        assert_eq!(
            openssl_checkend(&genuinely_expiring, "/tmp/x.pem", "0").await,
            Some(false)
        );

        // A load/parse failure: exit 1 WITH stderr output -- must be "no signal", never
        // "expiring".
        let load_failure = FakeCheckendRunner {
            success: false,
            stderr: "unable to load certificate",
        };
        assert_eq!(
            openssl_checkend(&load_failure, "/tmp/x.pem", "0").await,
            None
        );
    }

    #[tokio::test]
    async fn leaf_and_ca_health_checks_produce_no_warnings_when_openssl_exits_nonzero_with_stderr_output(
    ) {
        struct NoisyFailureRunner;
        #[async_trait::async_trait]
        impl CommandRunner for NoisyFailureRunner {
            async fn run(
                &self,
                _bin: &str,
                _args: &[String],
            ) -> Result<crate::services::process::CommandOutput, String> {
                // Mirrors a real openssl load/parse failure, and this file's other FakeRunners'
                // catch-all branches: non-zero exit WITH stderr output, never silent. Before the
                // openssl_checkend fix, this shape (an `Ok` response with `success: false`, as
                // opposed to `run` itself returning `Err`) was misread as a genuine expiry
                // result.
                Ok(crate::services::process::CommandOutput {
                    stdout: String::new(),
                    stderr: "unexpected command".to_string(),
                    success: false,
                })
            }
        }
        let (warnings, sans) = leaf_health_warnings(&NoisyFailureRunner, TEST_CA_PEM).await;
        assert!(
            warnings.is_empty(),
            "expected no warnings, got {warnings:?}"
        );
        assert!(sans.is_empty());

        let warnings = ca_health_warnings(&NoisyFailureRunner, TEST_CA_PEM).await;
        assert!(
            warnings.is_empty(),
            "expected no warnings, got {warnings:?}"
        );
    }

    #[test]
    fn sni_matches_host_handles_exact_and_wildcard() {
        assert!(sni_matches_host(
            "local.beluga.internal",
            "local.beluga.internal"
        ));
        assert!(sni_matches_host(
            "*.local.beluga.internal",
            "argocd.local.beluga.internal"
        ));
        assert!(!sni_matches_host(
            "*.local.beluga.internal",
            "local.beluga.internal"
        ));
        assert!(!sni_matches_host(
            "*.local.beluga.internal",
            "a.b.local.beluga.internal"
        ));
        assert!(!sni_matches_host(
            "*.local.beluga.internal",
            "evillocal.beluga.internal"
        ));
        assert!(!sni_matches_host(
            "*.local.beluga.internal",
            "argocd.other.internal"
        ));
    }

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
                    "hosts": ["*.local.beluga.internal", "local.beluga.internal"],
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
                                    "hosts": ["*.local.beluga.internal", "local.beluga.internal"],
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

    struct FakeSameFingerprintDifferentSecretsRunner;

    #[async_trait::async_trait]
    impl CommandRunner for FakeSameFingerprintDifferentSecretsRunner {
        async fn run(
            &self,
            bin: &str,
            args: &[String],
        ) -> Result<crate::services::process::CommandOutput, String> {
            if bin == "kubectl" {
                if let Some(path_idx) = args.iter().position(|a| a == "--raw") {
                    let raw_path = &args[path_idx + 1];
                    // One host resolves via an Ingress, the other via an ApisixTls -- two
                    // different secret refs, in different namespaces, so the pre-fetch
                    // (namespace, name) dedup in discover_cluster_cas does NOT collapse them.
                    if raw_path == "/apis/networking.k8s.io/v1/ingresses" {
                        return Ok(ok_output(
                            r#"{
                            "items": [{
                                "metadata": { "name": "web", "namespace": "apps" },
                                "spec": {
                                    "tls": [{ "hosts": ["ingress.example.internal"], "secretName": "ingress-tls-secret" }]
                                }
                            }]
                        }"#,
                        ));
                    }
                    if raw_path == "/apis/apisix.apache.org/v2/apisixtlses" {
                        return Ok(ok_output(
                            r#"{
                            "items": [{
                                "metadata": { "name": "apisix-gateway-tls", "namespace": "platform-system" },
                                "spec": {
                                    "hosts": ["apisix.example.internal"],
                                    "secret": { "name": "apisix-gateway-tls-secret", "namespace": "platform-system" }
                                }
                            }]
                        }"#,
                        ));
                    }
                    // Both secrets hold byte-identical ca.crt content (e.g. the same
                    // cert-manager ClusterIssuer backing both resources).
                    if raw_path == "/api/v1/namespaces/apps/secrets/ingress-tls-secret"
                        || raw_path
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
    async fn discover_cluster_cas_collapses_different_secrets_sharing_one_fingerprint() {
        let endpoints = vec![
            DiscoveredEndpoint {
                host: "ingress.example.internal".to_string(),
                ip: "192.168.77.10".to_string(),
                source: "ingress".to_string(),
                resource_name: "apps/web".to_string(),
            },
            DiscoveredEndpoint {
                host: "apisix.example.internal".to_string(),
                ip: "192.168.77.200".to_string(),
                source: "apisix".to_string(),
                resource_name: "platform-system/argocd".to_string(),
            },
        ];
        let temp_kc = std::env::temp_dir().join("test-ca-trust-dummy-kc-fp-collapse.yaml");
        let _ = std::fs::write(&temp_kc, "dummy");

        let result = discover_cluster_cas(
            &FakeSameFingerprintDifferentSecretsRunner,
            &temp_kc,
            &endpoints,
        )
        .await
        .unwrap();
        let _ = std::fs::remove_file(&temp_kc);

        // apps/ingress-tls-secret and platform-system/apisix-gateway-tls-secret are two
        // distinct secret refs -- proving this collapse is NOT just the existing pre-fetch
        // (namespace, name) dedup -- but their ca.crt is byte-identical, so the post-fetch
        // fingerprint pass must collapse them into a single row with both source_hosts merged.
        assert_eq!(result.len(), 1);
        assert_eq!(
            result[0].meta.fingerprint_sha256,
            "8b75bf97e19ce7efe9bb4d6c76b4f10e072a9d6ea89d8f438f423afea7156246"
        );
        assert_eq!(result[0].source_hosts.len(), 2);
        assert!(result[0]
            .source_hosts
            .contains(&"ingress.example.internal".to_string()));
        assert!(result[0]
            .source_hosts
            .contains(&"apisix.example.internal".to_string()));
        // Regression: this fixture's FakeRunner doesn't recognize `-text`/`-checkend` and falls
        // through to a catch-all `success: false` response with non-empty stderr. Before the
        // openssl_checkend fix, that was misread as a genuine "will expire" result, producing a
        // false "CA 인증서가 30일 이내에 만료 예정" warning for TEST_CA_PEM, which is valid until
        // 2036.
        assert!(
            result[0].meta.warnings.is_empty(),
            "expected no warnings from an unrecognized-command openssl response, got {:?}",
            result[0].meta.warnings
        );
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

        // Unsafe namespace, otherwise-safe name.
        let result = fetch_ca(&UnusedRunner, &temp_kc, "../../etc", "passwd").await;
        assert!(result.is_err());

        // Safe namespace, unsafe name -- proves the `name` half of the `||` check is enforced
        // independently, not merely short-circuited past because `namespace` already failed.
        let result = fetch_ca(&UnusedRunner, &temp_kc, "default", "../../etc").await;
        assert!(result.is_err());

        let _ = std::fs::remove_file(&temp_kc);
    }

    #[tokio::test]
    async fn discover_cluster_cas_skips_secret_fetch_failure_without_failing_the_batch() {
        struct FakePartialFailureRunner;

        #[async_trait::async_trait]
        impl CommandRunner for FakePartialFailureRunner {
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
                                "items": [
                                    {
                                        "metadata": { "name": "apisix-gateway-tls", "namespace": "platform-system" },
                                        "spec": {
                                            "hosts": ["*.local.beluga.internal"],
                                            "secret": { "name": "apisix-gateway-tls-secret", "namespace": "platform-system" }
                                        }
                                    },
                                    {
                                        "metadata": { "name": "forbidden-tls", "namespace": "restricted-ns" },
                                        "spec": {
                                            "hosts": ["forbidden.example.internal"],
                                            "secret": { "name": "forbidden-secret", "namespace": "restricted-ns" }
                                        }
                                    }
                                ]
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
                        // Simulates an RBAC-denied `kubectl get --raw` on this one secret: a
                        // non-2xx API response, surfaced by kubectl as a failed exit status.
                        if raw_path == "/api/v1/namespaces/restricted-ns/secrets/forbidden-secret" {
                            return Ok(crate::services::process::CommandOutput {
                                stdout: r#"{"code":403,"reason":"Forbidden"}"#.to_string(),
                                stderr: "Forbidden".to_string(),
                                success: false,
                            });
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

        let endpoints = vec![
            DiscoveredEndpoint {
                host: "argocd.local.beluga.internal".to_string(),
                ip: "192.168.77.200".to_string(),
                source: "apisix".to_string(),
                resource_name: "platform-system/argocd".to_string(),
            },
            DiscoveredEndpoint {
                host: "forbidden.example.internal".to_string(),
                ip: "192.168.77.201".to_string(),
                source: "apisix".to_string(),
                resource_name: "restricted-ns/secret-app".to_string(),
            },
        ];
        let temp_kc = std::env::temp_dir().join("test-ca-trust-dummy-kc-5.yaml");
        let _ = std::fs::write(&temp_kc, "dummy");

        let result = discover_cluster_cas(&FakePartialFailureRunner, &temp_kc, &endpoints)
            .await
            .unwrap();
        let _ = std::fs::remove_file(&temp_kc);

        // The forbidden secret's fetch failure (`Err(_) => continue`) must not fail the whole
        // batch -- the CA behind the still-fetchable secret is present, and only that one.
        assert_eq!(result.len(), 1);
        assert_eq!(
            result[0].secret_ref,
            "platform-system/apisix-gateway-tls-secret"
        );
        assert_eq!(
            result[0].source_hosts,
            vec!["argocd.local.beluga.internal".to_string()]
        );
    }

    /// Serves the fixed apisix/secret fixture used by both SAN-coverage tests below, varying
    /// only the leaf's SAN list via `leaf_sans_text` -- e.g. `"DNS:other.local.beluga.internal"`
    /// or `"DNS:*.local.beluga.internal"`. CA structure and expiry are always healthy so the
    /// only warning either test can see is check 3's SAN-coverage one.
    struct FakeSanCheckRunner {
        leaf_sans_text: &'static str,
    }

    #[async_trait::async_trait]
    impl CommandRunner for FakeSanCheckRunner {
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
                                    "hosts": ["argocd.local.beluga.internal"],
                                    "secret": { "name": "apisix-gateway-tls-secret", "namespace": "platform-system" }
                                }
                            }]
                        }"#,
                        ));
                    }
                    if raw_path
                        == "/api/v1/namespaces/platform-system/secrets/apisix-gateway-tls-secret"
                    {
                        let cert_b64 = BASE64_STANDARD.encode(TEST_CA_PEM.as_bytes());
                        return Ok(ok_output(&format!(
                            r#"{{"data": {{"ca.crt": "{cert_b64}", "tls.crt": "{cert_b64}", "tls.key": "unused"}}}}"#
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
                if args.contains(&"-text".to_string()) {
                    return Ok(ok_output(&format!(
                        "            X509v3 Basic Constraints: critical\n                \
                         CA:TRUE\n            X509v3 Key Usage: critical\n                \
                         Digital Signature, Certificate Sign, CRL Sign\n            X509v3 \
                         Extended Key Usage: \n                TLS Web Server \
                         Authentication\n            X509v3 Subject Alternative Name: \n         \
                         {}\n    Signature Algorithm: sha256WithRSAEncryption",
                        self.leaf_sans_text
                    )));
                }
                if args.contains(&"-checkend".to_string()) {
                    // Far from expiry in both scenarios this fake serves.
                    return Ok(ok_output(""));
                }
            }
            Ok(crate::services::process::CommandOutput {
                stdout: String::new(),
                stderr: format!("unexpected command: {bin} {args:?}"),
                success: false,
            })
        }
    }

    #[tokio::test]
    async fn discover_cluster_cas_flags_host_not_covered_by_leaf_san() {
        let endpoints = vec![DiscoveredEndpoint {
            host: "argocd.local.beluga.internal".to_string(),
            ip: "192.168.77.200".to_string(),
            source: "apisix".to_string(),
            resource_name: "platform-system/argocd".to_string(),
        }];
        let temp_kc = std::env::temp_dir().join("test-ca-trust-dummy-kc-san-mismatch.yaml");
        let _ = std::fs::write(&temp_kc, "dummy");

        let runner = FakeSanCheckRunner {
            leaf_sans_text: "DNS:other.local.beluga.internal",
        };
        let result = discover_cluster_cas(&runner, &temp_kc, &endpoints)
            .await
            .unwrap();
        let _ = std::fs::remove_file(&temp_kc);

        assert_eq!(result.len(), 1);
        assert!(
            result[0]
                .meta
                .warnings
                .iter()
                .any(|w| w.contains("argocd.local.beluga.internal")),
            "expected a SAN-coverage warning naming the uncovered host, got {:?}",
            result[0].meta.warnings
        );
    }

    #[tokio::test]
    async fn discover_cluster_cas_no_san_warning_when_wildcard_covers_host() {
        let endpoints = vec![DiscoveredEndpoint {
            host: "argocd.local.beluga.internal".to_string(),
            ip: "192.168.77.200".to_string(),
            source: "apisix".to_string(),
            resource_name: "platform-system/argocd".to_string(),
        }];
        let temp_kc = std::env::temp_dir().join("test-ca-trust-dummy-kc-san-match.yaml");
        let _ = std::fs::write(&temp_kc, "dummy");

        let runner = FakeSanCheckRunner {
            leaf_sans_text: "DNS:*.local.beluga.internal",
        };
        let result = discover_cluster_cas(&runner, &temp_kc, &endpoints)
            .await
            .unwrap();
        let _ = std::fs::remove_file(&temp_kc);

        assert_eq!(result.len(), 1);
        assert!(
            result[0].meta.warnings.is_empty(),
            "expected no warnings when the wildcard SAN covers the host, got {:?}",
            result[0].meta.warnings
        );
    }

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
                Ok(ok_output(
                    "\"/Users/m/Library/Keychains/login.keychain-db\"",
                ))
            }
        }
        let path = resolve_login_keychain_path(&FakeSecurityRunner)
            .await
            .unwrap();
        assert_eq!(path, "/Users/m/Library/Keychains/login.keychain-db");
    }

    #[tokio::test]
    async fn trust_ca_builds_expected_argv_and_cleans_up_temp_file() {
        use std::sync::{Arc, Mutex};

        #[allow(clippy::type_complexity)]
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
                self.calls
                    .lock()
                    .unwrap()
                    .push((bin.to_string(), args.to_vec()));
                if bin == "security" && args.first().map(String::as_str) == Some("default-keychain")
                {
                    return Ok(ok_output(
                        "\"/Users/m/Library/Keychains/login.keychain-db\"",
                    ));
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
                    return Ok(ok_output(
                        "\"/Users/m/Library/Keychains/login.keychain-db\"",
                    ));
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

        #[allow(clippy::type_complexity)]
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
                self.calls
                    .lock()
                    .unwrap()
                    .push((bin.to_string(), args.to_vec()));
                if bin == "security" && args.first().map(String::as_str) == Some("default-keychain")
                {
                    return Ok(ok_output(
                        "\"/Users/m/Library/Keychains/login.keychain-db\"",
                    ));
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
        assert!(untrust_ca(&UnusedRunner, "not-a-fingerprint")
            .await
            .is_err());
        assert!(untrust_ca(&UnusedRunner, "").await.is_err());
        assert!(
            untrust_ca(&UnusedRunner, "67fc8cc8df72476829ecd88d188331a6d29baa")
                .await
                .is_err()
        ); // 39 chars, one short
    }

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
        assert!(
            find.success,
            "the trusted cert should be findable by its CN"
        );
        assert!(
            delete.success,
            "delete-certificate failed: {}",
            delete.stderr
        );
    }

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
                warnings: Vec::new(),
                leaf_sans: Vec::new(),
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
}
