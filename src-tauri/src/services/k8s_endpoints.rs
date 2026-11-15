#![allow(dead_code)]

use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::services::process::{CommandLimits, CommandRunner};
use crate::services::service_login::lookup_service_login;
use crate::services::validate::{
    is_safe_gateway_address, is_safe_host_domain, is_safe_ip_address, is_safe_k8s_dns_label,
};

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiscoveredEndpoint {
    #[serde(default)]
    pub id: String,
    pub host: String,
    pub ip: String,
    pub source: String, // "apisix", "ingress", "istio", "gateway-api", "service"
    pub resource_name: String, // e.g. "analytics/trino"
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub backend_service: Option<String>, // "<ns>/<service>", resolved Service namespace (ADR-0014 D2)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub backend_upstream: Option<String>, // "<route namespace>/<ApisixUpstream name>"
}

impl DiscoveredEndpoint {
    pub fn compute_id(source: &str, resource_name: &str, host: &str) -> String {
        format!("{source}:{resource_name}:{host}")
    }

    pub fn endpoint_id(&self) -> String {
        if self.id.is_empty() {
            Self::compute_id(&self.source, &self.resource_name, &self.host)
        } else {
            self.id.clone()
        }
    }
}

/// Resolves the same-namespace `<ns>/<service>` backend reference for a route/ingress/service
/// rule (ADR-0014 D2). When a single rule routes multiple paths to different Services (e.g.
/// Harbor `/` -> `harbor-portal` and `/api/` -> `harbor-core`), prefers the unique Service that
/// matches the D2 login registry; if two distinct D2 services appear on the same host rule,
/// returns `None` rather than guessing.
fn select_backend_service<S: AsRef<str>>(ns: &str, services: &[S]) -> Option<String> {
    if !is_safe_k8s_dns_label(ns) {
        return None;
    }
    let mut valid = Vec::new();
    for svc in services {
        let name = svc.as_ref().trim();
        if is_safe_k8s_dns_label(name) && !valid.iter().any(|existing: &String| existing == name) {
            valid.push(name.to_string());
        }
    }
    match valid.len() {
        0 => None,
        1 => Some(format!("{ns}/{}", valid[0])),
        _ => {
            let mut login_matches = valid
                .iter()
                .filter(|name| lookup_service_login(name).is_some());
            match (login_matches.next(), login_matches.next()) {
                (Some(unique), None) => Some(format!("{ns}/{unique}")),
                (Some(_), Some(_)) => None,
                (None, _) => Some(format!("{ns}/{}", valid[0])),
            }
        }
    }
}

pub(crate) fn decode_base64(input: &str) -> Option<Vec<u8>> {
    use base64::prelude::*;
    let clean: String = input.chars().filter(|c| !c.is_whitespace()).collect();
    BASE64_STANDARD.decode(clean).ok()
}

pub(crate) fn is_not_found(val: &serde_json::Value) -> bool {
    val.get("kind").and_then(|k| k.as_str()) == Some("Status")
        && val.get("code").and_then(|c| c.as_u64()) == Some(404)
        && val.get("reason").and_then(|r| r.as_str()) == Some("NotFound")
}

pub(crate) fn check_status_error(val: &serde_json::Value) -> Option<String> {
    if is_not_found(val) {
        return None;
    }
    let is_status = val.get("kind").and_then(|k| k.as_str()) == Some("Status");
    let is_failure = val.get("status").and_then(|s| s.as_str()) == Some("Failure");
    let code = val.get("code").and_then(|c| c.as_u64());
    let is_error_code = code.is_some_and(|c| !(200..=299).contains(&c));

    if is_status || is_failure || is_error_code {
        let reason = val
            .get("reason")
            .and_then(|r| r.as_str())
            .unwrap_or("Error");
        let msg = val
            .get("message")
            .and_then(|m| m.as_str())
            .unwrap_or("Kubernetes API error");
        let code_str = code.map(|c| format!(" (code {c})")).unwrap_or_default();
        return Some(format!("k8s Status {reason}{code_str}: {msg}"));
    }
    None
}

/// kubectl's wording for a missing resource type / API group (`get --raw` prints
/// `Error from server (NotFound): the server could not find the requested resource`).
fn stderr_says_not_found(stderr: &str) -> bool {
    stderr.contains("(NotFound)")
        || stderr.contains("the server doesn't have a resource type")
        || stderr.contains("the server could not find the requested resource")
}

/// kubectl relayed a real API server verdict on stderr, so a second transport cannot improve it.
pub(crate) fn stderr_is_api_answer(stderr: &str) -> bool {
    stderr_says_not_found(stderr)
        || stderr_says_unauthorized(stderr)
        || ["Forbidden", "forbidden"]
            .iter()
            .any(|w| stderr.contains(w))
}

/// A 401 reaches kubectl's stderr as either `(Unauthorized)` or, for the plain-text form,
/// `You must be logged in to the server (the server has asked for the client to provide
/// credentials)` -- both are the server's verdict on the credential curl would present too.
pub(crate) fn stderr_says_unauthorized(stderr: &str) -> bool {
    stderr.contains("Unauthorized") || stderr.contains("You must be logged in to the server")
}

pub(crate) fn is_valid_k8s_list(val: &serde_json::Value) -> bool {
    val.get("kind").and_then(|k| k.as_str()) != Some("Status")
        && val.get("items").and_then(|i| i.as_array()).is_some()
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
        if let Ok(val) = serde_json::from_str::<serde_json::Value>(&output.stdout) {
            if is_not_found(&val) {
                return Ok(None);
            }
            if let Some(err) = check_status_error(&val) {
                return Err(err);
            }
            if output.success {
                return Ok(Some(val));
            }
        }
        // An absent CRD is reported by kubectl on stderr only. The curl fallback cannot use
        // exec-plugin credentials (401), so it must never be the verdict for "not installed".
        if !output.success && stderr_says_not_found(&output.stderr) {
            return Ok(None);
        }
    }

    // 2. Fallback to system curl
    let stdout = curl_k8s_api(runner, kubeconfig_path, api_path).await?;

    let parsed: serde_json::Value = serde_json::from_str(&stdout)
        .map_err(|e| format!("failed to parse k8s response json: {e}"))?;

    if is_not_found(&parsed) {
        return Ok(None);
    }
    if let Some(err) = check_status_error(&parsed) {
        return Err(err);
    }

    Ok(Some(parsed))
}

/// Process-wide counter appended to temp cert/key file names, in addition to the nanosecond
/// timestamp, so concurrent callers (e.g. discover_cluster_endpoints's tokio::join! queries,
/// or verify running alongside discovery) never collide on the same file name even when the
/// clock tick is coarser than actual concurrency.
pub(crate) static TEMP_FILE_SEQ: AtomicU64 = AtomicU64::new(0);

