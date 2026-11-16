use serde::{Deserialize, Serialize};
use std::fmt;
use std::time::Duration;

use crate::services::config::Profile;
use crate::services::k8s_endpoints::DiscoveredEndpoint;
use crate::services::process::CommandLimits;
use crate::services::validate::{is_safe_k8s_dns_label, is_safe_ssh_identifier};

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
    /// Env var names on the backend workload that reference the real admin Secret.
    pub workload_env: Option<WorkloadEnv>,
}

/// Non-secret SSO evidence: the endpoint signs in through Keycloak (ADR-0014 SSO amendment).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SsoHint {
    /// Realm from a literal `...ISSUER` env on the backend pod; `None` when only the APISIX
    /// `openid-connect` plugin is known.
    pub realm: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EndpointLoginOption {
    pub endpoint_id: String,
    // Non-secret route provenance from the stored endpoint (never frontend input);
    // lets the UI warn when the route namespace differs from the Service namespace.
    pub route_namespace: String,
    pub route_resource: String,
    pub host: String,
    pub namespace: String,
    pub service_name: String,
    /// First Secret the reveal reads (the endpoint's own admin Secret, else the realm users).
    pub secret_name: String,
    #[serde(default)]
    pub sso: Option<SsoHint>,
}

#[derive(Clone, PartialEq, Eq, Serialize)]
pub struct LoginAccount {
    pub label: String,
    /// Secret data key a realm user came from, shown when the username differs (template).
    pub key: Option<String>,
    pub username: Option<String>,
    pub password: SecretValue,
}

impl fmt::Debug for LoginAccount {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LoginAccount")
            .field("label", &self.label)
            .field("username", &"<redacted>")
            .field("password", &"<redacted>")
            .finish()
    }
}

/// Accounts from one source (own admin Secret, Keycloak admin, realm users). A failing source
/// carries a fixed `error` code and no accounts so it never hides the other groups.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LoginGroup {
    pub label: String,
    pub namespace: String,
    pub secret_name: String,
    pub accounts: Vec<LoginAccount>,
    pub error: Option<LoginErrorCode>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RevealedEndpointLogin {
    pub endpoint_id: String,
    pub route_namespace: String,
    pub route_resource: String,
    pub host: String,
    pub namespace: String,
    pub service_name: String,
    pub groups: Vec<LoginGroup>,
}

#[cfg(test)]
impl RevealedEndpointLogin {
    /// The only account of a single-account (non-SSO) reveal.
    pub fn account(&self) -> &LoginAccount {
        assert_eq!(self.groups.len(), 1);
        assert_eq!(self.groups[0].accounts.len(), 1);
        &self.groups[0].accounts[0]
    }
}

mod backend;
mod keycloak;
mod plan;
mod snapshot;
mod sso_probe;
mod transport;
mod workload;
pub use plan::{
    check_endpoint_login_with_deps, endpoint_login_options_with_deps,
    reveal_endpoint_login_with_deps,
};
use workload::WorkloadEnv;

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
            workload_env: None,
        }),
        // grafana/grafana & kube-prometheus-stack: Service `<release>-grafana` or `grafana` -> same-named Secret
        "grafana" => Some(ServiceLoginSpec {
            secret_name: "grafana".to_string(),
            username: UsernameSource::SecretKey("admin-user"),
            password_key: "admin-password",
            chart_source: "grafana/grafana, kube-prometheus-stack",
            workload_env: Some(WorkloadEnv {
                username: "GF_SECURITY_ADMIN_USER",
                password: "GF_SECURITY_ADMIN_PASSWORD",
            }),
        }),
        // goharbor/harbor: Service `harbor-core` or `harbor` -> Secret `harbor-secrets`
        // (no workload_env: the measured static Secret already logs in)
        "harbor-core" | "harbor" => Some(ServiceLoginSpec {
            secret_name: "harbor-secrets".to_string(),
            username: UsernameSource::Fixed("admin"),
            password_key: "HARBOR_ADMIN_PASSWORD",
            chart_source: "goharbor/harbor",
            workload_env: None,
        }),
        // gitea-charts/gitea: Service `gitea-http` -> Secret `gitea-admin`
        "gitea-http" => Some(ServiceLoginSpec {
            secret_name: "gitea-admin".to_string(),
            username: UsernameSource::SecretKey("username"),
            password_key: "admin-password",
            chart_source: "gitea-charts/gitea",
            workload_env: Some(WorkloadEnv {
                username: "GITEA_ADMIN_USERNAME",
                password: "GITEA_ADMIN_PASSWORD",
            }),
        }),
        // apache/superset: Service `superset` -> Secret `superset-credential`
        "superset" => Some(ServiceLoginSpec {
            secret_name: "superset-credential".to_string(),
            username: UsernameSource::Fixed("admin"),
            password_key: "admin-password",
            chart_source: "apache/superset",
            workload_env: None,
        }),
        other => {
            if let Some(release) = other.strip_suffix("-grafana") {
                if !release.is_empty() && is_safe_k8s_dns_label(other) {
                    return Some(ServiceLoginSpec {
                        secret_name: other.to_string(),
                        username: UsernameSource::SecretKey("admin-user"),
                        password_key: "admin-password",
                        chart_source: "grafana/grafana, kube-prometheus-stack",
                        workload_env: Some(WorkloadEnv {
                            username: "GF_SECURITY_ADMIN_USER",
                            password: "GF_SECURITY_ADMIN_PASSWORD",
                        }),
                    });
                }
            }
            None
        }
    }
}

