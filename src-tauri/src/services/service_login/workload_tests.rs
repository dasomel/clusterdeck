use super::tests::{json_output, secret_json, seed_profile_and_endpoints, temp_paths};
use super::*;
use crate::services::process::CommandOutput;
use std::sync::Mutex;

struct WorkloadRunner {
    calls: Mutex<Vec<Vec<String>>>,
    service: serde_json::Value,
    pods: Result<CommandOutput, String>,
    secrets: Vec<(&'static str, String)>,
    /// Models kubectl being unable to list pods (macOS LNP) so only curl answers.
    kubectl_pods_transport_fails: bool,
    curl_urls: Mutex<Vec<String>>,
}

impl WorkloadRunner {
    fn new(pods: Result<CommandOutput, String>, secrets: Vec<(&'static str, String)>) -> Self {
        Self {
            calls: Mutex::new(Vec::new()),
            service: serde_json::json!({"spec": {"type": "ClusterIP",
                "selector": {"app.kubernetes.io/name": "grafana", "app.kubernetes.io/instance": "prom"}}}),
            pods,
            secrets,
            kubectl_pods_transport_fails: false,
            curl_urls: Mutex::new(Vec::new()),
        }
    }

    fn calls(&self) -> Vec<Vec<String>> {
        self.calls.lock().unwrap().clone()
    }
}

#[async_trait::async_trait]
impl CommandRunner for WorkloadRunner {
    async fn run(&self, bin: &str, args: &[String]) -> Result<CommandOutput, String> {
        if bin == "curl" {
            let url = args.last().unwrap().clone();
            self.curl_urls.lock().unwrap().push(url);
            return self.pods.clone();
        }
        self.calls.lock().unwrap().push(args.to_vec());
        let has = |word: &str| args.iter().any(|a| a == word);
        if has("can-i") {
            Ok(ok("yes\n"))
        } else if args.iter().any(|a| a.contains("/pods?")) {
            if self.kubectl_pods_transport_fails {
                return Err("dial tcp: no route to host".into());
            }
            self.pods.clone()
        } else if has("service") {
            json_output(self.service.clone())
        } else {
            let wanted = args
                .iter()
                .position(|a| a == "secret")
                .map(|i| &args[i + 1]);
            match self
                .secrets
                .iter()
                .find(|(n, _)| Some(&n.to_string()) == wanted)
            {
                Some((_, body)) => Ok(ok(body)),
                None => Ok(CommandOutput {
                    stdout: String::new(),
                    stderr: "Error from server (NotFound): secrets not found".into(),
                    success: false,
                }),
            }
        }
    }

