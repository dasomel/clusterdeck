#![allow(dead_code)]

use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::services::process::CommandRunner;
use crate::services::validate::{is_safe_host_domain, is_safe_ip_address};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiscoveredEndpoint {
    pub host: String,
    pub ip: String,
    pub source: String, // "apisix", "ingress", "istio", "gateway-api", "service"
    pub resource_name: String, // e.g. "analytics/trino"
}

fn decode_base64(input: &str) -> Option<Vec<u8>> {
    use base64::prelude::*;
    let clean: String = input.chars().filter(|c| !c.is_whitespace()).collect();
    BASE64_STANDARD.decode(clean).ok()
}

pub async fn query_k8s_api_json(
    runner: &dyn CommandRunner,
    kubeconfig_path: &Path,
    api_path: &str,
) -> Result<Option<serde_json::Value>, String> {
    // 1. Try kubectl --kubeconfig ... get --raw <api_path>
    let args = vec![
        "--kubeconfig".to_string(),
        kubeconfig_path.to_string_lossy().to_string(),
        "get".to_string(),
        "--raw".to_string(),
        api_path.to_string(),
    ];

    if let Ok(output) = runner.run("kubectl", &args).await {
        if output.success {
            if let Ok(val) = serde_json::from_str::<serde_json::Value>(&output.stdout) {
                if is_not_found(&val) {
                    return Ok(None);
                }
                return Ok(Some(val));
            }
        }
    }

    // 2. Fallback to system curl
    let stdout = curl_k8s_api(runner, kubeconfig_path, api_path).await?;

    let parsed: serde_json::Value = serde_json::from_str(&stdout)
        .map_err(|e| format!("failed to parse k8s response json: {e}"))?;

    if is_not_found(&parsed) {
        return Ok(None);
    }

    Ok(Some(parsed))
}

/// Process-wide counter appended to temp cert/key file names, in addition to the nanosecond
/// timestamp, so concurrent callers (e.g. discover_cluster_endpoints's tokio::join! queries,
/// or verify running alongside discovery) never collide on the same file name even when the
/// clock tick is coarser than actual concurrency.
static TEMP_FILE_SEQ: AtomicU64 = AtomicU64::new(0);

/// Creates `path` with owner-only (0600) permissions and writes `contents` to it in one step,
/// so the file (a decoded TLS client certificate or private key) never exists at the default,
/// world-readable permissions even momentarily -- unlike a create-then-chmod sequence, which
/// leaves that window open. Mirrors the temp-file idiom in kubeconfig.rs.
#[cfg(unix)]
fn write_owner_only_file(path: &Path, contents: &[u8]) -> std::io::Result<()> {
    use std::fs::OpenOptions;
    use std::io::Write;
    OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)?
        .write_all(contents)
}

#[cfg(not(unix))]
fn write_owner_only_file(path: &Path, contents: &[u8]) -> std::io::Result<()> {
    std::fs::write(path, contents)
}

