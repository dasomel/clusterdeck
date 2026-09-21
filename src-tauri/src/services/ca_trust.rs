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
}