/// Creates `path` with owner-only (0600) permissions and writes `contents` to it in one step,
/// so the file (a decoded TLS client certificate or private key) never exists at the default,
/// world-readable permissions even momentarily -- unlike a create-then-chmod sequence, which
/// leaves that window open. Mirrors the temp-file idiom in kubeconfig.rs.
#[cfg(unix)]
pub(crate) fn write_owner_only_file(path: &Path, contents: &[u8]) -> std::io::Result<()> {
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
pub(crate) fn write_owner_only_file(path: &Path, contents: &[u8]) -> std::io::Result<()> {
    std::fs::write(path, contents)
}

/// Splits a `scheme://host[:port][/path[?query][#fragment]]` URL into its `(scheme, host, port,
/// rest)` components, where `rest` is everything from the path onward (starting with `/`, `?`,
/// or `#`, or empty when the URL is bare `scheme://host[:port]`). Returns `None` if there is no
/// `://` separator. When the URL has no explicit `:port` (kubeconfig server URLs almost always
/// carry one in practice, but a hand-edited or default-port one might not), `port` falls back to
/// the scheme's default (443 for https, 80 for http) rather than being omitted, so a curl
/// `--connect-to` mapping built from it stays consistent with what kubectl would connect to.
///
/// Returning the parsed components (rather than a rewritten string) lets a caller rebuild the
/// URL with a substituted host via `format!("{scheme}://{new_host}:{port}{rest}")` -- a prior
/// version rewrote the URL with `str::replacen("://{host}:", ...)`, which silently did nothing
/// when the URL had no port (no literal `:` right after the host to match), leaving the request
/// on the wrong host and defeating the whole `--connect-to` TLS-consistency fix.
fn split_scheme_host_port(url: &str) -> Option<(&str, String, String, &str)> {
    let (scheme, after_scheme) = url.split_once("://")?;
    let rest_start = after_scheme
        .find(['/', '?', '#'])
        .unwrap_or(after_scheme.len());
    let (host_and_port, rest) = after_scheme.split_at(rest_start);
    if let Some((host, port)) = host_and_port.rsplit_once(':') {
        return Some((scheme, host.to_string(), port.to_string(), rest));
    }
    let default_port = match scheme {
        "https" => "443",
        "http" => "80",
        _ => return None,
    };
    Some((
        scheme,
        host_and_port.to_string(),
        default_port.to_string(),
        rest,
    ))
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
    let value = pin_current_context(value)?;
    curl_k8s_api_value(runner, &value, api_path, CurlOptions::default()).await
}

/// `curl_k8s_api` with the child bounded (time and output), for large optional lookups.
pub(crate) async fn curl_k8s_api_bounded(
    runner: &dyn CommandRunner,
    kubeconfig_path: &Path,
    api_path: &str,
    limits: CommandLimits,
) -> Result<String, String> {
    let raw = std::fs::read_to_string(kubeconfig_path)
        .map_err(|e| format!("failed to read kubeconfig: {e}"))?;
    let value: serde_yaml::Value =
        serde_yaml::from_str(&raw).map_err(|e| format!("failed to parse kubeconfig yaml: {e}"))?;
    let value = pin_current_context(value)?;
    let options = CurlOptions {
        post_json: None,
        limits: Some(limits),
    };
    curl_k8s_api_value(runner, &value, api_path, options).await
}

/// Reduces a kubeconfig to the `current-context`'s cluster and user so `curl_k8s_api_value`'s
/// first-entry lookup matches what kubectl would use. Fails closed when kubectl's choice cannot
/// be reproduced: an unresolvable `current-context`, or no `current-context` with anything but
/// exactly one cluster and one user, would make curl present another cluster's credential.
fn pin_current_context(value: serde_yaml::Value) -> Result<serde_yaml::Value, String> {
    let named = |list: &str, name: &str| {
        value
            .get(list)?
            .as_sequence()?
            .iter()
            .find(|e| e.get("name").and_then(|n| n.as_str()) == Some(name))
            .cloned()
    };
    let count = |list: &str| {
        value
            .get(list)
            .and_then(|l| l.as_sequence())
            .map_or(0, |l| l.len())
    };
    let Some(current) = value.get("current-context").and_then(|c| c.as_str()) else {
        return if count("clusters") == 1 && count("users") == 1 {
            Ok(value)
        } else {
            Err("kubeconfig has no current-context and is ambiguous".to_string())
        };
    };
    let pinned = (|| {
        let ctx = named("contexts", current)?;
        let ctx = ctx.get("context")?;
        let cluster = named("clusters", ctx.get("cluster")?.as_str()?)?;
        let user = named("users", ctx.get("user")?.as_str()?)?;
        let mut map = serde_yaml::Mapping::new();
        map.insert(
            "clusters".into(),
            serde_yaml::Value::Sequence(vec![cluster]),
        );
        map.insert("users".into(), serde_yaml::Value::Sequence(vec![user]));
        Some(serde_yaml::Value::Mapping(map))
    })();
    pinned.ok_or_else(|| "kubeconfig current-context cannot be resolved".to_string())
}

/// Extras for `curl_k8s_api_value`. Defaults reproduce the legacy unbounded GET.
#[derive(Clone, Default)]
pub(crate) struct CurlOptions<'a> {
    /// A JSON body switches the request to POST (SelfSubjectAccessReview; no secret inside).
    pub post_json: Option<&'a str>,
    /// Bounds the child (time and output); callers whose response can carry secret data set it.
    pub limits: Option<CommandLimits>,
}

/// `curl_k8s_api` on an already-parsed kubeconfig, which must hold exactly the cluster and
/// user to use as the first entries of `clusters`/`users` (callers pinning a context pass a
/// minified copy so the first entry is the pinned one).
pub(crate) async fn curl_k8s_api_value(
    runner: &dyn CommandRunner,
    value: &serde_yaml::Value,
    api_path: &str,
    options: CurlOptions<'_>,
) -> Result<String, String> {
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
    let token_file = user_obj
        .and_then(|u| u.get("tokenFile"))
        .and_then(|v| v.as_str());

    let cert_path = user_obj
        .and_then(|u| u.get("client-certificate"))
        .and_then(|v| v.as_str());
    let key_path = user_obj
        .and_then(|u| u.get("client-key"))
        .and_then(|v| v.as_str());

    let cluster_obj = value
        .get("clusters")
        .and_then(|c| c.get(0))
        .and_then(|c| c.get("cluster"));
    let ca_data = cluster_obj
        .and_then(|c| c.get("certificate-authority-data"))
        .and_then(|v| v.as_str());
    let ca_path = cluster_obj
        .and_then(|c| c.get("certificate-authority"))
        .and_then(|v| v.as_str());
    let tls_server_name = cluster_obj
        .and_then(|c| c.get("tls-server-name"))
        .and_then(|v| v.as_str());

    // kubeconfig.rs::normalize_with_host sets tls-server-name to the server URL's ORIGINAL host
    // (the one the fetched kubeconfig's TLS cert actually covers) whenever it rewrote the
    // endpoint to a different, SSH-reachable address. kubectl honors that field for TLS
    // verification; align this curl fallback the same way so it doesn't x509-fail against the
    // rewritten host kubectl would happily connect to. `--connect-to` keeps the request URL
    // (and therefore SNI/cert-hostname verification) on the original host while still routing
    // the actual TCP connection to the reachable rewritten address.
    let mut effective_server_url = server_url.clone();
    let mut connect_to: Option<String> = None;
    if let Some(tsn) = tls_server_name {
        if let Some((scheme, actual_host, port, rest)) = split_scheme_host_port(&server_url) {
            if tsn != actual_host {
                // Rebuilt from the parsed components, not a string replace: the URL's host may
                // be followed by `:` (explicit port), `/`/`?`/`#` (path/query/fragment), or
                // nothing at all (bare `scheme://host`), and all three must work.
                effective_server_url = format!("{scheme}://{tsn}:{port}{rest}");
                connect_to = Some(format!("{tsn}:{port}:{actual_host}:{port}"));
            }
        }
    }

    let target_url = format!("{effective_server_url}{api_path}");

    let now_nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let seq = TEMP_FILE_SEQ.fetch_add(1, Ordering::Relaxed);
    let temp = |prefix: &str, ext: &str| {
        std::env::temp_dir().join(format!("cd_tmp_{prefix}_{now_nanos}_{seq}.{ext}"))
    };
    // Dropped on every return path, so a failed write never leaves credentials behind.
    let mut temp_files = TempFiles(Vec::new());

    // `-q` must be the first argument: it keeps a user's ~/.curlrc (insecure, trace, extra
    // headers) from altering a request that carries cluster credentials.
    let mut curl_args = vec![
        "-q".to_string(),
        "-s".to_string(),
        "--connect-timeout".to_string(),
        "5".to_string(),
    ];

    // Verify the server's certificate against the kubeconfig's own CA (same trust source
    // `kubectl` itself uses, per client-go's rest.Config) instead of skipping verification
    // (`-k`/`--insecure`) outright. `-k` would let a network-path attacker (e.g. ARP/rogue-AP on
    // the same LAN as these frequently-local clusters) intercept this call, harvest the client
    // cert/bearer token sent below, and return spoofed cluster data -- this fallback is reachable
    // whenever kubectl fails or is absent (e.g. macOS Sequoia Local Network Privacy), not a rare
    // path. A kubeconfig CA that cannot be decoded/written fails closed: silently falling back
    // to the system trust store would validate against a different root set than kubectl does.
    // Only a kubeconfig with no CA at all uses the system store (still no `-k`).
    if let Some(data) = ca_data {
        let bytes = decode_base64(data).ok_or("kubeconfig CA data is not valid base64")?;
        let path = temp("ca", "crt");
        temp_files.0.push(path.clone());
        write_owner_only_file(&path, &bytes)
            .map_err(|e| format!("failed to stage kubeconfig CA: {e}"))?;
        curl_args.push("--cacert".to_string());
        curl_args.push(path.to_string_lossy().to_string());
    } else if let Some(path) = ca_path {
        curl_args.push("--cacert".to_string());
        curl_args.push(path.to_string());
    }

    if let (Some(c_data), Some(k_data)) = (cert_data, key_data) {
        let (c_bytes, k_bytes) = decode_base64(c_data)
            .zip(decode_base64(k_data))
            .ok_or("kubeconfig client certificate/key is not valid base64")?;
        let (cert, key) = (temp("cert", "crt"), temp("key", "key"));
        temp_files.0.extend([cert.clone(), key.clone()]);
        write_owner_only_file(&cert, &c_bytes)
            .and_then(|()| write_owner_only_file(&key, &k_bytes))
            .map_err(|e| format!("failed to stage kubeconfig client credentials: {e}"))?;
        curl_args.push("--cert".to_string());
        curl_args.push(cert.to_string_lossy().to_string());
        curl_args.push("--key".to_string());
        curl_args.push(key.to_string_lossy().to_string());
    } else if let (Some(c_path), Some(k_path)) = (cert_path, key_path) {
        curl_args.push("--cert".to_string());
        curl_args.push(c_path.to_string());
        curl_args.push("--key".to_string());
        curl_args.push(k_path.to_string());
    } else {
        // tokenFile wins over an inline token, as in client-go, so curl and kubectl present the
        // same identity.
        let file_token = match token_file {
            Some(path) => Some(
                std::fs::read_to_string(path)
                    .map_err(|e| format!("failed to read kubeconfig tokenFile: {e}"))?
                    .trim()
                    .to_string(),
            ),
            None => None,
        };
        if let Some(t) = file_token.as_deref().or(token) {
            // A header file keeps the bearer token out of argv (visible to `ps`).
            let path = temp("hdr", "txt");
            temp_files.0.push(path.clone());
            write_owner_only_file(&path, format!("Authorization: Bearer {t}\n").as_bytes())
                .map_err(|e| format!("failed to stage authorization header: {e}"))?;
            curl_args.push("-H".to_string());
            curl_args.push(format!("@{}", path.to_string_lossy()));
        }
    }

    if let Some(mapping) = connect_to {
        curl_args.push("--connect-to".to_string());
        curl_args.push(mapping);
    }

    if let Some(body) = options.post_json {
        curl_args.extend(
            ["-X", "POST", "-H", "Content-Type: application/json", "-d"].map(str::to_string),
        );
        curl_args.push(body.to_string());
    }

    curl_args.push(target_url);

    let res = match options.limits.clone() {
        Some(limits) => runner.run_bounded("curl", &curl_args, limits).await,
        None => runner.run("curl", &curl_args).await,
    };

    drop(temp_files);

    let output = res.map_err(|e| format!("curl execution failed: {e}"))?;
    if !output.success {
        if let Ok(val) = serde_json::from_str::<serde_json::Value>(&output.stdout) {
            if val.get("kind").and_then(|k| k.as_str()) == Some("Status") {
                return Ok(output.stdout);
            }
        }
        return Err(format!("curl failed: {}", output.stderr));
    }

    Ok(output.stdout)
}

