use std::path::Path;
use std::time::Duration;

use super::transport::get_json_limited;
use super::{ServiceLoginSpec, UsernameSource};
use crate::services::process::{CommandLimits, CommandRunner};
use crate::services::validate::{is_safe_k8s_dns_label, is_safe_k8s_secret_key};

// D2 amendment (workload-derived Secret): charts that set an existing admin Secret
// (`admin.existingSecret`, ...) wire it into the backend pod as env `secretKeyRef`s, which is
// the one place the real Secret name is authoritative. Cost: one extra bounded pod-list read
// per action; escape: drop the `workload_env` registry fields and this module, the static
// Secret remains.
const POD_LIST_LIMITS: CommandLimits = CommandLimits {
    timeout: Duration::from_secs(10),
    max_output_bytes: 2 * 1024 * 1024,
};
const POD_LIST_LIMIT: usize = 3;
const MAX_LITERAL_USERNAME: usize = 128;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WorkloadEnv {
    pub username: &'static str,
    pub password: &'static str,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum UserSource {
    Fixed(String),
    SecretKey(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ResolvedLogin {
    pub secret_name: String,
    pub username: UserSource,
    pub password_key: String,
}

impl ResolvedLogin {
    fn from_static(spec: &ServiceLoginSpec) -> Self {
        Self {
            secret_name: spec.secret_name.clone(),
            username: match spec.username {
                UsernameSource::Fixed(u) => UserSource::Fixed(u.to_string()),
                UsernameSource::SecretKey(k) => UserSource::SecretKey(k.to_string()),
            },
            password_key: spec.password_key.to_string(),
        }
    }
}

/// Label selector keys/values come from the live Service, so they are untrusted before they
/// reach an argv or URL: only `[A-Za-z0-9._-]` (plus one `/` in a key prefix) is accepted.
fn selector_string(service: &serde_json::Value) -> Option<String> {
    let selector = service.pointer("/spec/selector")?.as_object()?;
    if selector.is_empty() {
        return None;
    }
    let plain = |s: &str, max: usize| {
        !s.is_empty()
            && s.len() <= max
            && s.bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
    };
    let mut parts = Vec::new();
    for (key, value) in selector {
        let value = value.as_str()?;
        let key_ok = match key.split_once('/') {
            Some((prefix, name)) => plain(prefix, 253) && plain(name, 63),
            None => plain(key, 63),
        };
        if !key_ok || !(value.is_empty() || plain(value, 63)) {
            return None;
        }
        parts.push(format!("{key}={value}"));
    }
    Some(parts.join(","))
}

fn secret_ref(env: &serde_json::Value) -> Option<(String, String)> {
    let reference = env.pointer("/valueFrom/secretKeyRef")?;
    let name = reference.get("name")?.as_str()?;
    let key = reference.get("key")?.as_str()?;
    (is_safe_k8s_dns_label(name) && is_safe_k8s_secret_key(key))
        .then(|| (name.to_string(), key.to_string()))
}

/// P1-1: only a settled, controller-owned pod may vouch for a Secret name; a stray, unready
/// or terminating pod matching the selector could otherwise point at an unrelated Secret.
fn is_qualified(pod: &serde_json::Value) -> bool {
    let ready = pod
        .pointer("/status/conditions")
        .and_then(|c| c.as_array())
        .is_some_and(|conditions| {
            conditions.iter().any(|c| {
                c.get("type").and_then(|t| t.as_str()) == Some("Ready")
                    && c.get("status").and_then(|s| s.as_str()) == Some("True")
            })
        });
    let controlled = pod
        .pointer("/metadata/ownerReferences")
        .and_then(|o| o.as_array())
        .is_some_and(|owners| {
            owners.iter().any(|o| {
                o.get("controller").and_then(|c| c.as_bool()) == Some(true)
                    && matches!(
                        o.get("kind").and_then(|k| k.as_str()),
                        Some("ReplicaSet" | "StatefulSet" | "DaemonSet")
                    )
            })
        });
    pod.pointer("/status/phase").and_then(|p| p.as_str()) == Some("Running")
        && ready
        && controlled
        && pod.pointer("/metadata/deletionTimestamp").is_none()
}

/// Login for one container, plus whether it carries the password Secret reference.
/// P2-2: username and password come from the same container, never mixed across a sidecar.
fn from_container(
    container: &serde_json::Value,
    env: &WorkloadEnv,
    base: &ResolvedLogin,
) -> (ResolvedLogin, bool) {
    let find = |name: &str| {
        container
            .get("env")?
            .as_array()?
            .iter()
            .find(|e| e.get("name").and_then(|n| n.as_str()) == Some(name))
    };
    // A literal `value` is never accepted for the password: only a Secret reference proves
    // where the credential lives, and a literal would be echoed from a pod spec.
    let password = find(env.password).and_then(secret_ref);
    let has_password = password.is_some();
    let user_env = find(env.username);
    let mut login = base.clone();
    if let Some((secret_name, password_key)) = password {
        login.secret_name = secret_name;
        login.password_key = password_key;
    }
    // The username key is only trusted when it lives in the Secret that will be read, which
    // is the static one when the password env is a literal (P2-3).
    if let Some((name, key)) = user_env.and_then(secret_ref) {
        if name == login.secret_name {
            login.username = UserSource::SecretKey(key);
        }
    }
    if let Some(literal) = user_env
        .and_then(|user| user.get("value")?.as_str())
        .filter(|l| {
            !l.is_empty() && l.len() <= MAX_LITERAL_USERNAME && !l.chars().any(char::is_control)
        })
    {
        login.username = UserSource::Fixed(literal.to_string());
    }
    (login, has_password)
}

fn from_pod(pod: &serde_json::Value, env: &WorkloadEnv, base: &ResolvedLogin) -> ResolvedLogin {
    let candidates: Vec<(ResolvedLogin, bool)> = ["containers", "initContainers"]
        .iter()
        .filter_map(|kind| pod.pointer(&format!("/spec/{kind}"))?.as_array())
        .flatten()
        .map(|container| from_container(container, env, base))
        .collect();
    candidates
        .iter()
        .find(|(_, has_password)| *has_password)
        .or_else(|| candidates.iter().find(|(login, _)| login != base))
        .map_or_else(|| base.clone(), |(login, _)| login.clone())
}

/// Secret to read for `spec`: the one the backend workload references when it can be listed
/// and matches, otherwise the static registry Secret. Every failure (forbidden, no pods,
/// oversized, unsafe names) falls back silently; nothing from the pod spec is ever echoed.
pub(super) async fn resolve(
    runner: &dyn CommandRunner,
    kubeconfig: &Path,
    context: &str,
    namespace: &str,
    service: &serde_json::Value,
    spec: &ServiceLoginSpec,
) -> ResolvedLogin {
    let base = ResolvedLogin::from_static(spec);
    let Some(env) = spec.workload_env else {
        return base;
    };
    let Some(selector) = selector_string(service) else {
        return base;
    };
    let encoded = selector
        .replace('/', "%2F")
        .replace('=', "%3D")
        .replace(',', "%2C");
    // One raw URL for kubectl and curl so both are bounded by `limit`.
    let api_path = format!(
        "/api/v1/namespaces/{namespace}/pods?labelSelector={encoded}&limit={POD_LIST_LIMIT}"
    );
    let Ok(pods) = get_json_limited(
        runner,
        kubeconfig,
        context,
        &["--raw", &api_path],
        &api_path,
        POD_LIST_LIMITS,
    )
    .await
    else {
        return base;
    };
    // Qualified pods that disagree (rollout, stray pod) make the workload unreliable: use the
    // static Secret rather than pick one.
    let mut logins = pods
        .get("items")
        .and_then(|items| items.as_array())
        .into_iter()
        .flatten()
        .filter(|pod| is_qualified(pod))
        .map(|pod| from_pod(pod, &env, &base));
    match logins.next() {
        Some(first) if logins.all(|other| other == first) => first,
        _ => base,
    }
}
