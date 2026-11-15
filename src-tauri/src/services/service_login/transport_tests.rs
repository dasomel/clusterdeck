use super::tests::{json_output, secret_json, seed_profile_and_endpoints, temp_paths};
use super::*;
use crate::services::process::CommandOutput;
use base64::prelude::*;
use std::sync::Mutex;

/// Models the in-app macOS Local Network Privacy failure: kubectl cannot reach the API
/// server, system curl can. `kubectl` overrides apply per sub-command.
struct LnpRunner {
    calls: Mutex<Vec<(String, Vec<String>)>>,
    kubectl: Result<CommandOutput, String>,
    kubectl_secret: Option<Result<CommandOutput, String>>,
    curl_secret: String,
    curl_ssar: String,
    /// Content of each `-H @file` header file, read while the child "runs".
    header_files: Mutex<Vec<String>>,
}

fn ok(stdout: &str) -> Result<CommandOutput, String> {
    Ok(CommandOutput {
        stdout: stdout.to_string(),
        stderr: String::new(),
        success: true,
    })
}

fn no_route() -> Result<CommandOutput, String> {
    Ok(CommandOutput {
        stdout: String::new(),
        stderr: "dial tcp 192.0.2.50:6443: connect: no route to host".into(),
        success: false,
    })
}

fn allowed(value: bool) -> String {
    serde_json::json!({"kind": "SelfSubjectAccessReview", "status": {"allowed": value}}).to_string()
}

fn status(code: u16) -> String {
    serde_json::json!({"kind": "Status", "status": "Failure", "code": code}).to_string()
}

impl LnpRunner {
    fn new(curl_secret: String) -> Self {
        Self {
            calls: Mutex::new(Vec::new()),
            kubectl: no_route(),
            kubectl_secret: None,
            curl_secret,
            curl_ssar: allowed(true),
            header_files: Mutex::new(Vec::new()),
        }
    }

    fn calls(&self, bin: &str) -> Vec<Vec<String>> {
        let calls = self.calls.lock().unwrap();
        calls
            .iter()
            .filter(|(b, _)| b == bin)
            .map(|(_, a)| a.clone())
            .collect()
    }
}

#[async_trait::async_trait]
impl CommandRunner for LnpRunner {
    async fn run(&self, bin: &str, args: &[String]) -> Result<CommandOutput, String> {
        self.calls
            .lock()
            .unwrap()
            .push((bin.to_string(), args.to_vec()));
        if bin == "kubectl" {
            // Reachable-kubectl mode: only the secret GET is overridden.
            if let Some(res) = &self.kubectl_secret {
                return if args.iter().any(|a| a == "secret") {
                    res.clone()
                } else if args.iter().any(|a| a == "can-i") {
                    ok("yes")
                } else {
                    json_output(serde_json::json!({"spec": {"type": "ClusterIP"}}))
                };
            }
            return self.kubectl.clone();
        }
        if let Some(file) = args
            .windows(2)
            .find(|w| w[0] == "-H" && w[1].starts_with('@'))
            .map(|w| w[1][1..].to_string())
        {
            let content = std::fs::read_to_string(file).unwrap_or_default();
            self.header_files.lock().unwrap().push(content);
        }
        let url = args.last().unwrap();
        if url.ends_with("/selfsubjectaccessreviews") {
            ok(&self.curl_ssar)
        } else if url.contains("/secrets/") {
            ok(&self.curl_secret)
        } else {
            json_output(serde_json::json!({"spec": {"type": "ClusterIP"}}))
        }
    }

    async fn run_bounded(
        &self,
        bin: &str,
        args: &[String],
        limits: CommandLimits,
    ) -> Result<CommandOutput, String> {
        assert_eq!(limits.timeout, KUBECTL_LOGIN_TIMEOUT);
        assert_eq!(limits.max_output_bytes, KUBECTL_LOGIN_MAX_OUTPUT_BYTES);
        self.run(bin, args).await
    }
}

/// The first cluster/user belong to another context, so curl only targets the pinned one if
/// the snapshot is reduced to the selected context.
fn lnp_kubeconfig(user_extra: &str) -> String {
    let ca = BASE64_STANDARD.encode(b"fake-ca-pem");
    format!(
        "apiVersion: v1
current-context: lab-ctx
contexts:
- name: other
  context: {{cluster: c2, user: u2}}
- name: lab-ctx
  context: {{cluster: c, user: u}}
clusters:
- name: c2
  cluster: {{server: 'https://203.0.113.9:6443'}}
- name: c
  cluster:
    server: https://192.0.2.50:6443
    certificate-authority-data: {ca}
users:
- name: u2
  user: {{token: other-token}}
- name: u
  user:
{user_extra}
"
    )
}

const TOKEN_USER: &str = "    token: fake-test-token";