/// Owner-only temp files holding credential material, removed when dropped.
struct TempFiles(Vec<std::path::PathBuf>);

impl Drop for TempFiles {
    fn drop(&mut self) {
        for path in &self.0 {
            let _ = std::fs::remove_file(path);
        }
    }
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
                                        let resource_name = format!("{ns}/{name}");
                                        let backend_service = select_backend_service(ns, &[name]);
                                        for h in hostname.split(',') {
                                            let trimmed = h.trim();
                                            if is_safe_host_domain(trimmed) {
                                                let host = trimmed.to_string();
                                                endpoints.push(DiscoveredEndpoint {
                                                    id: DiscoveredEndpoint::compute_id(
                                                        "service",
                                                        &resource_name,
                                                        &host,
                                                    ),
                                                    host,
                                                    ip: ip.to_string(),
                                                    source: "service".to_string(),
                                                    resource_name: resource_name.clone(),
                                                    backend_service: backend_service.clone(),
                                                    backend_upstream: None,
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

        let default_backend_svc = item
            .get("spec")
            .and_then(|s| s.get("defaultBackend").or_else(|| s.get("backend")))
            .and_then(extract_ingress_backend_service_name);

        if let Some(rules) = item
            .get("spec")
            .and_then(|s| s.get("rules"))
            .and_then(|r| r.as_array())
        {
            for rule in rules {
                if let Some(host) = rule.get("host").and_then(|h| h.as_str()) {
                    if is_safe_host_domain(host) {
                        let mut rule_services: Vec<&str> = Vec::new();
                        if let Some(paths) = rule
                            .get("http")
                            .and_then(|h| h.get("paths"))
                            .and_then(|p| p.as_array())
                        {
                            for path_item in paths {
                                if let Some(svc_name) = path_item
                                    .get("backend")
                                    .and_then(extract_ingress_backend_service_name)
                                {
                                    rule_services.push(svc_name);
                                }
                            }
                        }
                        if rule_services.is_empty() {
                            if let Some(def_svc) = default_backend_svc {
                                rule_services.push(def_svc);
                            }
                        }
                        endpoints.push(DiscoveredEndpoint {
                            id: DiscoveredEndpoint::compute_id("ingress", &resource_name, host),
                            host: host.to_string(),
                            ip: chosen_ip.clone(),
                            source: "ingress".to_string(),
                            resource_name: resource_name.clone(),
                            backend_service: select_backend_service(ns, &rule_services),
                            backend_upstream: None,
                        });
                    }
                }
            }
        }
    }

    endpoints
}

fn extract_ingress_backend_service_name(backend: &serde_json::Value) -> Option<&str> {
    backend
        .get("service")
        .and_then(|s| s.get("name"))
        .and_then(|n| n.as_str())
        .or_else(|| backend.get("serviceName").and_then(|n| n.as_str()))
}

fn extract_apisix_rule_services(rule: &serde_json::Value) -> Vec<String> {
    let mut svcs = Vec::new();
    if let Some(backends) = rule.get("backends").and_then(|b| b.as_array()) {
        for b in backends {
            if let Some(name) = b.get("serviceName").and_then(|n| n.as_str()) {
                svcs.push(name.to_string());
            }
        }
    }
    if let Some(name) = rule
        .get("backend")
        .and_then(|b| b.get("serviceName"))
        .and_then(|n| n.as_str())
    {
        svcs.push(name.to_string());
    }
    svcs
}

// D2 (2026-11-16): the Secret belongs to the Service traffic reaches, even across
// namespaces. Only exact in-cluster Service DNS proves that binding; accepting arbitrary
// domains would guess a Secret. Cost: custom DNS suffixes get no login; escape: amend D2.
pub(crate) fn apisix_upstream_service(upstream: &serde_json::Value) -> Option<String> {
    let nodes = upstream.get("spec")?.get("externalNodes")?.as_array()?;
    let mut target = None;
    for node in nodes {
        if node.get("type")?.as_str()? != "Domain" {
            return None;
        }
        let labels: Vec<_> = node.get("name")?.as_str()?.split('.').collect();
        match labels.as_slice() {
            [svc, ns, "svc"] | [svc, ns, "svc", "cluster", "local"]
                if is_safe_k8s_dns_label(svc) && is_safe_k8s_dns_label(ns) =>
            {
                let service = format!("{ns}/{svc}");
                if target.as_ref().is_some_and(|old| old != &service) {
                    return None;
                }
                target = Some(service);
            }
            _ => return None,
        }
    }
    target
}

type ApisixBackend = (String, Option<String>);

fn apisix_rule_backend(
    rule: &serde_json::Value,
    ns: &str,
    upstreams: &serde_json::Value,
) -> Option<ApisixBackend> {
    let mut targets = Vec::new();
    let mut upstream_ref = None;
    if let Some(service) = select_backend_service(ns, &extract_apisix_rule_services(rule)) {
        targets.push(service);
    }
    if let Some(refs) = rule.get("upstreams") {
        for reference in refs.as_array()? {
            let name = reference.get("name")?.as_str()?;
            if !is_safe_k8s_dns_label(ns) || !is_safe_k8s_dns_label(name) {
                return None;
            }
            let upstream = upstreams.get("items")?.as_array()?.iter().find(|item| {
                item["metadata"]["namespace"].as_str() == Some(ns)
                    && item["metadata"]["name"].as_str() == Some(name)
            })?;
            let reference = format!("{ns}/{name}");
            if upstream_ref.as_ref().is_some_and(|old| old != &reference) {
                return None;
            }
            upstream_ref = Some(reference);
            targets.push(apisix_upstream_service(upstream)?);
        }
    }
    let first = targets.first()?;
    targets
        .iter()
        .all(|t| t == first)
        .then(|| (first.clone(), upstream_ref))
}

pub fn parse_apisix_routes(
    val: &serde_json::Value,
    upstreams: &serde_json::Value,
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

        let mut hosts: Vec<(String, Vec<Option<ApisixBackend>>)> = Vec::new();

        if let Some(spec) = item.get("spec") {
            for section in ["http", "stream", "tcp"] {
                if let Some(rules) = spec.get(section).and_then(|r| r.as_array()) {
                    for rule in rules {
                        let rule_svcs = vec![apisix_rule_backend(rule, ns, upstreams)];
                        if let Some(h_list) = rule
                            .get("match")
                            .and_then(|m| m.get("hosts"))
                            .and_then(|h| h.as_array())
                        {
                            for h in h_list {
                                if let Some(host_str) = h.as_str() {
                                    if let Some((_, existing_svcs)) =
                                        hosts.iter_mut().find(|(eh, _)| eh == host_str)
                                    {
                                        existing_svcs.extend(rule_svcs.clone());
                                    } else {
                                        hosts.push((host_str.to_string(), rule_svcs.clone()));
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }

        for (host, host_svcs) in hosts {
            if is_safe_host_domain(&host) {
                let backend_service = host_svcs
                    .iter()
                    .map(|binding| binding.as_ref().map(|(service, _)| service.clone()))
                    .collect::<Option<Vec<_>>>()
                    .and_then(|mut targets| {
                        targets.dedup();
                        match targets.as_slice() {
                            [] => None,
                            [only] => Some(only.clone()),
                            _ => {
                                // Preserve namespace-local multi-path selection (e.g. Harbor
                                // portal/core); cross-namespace targets must be unambiguous.
                                let names: Option<Vec<_>> = targets
                                    .iter()
                                    .map(|t| t.strip_prefix(&format!("{ns}/")))
                                    .collect();
                                select_backend_service(ns, &names?)
                            }
                        }
                    });
                let mut refs = host_svcs
                    .iter()
                    .flatten()
                    .filter(|(service, _)| Some(service) == backend_service.as_ref())
                    .filter_map(|(_, reference)| reference.clone())
                    .collect::<Vec<_>>();
                refs.sort();
                refs.dedup();
                // D2: one live upstream read proves the cached binding. Cost: ambiguous
                // upstream aliases get no login; escape: rescan with one upstream per backend.
                let backend_service = if refs.len() > 1 {
                    None
                } else {
                    backend_service
                };
                let backend_upstream = refs.pop();
                endpoints.push(DiscoveredEndpoint {
                    id: DiscoveredEndpoint::compute_id("apisix", &resource_name, &host),
                    host,
                    ip: chosen_ip.to_string(),
                    source: "apisix".to_string(),
                    resource_name: resource_name.clone(),
                    backend_service,
                    backend_upstream,
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
                            id: DiscoveredEndpoint::compute_id("istio", &resource_name, host_str),
                            host: host_str.to_string(),
                            ip: chosen_ip.to_string(),
                            source: "istio".to_string(),
                            resource_name: resource_name.clone(),
                            backend_service: None,
                            backend_upstream: None,
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

        let backend_service = extract_gateway_httproute_backend_service(item.get("spec"), ns);

        if let Some(hostnames) = item
            .get("spec")
            .and_then(|s| s.get("hostnames"))
            .and_then(|h| h.as_array())
        {
            for h in hostnames {
                if let Some(host_str) = h.as_str() {
                    if is_safe_host_domain(host_str) {
                        endpoints.push(DiscoveredEndpoint {
                            id: DiscoveredEndpoint::compute_id(
                                "gateway-api",
                                &resource_name,
                                host_str,
                            ),
                            host: host_str.to_string(),
                            ip: chosen_ip.to_string(),
                            source: "gateway-api".to_string(),
                            resource_name: resource_name.clone(),
                            backend_service: backend_service.clone(),
                            backend_upstream: None,
                        });
                    }
                }
            }
        }
    }

    endpoints
}

fn extract_gateway_httproute_backend_service(
    spec: Option<&serde_json::Value>,
    route_ns: &str,
) -> Option<String> {
    let rules = spec?.get("rules")?.as_array()?;
    let mut svcs = Vec::new();
    for rule in rules {
        let Some(backend_refs) = rule.get("backendRefs").and_then(|b| b.as_array()) else {
            continue;
        };
        for bref in backend_refs {
            // D2/D5 same-namespace rule: ignore cross-namespace backendRefs so a tenant route
            // cannot reference another namespace's Service/Secret.
            if let Some(ref_ns) = bref.get("namespace").and_then(|n| n.as_str()) {
                if !ref_ns.is_empty() && ref_ns != route_ns {
                    continue;
                }
            }
            let kind = bref
                .get("kind")
                .and_then(|k| k.as_str())
                .unwrap_or("Service");
            let group = bref.get("group").and_then(|g| g.as_str()).unwrap_or("");
            if kind != "Service" || (!group.is_empty() && group != "core") {
                continue;
            }
            if let Some(svc_name) = bref.get("name").and_then(|n| n.as_str()) {
                svcs.push(svc_name.to_string());
            }
        }
    }
    select_backend_service(route_ns, &svcs)
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiscoveryResult {
    pub endpoints: Vec<DiscoveredEndpoint>,
    pub succeeded_kinds: Vec<String>,
    pub failed_kinds: Vec<String>,
    pub errors_by_kind: Vec<(String, String)>,
}

/// Every `DiscoveredEndpoint::source` / `DiscoveryResult` kind the discovery can produce.
pub const DISCOVERY_KINDS: [&str; 5] = ["service", "ingress", "apisix", "istio", "gateway-api"];

pub fn display_kind_name(kind: &str) -> &'static str {
    match kind {
        "service" => "Services",
        "ingress" => "Ingress",
        "apisix" => "APISIX",
        "istio" => "Istio",
        "gateway-api" => "Gateway",
        _ => "Unknown",
    }
}

pub async fn discover_cluster_endpoints_detailed(
    runner: &dyn CommandRunner,
    kubeconfig_path: &Path,
    default_ip: Option<&str>,
    gateway_address: Option<&str>,
) -> Result<DiscoveryResult, String> {
    let mut all_endpoints = Vec::new();
    let mut succeeded_kinds = Vec::new();
    let mut failed_kinds = Vec::new();
    let mut errors_by_kind = Vec::new();

    // 1. Query Services to find LoadBalancer IPs
    let svc_res = query_k8s_api_json(runner, kubeconfig_path, "/api/v1/services").await;
    let svc_info = match svc_res {
        Ok(Some(val)) if is_valid_k8s_list(&val) => {
            succeeded_kinds.push("service".to_string());
            parse_services(&val)
        }
        Ok(Some(_)) => {
            let err = "malformed k8s list: missing items array".to_string();
            failed_kinds.push("service".to_string());
            errors_by_kind.push(("service".to_string(), err));
            ServiceDiscoveryInfo {
                service_ips: HashMap::new(),
                first_lb_ip: None,
                endpoints: Vec::new(),
            }
        }
        Ok(None) => {
            succeeded_kinds.push("service".to_string());
            ServiceDiscoveryInfo {
                service_ips: HashMap::new(),
                first_lb_ip: None,
                endpoints: Vec::new(),
            }
        }
        Err(e) => {
            failed_kinds.push("service".to_string());
            errors_by_kind.push(("service".to_string(), e));
            ServiceDiscoveryInfo {
                service_ips: HashMap::new(),
                first_lb_ip: None,
                endpoints: Vec::new(),
            }
        }
    };

    all_endpoints.extend(svc_info.endpoints);

    // Ingress/APISIX/Istio/Gateway targets come from the Services/LB lookup (or the profile's
    // fallback address). When that lookup failed, their IPs are guesses that must not replace
    // good cached ones, unless a gateway override pins every IP anyway.
    let services_failed = failed_kinds.iter().any(|k| k == "service");
    let ips_unreliable = services_failed && !gateway_address.is_some_and(is_safe_gateway_address);

    // D3 (#102): classify nonempty resource lists before parsers drop unresolved IPs.
    // Conservatively retain cached dependent kinds; true empty lists/absent CRDs still
    // remove records. Per-resource dependency tracking can relax this if needed later.
    let reliable_targets = |result: Result<Option<serde_json::Value>, String>| {
        result.and_then(|value| {
            if ips_unreliable
                && value.as_ref().is_some_and(|v| {
                    v.get("items")
                        .and_then(|i| i.as_array())
                        .is_some_and(|items| !items.is_empty())
                })
            {
                Err("target IPs depend on the Services lookup, which failed".to_string())
            } else {
                Ok(value)
            }
        })
    };

    let effective_fallback_ip = svc_info.first_lb_ip.as_deref().or(default_ip);

    // 2-5. Ingresses, APISIX routes, Istio VirtualServices, and Gateway HTTPRoutes run concurrently.
    let ingress_query = async {
        let res = query_k8s_api_json(
            runner,
            kubeconfig_path,
            "/apis/networking.k8s.io/v1/ingresses",
        )
        .await;
        match reliable_targets(res) {
            Ok(Some(ing_val)) if is_valid_k8s_list(&ing_val) => (
                Some("ingress".to_string()),
                None,
                parse_ingresses(&ing_val, effective_fallback_ip),
            ),
            Ok(Some(_)) => (
                None,
                Some((
                    "ingress".to_string(),
                    "malformed k8s list: missing items array".to_string(),
                )),
                Vec::new(),
            ),
            Ok(None) => (Some("ingress".to_string()), None, Vec::new()),
            Err(e) => (None, Some(("ingress".to_string(), e)), Vec::new()),
        }
    };

    let apisix_query = async {
        let res = query_k8s_api_json(
            runner,
            kubeconfig_path,
            "/apis/apisix.apache.org/v2/apisixroutes",
        )
        .await;
        match reliable_targets(res) {
            Ok(Some(apisix_val)) if is_valid_k8s_list(&apisix_val) => {
                // Fetch once only when a route references an ApisixUpstream. A failed
                // optional lookup removes login bindings, never the discovered endpoints.
                let needs_upstreams = apisix_val["items"].as_array().unwrap().iter().any(|item| {
                    ["http", "stream", "tcp"].iter().any(|section| {
                        item["spec"][section].as_array().is_some_and(|rules| {
                            rules.iter().any(|rule| rule.get("upstreams").is_some())
                        })
                    })
                });
                let mut upstreams = serde_json::json!({"items": []});
                let mut failure = None;
                if needs_upstreams {
                    let args = vec![
                        "--kubeconfig".to_string(),
                        kubeconfig_path.to_string_lossy().to_string(),
                        "get".to_string(),
                        "--raw".to_string(),
                        "/apis/apisix.apache.org/v2/apisixupstreams".to_string(),
                    ];
                    let limits = CommandLimits {
                        timeout: std::time::Duration::from_secs(10),
                        max_output_bytes: 2 * 1024 * 1024,
                    };
                    let mut result = runner.run_bounded("kubectl", &args, limits.clone()).await;
                    // macOS Local Network Privacy blocks kubectl inside the app bundle while
                    // system curl is exempt (same fallback as query_k8s_api_json).
                    // An API answer relayed on stderr (Forbidden, NotFound, Unauthorized) is
                    // authoritative; only transport failures are retried.
                    let transport_failure = match &result {
                        Ok(output) => !output.success && !stderr_is_api_answer(&output.stderr),
                        Err(_) => true,
                    };
                    if transport_failure {
                        result = curl_k8s_api_bounded(
                            runner,
                            kubeconfig_path,
                            "/apis/apisix.apache.org/v2/apisixupstreams",
                            limits,
                        )
                        .await
                        .map(|stdout| {
                            crate::services::process::CommandOutput {
                                stdout,
                                stderr: String::new(),
                                success: true,
                            }
                        });
                    }
                    match result {
                        Ok(output) if output.success => {
                            match serde_json::from_str::<serde_json::Value>(&output.stdout) {
                                Ok(value) if is_valid_k8s_list(&value) => upstreams = value,
                                _ => {
                                    failure = Some((
                                        "apisix".to_string(),
                                        "malformed ApisixUpstream list".to_string(),
                                    ))
                                }
                            }
                        }
                        _ => {
                            failure = Some((
                                "apisix".to_string(),
                                "ApisixUpstream lookup failed".to_string(),
                            ))
                        }
                    }
                }
                (
                    Some("apisix".to_string()),
                    failure,
                    parse_apisix_routes(
                        &apisix_val,
                        &upstreams,
                        &svc_info.service_ips,
                        effective_fallback_ip,
                    ),
                )
            }
            Ok(Some(_)) => (
                None,
                Some((
                    "apisix".to_string(),
                    "malformed k8s list: missing items array".to_string(),
                )),
                Vec::new(),
            ),
            Ok(None) => (Some("apisix".to_string()), None, Vec::new()),
            Err(e) => (None, Some(("apisix".to_string(), e)), Vec::new()),
        }
    };

    let istio_query = async {
        let res = match query_k8s_api_json(
            runner,
            kubeconfig_path,
            "/apis/networking.istio.io/v1beta1/virtualservices",
        )
        .await
        {
            Ok(Some(v)) => Ok(Some(v)),
            Ok(None) => {
                query_k8s_api_json(
                    runner,
                    kubeconfig_path,
                    "/apis/networking.istio.io/v1alpha3/virtualservices",
                )
                .await
            }
            Err(e) => Err(e),
        };
        match reliable_targets(res) {
            Ok(Some(v)) if is_valid_k8s_list(&v) => (
                Some("istio".to_string()),
                None,
                parse_istio_virtual_services(&v, &svc_info.service_ips, effective_fallback_ip),
            ),
            Ok(Some(_)) => (
                None,
                Some((
                    "istio".to_string(),
                    "malformed k8s list: missing items array".to_string(),
                )),
                Vec::new(),
            ),
            Ok(None) => (Some("istio".to_string()), None, Vec::new()),
            Err(e) => (None, Some(("istio".to_string(), e)), Vec::new()),
        }
    };

    let gateway_query = async {
        let res = query_k8s_api_json(
            runner,
            kubeconfig_path,
            "/apis/gateway.networking.k8s.io/v1/httproutes",
        )
        .await;
        match reliable_targets(res) {
            Ok(Some(gw_val)) if is_valid_k8s_list(&gw_val) => (
                Some("gateway-api".to_string()),
                None,
                parse_gateway_httproutes(&gw_val, effective_fallback_ip),
            ),
            Ok(Some(_)) => (
                None,
                Some((
                    "gateway-api".to_string(),
                    "malformed k8s list: missing items array".to_string(),
                )),
                Vec::new(),
            ),
            Ok(None) => (Some("gateway-api".to_string()), None, Vec::new()),
            Err(e) => (None, Some(("gateway-api".to_string(), e)), Vec::new()),
        }
    };

    let (ing_out, apisix_out, istio_out, gw_out) =
        tokio::join!(ingress_query, apisix_query, istio_query, gateway_query);

    for (succ, fail, eps) in [ing_out, apisix_out, istio_out, gw_out] {
        if let Some(s) = succ {
            succeeded_kinds.push(s);
        }
        if let Some(f) = fail {
            failed_kinds.push(f.0.clone());
            errors_by_kind.push(f);
        }
        all_endpoints.extend(eps);
    }

    // If all queries failed, return the primary error
    if succeeded_kinds.is_empty() {
        let err_summary = errors_by_kind
            .iter()
            .map(|(k, e)| format!("{}: {e}", display_kind_name(k)))
            .collect::<Vec<_>>()
            .join("; ");
        return Err(if err_summary.is_empty() {
            "all resource queries failed during discovery".to_string()
        } else {
            err_summary
        });
    }

    // Deduplicate by host (keep first occurrence, sorted by host)
    let mut dedup_map = BTreeMap::new();
    for ep in all_endpoints {
        if !dedup_map.contains_key(&ep.host) {
            dedup_map.insert(ep.host.clone(), ep);
        }
    }

    let mut endpoints: Vec<DiscoveredEndpoint> = dedup_map.into_values().collect();

    if let Some(gw) = gateway_address.filter(|g| is_safe_gateway_address(g)) {
        for ep in &mut endpoints {
            ep.ip = gw.to_string();
        }
    }

    Ok(DiscoveryResult {
        endpoints,
        succeeded_kinds,
        failed_kinds,
        errors_by_kind,
    })
}

pub async fn discover_cluster_endpoints(
    runner: &dyn CommandRunner,
    kubeconfig_path: &Path,
    default_ip: Option<&str>,
    gateway_address: Option<&str>,
) -> Result<Vec<DiscoveredEndpoint>, String> {
    let res =
        discover_cluster_endpoints_detailed(runner, kubeconfig_path, default_ip, gateway_address)
            .await?;
    Ok(res.endpoints)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_scheme_host_port_parses_scheme_host_port_and_rest() {
        assert_eq!(
            split_scheme_host_port("https://127.0.0.1:6443"),
            Some(("https", "127.0.0.1".to_string(), "6443".to_string(), ""))
        );
        assert_eq!(
            split_scheme_host_port("https://cluster.example.com:6443/api"),
            Some((
                "https",
                "cluster.example.com".to_string(),
                "6443".to_string(),
                "/api"
            ))
        );
        assert_eq!(split_scheme_host_port("not-a-url"), None);
    }

    #[test]
    fn split_scheme_host_port_defaults_port_from_scheme_when_absent() {
        assert_eq!(
            split_scheme_host_port("https://no-port-here"),
            Some(("https", "no-port-here".to_string(), "443".to_string(), ""))
        );
        assert_eq!(
            split_scheme_host_port("https://no-port-here/api/v1/services"),
            Some((
                "https",
                "no-port-here".to_string(),
                "443".to_string(),
                "/api/v1/services"
            ))
        );
        assert_eq!(
            split_scheme_host_port("http://no-port-here"),
            Some(("http", "no-port-here".to_string(), "80".to_string(), ""))
        );
        assert_eq!(split_scheme_host_port("ftp://no-port-here"), None);
    }

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

        let endpoints =
            parse_apisix_routes(&val, &serde_json::json!({"items": []}), &svc_ips, None);
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

    struct ArgCapturingRunner {
        captured_args: std::sync::Mutex<Option<Vec<String>>>,
    }

    #[async_trait::async_trait]
    impl CommandRunner for ArgCapturingRunner {
        async fn run(
            &self,
            bin: &str,
            args: &[String],
        ) -> Result<crate::services::process::CommandOutput, String> {
            assert_eq!(bin, "curl");
            *self.captured_args.lock().unwrap() = Some(args.to_vec());
            Ok(crate::services::process::CommandOutput {
                stdout: "{}".to_string(),
                stderr: String::new(),
                success: true,
            })
        }
    }

    #[tokio::test]
    async fn curl_k8s_api_fails_closed_on_undecodable_ca_and_runs_without_rc_or_argv_token() {
        let dir = std::env::temp_dir();
        let write = |name: &str, body: &str| {
            let p = dir.join(format!("{name}-{}.yaml", std::process::id()));
            std::fs::write(&p, body).unwrap();
            p
        };
        let bad_ca = write(
            "cd-test-bad-ca",
            "clusters:\n- cluster: {server: 'https://192.0.2.50:6443', certificate-authority-data: '!!notbase64!!'}\nusers:\n- user: {token: secret-tok}\n",
        );
        let runner = ArgCapturingRunner {
            captured_args: std::sync::Mutex::new(None),
        };
        let err = curl_k8s_api(&runner, &bad_ca, "/api").await.unwrap_err();
        assert!(err.contains("CA"), "{err}");
        assert!(
            runner.captured_args.lock().unwrap().is_none(),
            "curl must not run"
        );

        let ok = write(
            "cd-test-q",
            "current-context: b\ncontexts:\n- name: a\n  context: {cluster: ca, user: ua}\n- name: b\n  context: {cluster: cb, user: ub}\nclusters:\n- name: ca\n  cluster: {server: 'https://192.0.2.1:6443'}\n- name: cb\n  cluster: {server: 'https://192.0.2.2:6443'}\nusers:\n- name: ua\n  user: {token: tok-a}\n- name: ub\n  user: {token: tok-b}\n",
        );
        curl_k8s_api(&runner, &ok, "/api").await.unwrap();
        let args = runner.captured_args.lock().unwrap().clone().unwrap();
        assert_eq!(args[0], "-q");
        assert!(
            !args.join(" ").contains("tok-"),
            "token must stay out of argv"
        );
        assert_eq!(args.last().unwrap(), "https://192.0.2.2:6443/api");
        let _ = std::fs::remove_file(bad_ca);
        let _ = std::fs::remove_file(ok);
    }

    #[tokio::test]
    async fn curl_k8s_api_never_passes_insecure_flag() {
        // Regression test: curl_k8s_api must never disable TLS verification (`-k`/`--insecure`),
        // whether or not the kubeconfig has CA data -- a bare token-only kubeconfig with no
        // clusters[0].cluster.certificate-authority-data must still verify against curl's system
        // trust store, not skip verification.
        let kubeconfig_yaml =
            "clusters:\n- cluster:\n    server: https://127.0.0.1:6443\n  name: fake\nusers:\n- name: fake\n  user:\n    token: fake-token\n";
        let kubeconfig_path = std::env::temp_dir().join(format!(
            "clusterdeck-test-curl-no-ca-{}-{}.yaml",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::write(&kubeconfig_path, kubeconfig_yaml).unwrap();

        let runner = ArgCapturingRunner {
            captured_args: std::sync::Mutex::new(None),
        };
        let result = curl_k8s_api(&runner, &kubeconfig_path, "/api/v1/services").await;
        let _ = std::fs::remove_file(&kubeconfig_path);

        assert!(result.is_ok(), "curl_k8s_api failed: {result:?}");
        let args = runner.captured_args.lock().unwrap().clone().unwrap();
        assert!(
            !args.contains(&"-k".to_string()) && !args.contains(&"--insecure".to_string()),
            "curl args must never disable TLS verification: {args:?}"
        );
        assert!(
            !args.contains(&"--cacert".to_string()),
            "no CA data in kubeconfig means no --cacert should be passed either: {args:?}"
        );
    }

    #[tokio::test]
    async fn curl_k8s_api_uses_cacert_from_kubeconfig_and_cleans_up() {
        use base64::prelude::*;
        let ca_b64 = BASE64_STANDARD.encode(b"fake-ca-cert");
        let kubeconfig_yaml = format!(
            "clusters:\n- cluster:\n    server: https://127.0.0.1:6443\n    certificate-authority-data: {ca_b64}\n  name: fake\nusers:\n- name: fake\n  user:\n    token: fake-token\n"
        );
        let kubeconfig_path = std::env::temp_dir().join(format!(
            "clusterdeck-test-curl-with-ca-{}-{}.yaml",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::write(&kubeconfig_path, &kubeconfig_yaml).unwrap();

        let runner = ArgCapturingRunner {
            captured_args: std::sync::Mutex::new(None),
        };
        let result = curl_k8s_api(&runner, &kubeconfig_path, "/api/v1/services").await;
        let _ = std::fs::remove_file(&kubeconfig_path);

        assert!(result.is_ok(), "curl_k8s_api failed: {result:?}");
        let args = runner.captured_args.lock().unwrap().clone().unwrap();
        assert!(
            !args.contains(&"-k".to_string()),
            "must not pass -k when CA data is available: {args:?}"
        );
        let ca_idx = args
            .iter()
            .position(|a| a == "--cacert")
            .expect("--cacert missing from curl args");
        // curl_k8s_api unconditionally deletes the temp CA file before returning, so its content
        // must be captured from inside ArgCapturingRunner::run (while it still exists), not here.
        let ca_path = std::path::PathBuf::from(&args[ca_idx + 1]);
        assert!(
            !ca_path.exists(),
            "temp CA file should be removed after curl_k8s_api returns"
        );
    }

    #[tokio::test]
    async fn curl_k8s_api_routes_via_connect_to_and_keeps_url_on_original_host_when_tls_server_name_set(
    ) {
        // kubeconfig.rs::normalize_with_host sets tls-server-name to the ORIGINAL server host
        // (127.0.0.1) whenever it rewrites the endpoint to a reachable address (172.16.221.133,
        // used here as an obviously-fake profile-host address per AGENTS.md). curl must keep the
        // request URL/SNI on the original host (so TLS verification matches the cert kubectl
        // would accept) and use --connect-to to route the socket to the reachable address.
        let kubeconfig_yaml = "clusters:\n- cluster:\n    server: https://172.16.221.133:6443\n    tls-server-name: 127.0.0.1\n  name: fake\nusers:\n- name: fake\n  user:\n    token: fake-token\n";
        let kubeconfig_path = std::env::temp_dir().join(format!(
            "clusterdeck-test-curl-connect-to-{}-{}.yaml",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::write(&kubeconfig_path, kubeconfig_yaml).unwrap();

        let runner = ArgCapturingRunner {
            captured_args: std::sync::Mutex::new(None),
        };
        let result = curl_k8s_api(&runner, &kubeconfig_path, "/api/v1/services").await;
        let _ = std::fs::remove_file(&kubeconfig_path);

        assert!(result.is_ok(), "curl_k8s_api failed: {result:?}");
        let args = runner.captured_args.lock().unwrap().clone().unwrap();
        let connect_to_idx = args
            .iter()
            .position(|a| a == "--connect-to")
            .expect("--connect-to missing from curl args");
        assert_eq!(
            args[connect_to_idx + 1],
            "127.0.0.1:6443:172.16.221.133:6443"
        );
        let url = args.last().unwrap();
        assert_eq!(url, "https://127.0.0.1:6443/api/v1/services");
    }

    #[tokio::test]
    async fn curl_k8s_api_uses_scheme_default_port_for_connect_to_when_url_has_no_port() {
        // server URL with no explicit :port -- split_scheme_host_port must fall back to https's
        // default (443) rather than skip --connect-to entirely, AND the rebuilt request URL
        // must actually land on the original host: the prior string-replace implementation
        // (`replacen("://{host}:", ...)`) silently did nothing here, since there is no literal
        // `:` right after the host to match, leaving the URL on the wrong (rewritten) host.
        let kubeconfig_yaml = "clusters:\n- cluster:\n    server: https://172.16.221.133\n    tls-server-name: 127.0.0.1\n  name: fake\nusers:\n- name: fake\n  user:\n    token: fake-token\n";
        let kubeconfig_path = std::env::temp_dir().join(format!(
            "clusterdeck-test-curl-connect-to-default-port-{}-{}.yaml",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::write(&kubeconfig_path, kubeconfig_yaml).unwrap();

        let runner = ArgCapturingRunner {
            captured_args: std::sync::Mutex::new(None),
        };
        let result = curl_k8s_api(&runner, &kubeconfig_path, "/api/v1/services").await;
        let _ = std::fs::remove_file(&kubeconfig_path);

        assert!(result.is_ok(), "curl_k8s_api failed: {result:?}");
        let args = runner.captured_args.lock().unwrap().clone().unwrap();
        let connect_to_idx = args
            .iter()
            .position(|a| a == "--connect-to")
            .expect("--connect-to missing from curl args");
        assert_eq!(args[connect_to_idx + 1], "127.0.0.1:443:172.16.221.133:443");
        let url = args.last().unwrap();
        assert_eq!(url, "https://127.0.0.1:443/api/v1/services");
    }

    #[tokio::test]
    async fn curl_k8s_api_omits_connect_to_when_tls_server_name_absent() {
        let kubeconfig_yaml =
            "clusters:\n- cluster:\n    server: https://127.0.0.1:6443\n  name: fake\nusers:\n- name: fake\n  user:\n    token: fake-token\n";
        let kubeconfig_path = std::env::temp_dir().join(format!(
            "clusterdeck-test-curl-no-connect-to-{}-{}.yaml",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::write(&kubeconfig_path, kubeconfig_yaml).unwrap();

        let runner = ArgCapturingRunner {
            captured_args: std::sync::Mutex::new(None),
        };
        let result = curl_k8s_api(&runner, &kubeconfig_path, "/api/v1/services").await;
        let _ = std::fs::remove_file(&kubeconfig_path);

        assert!(result.is_ok(), "curl_k8s_api failed: {result:?}");
        let args = runner.captured_args.lock().unwrap().clone().unwrap();
        assert!(
            !args.contains(&"--connect-to".to_string()),
            "no tls-server-name means no --connect-to should be passed: {args:?}"
        );
        assert_eq!(
            args.last().unwrap(),
            "https://127.0.0.1:6443/api/v1/services"
        );
    }

    struct FakeK8sRunner {
        service_json: String,
        apisix_json: String,
        ingress_json: String,
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
                    if raw_path == "/apis/networking.k8s.io/v1/ingresses" {
                        return Ok(crate::services::process::CommandOutput {
                            stdout: self.ingress_json.clone(),
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
            ingress_json: "{}".into(),
        };

        let temp_kc = std::env::temp_dir().join("test-dummy-kc.yaml");
        let _ = std::fs::write(&temp_kc, "dummy");

        let endpoints = discover_cluster_endpoints(&runner, &temp_kc, None, None)
            .await
            .unwrap();
        let _ = std::fs::remove_file(&temp_kc);

        assert_eq!(endpoints.len(), 1);
        assert_eq!(endpoints[0].host, "trino.local.beluga.internal");
        assert_eq!(endpoints[0].ip, "192.168.77.200");
        assert_eq!(endpoints[0].source, "apisix");
    }

    // RFC 5737 documentation addresses only.
    async fn discover_with_gateway(gateway: Option<&str>) -> Vec<DiscoveredEndpoint> {
        let runner = FakeK8sRunner {
            service_json: r#"{
                "items": [{
                    "metadata": {
                        "name": "lb", "namespace": "edge",
                        "annotations": { "external-dns.alpha.kubernetes.io/hostname": "svc.example.test" }
                    },
                    "spec": { "type": "LoadBalancer" },
                    "status": { "loadBalancer": { "ingress": [{ "ip": "192.0.2.200" }] } }
                }]
            }"#
            .into(),
            apisix_json: r#"{
                "items": [{
                    "metadata": { "name": "r", "namespace": "app" },
                    "spec": { "http": [{ "match": { "hosts": ["apisix.example.test"] } }] }
                }]
            }"#
            .into(),
            ingress_json: r#"{
                "items": [{
                    "metadata": { "name": "i", "namespace": "app" },
                    "spec": { "rules": [{ "host": "ing.example.test" }] },
                    "status": { "loadBalancer": { "ingress": [{ "ip": "192.0.2.201" }] } }
                }]
            }"#
            .into(),
        };
        let kc = std::env::temp_dir().join(format!("test-gw-kc-{}.yaml", gateway.is_some()));
        let _ = std::fs::write(&kc, "dummy");
        let eps = discover_cluster_endpoints(&runner, &kc, Some("198.51.100.10"), gateway)
            .await
            .unwrap();
        let _ = std::fs::remove_file(&kc);
        eps
    }

    #[tokio::test]
    async fn gateway_address_overrides_ip_of_all_endpoint_sources() {
        let eps = discover_with_gateway(Some("203.0.113.10")).await;
        let sources: Vec<&str> = eps.iter().map(|e| e.source.as_str()).collect();
        assert!(
            sources.contains(&"apisix")
                && sources.contains(&"ingress")
                && sources.contains(&"service")
        );
        assert!(eps.iter().all(|e| e.ip == "203.0.113.10"), "{eps:?}");
    }

    #[tokio::test]
    async fn no_gateway_address_keeps_lb_and_fallback_behavior() {
        let eps = discover_with_gateway(None).await;
        let ip_of = |h: &str| eps.iter().find(|e| e.host == h).unwrap().ip.clone();
        assert_eq!(ip_of("svc.example.test"), "192.0.2.200");
        assert_eq!(ip_of("ing.example.test"), "192.0.2.201");
        assert_eq!(ip_of("apisix.example.test"), "192.0.2.200");
    }

    #[tokio::test]
    async fn invalid_gateway_address_is_ignored_defensively() {
        let eps = discover_with_gateway(Some("203.0.113.10\nevil")).await;
        assert!(eps.iter().all(|e| e.ip != "203.0.113.10\nevil"));
        assert!(eps.iter().any(|e| e.ip == "192.0.2.200"));
    }

    #[test]
    fn parsers_extract_backend_service_and_ignore_cross_namespace_gateway_refs() {
        let svc_val: serde_json::Value = serde_json::from_str(
            r#"{
                "items": [{
                    "metadata": {
                        "name": "argocd-server",
                        "namespace": "argocd",
                        "annotations": { "external-dns.alpha.kubernetes.io/hostname": "argocd.local.internal" }
                    },
                    "spec": { "type": "LoadBalancer" },
                    "status": { "loadBalancer": { "ingress": [{ "ip": "192.0.2.10" }] } }
                }]
            }"#,
        )
        .unwrap();
        let svc_eps = parse_services(&svc_val).endpoints;
        assert_eq!(svc_eps.len(), 1);
        assert_eq!(
            svc_eps[0].backend_service.as_deref(),
            Some("argocd/argocd-server")
        );
        assert_eq!(
            svc_eps[0].id,
            "service:argocd/argocd-server:argocd.local.internal"
        );

        let ing_val: serde_json::Value = serde_json::from_str(
            r#"{
                "items": [{
                    "metadata": { "name": "harbor-ingress", "namespace": "harbor" },
                    "spec": {
                        "rules": [{
                            "host": "harbor.local.internal",
                            "http": {
                                "paths": [
                                    { "path": "/", "backend": { "service": { "name": "harbor-portal" } } },
                                    { "path": "/api/", "backend": { "service": { "name": "harbor-core" } } }
                                ]
                            }
                        }]
                    }
                }]
            }"#,
        )
        .unwrap();
        let ing_eps = parse_ingresses(&ing_val, Some("192.0.2.10"));
        assert_eq!(ing_eps.len(), 1);
        assert_eq!(
            ing_eps[0].backend_service.as_deref(),
            Some("harbor/harbor-core")
        );

        let apisix_val: serde_json::Value = serde_json::from_str(
            r#"{
                "items": [{
                    "metadata": { "name": "grafana-route", "namespace": "monitoring" },
                    "spec": {
                        "http": [{
                            "match": { "hosts": ["grafana.local.internal"] },
                            "backends": [{ "serviceName": "kube-prom-grafana" }]
                        }]
                    }
                }]
            }"#,
        )
        .unwrap();
        let apisix_eps = parse_apisix_routes(
            &apisix_val,
            &serde_json::json!({"items": []}),
            &HashMap::new(),
            Some("192.0.2.10"),
        );
        assert_eq!(apisix_eps.len(), 1);
        assert_eq!(
            apisix_eps[0].backend_service.as_deref(),
            Some("monitoring/kube-prom-grafana")
        );

        let gw_val: serde_json::Value = serde_json::from_str(
            r#"{
                "items": [
                    {
                        "metadata": { "name": "kc-route", "namespace": "iam" },
                        "spec": {
                            "hostnames": ["keycloak.local.internal"],
                            "rules": [{ "backendRefs": [{ "name": "keycloak" }] }]
                        }
                    },
                    {
                        "metadata": { "name": "cross-ns-route", "namespace": "tenant" },
                        "spec": {
                            "hostnames": ["spoof.local.internal"],
                            "rules": [{ "backendRefs": [{ "name": "argocd-server", "namespace": "argocd" }] }]
                        }
                    }
                ]
            }"#,
        )
        .unwrap();
        let gw_eps = parse_gateway_httproutes(&gw_val, Some("192.0.2.10"));
        assert_eq!(gw_eps.len(), 2);
        assert_eq!(gw_eps[0].backend_service.as_deref(), Some("iam/keycloak"));
        assert_eq!(gw_eps[1].backend_service, None);

        // Legacy endpoints.json entry without `id` or `backend_service` deserializes cleanly.
        let legacy: DiscoveredEndpoint = serde_json::from_str(
            r#"{"host":"old.local.internal","ip":"192.0.2.10","source":"ingress","resource_name":"ns/ing"}"#,
        )
        .unwrap();
        assert_eq!(legacy.backend_service, None);
        assert_eq!(legacy.backend_upstream, None);
        assert_eq!(legacy.endpoint_id(), "ingress:ns/ing:old.local.internal");
    }
    #[test]
    fn apisix_upstream_external_nodes_require_exact_service_dns() {
        use serde_json::json;
        for name in [
            "argocd-server.argocd.svc",
            "argocd-server.argocd.svc.cluster.local",
        ] {
            assert_eq!(
                apisix_upstream_service(&json!({"spec": {"externalNodes": [
                    {"type": "Domain", "name": name, "port": 80}
                ]}}))
                .as_deref(),
                Some("argocd/argocd-server")
            );
        }
        let invalid = [
            json!([]),
            json!([{"type": "IP", "name": "192.0.2.1"}]),
            json!([{"type": "Domain", "name": "argocd-server.argocd.svc.evil.com"}]),
            json!([{"type": "Domain", "name": "argocd-server.argocd.svc"},
                {"type": "Domain", "name": "grafana.monitoring.svc"}]),
            json!([{"type": "Domain", "name": "argocd-server.argocd.svc"},
                {"type": "IP", "name": "192.0.2.1"}]),
            json!([{"type": "Domain", "name": "Argo.argocd.svc"}]),
            json!([{"type": "Domain", "name": "argo.bad_ns.svc"}]),
            json!([{"type": "Domain", "name": "-argo.argocd.svc"}]),
            json!([{"type": "Domain", "name": "argo.argocd.svc.cluster.local."}]),
            json!([{"type": "Domain", "name": "192.0.2.1"}]),
            json!([{"type": "Domain", "name": format!("{}.argocd.svc", "a".repeat(64))}]),
        ];
        for nodes in invalid {
            assert_eq!(
                apisix_upstream_service(&json!({"spec": {"externalNodes": nodes}})),
                None
            );
        }
        let node = json!({"type": "Domain", "name": "argocd-server.argocd.svc"});
        assert_eq!(
            apisix_upstream_service(&json!({"spec": {"externalNodes": [node, node]}})).as_deref(),
            Some("argocd/argocd-server")
        );
    }

    #[test]
    fn apisix_service_name_multi_path_keeps_namespace_local_registry_selection() {
        let routes = serde_json::json!({"items": [{
            "metadata": {"namespace": "devtools", "name": "registry"},
            "spec": {"http": [
                {"match": {"hosts": ["registry.example.test"]}, "backends": [{"serviceName": "harbor-portal"}]},
                {"match": {"hosts": ["registry.example.test"]}, "backends": [{"serviceName": "harbor-core"}]}
            ]}
        }]});
        let eps = parse_apisix_routes(
            &routes,
            &serde_json::json!({"items": []}),
            &HashMap::new(),
            Some("192.0.2.10"),
        );
        assert_eq!(
            eps[0].backend_service.as_deref(),
            Some("devtools/harbor-core")
        );
    }

    struct UpstreamRunner {
        upstream_namespace: &'static str,
        fail: bool,
        calls: std::sync::Mutex<Vec<String>>,
    }

    #[async_trait::async_trait]
    impl CommandRunner for UpstreamRunner {
        async fn run(
            &self,
            bin: &str,
            args: &[String],
        ) -> Result<crate::services::process::CommandOutput, String> {
            assert_eq!(bin, "kubectl");
            let path = args.last().unwrap();
            self.calls.lock().unwrap().push(path.clone());
            let val = match path.as_str() {
                "/apis/apisix.apache.org/v2/apisixroutes" => serde_json::json!({"items": [{
                    "metadata": {"namespace": "platform-system", "name": "admin-route"},
                    "spec": {"http": [{"match": {"hosts": ["admin.example.test"]},
                        "upstreams": [{"name": "admin-upstream"}]}]}
                }]}),
                "/apis/apisix.apache.org/v2/apisixupstreams" => {
                    if self.fail {
                        return Err("lookup unavailable".into());
                    }
                    serde_json::json!({"items": [{
                        "metadata": {"namespace": self.upstream_namespace, "name": "admin-upstream"},
                        "spec": {"externalNodes": [{"type": "Domain", "name": "argocd-server.argocd.svc.cluster.local", "port": 80}]}
                    }]})
                }
                _ => serde_json::json!({"items": []}),
            };
            Ok(crate::services::process::CommandOutput {
                stdout: val.to_string(),
                stderr: String::new(),
                success: true,
            })
        }

        async fn run_bounded(
            &self,
            bin: &str,
            args: &[String],
            limits: CommandLimits,
        ) -> Result<crate::services::process::CommandOutput, String> {
            assert_eq!(limits.timeout, std::time::Duration::from_secs(10));
            assert_eq!(limits.max_output_bytes, 2 * 1024 * 1024);
            self.run(bin, args).await
        }
    }

    #[tokio::test]
    async fn discovery_apisix_upstream_binds_login_to_service_namespace() {
        for (namespace, fail, resolves) in [
            ("platform-system", false, true),
            ("other", false, false),
            ("platform-system", true, false),
        ] {
            let runner = UpstreamRunner {
                upstream_namespace: namespace,
                fail,
                calls: Default::default(),
            };
            let result = discover_cluster_endpoints_detailed(
                &runner,
                Path::new("unused-fixture"),
                Some("192.0.2.10"),
                None,
            )
            .await
            .unwrap();
            assert_eq!(result.endpoints.len(), 1);
            let ep = &result.endpoints[0];
            assert_eq!(ep.resource_name, "platform-system/admin-route");
            let options =
                crate::services::service_login::endpoint_login_options_for_slice(&result.endpoints);
            if resolves {
                assert_eq!(ep.backend_service.as_deref(), Some("argocd/argocd-server"));
                assert_eq!(
                    ep.backend_upstream.as_deref(),
                    Some("platform-system/admin-upstream")
                );
                assert_eq!(options.len(), 1);
                assert_eq!(options[0].namespace, "argocd");
                assert_eq!(options[0].secret_name, "argocd-initial-admin-secret");
            } else {
                assert_eq!(ep.backend_service, None);
                assert!(options.is_empty());
            }
            assert_eq!(result.failed_kinds.contains(&"apisix".to_string()), fail);
            assert_eq!(
                runner
                    .calls
                    .lock()
                    .unwrap()
                    .iter()
                    .filter(|p| p.ends_with("/apisixupstreams"))
                    .count(),
                1
            );
        }
    }

    /// kubectl cannot reach the API server (macOS Local Network Privacy in the app bundle);
    /// only system curl can answer, using the kubeconfig's own CA and token.
    struct LnpUpstreamRunner {
        inner: UpstreamRunner,
        curl_calls: std::sync::Mutex<Vec<Vec<String>>>,
    }

    #[async_trait::async_trait]
    impl CommandRunner for LnpUpstreamRunner {
        async fn run(
            &self,
            bin: &str,
            args: &[String],
        ) -> Result<crate::services::process::CommandOutput, String> {
            if bin == "curl" {
                self.curl_calls.lock().unwrap().push(args.to_vec());
                let url = args.last().unwrap().clone();
                let path = url.strip_prefix("https://192.0.2.50:6443").unwrap();
                let mut kubectl_args = args.to_vec();
                kubectl_args.push(path.to_string());
                return self.inner.run("kubectl", &kubectl_args).await;
            }
            if args.last().unwrap().ends_with("/apisixupstreams") {
                return Err("kubectl: dial tcp: no route to host".into());
            }
            self.inner.run(bin, args).await
        }
    }

    struct Real401Runner {
        inner: UpstreamRunner,
        curl_calls: std::sync::Mutex<usize>,
    }

    #[async_trait::async_trait]
    impl CommandRunner for Real401Runner {
        async fn run(
            &self,
            bin: &str,
            args: &[String],
        ) -> Result<crate::services::process::CommandOutput, String> {
            if bin == "curl" {
                *self.curl_calls.lock().unwrap() += 1;
            }
            if args.last().unwrap().ends_with("/apisixupstreams") {
                return Ok(crate::services::process::CommandOutput {
                    stdout: String::new(),
                    stderr: "error: You must be logged in to the server (the server has asked for the client to provide credentials)".into(),
                    success: false,
                });
            }
            self.inner.run(bin, args).await
        }
    }

    #[test]
    fn real_kubectl_401_message_is_an_api_answer() {
        assert!(stderr_is_api_answer(
            "error: You must be logged in to the server (the server has asked for the client to provide credentials)"
        ));
    }

    #[tokio::test]
    async fn upstream_fallback_does_not_retry_real_kubectl_401_through_curl() {
        let kubeconfig = std::env::temp_dir().join(format!(
            "clusterdeck-401-upstream-{}.yaml",
            std::process::id()
        ));
        std::fs::write(
            &kubeconfig,
            "clusters:\n- name: c\n  cluster:\n    server: https://192.0.2.50:6443\nusers:\n- name: u\n  user:\n    token: fake-test-token\n",
        )
        .unwrap();
        let runner = Real401Runner {
            inner: UpstreamRunner {
                upstream_namespace: "platform-system",
                fail: false,
                calls: Default::default(),
            },
            curl_calls: Default::default(),
        };
        let result =
            discover_cluster_endpoints_detailed(&runner, &kubeconfig, Some("192.0.2.10"), None)
                .await
                .unwrap();
        let _ = std::fs::remove_file(&kubeconfig);
        assert!(result.failed_kinds.contains(&"apisix".to_string()));
        assert_eq!(*runner.curl_calls.lock().unwrap(), 0);
    }

    #[test]
    fn pin_current_context_fails_closed_when_unresolvable() {
        let yaml = |s: &str| serde_yaml::from_str::<serde_yaml::Value>(s).unwrap();
        // current-context names a missing context: curl must not fall back to clusters[0].
        assert!(pin_current_context(yaml(
            "current-context: gone\ncontexts: []\nclusters:\n- name: a\n  cluster: {server: 'https://192.0.2.1'}\nusers:\n- name: a\n  user: {token: t}\n"
        ))
        .is_err());
        // No current-context and more than one cluster: ambiguous.
        assert!(
            pin_current_context(yaml("clusters:\n- name: a\n- name: b\nusers:\n- name: a\n"))
                .is_err()
        );
        // No current-context but exactly one cluster and one user: unambiguous.
        assert!(pin_current_context(yaml("clusters:\n- name: a\nusers:\n- name: a\n")).is_ok());
    }

    #[tokio::test]
    async fn discovery_upstream_list_falls_back_to_curl_when_kubectl_fails() {
        let kubeconfig = std::env::temp_dir().join(format!(
            "clusterdeck-lnp-upstream-{}.yaml",
            std::process::id()
        ));
        std::fs::write(
            &kubeconfig,
            "clusters:\n- name: c\n  cluster:\n    server: https://192.0.2.50:6443\nusers:\n- name: u\n  user:\n    token: fake-test-token\n",
        )
        .unwrap();
        let runner = LnpUpstreamRunner {
            inner: UpstreamRunner {
                upstream_namespace: "platform-system",
                fail: false,
                calls: Default::default(),
            },
            curl_calls: Default::default(),
        };
        let result =
            discover_cluster_endpoints_detailed(&runner, &kubeconfig, Some("192.0.2.10"), None)
                .await
                .unwrap();
        let _ = std::fs::remove_file(&kubeconfig);
        assert_eq!(
            result.endpoints[0].backend_service.as_deref(),
            Some("argocd/argocd-server")
        );
        assert!(!result.failed_kinds.contains(&"apisix".to_string()));
        let curls = runner.curl_calls.lock().unwrap();
        assert!(curls
            .iter()
            .any(|a| a.last().unwrap().ends_with("/apisixupstreams")));
        assert!(curls
            .iter()
            .flatten()
            .all(|a| a != "-k" && a != "--insecure"));
    }
}