/// Fallback to system curl for querying the k8s API directly, used when kubectl fails (e.g.
/// macOS Sequoia Local Network Privacy blocks third-party sockets) or kubectl is not installed;
/// curl bypasses LNP restrictions. Shared by query_k8s_api_json's step 2 and
/// verify::verify_cluster_detailed's step 2. Returns curl's raw stdout on success.
pub(crate) async fn curl_k8s_api(
    runner: &dyn CommandRunner,
    kubeconfig_path: &Path,
    api_path: &str,
) -> Result<String, String> {
    let raw = std::fs::read_to_string(kubeconfig_path)
        .map_err(|e| format!("failed to read kubeconfig: {e}"))?;
    let value: serde_yaml::Value =
        serde_yaml::from_str(&raw).map_err(|e| format!("failed to parse kubeconfig yaml: {e}"))?;

    let server_url = match value
        .get("clusters")
        .and_then(|c| c.get(0))
        .and_then(|c| c.get("cluster"))
        .and_then(|c| c.get("server"))
        .and_then(|s| s.as_str())
    {
        Some(s) => s.trim_end_matches('/').to_string(),
        None => return Err("server url not found in kubeconfig".to_string()),
    };

    let user_obj = value
        .get("users")
        .and_then(|u| u.get(0))
        .and_then(|u| u.get("user"));

    let cert_data = user_obj
        .and_then(|u| u.get("client-certificate-data"))
        .and_then(|v| v.as_str());
    let key_data = user_obj
        .and_then(|u| u.get("client-key-data"))
        .and_then(|v| v.as_str());
    let token = user_obj
        .and_then(|u| u.get("token"))
        .and_then(|v| v.as_str());

    let cert_path = user_obj
        .and_then(|u| u.get("client-certificate"))
        .and_then(|v| v.as_str());
    let key_path = user_obj
        .and_then(|u| u.get("client-key"))
        .and_then(|v| v.as_str());

    let target_url = format!("{server_url}{api_path}");

    let now_nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let seq = TEMP_FILE_SEQ.fetch_add(1, Ordering::Relaxed);
    let temp_cert = std::env::temp_dir().join(format!("cd_tmp_cert_{now_nanos}_{seq}.crt"));
    let temp_key = std::env::temp_dir().join(format!("cd_tmp_key_{now_nanos}_{seq}.key"));

    let mut curl_args = vec![
        "-k".to_string(),
        "-s".to_string(),
        "--connect-timeout".to_string(),
        "5".to_string(),
    ];

    if let (Some(c_data), Some(k_data)) = (cert_data, key_data) {
        if let (Some(c_bytes), Some(k_bytes)) = (decode_base64(c_data), decode_base64(k_data)) {
            if write_owner_only_file(&temp_cert, &c_bytes).is_ok()
                && write_owner_only_file(&temp_key, &k_bytes).is_ok()
            {
                curl_args.push("--cert".to_string());
                curl_args.push(temp_cert.to_string_lossy().to_string());
                curl_args.push("--key".to_string());
                curl_args.push(temp_key.to_string_lossy().to_string());
            }
        }
    } else if let (Some(c_path), Some(k_path)) = (cert_path, key_path) {
        curl_args.push("--cert".to_string());
        curl_args.push(c_path.to_string());
        curl_args.push("--key".to_string());
        curl_args.push(k_path.to_string());
    } else if let Some(t) = token {
        curl_args.push("-H".to_string());
        curl_args.push(format!("Authorization: Bearer {t}"));
    }

    curl_args.push(target_url);

    let res = runner.run("curl", &curl_args).await;

    // Unconditional: a failed key write must not leave the cert (or a partial key) behind.
    let _ = std::fs::remove_file(&temp_cert);
    let _ = std::fs::remove_file(&temp_key);

    let output = res.map_err(|e| format!("curl execution failed: {e}"))?;
    if !output.success {
        return Err(format!("curl failed: {}", output.stderr));
    }

    Ok(output.stdout)
}

fn is_not_found(val: &serde_json::Value) -> bool {
    if let Some(code) = val.get("code").and_then(|c| c.as_u64()) {
        if code == 404 {
            return true;
        }
    }
    if let Some(reason) = val.get("reason").and_then(|r| r.as_str()) {
        if reason == "NotFound" {
            return true;
        }
    }
    false
}

pub struct ServiceDiscoveryInfo {
    pub service_ips: HashMap<String, String>, // "namespace/name" and "name" -> IP
    pub first_lb_ip: Option<String>,
    pub endpoints: Vec<DiscoveredEndpoint>,
}

