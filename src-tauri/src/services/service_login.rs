use serde::{Deserialize, Serialize};
use std::fmt;
use std::time::Duration;

use crate::commands::local_dns::cached_endpoints;
use crate::services::config::Profile;
use crate::services::k8s_endpoints::{decode_base64, DiscoveredEndpoint};
use crate::services::paths::ClusterDeckPaths;
use crate::services::process::{CommandLimits, CommandRunner};
use crate::services::store;
use crate::services::validate::{
    is_safe_k8s_dns_label, is_safe_profile_id, is_safe_ssh_identifier,
};

pub const KUBECTL_LOGIN_TIMEOUT: Duration = Duration::from_secs(10);
pub const KUBECTL_LOGIN_MAX_OUTPUT_BYTES: usize = 64 * 1024;
pub const KUBECTL_LOGIN_LIMITS: CommandLimits = CommandLimits {
    timeout: KUBECTL_LOGIN_TIMEOUT,
    max_output_bytes: KUBECTL_LOGIN_MAX_OUTPUT_BYTES,
};

/// Fixed error codes returned to the frontend (ADR-0014 D3/D5). Raw `kubectl` stdout/stderr
/// is never interpolated so cluster error output or secret payloads cannot leak.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LoginErrorCode {
    Forbidden,
    NotFound,
    BackendMismatch,
    UnsupportedService,
    ImpersonationRefused,
    Timeout,
    ForbiddenCaller,
    KubectlFailed,
}

impl LoginErrorCode {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Forbidden => "forbidden",
            Self::NotFound => "not_found",
            Self::BackendMismatch => "backend_mismatch",
            Self::UnsupportedService => "unsupported_service",
            Self::ImpersonationRefused => "impersonation_refused",
            Self::Timeout => "timeout",
            Self::ForbiddenCaller => "forbidden_caller",
            Self::KubectlFailed => "kubectl_failed",
        }
    }
}

impl fmt::Display for LoginErrorCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Wrapper for a revealed secret value that never implements `Display` and redacts its value in
/// `Debug` so formatting a response or error cannot leak the credential (ADR-0014 D3/D5).
#[derive(Clone, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct SecretValue(String);

impl SecretValue {
    pub fn new(value: String) -> Self {
        Self(value)
    }

