use super::transport::get_json;
use super::LoginErrorCode;
use crate::services::k8s_endpoints::{apisix_upstream_service, DiscoveredEndpoint};
use crate::services::process::CommandRunner;
use crate::services::validate::is_safe_k8s_dns_label;
use std::path::Path;

async fn get_resource(
    runner: &dyn CommandRunner,
    kubeconfig: &Path,
    context: &str,
    namespace: &str,
    resource: &str,
    name: &str,
    api_path: &str,
) -> Result<serde_json::Value, LoginErrorCode> {
    if !is_safe_k8s_dns_label(namespace) || !is_safe_k8s_dns_label(name) {
        return Err(LoginErrorCode::BackendMismatch);
    }
    get_json(
        runner,
        kubeconfig,
        context,
        &[resource, name, "-n", namespace],
        api_path,
    )
    .await
}

pub(super) async fn validate_live_backend(
    runner: &dyn CommandRunner,
    kubeconfig: &Path,
    context: &str,
    endpoint: &DiscoveredEndpoint,
    namespace: &str,
    service: &str,
) -> Result<serde_json::Value, LoginErrorCode> {
    // D2: cached DNS targets can drift. Re-read only their source and target with the
    // same credentials. Cost: two bounded reads; escape: rescan after a mismatch.
    if let Some(reference) = &endpoint.backend_upstream {
        let (route_ns, name) = reference
            .split_once('/')
            .ok_or(LoginErrorCode::BackendMismatch)?;
        if endpoint.source != "apisix"
            || endpoint.resource_name.split_once('/').map(|(ns, _)| ns) != Some(route_ns)
            || !is_safe_k8s_dns_label(route_ns)
            || !is_safe_k8s_dns_label(name)
        {
            return Err(LoginErrorCode::BackendMismatch);
        }
        let upstream = get_resource(
            runner,
            kubeconfig,
            context,
            route_ns,
            "apisixupstreams.apisix.apache.org",
            name,
            &format!("/apis/apisix.apache.org/v2/namespaces/{route_ns}/apisixupstreams/{name}"),
        )
        .await
        .map_err(|error| {
            if error == LoginErrorCode::NotFound {
                LoginErrorCode::BackendMismatch
            } else {
                error
            }
        })?;
        if apisix_upstream_service(&upstream).as_deref() != endpoint.backend_service.as_deref() {
            return Err(LoginErrorCode::BackendMismatch);
        }
    } else if endpoint.resource_name.split_once('/').map(|(ns, _)| ns) != Some(namespace) {
        // Old cross-namespace cache records lack the upstream needed to prove the binding.
        return Err(LoginErrorCode::BackendMismatch);
    }
    let target = get_resource(
        runner,
        kubeconfig,
        context,
        namespace,
        "service",
        service,
        &format!("/api/v1/namespaces/{namespace}/services/{service}"),
    )
    .await?;
    match target
        .pointer("/spec/type")
        .and_then(|value| value.as_str())
    {
        Some("ExternalName") => Err(LoginErrorCode::BackendMismatch),
        Some("ClusterIP" | "NodePort" | "LoadBalancer") => Ok(target),
        _ => Err(LoginErrorCode::BackendMismatch),
    }
}