    async fn run_bounded(
        &self,
        bin: &str,
        args: &[String],
        limits: CommandLimits,
    ) -> Result<CommandOutput, String> {
        let pods = bin == "curl" || args.iter().any(|a| a.contains("/pods?"));
        assert_eq!(
            limits.max_output_bytes,
            if pods { 2 * 1024 * 1024 } else { 64 * 1024 }
        );
        self.run(bin, args).await
    }
}

fn ok(stdout: &str) -> CommandOutput {
    CommandOutput {
        stdout: stdout.to_string(),
        stderr: String::new(),
        success: true,
    }
}

fn pods_with_env(env: serde_json::Value) -> Result<CommandOutput, String> {
    json_output(serde_json::json!({"items": [qualified_pod(vec![
        serde_json::json!({"name": "grafana", "env": env})])]}))
}

fn qualified_pod(containers: Vec<serde_json::Value>) -> serde_json::Value {
    serde_json::json!({
        "metadata": {"ownerReferences": [{"kind": "ReplicaSet", "controller": true}]},
        "status": {"phase": "Running", "conditions": [{"type": "Ready", "status": "True"}]},
        "spec": {"initContainers": [], "containers": containers}})
}

fn pods_of(items: Vec<serde_json::Value>) -> Result<CommandOutput, String> {
    json_output(serde_json::json!({ "items": items }))
}

fn grafana_container(secret: &str) -> serde_json::Value {
    serde_json::json!({"name": "grafana", "env": [secret_env(
        "GF_SECURITY_ADMIN_PASSWORD", secret, "admin-password")]})
}

fn secret_env(name: &str, secret: &str, key: &str) -> serde_json::Value {
    serde_json::json!({"name": name, "valueFrom": {"secretKeyRef": {"name": secret, "key": key}}})
}

fn kubeconfig() -> &'static str {
    "apiVersion: v1\ncurrent-context: lab-ctx\ncontexts:\n- name: lab-ctx\n  context:\n    cluster: c\n    user: u\nclusters:\n- name: c\n  cluster:\n    server: https://192.0.2.50:6443\nusers:\n- name: u\n  user:\n    token: fake-test-token\n"
}

fn endpoint(ns: &str, svc: &str) -> DiscoveredEndpoint {
    DiscoveredEndpoint {
        id: DiscoveredEndpoint::compute_id(
            "ingress",
            &format!("{ns}/{svc}-ing"),
            "x.example.internal",
        ),
        host: "x.example.internal".into(),
        ip: "192.0.2.20".into(),
        source: "ingress".into(),
        resource_name: format!("{ns}/{svc}-ing"),
        backend_service: Some(format!("{ns}/{svc}")),
        backend_upstream: None,
    }
}

async fn reveal(
    tag: &str,
    ns: &str,
    svc: &str,
    runner: &WorkloadRunner,
) -> Result<RevealedEndpointLogin, LoginErrorCode> {
    let paths = temp_paths(tag);
    let ep = endpoint(ns, svc);
    seed_profile_and_endpoints(&paths, "lab", kubeconfig(), vec![ep.clone()]);
    let res = reveal_endpoint_login_with_deps(runner, &paths, "lab", &ep.id).await;
    let _ = std::fs::remove_dir_all(&paths.base);
    res
}

fn secret_calls(runner: &WorkloadRunner) -> Vec<String> {
    runner
        .calls()
        .iter()
        .filter(|a| {
            a.iter().any(|x| x == "secret" || x == "secret/")
                || a.iter().any(|x| x.starts_with("secret/"))
        })
        .map(|a| a.join(" "))
        .collect()
}

#[tokio::test]
async fn grafana_secret_comes_from_the_workload_env_and_label_shows_it() {
    let env = serde_json::json!([
        secret_env("GF_SECURITY_ADMIN_USER", "grafana-secrets", "admin-user"),
        secret_env(
            "GF_SECURITY_ADMIN_PASSWORD",
            "grafana-secrets",
            "admin-password"
        ),
    ]);
    let runner = WorkloadRunner::new(
        pods_with_env(env),
        vec![(
            "grafana-secrets",
            secret_json(&[("admin-user", "gadmin"), ("admin-password", "fake-pw")]),
        )],
    );
    let res = reveal("wl-grafana", "monitoring", "prom-grafana", &runner)
        .await
        .unwrap();
    assert_eq!(res.secret_name, "grafana-secrets");
    assert_eq!(res.username.as_deref(), Some("gadmin"));
    assert_eq!(res.password.expose_for_test(), "fake-pw");
    // can-i runs on the resolved Secret, and the static one is never touched.
    let calls = runner.calls();
    assert!(calls
        .iter()
        .any(|a| a.iter().any(|x| x == "secret/grafana-secrets")));
    assert!(!calls.iter().any(|a| a
        .iter()
        .any(|x| x.contains("prom-grafana") && x.starts_with("secret"))));
    // P1-2: the Secret actually GET is the one reported back to the UI.
    assert!(secret_calls(&runner)
        .iter()
        .any(|c| c.contains(&format!("secret {} ", res.secret_name))));
    let pod_call = calls
        .iter()
        .find(|a| a.iter().any(|x| x.contains("/pods?")))
        .unwrap();
    assert!(pod_call.iter().any(|x| x
        == "/api/v1/namespaces/monitoring/pods?labelSelector=app.kubernetes.io%2Finstance%3Dprom%2Capp.kubernetes.io%2Fname%3Dgrafana&limit=3"));
    assert!(!pod_call.iter().any(|x| x == "-o"));
}

#[tokio::test]
async fn check_gates_can_i_on_the_resolved_secret() {
    let env = serde_json::json!([secret_env(
        "GF_SECURITY_ADMIN_PASSWORD",
        "grafana-secrets",
        "admin-password"
    )]);
    let runner = WorkloadRunner::new(pods_with_env(env), vec![]);
    let paths = temp_paths("wl-check");
    let ep = endpoint("monitoring", "prom-grafana");
    seed_profile_and_endpoints(&paths, "lab", kubeconfig(), vec![ep.clone()]);
    let opt = check_endpoint_login_with_deps(&runner, &paths, "lab", &ep.id)
        .await
        .unwrap();
    assert_eq!(opt.secret_name, "grafana-secrets");
    assert!(runner
        .calls()
        .iter()
        .any(|a| a.iter().any(|x| x == "secret/grafana-secrets")));
    let _ = std::fs::remove_dir_all(&paths.base);
}

#[tokio::test]
async fn no_pod_or_forbidden_pods_fall_back_to_the_static_secret() {
    let static_body = secret_json(&[("admin-user", "u"), ("admin-password", "static-pw")]);
    let forbidden = Ok(CommandOutput {
        stdout: String::new(),
        stderr: "Error from server (Forbidden): pods is forbidden".into(),
        success: false,
    });
    for pods in [json_output(serde_json::json!({"items": []})), forbidden] {
        let runner = WorkloadRunner::new(pods, vec![("prom-grafana", static_body.clone())]);
        let res = reveal("wl-fallback", "monitoring", "prom-grafana", &runner)
            .await
            .unwrap();
        assert_eq!(res.secret_name, "prom-grafana");
        assert_eq!(res.password.expose_for_test(), "static-pw");
    }
}

#[tokio::test]
async fn literal_password_is_ignored_but_literal_username_is_accepted() {
    let static_body = secret_json(&[("admin-user", "u"), ("admin-password", "static-pw")]);
    let literal_pw =
        serde_json::json!([{"name": "GF_SECURITY_ADMIN_PASSWORD", "value": "literal-pw"}]);
    let runner = WorkloadRunner::new(
        pods_with_env(literal_pw),
        vec![("prom-grafana", static_body)],
    );
    let res = reveal("wl-lit-pw", "monitoring", "prom-grafana", &runner)
        .await
        .unwrap();
    assert_eq!(res.password.expose_for_test(), "static-pw");

    let env = serde_json::json!([
        {"name": "GF_SECURITY_ADMIN_USER", "value": "literal-admin"},
        secret_env("GF_SECURITY_ADMIN_PASSWORD", "grafana-secrets", "admin-password"),
    ]);
    let runner = WorkloadRunner::new(
        pods_with_env(env),
        vec![(
            "grafana-secrets",
            secret_json(&[("admin-password", "fake-pw")]),
        )],
    );
    let res = reveal("wl-lit-user", "monitoring", "prom-grafana", &runner)
        .await
        .unwrap();
    assert_eq!(res.username.as_deref(), Some("literal-admin"));
    assert_eq!(res.password.expose_for_test(), "fake-pw");
}

#[tokio::test]
async fn unsafe_secret_name_or_key_from_the_pod_is_ignored() {
    let static_body = secret_json(&[("admin-user", "u"), ("admin-password", "static-pw")]);
    for (name, key) in [
        ("../evil", "admin-password"),
        ("Bad_Name", "k"),
        ("grafana-secrets", "a/b"),
        ("grafana-secrets", ".."),
    ] {
        let env = serde_json::json!([secret_env("GF_SECURITY_ADMIN_PASSWORD", name, key)]);
        let runner = WorkloadRunner::new(
            pods_with_env(env),
            vec![("prom-grafana", static_body.clone())],
        );
        let res = reveal("wl-unsafe", "monitoring", "prom-grafana", &runner)
            .await
            .unwrap();
        assert_eq!(res.secret_name, "prom-grafana", "{name}/{key}");
        for call in runner.calls() {
            assert!(!call
                .iter()
                .any(|a| a.contains("evil") || a.contains("Bad_Name") || a == "a/b"));
        }
        assert_eq!(secret_calls(&runner).len(), 2, "one can-i + one GET");
    }
}

#[tokio::test]
async fn unsafe_selector_skips_the_pod_list() {
    let mut runner = WorkloadRunner::new(
        pods_with_env(serde_json::json!([])),
        vec![("prom-grafana", secret_json(&[("admin-password", "pw")]))],
    );
    runner.service =
        serde_json::json!({"spec": {"type": "ClusterIP", "selector": {"app": "x,y=z&limit=1"}}});
    reveal("wl-selector", "monitoring", "prom-grafana", &runner)
        .await
        .unwrap();
    assert!(!runner
        .calls()
        .iter()
        .any(|a| a.iter().any(|x| x.contains("/pods?"))));
}

#[tokio::test]
async fn gitea_username_comes_from_the_workload_env() {
    // Measured: the chart sets both as literals; the literal password is ignored, the
    // username is used with the static Secret's password.
    let env = serde_json::json!([
        {"name": "GITEA_ADMIN_USERNAME", "value": "gitea_admin"},
        {"name": "GITEA_ADMIN_PASSWORD", "value": "literal-pw"},
    ]);
    let runner = WorkloadRunner::new(
        pods_with_env(env),
        vec![("gitea-admin", secret_json(&[("admin-password", "fake-pw")]))],
    );
    let res = reveal("wl-gitea", "git", "gitea-http", &runner)
        .await
        .unwrap();
    assert_eq!(res.username.as_deref(), Some("gitea_admin"));
    assert_eq!(res.password.expose_for_test(), "fake-pw");
}

#[test]
fn keycloak_service_no_longer_offers_a_login() {
    let options = endpoint_login_options_for_slice(&[
        endpoint("iam", "keycloak"),
        endpoint("iam", "keycloak-service"),
    ]);
    assert!(options.is_empty());
}

#[tokio::test]
async fn pod_list_over_curl_keeps_the_encoded_selector_and_limit() {
    let mut runner = WorkloadRunner::new(
        pods_of(vec![qualified_pod(vec![grafana_container(
            "grafana-secrets",
        )])]),
        vec![(
            "grafana-secrets",
            secret_json(&[("admin-password", "fake-pw")]),
        )],
    );
    runner.kubectl_pods_transport_fails = true;
    let res = reveal("wl-curl", "monitoring", "prom-grafana", &runner)
        .await
        .unwrap();
    assert_eq!(res.secret_name, "grafana-secrets");
    let urls = runner.curl_urls.lock().unwrap().clone();
    assert_eq!(urls.len(), 1, "{urls:?}");
    assert!(urls[0].ends_with(
        "/api/v1/namespaces/monitoring/pods?labelSelector=app.kubernetes.io%2Finstance%3Dprom%2Capp.kubernetes.io%2Fname%3Dgrafana&limit=3"
    ));
}

#[tokio::test]
async fn unready_terminating_or_ownerless_pods_are_skipped() {
    let static_body = secret_json(&[("admin-password", "static-pw")]);
    let mut unready = qualified_pod(vec![grafana_container("evil-secret")]);
    unready["status"]["conditions"][0]["status"] = "False".into();
    let mut pending = qualified_pod(vec![grafana_container("evil-secret")]);
    pending["status"]["phase"] = "Pending".into();
    let mut terminating = qualified_pod(vec![grafana_container("evil-secret")]);
    terminating["metadata"]["deletionTimestamp"] = "2026-11-16T00:00:00Z".into();
    let mut ownerless = qualified_pod(vec![grafana_container("evil-secret")]);
    ownerless["metadata"] = serde_json::json!({});
    let mut job_owned = qualified_pod(vec![grafana_container("evil-secret")]);
    job_owned["metadata"]["ownerReferences"][0]["kind"] = "Job".into();
    for pod in [unready, pending, terminating, ownerless, job_owned] {
        let runner = WorkloadRunner::new(
            pods_of(vec![pod]),
            vec![("prom-grafana", static_body.clone())],
        );
        let res = reveal("wl-skip", "monitoring", "prom-grafana", &runner)
            .await
            .unwrap();
        assert_eq!(res.secret_name, "prom-grafana");
        assert!(!runner
            .calls()
            .iter()
            .any(|a| a.iter().any(|x| x.contains("evil-secret"))));
    }
}

#[tokio::test]
async fn disagreeing_pods_fall_back_to_the_static_secret_and_agreeing_pods_resolve() {
    let static_body = secret_json(&[("admin-password", "static-pw")]);
    let runner = WorkloadRunner::new(
        pods_of(vec![
            qualified_pod(vec![grafana_container("grafana-secrets")]),
            qualified_pod(vec![grafana_container("other-secrets")]),
        ]),
        vec![("prom-grafana", static_body)],
    );
    let res = reveal("wl-disagree", "monitoring", "prom-grafana", &runner)
        .await
        .unwrap();
    assert_eq!(res.secret_name, "prom-grafana");

    let runner = WorkloadRunner::new(
        pods_of(vec![
            qualified_pod(vec![grafana_container("grafana-secrets")]),
            qualified_pod(vec![grafana_container("grafana-secrets")]),
        ]),
        vec![(
            "grafana-secrets",
            secret_json(&[("admin-password", "fake-pw")]),
        )],
    );
    let res = reveal("wl-agree", "monitoring", "prom-grafana", &runner)
        .await
        .unwrap();
    assert_eq!(res.secret_name, "grafana-secrets");
}

#[tokio::test]
async fn username_and_password_never_mix_across_containers() {
    let sidecar = serde_json::json!({"name": "sidecar", "env": [
        {"name": "GF_SECURITY_ADMIN_USER", "value": "sidecar-user"}]});
    let app = serde_json::json!({"name": "grafana", "env": [
        secret_env("GF_SECURITY_ADMIN_USER", "grafana-secrets", "admin-user"),
        secret_env("GF_SECURITY_ADMIN_PASSWORD", "grafana-secrets", "admin-password")]});
    let runner = WorkloadRunner::new(
        pods_of(vec![qualified_pod(vec![sidecar, app])]),
        vec![(
            "grafana-secrets",
            secret_json(&[("admin-user", "gadmin"), ("admin-password", "fake-pw")]),
        )],
    );
    let res = reveal("wl-mix", "monitoring", "prom-grafana", &runner)
        .await
        .unwrap();
    assert_eq!(res.username.as_deref(), Some("gadmin"));
    assert_eq!(res.password.expose_for_test(), "fake-pw");
}

#[tokio::test]
async fn username_secret_ref_into_the_static_secret_is_used_when_password_is_literal() {
    let env = serde_json::json!([
        secret_env("GF_SECURITY_ADMIN_USER", "prom-grafana", "admin-user"),
        {"name": "GF_SECURITY_ADMIN_PASSWORD", "value": "literal-pw"},
    ]);
    let runner = WorkloadRunner::new(
        pods_with_env(env),
        vec![(
            "prom-grafana",
            secret_json(&[
                ("admin-user", "static-admin"),
                ("admin-password", "static-pw"),
            ]),
        )],
    );
    let res = reveal("wl-user-ref", "monitoring", "prom-grafana", &runner)
        .await
        .unwrap();
    assert_eq!(res.secret_name, "prom-grafana");
    assert_eq!(res.username.as_deref(), Some("static-admin"));
    assert_eq!(res.password.expose_for_test(), "static-pw");
}

#[tokio::test]
async fn can_i_denied_on_the_resolved_secret_never_gets_it() {
    struct Denied(WorkloadRunner);
    #[async_trait::async_trait]
    impl CommandRunner for Denied {
        async fn run(&self, bin: &str, args: &[String]) -> Result<CommandOutput, String> {
            if args.iter().any(|a| a == "can-i") {
                self.0.calls.lock().unwrap().push(args.to_vec());
                return Ok(ok("no\n"));
            }
            self.0.run(bin, args).await
        }
    }
    let env = serde_json::json!([secret_env(
        "GF_SECURITY_ADMIN_PASSWORD",
        "grafana-secrets",
        "admin-password"
    )]);
    let runner = Denied(WorkloadRunner::new(
        pods_with_env(env),
        vec![(
            "grafana-secrets",
            secret_json(&[("admin-password", "fake-pw")]),
        )],
    ));
    let paths = temp_paths("wl-denied");
    let ep = endpoint("monitoring", "prom-grafana");
    seed_profile_and_endpoints(&paths, "lab", kubeconfig(), vec![ep.clone()]);
    let res = reveal_endpoint_login_with_deps(&runner, &paths, "lab", &ep.id).await;
    let _ = std::fs::remove_dir_all(&paths.base);
    assert!(matches!(res, Err(LoginErrorCode::Forbidden)));
    let gets: Vec<_> = secret_calls(&runner.0)
        .into_iter()
        .filter(|c| !c.contains("can-i"))
        .collect();
    assert!(gets.is_empty(), "{gets:?}");
}