pub fn parse_services(val: &serde_json::Value) -> ServiceDiscoveryInfo {
    let mut service_ips = HashMap::new();
    let mut first_lb_ip = None;
    let mut endpoints = Vec::new();

    if let Some(items) = val.get("items").and_then(|i| i.as_array()) {
        for item in items {
            let metadata = match item.get("metadata") {
                Some(m) => m,
                None => continue,
            };
            let name = metadata
                .get("name")
                .and_then(|n| n.as_str())
                .unwrap_or_default();
            let ns = metadata
                .get("namespace")
                .and_then(|n| n.as_str())
                .unwrap_or_default();
            let spec = item.get("spec");
            let svc_type = spec
                .and_then(|s| s.get("type"))
                .and_then(|t| t.as_str())
                .unwrap_or_default();

            if svc_type == "LoadBalancer" {
                if let Some(ingress_list) = item
                    .get("status")
                    .and_then(|s| s.get("loadBalancer"))
                    .and_then(|lb| lb.get("ingress"))
                    .and_then(|i| i.as_array())
                {
                    for ing in ingress_list {
                        if let Some(ip) = ing.get("ip").and_then(|v| v.as_str()) {
                            if is_safe_ip_address(ip) {
                                if first_lb_ip.is_none() {
                                    first_lb_ip = Some(ip.to_string());
                                }
                                service_ips.insert(format!("{ns}/{name}"), ip.to_string());
                                service_ips.insert(name.to_string(), ip.to_string());

                                // Check annotations for external hostname
                                if let Some(annotations) = metadata.get("annotations") {
                                    if let Some(hostname) = annotations
                                        .get("external-dns.alpha.kubernetes.io/hostname")
                                        .and_then(|h| h.as_str())
                                    {
                                        for h in hostname.split(',') {
                                            let trimmed = h.trim();
                                            if is_safe_host_domain(trimmed) {
                                                endpoints.push(DiscoveredEndpoint {
                                                    host: trimmed.to_string(),
                                                    ip: ip.to_string(),
                                                    source: "service".to_string(),
                                                    resource_name: format!("{ns}/{name}"),
                                                });
                                            }
                                        }
                                    }
                                }
                                break;
                            }
                        }
                    }
                }
            }
        }
    }

    ServiceDiscoveryInfo {
        service_ips,
        first_lb_ip,
        endpoints,
    }
}

pub fn parse_ingresses(
    val: &serde_json::Value,
    fallback_ip: Option<&str>,
) -> Vec<DiscoveredEndpoint> {
    let mut endpoints = Vec::new();
    let items = match val.get("items").and_then(|i| i.as_array()) {
        Some(i) => i,
        None => return endpoints,
    };

    for item in items {
        let metadata = match item.get("metadata") {
            Some(m) => m,
            None => continue,
        };
        let name = metadata
            .get("name")
            .and_then(|n| n.as_str())
            .unwrap_or_default();
        let ns = metadata
            .get("namespace")
            .and_then(|n| n.as_str())
            .unwrap_or_default();
        let resource_name = format!("{ns}/{name}");

        // Determine Ingress IP
        let mut ing_ip = None;
        if let Some(ingress_list) = item
            .get("status")
            .and_then(|s| s.get("loadBalancer"))
            .and_then(|lb| lb.get("ingress"))
            .and_then(|i| i.as_array())
        {
            for ing in ingress_list {
                if let Some(ip) = ing.get("ip").and_then(|v| v.as_str()) {
                    if is_safe_ip_address(ip) {
                        ing_ip = Some(ip.to_string());
                        break;
                    }
                }
            }
        }

        let chosen_ip = match ing_ip.as_deref().or(fallback_ip) {
            Some(ip) if is_safe_ip_address(ip) => ip.to_string(),
            _ => continue,
        };

        if let Some(rules) = item
            .get("spec")
            .and_then(|s| s.get("rules"))
            .and_then(|r| r.as_array())
        {
            for rule in rules {
                if let Some(host) = rule.get("host").and_then(|h| h.as_str()) {
                    if is_safe_host_domain(host) {
                        endpoints.push(DiscoveredEndpoint {
                            host: host.to_string(),
                            ip: chosen_ip.clone(),
                            source: "ingress".to_string(),
                            resource_name: resource_name.clone(),
                        });
                    }
                }
            }
        }
    }

    endpoints
}

