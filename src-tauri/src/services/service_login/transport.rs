use std::path::Path;

use super::{LoginErrorCode, KUBECTL_LOGIN_LIMITS};
use crate::services::k8s_endpoints::{curl_k8s_api_value, stderr_says_unauthorized, CurlOptions};
use crate::services::process::{CommandLimits, CommandOutput, CommandRunner};
use crate::services::validate::is_safe_k8s_dns_label;

// D1 (amended): kubectl first; system curl only when kubectl failed at the transport level,
// against the same pinned snapshot and the same credential. macOS Local Network Privacy blocks
// third-party binaries (kubectl) spawned by the app but exempts /usr/bin/curl. Cost: a second
// request on transport failure; escape: delete this module, nothing else depends on it.

fn kubectl_error(error: &str) -> LoginErrorCode {
    if error == "command timed out" {
        LoginErrorCode::Timeout
    } else {
        LoginErrorCode::KubectlFailed
    }
}

/// An API server answer relayed on kubectl's stderr is authoritative; retrying it through
/// curl could only mask a denial.
pub(super) fn authoritative_stderr(stderr: &str) -> Option<LoginErrorCode> {
    if stderr_says_unauthorized(stderr) {
        // Bad credentials are the server's verdict; curl would present the same ones.
        Some(LoginErrorCode::KubectlFailed)
    } else if stderr.contains("Forbidden") || stderr.contains("forbidden") {
        Some(LoginErrorCode::Forbidden)
    } else if stderr.contains("NotFound") || stderr.contains("not found") {
        Some(LoginErrorCode::NotFound)
    } else {
        None
    }
}

/// Builds a minified kubeconfig for `context` in memory so curl (which reads the first
/// cluster/user) uses exactly what kubectl would. `None` when the context, its cluster or its
/// user is missing, or the user relies on an exec/auth-provider plugin or carries no
/// certificate/token curl can present -- curl would go anonymous, so kubectl's error stands.
fn pinned_config(kubeconfig: &Path, context: &str) -> Option<serde_yaml::Value> {
    let value: serde_yaml::Value = serde_yaml::from_slice(&std::fs::read(kubeconfig).ok()?).ok()?;
    let ctx = value
        .get("contexts")?
        .as_sequence()?
        .iter()
        .find(|entry| entry.get("name").and_then(|n| n.as_str()) == Some(context))?
        .get("context")?;
    let pick = |list: &str, key: &str| {
        let name = ctx.get(key)?.as_str()?;
        value
            .get(list)?
            .as_sequence()?
            .iter()
            .find(|entry| entry.get("name").and_then(|n| n.as_str()) == Some(name))
            .cloned()
    };
    let cluster = pick("clusters", "cluster")?;
    let user = pick("users", "user")?;
    let user_obj = user.get("user")?;
    let has = |key: &str| user_obj.get(key).is_some_and(|v| !v.is_null());
    let usable = has("token")
        || has("tokenFile")
        || (has("client-certificate-data") && has("client-key-data"))
        || (has("client-certificate") && has("client-key"));
    if has("exec") || has("auth-provider") || !usable {
        return None;
    }
    let mut pinned = serde_yaml::Mapping::new();
    pinned.insert(
        "clusters".into(),
        serde_yaml::Value::Sequence(vec![cluster]),
    );
    pinned.insert("users".into(), serde_yaml::Value::Sequence(vec![user]));
    Some(serde_yaml::Value::Mapping(pinned))
}

/// Runs curl against the API. `Err(KubectlFailed)` means "curl could not answer" (the caller
/// then reports kubectl's own code); other errors are the server's authoritative Status.
async fn curl_json(
    runner: &dyn CommandRunner,
    kubeconfig: &Path,
    context: &str,
    api_path: &str,
    post_json: Option<&str>,
    limits: CommandLimits,
) -> Result<serde_json::Value, LoginErrorCode> {
    let config = pinned_config(kubeconfig, context).ok_or(LoginErrorCode::KubectlFailed)?;
    let options = CurlOptions {
        post_json,
        limits: Some(limits),
    };
    let stdout = curl_k8s_api_value(runner, &config, api_path, options)
        .await
        .map_err(|_| LoginErrorCode::KubectlFailed)?;
    let value: serde_json::Value =
        serde_json::from_str(&stdout).map_err(|_| LoginErrorCode::KubectlFailed)?;
    if value.get("kind").and_then(|k| k.as_str()) == Some("Status") {
        return Err(match value.get("code").and_then(|c| c.as_u64()) {
            Some(403) => LoginErrorCode::Forbidden,
            Some(404) => LoginErrorCode::NotFound,
            _ => LoginErrorCode::KubectlFailed,
        });
    }
    Ok(value)
}

fn after_kubectl_failure(
    curl: Result<serde_json::Value, LoginErrorCode>,
    kubectl: LoginErrorCode,
) -> Result<serde_json::Value, LoginErrorCode> {
    match curl {
        Err(LoginErrorCode::KubectlFailed) => Err(kubectl),
        other => other,
    }
}