fn endpoint() -> DiscoveredEndpoint {
    DiscoveredEndpoint {
        id: "ep".into(),
        host: "grafana.example.test".into(),
        ip: "192.0.2.10".into(),
        source: "ingress".into(),
        resource_name: "monitoring/route".into(),
        backend_service: Some("monitoring/grafana".into()),
        backend_upstream: None,
    }
}

fn setup(tag: &str, user_extra: &str) -> (ClusterDeckPaths, DiscoveredEndpoint) {
    let paths = temp_paths(tag);
    let ep = endpoint();
    seed_profile_and_endpoints(&paths, "lab", &lnp_kubeconfig(user_extra), vec![ep.clone()]);
    (paths, ep)
}

#[tokio::test]
async fn kubectl_transport_failure_reveals_through_pinned_curl() {
    let (paths, ep) = setup("lnp-ok", TOKEN_USER);
    let runner = LnpRunner::new(secret_json(&[
        ("admin-user", "graf-admin"),
        ("admin-password", "fake-graf-pw"),
    ]));
    let login = reveal_endpoint_login_with_deps(&runner, &paths, "lab", &ep.id)
        .await
        .unwrap();
    assert_eq!(login.username.as_deref(), Some("graf-admin"));
    assert_eq!(login.password.expose_for_test(), "fake-graf-pw");

    let curls = runner.calls("curl");
    assert_eq!(curls.len(), 3, "service, SSAR, secret");
    for args in &curls {
        let joined = args.join(" ");
        assert!(joined.contains("--cacert"));
        assert!(!args.iter().any(|a| a == "-k" || a == "--insecure"));
        assert!(args.last().unwrap().starts_with("https://192.0.2.50:6443/"));
        assert_eq!(args[0], "-q", "-q must precede every other curl argument");
        assert!(
            !joined.contains("fake-test-token"),
            "token must not be in argv"
        );
        assert!(!joined.contains("other-token") && !joined.contains("203.0.113.9"));
    }
    let headers = runner.header_files.lock().unwrap().clone();
    assert_eq!(headers.len(), 3);
    assert!(headers
        .iter()
        .all(|h| h.contains("Authorization: Bearer fake-test-token")));
    let ssar = curls
        .iter()
        .find(|a| a.last().unwrap().ends_with("/selfsubjectaccessreviews"))
        .unwrap();
    assert!(ssar.windows(2).any(|w| w == ["-X", "POST"]));
    let body: serde_json::Value =
        serde_json::from_str(&ssar[ssar.iter().position(|a| a == "-d").unwrap() + 1]).unwrap();
    assert_eq!(
        body["spec"]["resourceAttributes"],
        serde_json::json!({"namespace": "monitoring", "verb": "get",
            "resource": "secrets", "name": "grafana"})
    );
    let _ = std::fs::remove_dir_all(&paths.base);
}

#[tokio::test]
async fn curl_status_bodies_map_to_fixed_codes_without_echoing_output() {
    for (body, expected) in [
        (status(403), LoginErrorCode::Forbidden),
        (status(404), LoginErrorCode::NotFound),
        (status(500), LoginErrorCode::KubectlFailed),
        (
            "LEAK_MARKER not json".to_string(),
            LoginErrorCode::KubectlFailed,
        ),
    ] {
        let (paths, ep) = setup("lnp-status", TOKEN_USER);
        let runner = LnpRunner::new(body);
        let err = reveal_endpoint_login_with_deps(&runner, &paths, "lab", &ep.id)
            .await
            .unwrap_err();
        assert_eq!(err, expected);
        assert!(!format!("{err:?} {err}").contains("LEAK_MARKER"));
        let _ = std::fs::remove_dir_all(&paths.base);
    }
}

#[tokio::test]
async fn authoritative_kubectl_forbidden_is_not_retried_through_curl() {
    let (paths, ep) = setup("lnp-authoritative", TOKEN_USER);
    let mut runner = LnpRunner::new(secret_json(&[("admin-password", "must-not-appear")]));
    runner.kubectl_secret = Some(Ok(CommandOutput {
        stdout: String::new(),
        stderr: "Error from server (Forbidden): secrets \"grafana\" is forbidden".into(),
        success: false,
    }));
    let err = reveal_endpoint_login_with_deps(&runner, &paths, "lab", &ep.id)
        .await
        .unwrap_err();
    assert_eq!(err, LoginErrorCode::Forbidden);
    assert!(runner.calls("curl").is_empty());
    let _ = std::fs::remove_dir_all(&paths.base);
}