pub fn parse_apisix_routes(
    val: &serde_json::Value,
    service_ips: &HashMap<String, String>,
    fallback_ip: Option<&str>,
) -> Vec<DiscoveredEndpoint> {
    let mut endpoints = Vec::new();
    let items = match val.get("items").and_then(|i| i.as_array()) {
        Some(i) => i,
        None => return endpoints,
    };

    // Locate APISIX gateway service IP
    let apisix_ip = service_ips
        .get("platform-system/apisix-gateway")
        .or_else(|| service_ips.get("apisix-gateway"))
        .or_else(|| service_ips.get("apisix/apisix-gateway"))
        .map(|s| s.as_str())
        .or(fallback_ip);

    let chosen_ip = match apisix_ip {
        Some(ip) if is_safe_ip_address(ip) => ip,
        _ => return endpoints,
    };

    for item in items {
        let metadata = match item.get("metadata") {
            Some(m) => m,
            None => continue,
        };
        let name = metadata
            .get("name")
            .and_then(|n| n.as_str())
            .unwrap_or_default();
        let ns = metadata
            .get("namespace")
            .and_then(|n| n.as_str())
            .unwrap_or_default();
        let resource_name = format!("{ns}/{name}");

        let mut hosts = Vec::new();

        if let Some(spec) = item.get("spec") {
            // Check http rules
            if let Some(http_rules) = spec.get("http").and_then(|h| h.as_array()) {
                for rule in http_rules {
                    if let Some(match_obj) = rule.get("match") {
                        if let Some(h_list) = match_obj.get("hosts").and_then(|h| h.as_array()) {
                            for h in h_list {
                                if let Some(host_str) = h.as_str() {
                                    hosts.push(host_str.to_string());
                                }
                            }
                        }
                    }
                }
            }

            // Check stream rules
            if let Some(stream_rules) = spec.get("stream").and_then(|s| s.as_array()) {
                for rule in stream_rules {
                    if let Some(match_obj) = rule.get("match") {
                        if let Some(h_list) = match_obj.get("hosts").and_then(|h| h.as_array()) {
                            for h in h_list {
                                if let Some(host_str) = h.as_str() {
                                    hosts.push(host_str.to_string());
                                }
                            }
                        }
                    }
                }
            }

            // Check tcp rules
            if let Some(tcp_rules) = spec.get("tcp").and_then(|t| t.as_array()) {
                for rule in tcp_rules {
                    if let Some(match_obj) = rule.get("match") {
                        if let Some(h_list) = match_obj.get("hosts").and_then(|h| h.as_array()) {
                            for h in h_list {
                                if let Some(host_str) = h.as_str() {
                                    hosts.push(host_str.to_string());
                                }
                            }
                        }
                    }
                }
            }
        }

        for host in hosts {
            if is_safe_host_domain(&host) {
                endpoints.push(DiscoveredEndpoint {
                    host,
                    ip: chosen_ip.to_string(),
                    source: "apisix".to_string(),
                    resource_name: resource_name.clone(),
                });
            }
        }
    }

    endpoints
}

pub fn parse_istio_virtual_services(
    val: &serde_json::Value,
    service_ips: &HashMap<String, String>,
    fallback_ip: Option<&str>,
) -> Vec<DiscoveredEndpoint> {
    let mut endpoints = Vec::new();
    let items = match val.get("items").and_then(|i| i.as_array()) {
        Some(i) => i,
        None => return endpoints,
    };

    let istio_ip = service_ips
        .get("istio-system/istio-ingressgateway")
        .or_else(|| service_ips.get("istio-ingressgateway"))
        .map(|s| s.as_str())
        .or(fallback_ip);

    let chosen_ip = match istio_ip {
        Some(ip) if is_safe_ip_address(ip) => ip,
        _ => return endpoints,
    };

    for item in items {
        let metadata = match item.get("metadata") {
            Some(m) => m,
            None => continue,
        };
        let name = metadata
            .get("name")
            .and_then(|n| n.as_str())
            .unwrap_or_default();
        let ns = metadata
            .get("namespace")
            .and_then(|n| n.as_str())
            .unwrap_or_default();
        let resource_name = format!("{ns}/{name}");

        if let Some(hosts) = item
            .get("spec")
            .and_then(|s| s.get("hosts"))
            .and_then(|h| h.as_array())
        {
            for h in hosts {
                if let Some(host_str) = h.as_str() {
                    // Filter out wildcard and cluster-internal single labels (e.g. "my-svc")
                    if host_str.contains('.') && is_safe_host_domain(host_str) {
                        endpoints.push(DiscoveredEndpoint {
                            host: host_str.to_string(),
                            ip: chosen_ip.to_string(),
                            source: "istio".to_string(),
                            resource_name: resource_name.clone(),
                        });
                    }
                }
            }
        }
    }

    endpoints
}

