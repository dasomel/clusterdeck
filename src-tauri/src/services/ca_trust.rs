#![allow(dead_code)]

use std::collections::BTreeMap;
use std::path::Path;

use sha1::Sha1;
use sha2::{Digest, Sha256};

use crate::services::k8s_endpoints::decode_base64;
use crate::services::k8s_endpoints::{query_k8s_api_json, DiscoveredEndpoint};
use crate::services::validate::is_safe_host_domain;

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
        let snis = match spec.get("snis").and_then(|s| s.as_array()) {
            Some(s) => s,
            None => continue,
        };
        let matched = snis.iter().any(|s| {
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
    let keychain_path = resolve_login_keychain_path(runner).await?;
    let output = runner
        .run(
            "security",
            &[
                "delete-certificate".to_string(),
                "-Z".to_string(),
                fingerprint_sha1.to_string(),
                "-t".to_string(),
                "-k".to_string(),
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
                                            "snis": ["*.local.beluga.internal"],
                                            "secret": { "name": "apisix-gateway-tls-secret", "namespace": "platform-system" }
                                        }
                                    },
                                    {
                                        "metadata": { "name": "forbidden-tls", "namespace": "restricted-ns" },
                                        "spec": {
                                            "snis": ["forbidden.example.internal"],
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
        assert!(add_call.1.contains(&"-r".to_string()));
        assert!(add_call.1.contains(&"trustRoot".to_string()));
        assert!(add_call
            .1
            .contains(&"/Users/m/Library/Keychains/login.keychain-db".to_string()));
        let temp_path = add_call.1.last().unwrap();
        assert!(
            !Path::new(temp_path).exists(),
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
        assert!(delete_call.1.contains(&"-Z".to_string()));
        assert!(delete_call
            .1
            .contains(&"67fc8cc8df72476829ecd88d188331a6d29baabb".to_string()));
        assert!(delete_call.1.contains(&"-t".to_string()));
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
        assert!(create.success, "create-keychain failed: {}", create.stderr);

        let add = runner
            .run(
                "security",
                &[
                    "add-trusted-cert".to_string(),
                    "-k".to_string(),
                    test_keychain.to_string_lossy().to_string(),
                    "-r".to_string(),
                    "trustRoot".to_string(),
                    temp_pem.to_string_lossy().to_string(),
                ],
            )
            .await
            .unwrap();
        assert!(add.success, "add-trusted-cert failed: {}", add.stderr);

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
        assert!(
            find.success,
            "the trusted cert should be findable by its CN"
        );

        let delete = runner
            .run(
                "security",
                &[
                    "delete-certificate".to_string(),
                    "-Z".to_string(),
                    "67fc8cc8df72476829ecd88d188331a6d29baabb".to_string(),
                    "-t".to_string(),
                    "-k".to_string(),
                    test_keychain.to_string_lossy().to_string(),
                ],
            )
            .await
            .unwrap();
        assert!(
            delete.success,
            "delete-certificate failed: {}",
            delete.stderr
        );

        let _ = std::fs::remove_file(&temp_pem);
        let _ = runner
            .run(
                "security",
                &[
                    "delete-keychain".to_string(),
                    test_keychain.to_string_lossy().to_string(),
                ],
            )
            .await;
    }
}