#[tokio::test]
async fn ssar_over_curl_allows_or_denies_the_check() {
    for (allow, expected) in [(true, Ok(())), (false, Err(LoginErrorCode::Forbidden))] {
        let (paths, ep) = setup("lnp-ssar", TOKEN_USER);
        let mut runner = LnpRunner::new(secret_json(&[]));
        runner.curl_ssar = allowed(allow);
        let result = check_endpoint_login_with_deps(&runner, &paths, "lab", &ep.id)
            .await
            .map(|_| ());
        assert_eq!(result, expected);
        let _ = std::fs::remove_dir_all(&paths.base);
    }
}

#[tokio::test]
async fn exec_plugin_user_keeps_the_kubectl_error_and_never_calls_curl() {
    let exec_user = "    exec: {apiVersion: client.authentication.k8s.io/v1, command: fake-plugin}";
    for (kubectl, expected) in [
        (no_route(), LoginErrorCode::KubectlFailed),
        (
            Err("command timed out".to_string()),
            LoginErrorCode::Timeout,
        ),
    ] {
        let (paths, ep) = setup("lnp-exec", exec_user);
        let mut runner = LnpRunner::new(secret_json(&[]));
        runner.kubectl = kubectl;
        let err = reveal_endpoint_login_with_deps(&runner, &paths, "lab", &ep.id)
            .await
            .unwrap_err();
        assert_eq!(err, expected);
        assert!(runner.calls("curl").is_empty());
        let _ = std::fs::remove_dir_all(&paths.base);
    }
}

#[tokio::test]
async fn authoritative_kubectl_unauthorized_is_not_retried_through_curl() {
    let (paths, ep) = setup("lnp-unauthorized", TOKEN_USER);
    let mut runner = LnpRunner::new(secret_json(&[("admin-password", "must-not-appear")]));
    runner.kubectl_secret = Some(Ok(CommandOutput {
        stdout: String::new(),
        stderr: "error: You must be logged in to the server (Unauthorized)".into(),
        success: false,
    }));
    let err = reveal_endpoint_login_with_deps(&runner, &paths, "lab", &ep.id)
        .await
        .unwrap_err();
    assert_eq!(err, LoginErrorCode::KubectlFailed);
    assert!(runner.calls("curl").is_empty());
    let _ = std::fs::remove_dir_all(&paths.base);
}

const REAL_401: &str = "error: You must be logged in to the server (the server has asked for the client to provide credentials)";

fn real_401() -> Result<CommandOutput, String> {
    Ok(CommandOutput {
        stdout: String::new(),
        stderr: REAL_401.into(),
        success: false,
    })
}

#[tokio::test]
async fn real_kubectl_401_message_on_get_is_not_retried_through_curl() {
    let (paths, ep) = setup("lnp-401-get", TOKEN_USER);
    let mut runner = LnpRunner::new(secret_json(&[("admin-password", "must-not-appear")]));
    runner.kubectl_secret = Some(real_401());
    let err = reveal_endpoint_login_with_deps(&runner, &paths, "lab", &ep.id)
        .await
        .unwrap_err();
    assert_eq!(err, LoginErrorCode::KubectlFailed);
    assert!(runner.calls("curl").is_empty());
    let _ = std::fs::remove_dir_all(&paths.base);
}

#[tokio::test]
async fn real_kubectl_401_message_on_can_i_is_not_retried_through_curl() {
    let (paths, ep) = setup("lnp-401-cani", TOKEN_USER);
    let mut runner = LnpRunner::new(secret_json(&[("admin-password", "must-not-appear")]));
    runner.kubectl = real_401();
    let err = reveal_endpoint_login_with_deps(&runner, &paths, "lab", &ep.id)
        .await
        .unwrap_err();
    assert_eq!(err, LoginErrorCode::KubectlFailed);
    assert!(runner.calls("curl").is_empty());
    let _ = std::fs::remove_dir_all(&paths.base);
}

#[tokio::test]
async fn token_file_takes_precedence_over_inline_token_like_kubectl() {
    let token_file = std::env::temp_dir().join(format!("cd_test_tokenfile_{}", std::process::id()));
    std::fs::write(&token_file, "file-token\n").unwrap();
    let user = format!(
        "    token: fake-test-token\n    tokenFile: {}",
        token_file.display()
    );
    let (paths, ep) = setup("lnp-tokenfile", &user);
    let runner = LnpRunner::new(secret_json(&[("admin-password", "fake-graf-pw")]));
    reveal_endpoint_login_with_deps(&runner, &paths, "lab", &ep.id)
        .await
        .unwrap();
    let headers = runner.header_files.lock().unwrap().clone();
    assert_eq!(headers.len(), 3);
    assert!(headers.iter().all(|h| h.contains("Bearer file-token")));
    assert!(!headers.iter().any(|h| h.contains("fake-test-token")));
    let _ = std::fs::remove_file(&token_file);
    let _ = std::fs::remove_dir_all(&paths.base);
}