pub fn parse_gateway_httproutes(
    val: &serde_json::Value,
    fallback_ip: Option<&str>,
) -> Vec<DiscoveredEndpoint> {
    let mut endpoints = Vec::new();
    let items = match val.get("items").and_then(|i| i.as_array()) {
        Some(i) => i,
        None => return endpoints,
    };

    let chosen_ip = match fallback_ip {
        Some(ip) if is_safe_ip_address(ip) => ip,
        _ => return endpoints,
    };

    for item in items {
        let metadata = match item.get("metadata") {
            Some(m) => m,
            None => continue,
        };
        let name = metadata
            .get("name")
            .and_then(|n| n.as_str())
            .unwrap_or_default();
        let ns = metadata
            .get("namespace")
            .and_then(|n| n.as_str())
            .unwrap_or_default();
        let resource_name = format!("{ns}/{name}");

        if let Some(hostnames) = item
            .get("spec")
            .and_then(|s| s.get("hostnames"))
            .and_then(|h| h.as_array())
        {
            for h in hostnames {
                if let Some(host_str) = h.as_str() {
                    if is_safe_host_domain(host_str) {
                        endpoints.push(DiscoveredEndpoint {
                            host: host_str.to_string(),
                            ip: chosen_ip.to_string(),
                            source: "gateway-api".to_string(),
                            resource_name: resource_name.clone(),
                        });
                    }
                }
            }
        }
    }

    endpoints
}