/// `kubectl get <kubectl_get_args> -o json`, falling back to `GET api_path` via curl.
/// `api_path` must be built from DNS-label-validated namespace/name by the caller.
pub(super) async fn get_json(
    runner: &dyn CommandRunner,
    kubeconfig: &Path,
    context: &str,
    kubectl_get_args: &[&str],
    api_path: &str,
) -> Result<serde_json::Value, LoginErrorCode> {
    get_json_limited(
        runner,
        kubeconfig,
        context,
        kubectl_get_args,
        api_path,
        KUBECTL_LOGIN_LIMITS,
    )
    .await
}

/// `get_json` with a caller-chosen output bound, for reads that cannot carry secret data and
/// may be larger than a single object (pod lists).
pub(super) async fn get_json_limited(
    runner: &dyn CommandRunner,
    kubeconfig: &Path,
    context: &str,
    kubectl_get_args: &[&str],
    api_path: &str,
    limits: CommandLimits,
) -> Result<serde_json::Value, LoginErrorCode> {
    let mut args = vec![
        "--kubeconfig",
        kubeconfig.to_str().ok_or(LoginErrorCode::KubectlFailed)?,
        "--context",
        context,
        "get",
    ];
    args.extend_from_slice(kubectl_get_args);
    // `get --raw` already prints the API body and rejects `-o`.
    if kubectl_get_args.first() != Some(&"--raw") {
        args.extend(["-o", "json"]);
    }
    let args: Vec<String> = args.into_iter().map(str::to_string).collect();
    let failure = match runner.run_bounded("kubectl", &args, limits.clone()).await {
        Ok(out) if out.success => {
            return serde_json::from_str(&out.stdout).map_err(|_| LoginErrorCode::KubectlFailed)
        }
        Ok(out) => {
            if let Some(code) = authoritative_stderr(&out.stderr) {
                return Err(code);
            }
            LoginErrorCode::KubectlFailed
        }
        Err(error) => kubectl_error(&error),
    };
    after_kubectl_failure(
        curl_json(runner, kubeconfig, context, api_path, None, limits).await,
        failure,
    )
}

/// `kubectl auth can-i get secret/<name> -n <ns>`, falling back to a SelfSubjectAccessReview.
pub(super) async fn can_i_get_secret(
    runner: &dyn CommandRunner,
    kubeconfig: &Path,
    context: &str,
    namespace: &str,
    secret_name: &str,
) -> Result<(), LoginErrorCode> {
    if !is_safe_k8s_dns_label(namespace) || !is_safe_k8s_dns_label(secret_name) {
        return Err(LoginErrorCode::KubectlFailed);
    }
    let kc_str = kubeconfig.to_str().ok_or(LoginErrorCode::KubectlFailed)?;
    let args = [
        "--kubeconfig",
        kc_str,
        "--context",
        context,
        "auth",
        "can-i",
        "get",
        &format!("secret/{secret_name}"),
        "-n",
        namespace,
    ]
    .map(str::to_string);
    let failure = match runner
        .run_bounded("kubectl", &args, KUBECTL_LOGIN_LIMITS)
        .await
    {
        Ok(out) => match can_i_answer(&out) {
            Some(answer) => return answer,
            None => LoginErrorCode::KubectlFailed,
        },
        Err(error) => kubectl_error(&error),
    };
    let body = serde_json::json!({"apiVersion": "authorization.k8s.io/v1",
        "kind": "SelfSubjectAccessReview",
        "spec": {"resourceAttributes": {"namespace": namespace, "verb": "get",
            "resource": "secrets", "name": secret_name}}})
    .to_string();
    let review = after_kubectl_failure(
        curl_json(
            runner,
            kubeconfig,
            context,
            "/apis/authorization.k8s.io/v1/selfsubjectaccessreviews",
            Some(&body),
            KUBECTL_LOGIN_LIMITS,
        )
        .await,
        failure,
    )?;
    if review.pointer("/status/allowed").and_then(|v| v.as_bool()) == Some(true) {
        Ok(())
    } else if review.pointer("/status/allowed").is_some() {
        Err(LoginErrorCode::Forbidden)
    } else {
        Err(failure)
    }
}

/// `Some` when kubectl produced an authoritative answer, `None` when it failed in transit.
fn can_i_answer(out: &CommandOutput) -> Option<Result<(), LoginErrorCode>> {
    let answer = out.stdout.trim();
    if out.success && answer == "yes" {
        return Some(Ok(()));
    }
    if stderr_says_unauthorized(&out.stderr) {
        return Some(Err(LoginErrorCode::KubectlFailed));
    }
    if answer == "no"
        || out.success
        || out.stderr.contains("Forbidden")
        || out.stderr.contains("forbidden")
    {
        return Some(Err(LoginErrorCode::Forbidden));
    }
    None
}
