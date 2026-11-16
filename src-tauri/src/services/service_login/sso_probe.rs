use std::time::Duration;

use super::keycloak::parse_realm_url;
use crate::services::k8s_endpoints::DiscoveredEndpoint;
use crate::services::process::{CommandLimits, CommandRunner};
use crate::services::validate::{is_safe_host_domain, is_safe_ip_address};

// ADR-0014 SSO amendment 2: the app's own public login surface is read-only evidence that it
// signs in via an OIDC provider. Two unauthenticated GETs per endpoint at most, sent to the
// endpoint's stored host/ip with TLS verification on. Cost: outbound requests to cluster
// apps during a check; escape: delete this module and its call sites.
const PROBE_LIMITS: CommandLimits = CommandLimits {
    timeout: Duration::from_secs(6),
    max_output_bytes: 64 * 1024,
};

/// Evidence from a login-surface probe; `realm` is known only for a Keycloak redirect.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ProbeEvidence {
    pub realm: Option<String>,
}

/// Fixed argv, no `-k`, no redirect following, no cookies/credentials. `-q` first so
/// `~/.curlrc` cannot inject options; `--resolve` pins the stored address so DNS state cannot
/// redirect the request elsewhere.
fn curl_args(host: &str, ip: &str, path: &str, redirect_only: bool) -> Vec<String> {
    let ip = if ip.contains(':') {
        format!("[{ip}]")
    } else {
        ip.to_string()
    };
    let mut args: Vec<String> = [
        "-q",
        "-s",
        "--fail",
        "--proto",
        "=https",
        "--max-time",
        "5",
        "--max-filesize",
        "65536",
        // Inherited HTTPS_PROXY/ALL_PROXY would bypass the --resolve pin.
        "--noproxy",
        "*",
        "--resolve",
    ]
    .map(str::to_string)
    .to_vec();
    args.push(format!("{host}:443:{ip}"));
    if redirect_only {
        args.extend(["-o", "/dev/null", "-w", "%{redirect_url}"].map(str::to_string));
    }
    args.push(format!("https://{host}{path}"));
    args
}

/// curl (inet_aton) reads `127.1`, `0x7f.1`, `0177.0.0.1` or `2130706433` as an IPv4 address and
/// then ignores `--resolve`, so a probe host must be a real DNS name: at least one dot and not
/// made only of decimal/octal/hex numeric labels.
fn is_probeable_host(host: &str) -> bool {
    let numeric = |label: &str| {
        let digits = label
            .strip_prefix("0x")
            .or_else(|| label.strip_prefix("0X"))
            .map_or((label, false), |hex| (hex, true));
        !digits.0.is_empty()
            && digits.0.bytes().all(|b| {
                if digits.1 {
                    b.is_ascii_hexdigit()
                } else {
                    b.is_ascii_digit()
                }
            })
    };
    let labels: Vec<&str> = host.split('.').collect();
    labels.len() >= 2 && !labels.iter().all(|label| numeric(label))
}

fn providers_show_oidc(body: &str) -> bool {
    let Ok(serde_json::Value::Object(providers)) = serde_json::from_str(body) else {
        return false;
    };
    providers.iter().any(|(key, provider)| {
        let id = provider.get("id").and_then(|v| v.as_str()).unwrap_or(key);
        provider
            .get("type")
            .and_then(|v| v.as_str())
            .is_some_and(|t| t.eq_ignore_ascii_case("oidc"))
            || key.to_ascii_lowercase().contains("keycloak")
            || id.to_ascii_lowercase().contains("keycloak")
    })
}

/// `https://<h>/realms/<realm>/protocol/openid-connect/auth...` -> realm.
fn redirect_realm(url: &str) -> Option<String> {
    let rest = url.trim().strip_prefix("https://")?;
    let (host, path) = rest.split_once("/realms/")?;
    let (realm, tail) = path.split_once("/protocol/openid-connect/auth")?;
    // Reuse the issuer parser so host and realm obey one set of rules.
    let (_, realm) = parse_realm_url(&format!("https://{host}/realms/{realm}"))?;
    (tail.is_empty() || tail.starts_with('?') || tail.starts_with('/')).then_some(realm)
}

pub(super) async fn probe_login_surface(
    runner: &dyn CommandRunner,
    ep: &DiscoveredEndpoint,
) -> Option<ProbeEvidence> {
    if !is_safe_host_domain(&ep.host) || !is_probeable_host(&ep.host) || !is_safe_ip_address(&ep.ip)
    {
        return None;
    }
    let providers = runner
        .run_bounded(
            "curl",
            &curl_args(&ep.host, &ep.ip, "/api/auth/providers", false),
            PROBE_LIMITS,
        )
        .await;
    if let Ok(out) = providers {
        if out.success && providers_show_oidc(&out.stdout) {
            return Some(ProbeEvidence { realm: None });
        }
    }
    let root = runner
        .run_bounded(
            "curl",
            &curl_args(&ep.host, &ep.ip, "/", true),
            PROBE_LIMITS,
        )
        .await
        .ok()?;
    if !root.success {
        return None;
    }
    redirect_realm(&root.stdout).map(|realm| ProbeEvidence { realm: Some(realm) })
}