pub async fn discover_cluster_endpoints(
    runner: &dyn CommandRunner,
    kubeconfig_path: &Path,
    default_ip: Option<&str>,
) -> Result<Vec<DiscoveredEndpoint>, String> {
    let mut all_endpoints = Vec::new();

    // Step 1 (services) is the baseline endpoint listing: if even it fails, the cluster is
    // effectively unusable, so its error propagates via `?`. Steps 2-5 below query optional
    // resources (their CRDs may not be installed, or RBAC may forbid them), so each is
    // best-effort -- a failure there must not discard endpoints already found.
    // 1. Query Services to find LoadBalancer IPs
    let svc_val = query_k8s_api_json(runner, kubeconfig_path, "/api/v1/services").await?;
    let svc_info = svc_val
        .as_ref()
        .map(parse_services)
        .unwrap_or_else(|| ServiceDiscoveryInfo {
            service_ips: HashMap::new(),
            first_lb_ip: None,
            endpoints: Vec::new(),
        });

    all_endpoints.extend(svc_info.endpoints);

    let effective_fallback_ip = svc_info.first_lb_ip.as_deref().or(default_ip);

    // 2-5. Ingresses, APISIX routes, Istio VirtualServices, and Gateway HTTPRoutes don't
    // depend on each other (only on step 1's service_ips/fallback_ip), so run them
    // concurrently instead of sequentially awaiting each one.
    let ingress_query = async {
        // 2. Query Ingresses (networking.k8s.io/v1)
        if let Ok(Some(ing_val)) = query_k8s_api_json(
            runner,
            kubeconfig_path,
            "/apis/networking.k8s.io/v1/ingresses",
        )
        .await
        {
            parse_ingresses(&ing_val, effective_fallback_ip)
        } else {
            Vec::new()
        }
    };

    let apisix_query = async {
        // 3. Query APISIX (apisix.apache.org/v2 ApisixRoute)
        if let Ok(Some(apisix_val)) = query_k8s_api_json(
            runner,
            kubeconfig_path,
            "/apis/apisix.apache.org/v2/apisixroutes",
        )
        .await
        {
            parse_apisix_routes(&apisix_val, &svc_info.service_ips, effective_fallback_ip)
        } else {
            Vec::new()
        }
    };

    let istio_query = async {
        // 4. Query Istio VirtualServices (networking.istio.io/v1beta1, falling back to
        // v1alpha3 sequentially — the two API versions are mutually exclusive per cluster).
        let istio_val = match query_k8s_api_json(
            runner,
            kubeconfig_path,
            "/apis/networking.istio.io/v1beta1/virtualservices",
        )
        .await
        {
            Ok(Some(v)) => Some(v),
            _ => query_k8s_api_json(
                runner,
                kubeconfig_path,
                "/apis/networking.istio.io/v1alpha3/virtualservices",
            )
            .await
            .unwrap_or(None),
        };
        match istio_val {
            Some(v) => {
                parse_istio_virtual_services(&v, &svc_info.service_ips, effective_fallback_ip)
            }
            None => Vec::new(),
        }
    };

    let gateway_query = async {
        // 5. Query Gateway API HTTPRoutes (gateway.networking.k8s.io/v1)
        if let Ok(Some(gw_val)) = query_k8s_api_json(
            runner,
            kubeconfig_path,
            "/apis/gateway.networking.k8s.io/v1/httproutes",
        )
        .await
        {
            parse_gateway_httproutes(&gw_val, effective_fallback_ip)
        } else {
            Vec::new()
        }
    };

    let (ingress_endpoints, apisix_endpoints, istio_endpoints, gateway_endpoints) =
        tokio::join!(ingress_query, apisix_query, istio_query, gateway_query);

    // Keep the same extend order as before (ingress, apisix, istio, gateway) so the
    // "keep first occurrence" dedup below is unaffected by running the queries concurrently.
    all_endpoints.extend(ingress_endpoints);
    all_endpoints.extend(apisix_endpoints);
    all_endpoints.extend(istio_endpoints);
    all_endpoints.extend(gateway_endpoints);

    // Deduplicate by host (keep first occurrence, sorted by host)
    let mut dedup_map = BTreeMap::new();
    for ep in all_endpoints {
        if !dedup_map.contains_key(&ep.host) {
            dedup_map.insert(ep.host.clone(), ep);
        }
    }

    Ok(dedup_map.into_values().collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_services_extracts_load_balancer_ip_and_annotations() {
        let json_str = r#"{
            "items": [
                {
                    "metadata": {
                        "name": "apisix-gateway",
                        "namespace": "platform-system",
                        "annotations": {
                            "external-dns.alpha.kubernetes.io/hostname": "gateway.local.internal"
                        }
                    },
                    "spec": { "type": "LoadBalancer" },
                    "status": {
                        "loadBalancer": {
                            "ingress": [{ "ip": "192.168.77.200" }]
                        }
                    }
                },
                {
                    "metadata": { "name": "internal-svc", "namespace": "default" },
                    "spec": { "type": "ClusterIP" },
                    "status": {}
                }
            ]
        }"#;

        let val: serde_json::Value = serde_json::from_str(json_str).unwrap();
        let info = parse_services(&val);

        assert_eq!(info.first_lb_ip, Some("192.168.77.200".to_string()));
        assert_eq!(
            info.service_ips.get("platform-system/apisix-gateway"),
            Some(&"192.168.77.200".to_string())
        );
        assert_eq!(
            info.service_ips.get("apisix-gateway"),
            Some(&"192.168.77.200".to_string())
        );
        assert_eq!(info.endpoints.len(), 1);
        assert_eq!(info.endpoints[0].host, "gateway.local.internal");
        assert_eq!(info.endpoints[0].ip, "192.168.77.200");
    }

    #[test]
    fn parse_apisix_routes_extracts_hosts_and_maps_to_gateway_ip() {
        let json_str = r#"{
            "items": [
                {
                    "metadata": { "name": "trino", "namespace": "analytics" },
                    "spec": {
                        "http": [
                            {
                                "match": {
                                    "hosts": ["trino.local.beluga.internal", "evil\nhost.internal"]
                                }
                            }
                        ]
                    }
                },
                {
                    "metadata": { "name": "s3", "namespace": "storage" },
                    "spec": {
                        "http": [
                            {
                                "match": {
                                    "hosts": ["s3.local.beluga.internal"]
                                }
                            }
                        ]
                    }
                }
            ]
        }"#;

        let val: serde_json::Value = serde_json::from_str(json_str).unwrap();
        let mut svc_ips = HashMap::new();
        svc_ips.insert(
            "platform-system/apisix-gateway".into(),
            "192.168.77.200".into(),
        );

        let endpoints = parse_apisix_routes(&val, &svc_ips, None);
        assert_eq!(endpoints.len(), 2);
        assert_eq!(endpoints[0].host, "trino.local.beluga.internal");
        assert_eq!(endpoints[0].ip, "192.168.77.200");
        assert_eq!(endpoints[0].source, "apisix");
        assert_eq!(endpoints[1].host, "s3.local.beluga.internal");
        assert_eq!(endpoints[1].ip, "192.168.77.200");
    }

    #[test]
    fn parse_ingresses_extracts_rules_and_status_ip() {
        let json_str = r#"{
            "items": [
                {
                    "metadata": { "name": "grafana-ing", "namespace": "monitoring" },
                    "spec": {
                        "rules": [
                            { "host": "grafana.local.internal" }
                        ]
                    },
                    "status": {
                        "loadBalancer": {
                            "ingress": [{ "ip": "192.168.77.201" }]
                        }
                    }
                }
            ]
        }"#;

        let val: serde_json::Value = serde_json::from_str(json_str).unwrap();
        let endpoints = parse_ingresses(&val, None);
        assert_eq!(endpoints.len(), 1);
        assert_eq!(endpoints[0].host, "grafana.local.internal");
        assert_eq!(endpoints[0].ip, "192.168.77.201");
        assert_eq!(endpoints[0].source, "ingress");
    }

    #[test]
    fn parse_istio_virtual_services_filters_wildcards_and_short_names() {
        let json_str = r#"{
            "items": [
                {
                    "metadata": { "name": "details", "namespace": "default" },
                    "spec": {
                        "hosts": ["*", "details", "details.example.com"]
                    }
                }
            ]
        }"#;

        let val: serde_json::Value = serde_json::from_str(json_str).unwrap();
        let endpoints = parse_istio_virtual_services(&val, &HashMap::new(), Some("192.168.77.200"));
        assert_eq!(endpoints.len(), 1);
        assert_eq!(endpoints[0].host, "details.example.com");
        assert_eq!(endpoints[0].ip, "192.168.77.200");
        assert_eq!(endpoints[0].source, "istio");
    }

    struct CurlPermCapturingRunner {
        // (cert temp path, cert mode & 0o777, key temp path, key mode & 0o777)
        captured: std::sync::Mutex<Option<(std::path::PathBuf, u32, std::path::PathBuf, u32)>>,
    }

    #[async_trait::async_trait]
    impl CommandRunner for CurlPermCapturingRunner {
        async fn run(
            &self,
            bin: &str,
            args: &[String],
        ) -> Result<crate::services::process::CommandOutput, String> {
            assert_eq!(bin, "curl");
            use std::os::unix::fs::PermissionsExt;

            let cert_path = std::path::PathBuf::from(
                args.iter()
                    .position(|a| a == "--cert")
                    .map(|i| &args[i + 1])
                    .expect("--cert missing from curl args"),
            );
            let key_path = std::path::PathBuf::from(
                args.iter()
                    .position(|a| a == "--key")
                    .map(|i| &args[i + 1])
                    .expect("--key missing from curl args"),
            );
            let cert_mode = std::fs::metadata(&cert_path).unwrap().permissions().mode() & 0o777;
            let key_mode = std::fs::metadata(&key_path).unwrap().permissions().mode() & 0o777;
            *self.captured.lock().unwrap() = Some((cert_path, cert_mode, key_path, key_mode));

            Ok(crate::services::process::CommandOutput {
                stdout: "{}".to_string(),
                stderr: String::new(),
                success: true,
            })
        }
    }

    #[tokio::test]
    async fn curl_k8s_api_writes_temp_cert_and_key_as_owner_only_and_cleans_up() {
        // Fixture data is obviously fake (AGENTS.md forbids real credentials/infra details in
        // tests): base64 of literal placeholder strings, and a loopback server address.
        use base64::prelude::*;
        let cert_b64 = BASE64_STANDARD.encode(b"fake-cert");
        let key_b64 = BASE64_STANDARD.encode(b"fake-key");
        let kubeconfig_yaml = format!(
            "clusters:\n- cluster:\n    server: https://127.0.0.1:6443\n  name: fake\nusers:\n- name: fake\n  user:\n    client-certificate-data: {cert_b64}\n    client-key-data: {key_b64}\n"
        );

        let kubeconfig_path = std::env::temp_dir().join(format!(
            "clusterdeck-test-curl-k8s-api-kubeconfig-{}-{}.yaml",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::write(&kubeconfig_path, &kubeconfig_yaml).unwrap();

        let runner = CurlPermCapturingRunner {
            captured: std::sync::Mutex::new(None),
        };

        let result = curl_k8s_api(&runner, &kubeconfig_path, "/api/v1/services").await;
        let _ = std::fs::remove_file(&kubeconfig_path);

        assert!(result.is_ok(), "curl_k8s_api failed: {result:?}");
        let (cert_path, cert_mode, key_path, key_mode) = runner
            .captured
            .lock()
            .unwrap()
            .clone()
            .expect("curl was never invoked with --cert/--key");
        assert_eq!(cert_mode, 0o600, "temp cert file must be owner-only");
        assert_eq!(key_mode, 0o600, "temp key file must be owner-only");
        assert!(
            !cert_path.exists(),
            "temp cert file should be removed after curl_k8s_api returns"
        );
        assert!(
            !key_path.exists(),
            "temp key file should be removed after curl_k8s_api returns"
        );
    }

    struct FakeK8sRunner {
        service_json: String,
        apisix_json: String,
    }

    #[async_trait::async_trait]
    impl CommandRunner for FakeK8sRunner {
        async fn run(
            &self,
            bin: &str,
            args: &[String],
        ) -> Result<crate::services::process::CommandOutput, String> {
            if bin == "kubectl" {
                if let Some(path) = args.iter().position(|a| a == "--raw") {
                    let raw_path = &args[path + 1];
                    if raw_path == "/api/v1/services" {
                        return Ok(crate::services::process::CommandOutput {
                            stdout: self.service_json.clone(),
                            stderr: String::new(),
                            success: true,
                        });
                    }
                    if raw_path == "/apis/apisix.apache.org/v2/apisixroutes" {
                        return Ok(crate::services::process::CommandOutput {
                            stdout: self.apisix_json.clone(),
                            stderr: String::new(),
                            success: true,
                        });
                    }
                }
            }
            Ok(crate::services::process::CommandOutput {
                stdout: "{}".to_string(),
                stderr: "NotFound".to_string(),
                success: false,
            })
        }
    }

    #[tokio::test]
    async fn discover_cluster_endpoints_integrates_services_and_apisix() {
        let runner = FakeK8sRunner {
            service_json: r#"{
                "items": [{
                    "metadata": { "name": "apisix-gateway", "namespace": "platform-system" },
                    "spec": { "type": "LoadBalancer" },
                    "status": { "loadBalancer": { "ingress": [{ "ip": "192.168.77.200" }] } }
                }]
            }"#
            .into(),
            apisix_json: r#"{
                "items": [{
                    "metadata": { "name": "trino", "namespace": "analytics" },
                    "spec": { "http": [{ "match": { "hosts": ["trino.local.beluga.internal"] } }] }
                }]
            }"#
            .into(),
        };

        let temp_kc = std::env::temp_dir().join("test-dummy-kc.yaml");
        let _ = std::fs::write(&temp_kc, "dummy");

        let endpoints = discover_cluster_endpoints(&runner, &temp_kc, None)
            .await
            .unwrap();
        let _ = std::fs::remove_file(&temp_kc);

        assert_eq!(endpoints.len(), 1);
        assert_eq!(endpoints[0].host, "trino.local.beluga.internal");
        assert_eq!(endpoints[0].ip, "192.168.77.200");
        assert_eq!(endpoints[0].source, "apisix");
    }
}