/// Validated `(namespace, service)` of the endpoint's stored backend. Says nothing about
/// whether the Service has a known login; that is the plan's job.
fn resolve_backend(ep: &DiscoveredEndpoint) -> Result<(String, String), LoginErrorCode> {
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
    Ok((ns.to_string(), service_name.to_string()))
}

// resolve_backend already validated this as a safe DNS label.
fn route_namespace(ep: &DiscoveredEndpoint) -> String {
    ep.resource_name
        .split_once('/')
        .map(|(ns, _)| ns.to_string())
        .unwrap_or_default()
}

fn option_for(
    ep: &DiscoveredEndpoint,
    namespace: String,
    service_name: String,
    secret_name: String,
    sso: Option<SsoHint>,
) -> EndpointLoginOption {
    EndpointLoginOption {
        endpoint_id: ep.endpoint_id(),
        route_namespace: route_namespace(ep),
        route_resource: ep.resource_name.clone(),
        host: ep.host.clone(),
        namespace,
        service_name,
        secret_name,
        sso,
    }
}

/// Options knowable without any cluster call: registry/Keycloak services, plus APISIX
/// `openid-connect` routes when the profile has a Keycloak endpoint to take accounts from.
pub fn endpoint_login_options_for_slice(
    endpoints: &[DiscoveredEndpoint],
) -> Vec<EndpointLoginOption> {
    let has_keycloak = keycloak::find_keycloak_endpoint(endpoints, None).is_some();
    endpoints
        .iter()
        .filter_map(|ep| {
            let (namespace, service_name) = resolve_backend(ep).ok()?;
            let sso = (ep.sso_oidc && has_keycloak).then_some(SsoHint { realm: None });
            if keycloak::is_keycloak_service(&service_name) {
                return Some(option_for(
                    ep,
                    namespace,
                    service_name,
                    keycloak::KEYCLOAK_ADMIN_SECRETS[0].to_string(),
                    None,
                ));
            }
            if let Some(spec) = lookup_service_login(&service_name) {
                if !is_safe_k8s_dns_label(&spec.secret_name) {
                    return None;
                }
                return Some(option_for(
                    ep,
                    namespace,
                    service_name,
                    spec.secret_name,
                    sso,
                ));
            }
            sso.map(|hint| {
                option_for(
                    ep,
                    namespace,
                    service_name,
                    keycloak::REALM_USERS_SECRET.to_string(),
                    Some(hint),
                )
            })
        })
        .collect()
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

#[cfg(test)]
use crate::services::{paths::ClusterDeckPaths, process::CommandRunner, store};
#[cfg(test)]
mod keycloak_tests;
#[cfg(test)]
mod tests;
#[cfg(test)]
mod transport_tests;
#[cfg(test)]
mod workload_tests;