    #[cfg(test)]
    pub fn expose_for_test(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for SecretValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("\"[REDACTED]\"")
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UsernameSource {
    Fixed(&'static str),
    SecretKey(&'static str),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServiceLoginSpec {
    pub secret_name: String,
    pub username: UsernameSource,
    pub password_key: &'static str,
    pub chart_source: &'static str,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EndpointLoginOption {
    pub endpoint_id: String,
    pub namespace: String,
    pub service_name: String,
    pub secret_name: String,
}

#[derive(Clone, PartialEq, Eq, Serialize)]
pub struct RevealedEndpointLogin {
    pub endpoint_id: String,
    pub namespace: String,
    pub service_name: String,
    pub secret_name: String,
    pub username: Option<String>,
    pub password: SecretValue,
}

impl fmt::Debug for RevealedEndpointLogin {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RevealedEndpointLogin")
            .field("endpoint_id", &self.endpoint_id)
            .field("namespace", &self.namespace)
            .field("service_name", &self.service_name)
            .field("secret_name", &self.secret_name)
            .field("username", &"<redacted>")
            .field("password", &"<redacted>")
            .finish()
    }
}

mod backend;
mod snapshot;
mod transport;
use snapshot::KubeconfigSnapshot;

/// D2 static registry mapping a Kubernetes Service name to its initial-admin Secret convention.
/// Widening this table or adding user-defined mappings requires a follow-up ADR (ADR-0014 D2).
pub fn lookup_service_login(service_name: &str) -> Option<ServiceLoginSpec> {
    if !is_safe_k8s_dns_label(service_name) {
        return None;
    }
    match service_name {
        // argo/argo-cd chart: Service `argocd-server` -> Secret `argocd-initial-admin-secret`
        "argocd-server" => Some(ServiceLoginSpec {
            secret_name: "argocd-initial-admin-secret".to_string(),
            username: UsernameSource::Fixed("admin"),
            password_key: "password",
            chart_source: "argo/argo-cd",
        }),
        // grafana/grafana & kube-prometheus-stack: Service `<release>-grafana` or `grafana` -> same-named Secret
        "grafana" => Some(ServiceLoginSpec {
            secret_name: "grafana".to_string(),
            username: UsernameSource::SecretKey("admin-user"),
            password_key: "admin-password",
            chart_source: "grafana/grafana, kube-prometheus-stack",
        }),
        // Keycloak operator / bitnami/keycloak: both measured Service aliases share this Secret.
        "keycloak" | "keycloak-service" => Some(ServiceLoginSpec {
            secret_name: "keycloak-initial-admin".to_string(),
            username: UsernameSource::SecretKey("username"),
            password_key: "password",
            chart_source: "Keycloak operator / bitnami/keycloak",
        }),
        // goharbor/harbor: Service `harbor-core` or `harbor` -> Secret `harbor-secrets`
        "harbor-core" | "harbor" => Some(ServiceLoginSpec {
            secret_name: "harbor-secrets".to_string(),
            username: UsernameSource::Fixed("admin"),
            password_key: "HARBOR_ADMIN_PASSWORD",
            chart_source: "goharbor/harbor",
        }),
        // gitea-charts/gitea: Service `gitea-http` -> Secret `gitea-admin`
        "gitea-http" => Some(ServiceLoginSpec {
            secret_name: "gitea-admin".to_string(),
            username: UsernameSource::SecretKey("username"),
            password_key: "admin-password",
            chart_source: "gitea-charts/gitea",
        }),
        // apache/superset: Service `superset` -> Secret `superset-credential`
        "superset" => Some(ServiceLoginSpec {
            secret_name: "superset-credential".to_string(),
            username: UsernameSource::Fixed("admin"),
            password_key: "admin-password",
            chart_source: "apache/superset",
        }),
        other => {
            if let Some(release) = other.strip_suffix("-grafana") {
                if !release.is_empty() && is_safe_k8s_dns_label(other) {
                    return Some(ServiceLoginSpec {
                        secret_name: other.to_string(),
                        username: UsernameSource::SecretKey("admin-user"),
                        password_key: "admin-password",
                        chart_source: "grafana/grafana, kube-prometheus-stack",
                    });
                }
            }
            None
        }
    }
}

fn resolve_endpoint_target(
    ep: &DiscoveredEndpoint,
) -> Result<(String, String, ServiceLoginSpec), LoginErrorCode> {
    let backend = ep
        .backend_service
        .as_deref()
        .ok_or(LoginErrorCode::UnsupportedService)?;
    let (ns, service_name) = backend
        .split_once('/')
        .ok_or(LoginErrorCode::UnsupportedService)?;
    let (resource_ns, _) = ep
        .resource_name
        .split_once('/')
        .ok_or(LoginErrorCode::UnsupportedService)?;
    // D2: discovery resolves exact APISIX Service DNS targets across namespaces.
    // Keep other sources namespace-local; the Secret belongs to the reached Service.
    if (ns != resource_ns && ep.source != "apisix")
        || !is_safe_k8s_dns_label(resource_ns)
        || !is_safe_k8s_dns_label(ns)
        || !is_safe_k8s_dns_label(service_name)
    {
        return Err(LoginErrorCode::UnsupportedService);
    }
    let spec = lookup_service_login(service_name).ok_or(LoginErrorCode::UnsupportedService)?;
    if !is_safe_k8s_dns_label(&spec.secret_name) {
        return Err(LoginErrorCode::UnsupportedService);
    }
    Ok((ns.to_string(), service_name.to_string(), spec))
}

pub fn endpoint_login_options_for_slice(
    endpoints: &[DiscoveredEndpoint],
) -> Vec<EndpointLoginOption> {
    endpoints
        .iter()
        .filter_map(|ep| {
            let (namespace, service_name, spec) = resolve_endpoint_target(ep).ok()?;
            Some(EndpointLoginOption {
                endpoint_id: ep.endpoint_id(),
                namespace,
                service_name,
                secret_name: spec.secret_name,
            })
        })
        .collect()
}

pub fn endpoint_login_options(
    paths: &ClusterDeckPaths,
    profile_id: &str,
) -> Result<Vec<EndpointLoginOption>, LoginErrorCode> {
    if !is_safe_profile_id(profile_id) {
        return Err(LoginErrorCode::NotFound);
    }
    let _ = store::get_profile(paths, profile_id).map_err(|_| LoginErrorCode::NotFound)?;
    let endpoints = cached_endpoints(&paths.endpoints_cache_file(), profile_id);
    Ok(endpoint_login_options_for_slice(&endpoints))
}

fn resolve_context_and_check_impersonation(
    profile: &Profile,
    raw: &[u8],
) -> Result<String, LoginErrorCode> {
    let val: serde_yaml::Value =
        serde_yaml::from_slice(raw).map_err(|_| LoginErrorCode::KubectlFailed)?;

    let current_ctx = val
        .get("current-context")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty());
    let profile_ctx = profile
        .kubeconfig
        .as_ref()
        .map(|k| k.context.trim())
        .filter(|s| !s.is_empty());
    let context = current_ctx
        .or(profile_ctx)
        .unwrap_or(profile.id.as_str())
        .to_string();
    if !is_safe_ssh_identifier(&context) {
        return Err(LoginErrorCode::KubectlFailed);
    }

    let selected_user = val
        .get("contexts")
        .and_then(|c| c.as_sequence())
        .and_then(|contexts| {
            contexts.iter().find_map(|entry| {
                let name = entry.get("name").and_then(|n| n.as_str())?;
                if name == context {
                    entry
                        .get("context")
                        .and_then(|ctx| ctx.get("user"))
                        .and_then(|u| u.as_str())
                } else {
                    None
                }
            })
        });

    if let Some(users) = val.get("users").and_then(|u| u.as_sequence()) {
        for entry in users {
            let matches_user = match selected_user {
                Some(target) => entry.get("name").and_then(|n| n.as_str()) == Some(target),
                None => true,
            };
            if matches_user {
                if let Some(user_obj) = entry.get("user") {
                    for key in ["as", "as-groups", "as-user-extra"] {
                        if user_obj.get(key).is_some_and(|v| !v.is_null()) {
                            return Err(LoginErrorCode::ImpersonationRefused);
                        }
                    }
                }
            }
        }
    }

    Ok(context)
}

pub async fn check_endpoint_login_with_deps(
    runner: &dyn CommandRunner,
    paths: &ClusterDeckPaths,
    profile_id: &str,
    endpoint_id: &str,
) -> Result<EndpointLoginOption, LoginErrorCode> {
    if !is_safe_profile_id(profile_id) || endpoint_id.is_empty() {
        return Err(LoginErrorCode::NotFound);
    }
    let profile = store::get_profile(paths, profile_id).map_err(|_| LoginErrorCode::NotFound)?;
    let kubeconfig_path = paths.kubeconfig_file(&profile.id);
    let raw = std::fs::read(&kubeconfig_path).map_err(|_| LoginErrorCode::KubectlFailed)?;
    let context = resolve_context_and_check_impersonation(&profile, &raw)?;
    let snapshot = KubeconfigSnapshot::create(paths, &raw)?;
    let kubeconfig_path = snapshot.path();
    let endpoints = cached_endpoints(&paths.endpoints_cache_file(), profile_id);
    let ep = endpoints
        .iter()
        .find(|e| e.endpoint_id() == endpoint_id)
        .ok_or(LoginErrorCode::NotFound)?;
    let (namespace, service_name, spec) = resolve_endpoint_target(ep)?;
    backend::validate_live_backend(
        runner,
        kubeconfig_path,
        &context,
        ep,
        &namespace,
        &service_name,
    )
    .await?;
    transport::can_i_get_secret(
        runner,
        kubeconfig_path,
        &context,
        &namespace,
        &spec.secret_name,
    )
    .await?;
    Ok(EndpointLoginOption {
        endpoint_id: ep.endpoint_id(),
        namespace,
        service_name,
        secret_name: spec.secret_name,
    })
}

pub async fn reveal_endpoint_login_with_deps(
    runner: &dyn CommandRunner,
    paths: &ClusterDeckPaths,
    profile_id: &str,
    endpoint_id: &str,
) -> Result<RevealedEndpointLogin, LoginErrorCode> {
    if !is_safe_profile_id(profile_id) || endpoint_id.is_empty() {
        return Err(LoginErrorCode::NotFound);
    }
    let profile = store::get_profile(paths, profile_id).map_err(|_| LoginErrorCode::NotFound)?;
    let kubeconfig_path = paths.kubeconfig_file(&profile.id);
    let raw = std::fs::read(&kubeconfig_path).map_err(|_| LoginErrorCode::KubectlFailed)?;
    let context = resolve_context_and_check_impersonation(&profile, &raw)?;
    let snapshot = KubeconfigSnapshot::create(paths, &raw)?;
    let kubeconfig_path = snapshot.path();
    let endpoints = cached_endpoints(&paths.endpoints_cache_file(), profile_id);
    let ep = endpoints
        .iter()
        .find(|e| e.endpoint_id() == endpoint_id)
        .ok_or(LoginErrorCode::NotFound)?;
    let (namespace, service_name, spec) = resolve_endpoint_target(ep)?;

    backend::validate_live_backend(
        runner,
        kubeconfig_path,
        &context,
        ep,
        &namespace,
        &service_name,
    )
    .await?;
    transport::can_i_get_secret(
        runner,
        kubeconfig_path,
        &context,
        &namespace,
        &spec.secret_name,
    )
    .await?;

    let api_path = format!(
        "/api/v1/namespaces/{namespace}/secrets/{}",
        spec.secret_name
    );
    let val = transport::get_json(
        runner,
        kubeconfig_path,
        &context,
        &["secret", &spec.secret_name, "-n", &namespace],
        &api_path,
    )
    .await?;
    let data = val
        .get("data")
        .and_then(|d| d.as_object())
        .ok_or(LoginErrorCode::NotFound)?;

    let raw_pw_b64 = data
        .get(spec.password_key)
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .ok_or(LoginErrorCode::NotFound)?;
    let pw_bytes = decode_base64(raw_pw_b64).ok_or(LoginErrorCode::KubectlFailed)?;
    let pw_str = String::from_utf8(pw_bytes).map_err(|_| LoginErrorCode::KubectlFailed)?;

    let username = match spec.username {
        UsernameSource::Fixed(u) => Some(u.to_string()),
        UsernameSource::SecretKey(k) => match data.get(k).and_then(|v| v.as_str()) {
            Some(b64) if !b64.is_empty() => {
                let bytes = decode_base64(b64).ok_or(LoginErrorCode::KubectlFailed)?;
                Some(String::from_utf8(bytes).map_err(|_| LoginErrorCode::KubectlFailed)?)
            }
            _ => None,
        },
    };

    Ok(RevealedEndpointLogin {
        endpoint_id: ep.endpoint_id(),
        namespace,
        service_name,
        secret_name: spec.secret_name,
        username,
        password: SecretValue::new(pw_str),
    })
}

#[cfg(test)]
mod tests;
#[cfg(test)]
mod transport_tests;
