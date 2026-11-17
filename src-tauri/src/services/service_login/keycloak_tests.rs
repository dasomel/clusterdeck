use super::tests::{json_output, secret_json, seed_profile_and_endpoints, temp_paths};
use super::*;
use crate::services::process::CommandOutput;
use std::collections::{HashMap, HashSet};
use std::sync::Mutex;

/// Serves Services, pods and Secrets from maps keyed by `ns/name`; every call is recorded.
#[derive(Default)]
struct KcRunner {
    calls: Mutex<Vec<String>>,
    secrets: HashMap<String, String>,
    denied: HashSet<String>,
    /// namespace -> pod list body
    pods: HashMap<String, serde_json::Value>,
    /// login-surface path -> curl result; absent paths fail like a TLS error
    http: HashMap<String, Result<CommandOutput, String>>,
    /// (args, timeout secs, max output bytes) of every curl call
    curl_calls: Mutex<Vec<(Vec<String>, u64, usize)>>,
    /// artificial latency of every kubectl call (deadline tests)
    delay: Option<std::time::Duration>,
    /// latency only for kubectl calls in this namespace
    slow_ns: Option<(String, std::time::Duration)>,
}

impl KcRunner {
    fn secret(mut self, ns: &str, name: &str, entries: &[(&str, &str)]) -> Self {
        self.secrets
            .insert(format!("{ns}/{name}"), secret_json(entries));
        self
    }
    fn deny(mut self, ns: &str, name: &str) -> Self {
        self.denied.insert(format!("{ns}/{name}"));
        self
    }
    fn pod_env(mut self, ns: &str, env: serde_json::Value) -> Self {
        self.pods.insert(
            ns.to_string(),
            serde_json::json!({"items": [{
                "metadata": {"ownerReferences": [{"kind": "ReplicaSet", "controller": true}]},
                "status": {"phase": "Running", "conditions": [{"type": "Ready", "status": "True"}]},
                "spec": {"containers": [{"name": "app", "env": env}]}}]}),
        );
        self
    }
    fn http(mut self, path: &str, result: Result<CommandOutput, String>) -> Self {
        self.http.insert(path.to_string(), result);
        self
    }
    fn calls(&self) -> Vec<String> {
        self.calls.lock().unwrap().clone()
    }
    fn secret_reads(&self) -> Vec<String> {
        self.calls()
            .into_iter()
            .filter(|c| c.contains(" secret ") && !c.contains("can-i"))
            .collect()
    }
}

fn out(stdout: &str, stderr: &str, success: bool) -> Result<CommandOutput, String> {
    Ok(CommandOutput {
        stdout: stdout.into(),
        stderr: stderr.into(),
        success,
    })
}

#[async_trait::async_trait]
impl CommandRunner for KcRunner {
    async fn run(&self, bin: &str, args: &[String]) -> Result<CommandOutput, String> {
        if bin == "curl" {
            let url = args.last().unwrap();
            let path = url.split_once("example.internal").map_or("", |(_, p)| p);
            return self
                .http
                .get(path)
                .cloned()
                .unwrap_or(Err("curl: (60) SSL certificate problem".into()));
        }
        if let Some(delay) = self.delay {
            tokio::time::sleep(delay).await;
        }
        if let Some((slow, delay)) = &self.slow_ns {
            if args.windows(2).any(|w| w[0] == "-n" && &w[1] == slow) {
                tokio::time::sleep(*delay).await;
            }
        }
        let line = args.join(" ");
        self.calls.lock().unwrap().push(line.clone());
        let ns = args
            .iter()
            .position(|a| a == "-n")
            .map(|i| args[i + 1].clone())
            .unwrap_or_default();
        if let Some(i) = args.iter().position(|a| a == "can-i") {
            let name = args[i + 2].trim_start_matches("secret/");
            let key = format!("{ns}/{name}");
            return if self.denied.contains(&key) {
                out("no\n", "", false)
            } else {
                out("yes\n", "", true)
            };
        }
        if let Some(path) = args.iter().find(|a| a.contains("/pods?")) {
            let pod_ns = path.split('/').nth(4).unwrap_or_default();
            let body = self
                .pods
                .get(pod_ns)
                .cloned()
                .unwrap_or(serde_json::json!({"items": []}));
            return json_output(body);
        }
        if args.iter().any(|a| a == "service") {
            return json_output(
                serde_json::json!({"spec": {"type": "ClusterIP", "selector": {"app": "x"}}}),
            );
        }
        let i = args.iter().position(|a| a == "secret").unwrap();
        match self.secrets.get(&format!("{ns}/{}", args[i + 1])) {
            Some(body) => out(body, "", true),
            None => out("", "Error from server (NotFound): secrets not found", false),
        }
    }

    async fn run_bounded(
        &self,
        bin: &str,
        args: &[String],
        limits: CommandLimits,
    ) -> Result<CommandOutput, String> {
        if bin == "curl" {
            self.curl_calls.lock().unwrap().push((
                args.to_vec(),
                limits.timeout.as_secs(),
                limits.max_output_bytes,
            ));
        }
        self.run(bin, args).await
    }
}

fn kubeconfig() -> &'static str {
    "apiVersion: v1\ncurrent-context: lab-ctx\ncontexts:\n- name: lab-ctx\n  context:\n    cluster: c\n    user: u\nclusters:\n- name: c\n  cluster:\n    server: https://192.0.2.50:6443\nusers:\n- name: u\n  user:\n    token: fake-test-token\n"
}

fn ep(ns: &str, svc: &str, host: &str, sso_oidc: bool) -> DiscoveredEndpoint {
    DiscoveredEndpoint {
        id: DiscoveredEndpoint::compute_id("ingress", &format!("{ns}/{svc}-ing"), host),
        host: host.into(),
        ip: "192.0.2.20".into(),
        source: "ingress".into(),
        resource_name: format!("{ns}/{svc}-ing"),
        backend_service: Some(format!("{ns}/{svc}")),
        backend_upstream: None,
        sso_oidc,
    }
}

fn keycloak_ep() -> DiscoveredEndpoint {
    ep("iam", "keycloak-service", "sso.example.internal", false)
}

async fn reveal(
    tag: &str,
    runner: &KcRunner,
    endpoints: Vec<DiscoveredEndpoint>,
    target: usize,
) -> Result<RevealedEndpointLogin, LoginErrorCode> {
    reveal_with_template(tag, runner, endpoints, target, None).await
}

async fn reveal_with_template(
    tag: &str,
    runner: &KcRunner,
    endpoints: Vec<DiscoveredEndpoint>,
    target: usize,
    template: Option<&str>,
) -> Result<RevealedEndpointLogin, LoginErrorCode> {
    let paths = temp_paths(tag);
    seed_profile_and_endpoints(&paths, "lab", kubeconfig(), endpoints.clone());
    if template.is_some() {
        let mut profile = store::get_profile(&paths, "lab").unwrap();
        profile.sso_username_template = template.map(str::to_string);
        store::save_profiles(&paths, &[profile]).unwrap();
    }
    let res = reveal_endpoint_login_with_deps(runner, &paths, "lab", &endpoints[target].id).await;
    let _ = std::fs::remove_dir_all(&paths.base);
    res
}

fn names(group: &LoginGroup) -> Vec<&str> {
    group
        .accounts
        .iter()
        .map(|a| a.username.as_deref().unwrap())
        .collect()
}

#[tokio::test]
async fn keycloak_lists_permanent_admin_and_realm_users_and_never_reads_initial_admin() {
    let runner = KcRunner::default()
        .secret(
            "iam",
            "keycloak-initial-admin",
            &[("username", "boot"), ("password", "boot-pw")],
        )
        .secret(
            "iam",
            "keycloak-admin",
            &[("username", "kcadmin"), ("password", "kc-pw")],
        )
        .secret(
            "iam",
            "keycloak-user-passwords",
            &[
                ("dev", "dev-pw"),
                ("admin", "adm-pw"),
                ("bad key", "x"),
                ("a/b", "y"),
            ],
        );
    let res = reveal("kc-ok", &runner, vec![keycloak_ep()], 0)
        .await
        .unwrap();
    assert_eq!(res.groups.len(), 2);
    let admin = &res.groups[0];
    assert_eq!(admin.label, "Admin console (master realm)");
    assert_eq!(admin.secret_name, "keycloak-admin");
    assert_eq!(admin.accounts[0].username.as_deref(), Some("kcadmin"));
    assert_eq!(admin.accounts[0].password.expose_for_test(), "kc-pw");
    let users = &res.groups[1];
    // Sorted, unsafe keys skipped.
    assert_eq!(names(users), ["admin", "dev"]);
    assert_eq!(users.accounts[1].password.expose_for_test(), "dev-pw");
    assert!(runner
        .calls()
        .iter()
        .all(|c| !c.contains("keycloak-initial-admin")));
    let calls = runner.calls();
    for secret in ["keycloak-admin", "keycloak-user-passwords"] {
        assert!(calls
            .iter()
            .any(|c| c.contains(&format!("can-i get secret/{secret}"))));
    }
}

#[tokio::test]
async fn keycloak_admin_falls_back_to_admin_credential_and_both_missing_is_not_found() {
    let runner = KcRunner::default().secret(
        "iam",
        "keycloak-admin-credential",
        &[("username", "root"), ("password", "cred-pw")],
    );
    let ep = ep("iam", "keycloak", "sso.example.internal", false);
    let res = reveal("kc-fb", &runner, vec![ep.clone()], 0).await.unwrap();
    assert_eq!(res.groups[0].secret_name, "keycloak-admin-credential");
    assert_eq!(res.groups[0].accounts[0].username.as_deref(), Some("root"));
    // Realm users secret is absent: that group fails alone.
    assert_eq!(res.groups[1].error, Some(LoginErrorCode::NotFound));

    let empty = KcRunner::default();
    let err = reveal("kc-none", &empty, vec![ep], 0).await.unwrap_err();
    assert_eq!(err, LoginErrorCode::NotFound);
}

#[tokio::test]
async fn one_forbidden_source_does_not_hide_the_other_and_realm_users_are_capped() {
    let many: Vec<(String, String)> = (0..25)
        .map(|i| (format!("user{i:02}"), "pw".into()))
        .collect();
    let entries: Vec<(&str, &str)> = many.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
    let runner = KcRunner::default()
        .secret(
            "iam",
            "keycloak-admin",
            &[("username", "kcadmin"), ("password", "kc-pw")],
        )
        .deny("iam", "keycloak-admin")
        .secret("iam", "keycloak-user-passwords", &entries);
    let res = reveal("kc-part", &runner, vec![keycloak_ep()], 0)
        .await
        .unwrap();
    assert_eq!(res.groups[0].error, Some(LoginErrorCode::Forbidden));
    assert!(res.groups[0].accounts.is_empty());
    assert_eq!(res.groups[1].accounts.len(), 20);
    // A denied can-i must never be followed by a GET of that Secret.
    assert!(runner
        .secret_reads()
        .iter()
        .all(|c| !c.contains("keycloak-admin ")));
}

#[tokio::test]
async fn sso_plugin_evidence_shows_realm_users_next_to_the_apps_own_admin() {
    let runner = KcRunner::default()
        .secret(
            "argocd",
            "argocd-initial-admin-secret",
            &[("password", "argo-pw")],
        )
        .secret("iam", "keycloak-user-passwords", &[("dev", "dev-pw")]);
    let endpoints = vec![
        ep("argocd", "argocd-server", "argo.example.internal", true),
        keycloak_ep(),
    ];
    let res = reveal("sso-own", &runner, endpoints, 0).await.unwrap();
    assert_eq!(res.groups.len(), 2);
    assert_eq!(res.groups[0].accounts[0].username.as_deref(), Some("admin"));
    assert_eq!(res.groups[1].label, "SSO (Keycloak realm unknown)");
    assert_eq!(res.groups[1].namespace, "iam");
    assert_eq!(names(&res.groups[1]), ["dev"]);
}

#[tokio::test]
async fn sso_issuer_env_gives_realm_label_and_portal_like_apps_without_admin_get_a_login() {
    let runner = KcRunner::default()
        .pod_env(
            "mgr",
            serde_json::json!([
                {"name": "AUTH_MODE", "value": "oidc"},
                {"name": "OIDC_ISSUER", "value": "https://sso.example.internal/realms/beluga"}]),
        )
        .secret("iam", "keycloak-user-passwords", &[("analyst", "an-pw")]);
    let endpoints = vec![
        ep("mgr", "manager", "mgr.example.internal", false),
        keycloak_ep(),
    ];
    let res = reveal("sso-env", &runner, endpoints.clone(), 0)
        .await
        .unwrap();
    assert_eq!(res.groups.len(), 1);
    assert_eq!(res.groups[0].label, "SSO (Keycloak realm beluga)");
    assert_eq!(names(&res.groups[0]), ["analyst"]);

    // The options list offers the Reveal button for it without any flag in the registry.
    let paths = temp_paths("sso-opts");
    seed_profile_and_endpoints(&paths, "lab", kubeconfig(), endpoints);
    let opts = endpoint_login_options_with_deps(&runner, &paths, "lab")
        .await
        .unwrap();
    let _ = std::fs::remove_dir_all(&paths.base);
    let mgr = opts.iter().find(|o| o.service_name == "manager").unwrap();
    assert_eq!(
        mgr.sso,
        Some(SsoHint {
            realm: Some("beluga".into())
        })
    );
}

#[tokio::test]
async fn issuer_in_a_secret_is_only_an_env_name_hint_and_that_secret_is_never_read() {
    // The issuer value arrives via valueFrom: its realm stays unknown and portal-env is not read.
    let runner = KcRunner::default()
        .pod_env(
            "portal",
            serde_json::json!([{"name": "OIDC_ISSUER",
                "valueFrom": {"secretKeyRef": {"name": "portal-env", "key": "OIDC_ISSUER"}}}]),
        )
        .secret(
            "portal",
            "portal-env",
            &[("OIDC_ISSUER", "https://sso.example.internal/realms/x")],
        )
        .secret("iam", "keycloak-user-passwords", &[("dev", "dev-pw")]);
    let endpoints = vec![
        ep("portal", "portal", "portal.example.internal", false),
        keycloak_ep(),
    ];
    let res = reveal("sso-none", &runner, endpoints.clone(), 0)
        .await
        .unwrap();
    assert_eq!(res.groups[0].label, "SSO (Keycloak realm unknown)");
    assert!(runner.calls().iter().all(|c| !c.contains("portal-env")));

    // Without any env name or probe hit there is no evidence and no secret read at all.
    let bare = KcRunner::default().secret("iam", "keycloak-user-passwords", &[("dev", "dev-pw")]);
    let err = reveal("sso-bare", &bare, endpoints.clone(), 0)
        .await
        .unwrap_err();
    assert_eq!(err, LoginErrorCode::UnsupportedService);
    assert!(bare.secret_reads().is_empty());
    let paths = temp_paths("sso-none-opts");
    seed_profile_and_endpoints(&paths, "lab", kubeconfig(), endpoints);
    let opts = endpoint_login_options_with_deps(&bare, &paths, "lab")
        .await
        .unwrap();
    let _ = std::fs::remove_dir_all(&paths.base);
    assert!(opts.iter().all(|o| o.service_name != "portal"));
    assert!(bare.secret_reads().is_empty());
}

#[test]
fn slice_options_offer_keycloak_and_plugin_sso_only_when_a_keycloak_endpoint_exists() {
    let app = ep("portal", "portal", "portal.example.internal", true);
    assert!(endpoint_login_options_for_slice(std::slice::from_ref(&app)).is_empty());
    let opts = endpoint_login_options_for_slice(&[app, keycloak_ep()]);
    assert_eq!(opts.len(), 2);
    let portal = opts.iter().find(|o| o.service_name == "portal").unwrap();
    assert_eq!(portal.secret_name, "keycloak-user-passwords");
    assert_eq!(portal.sso, Some(SsoHint { realm: None }));
    let kc = opts
        .iter()
        .find(|o| o.service_name == "keycloak-service")
        .unwrap();
    assert_eq!(kc.secret_name, "keycloak-admin");
    assert_eq!(kc.sso, None);
}

#[tokio::test]
async fn check_is_enabled_when_any_source_is_permitted() {
    let runner = KcRunner::default()
        .deny("iam", "keycloak-admin")
        .deny("iam", "keycloak-admin-credential");
    let paths = temp_paths("kc-check");
    let kc = keycloak_ep();
    seed_profile_and_endpoints(&paths, "lab", kubeconfig(), vec![kc.clone()]);
    let ok = check_endpoint_login_with_deps(&runner, &paths, "lab", &kc.id).await;
    assert!(ok.is_ok());
    let runner = KcRunner::default()
        .deny("iam", "keycloak-admin")
        .deny("iam", "keycloak-admin-credential")
        .deny("iam", "keycloak-user-passwords");
    let err = check_endpoint_login_with_deps(&runner, &paths, "lab", &kc.id).await;
    let _ = std::fs::remove_dir_all(&paths.base);
    assert_eq!(err.unwrap_err(), LoginErrorCode::Forbidden);
}

fn http_ok(body: &str) -> Result<CommandOutput, String> {
    out(body, "", true)
}

fn portal_endpoints() -> Vec<DiscoveredEndpoint> {
    vec![
        ep("portal", "portal", "portal.example.internal", false),
        keycloak_ep(),
    ]
}

#[tokio::test]
async fn http_probe_providers_json_with_oidc_is_evidence_and_probe_args_are_safe_and_bounded() {
    let runner = KcRunner::default()
        .http(
            "/api/auth/providers",
            http_ok(r#"{"keycloak":{"id":"keycloak","type":"oidc"}}"#),
        )
        .secret("iam", "keycloak-user-passwords", &[("dev", "dev-pw")]);
    let res = reveal("probe-prov", &runner, portal_endpoints(), 0)
        .await
        .unwrap();
    assert_eq!(res.groups[0].label, "SSO (Keycloak realm unknown)");
    assert_eq!(names(&res.groups[0]), ["dev"]);
    let calls = runner.curl_calls.lock().unwrap().clone();
    assert_eq!(calls.len(), 1);
    let (args, secs, bytes) = &calls[0];
    assert_eq!(args[0], "-q");
    let np = args.iter().position(|a| a == "--noproxy").unwrap();
    assert_eq!(args[np + 1], "*");
    assert!(!args
        .iter()
        .any(|a| a == "-k" || a == "--insecure" || a == "-L" || a == "--location"));
    let i = args.iter().position(|a| a == "--resolve").unwrap();
    assert_eq!(args[i + 1], "portal.example.internal:443:192.0.2.20");
    for flag in ["--max-time", "--max-filesize", "--proto"] {
        assert!(args.iter().any(|a| a == flag), "{flag}");
    }
    assert_eq!(
        args.last().unwrap(),
        "https://portal.example.internal/api/auth/providers"
    );
    assert_eq!((*secs, *bytes), (6, 64 * 1024));
}

#[tokio::test]
async fn http_probe_redirect_to_a_realm_auth_url_gives_the_realm() {
    let runner = KcRunner::default()
        .http("/api/auth/providers", out("", "", false))
        .http(
            "/",
            http_ok("https://sso.example.internal/realms/x/protocol/openid-connect/auth?client_id=a&state=1"),
        )
        .secret("iam", "keycloak-user-passwords", &[("dev", "dev-pw")]);
    let res = reveal("probe-redir", &runner, portal_endpoints(), 0)
        .await
        .unwrap();
    assert_eq!(res.groups[0].label, "SSO (Keycloak realm x)");
}

#[tokio::test]
async fn http_probe_failures_and_garbage_are_no_evidence() {
    let cases: Vec<(&str, KcRunner)> = vec![
        ("tls", KcRunner::default()),
        (
            "404",
            KcRunner::default()
                .http("/api/auth/providers", out("<html>", "", false))
                .http("/", out("", "", false)),
        ),
        (
            "garbage",
            KcRunner::default()
                .http("/api/auth/providers", http_ok("<html>login</html>"))
                .http("/", http_ok("https://evil.example.internal/realms/x/other"))
                .secret("iam", "keycloak-user-passwords", &[("dev", "dev-pw")]),
        ),
        (
            "json-no-oidc",
            KcRunner::default()
                .http(
                    "/api/auth/providers",
                    http_ok(r#"{"credentials":{"type":"credentials"}}"#),
                )
                .http("/", http_ok("")),
        ),
    ];
    for (tag, runner) in cases {
        let err = reveal(tag, &runner, portal_endpoints(), 0)
            .await
            .unwrap_err();
        assert_eq!(err, LoginErrorCode::UnsupportedService, "{tag}");
        assert!(runner.secret_reads().is_empty(), "{tag}");
    }
}

#[tokio::test]
async fn oauth_or_oidc_env_names_are_evidence_without_reading_values_and_fill_a_missing_admin() {
    // Beluga-style Grafana: only a client-secret env (a Secret reference), no admin secret.
    let runner = KcRunner::default()
        .pod_env(
            "monitoring",
            serde_json::json!([{"name": "GF_AUTH_GENERIC_OAUTH_CLIENT_SECRET",
                "valueFrom": {"secretKeyRef": {"name": "keycloak-client-secrets", "key": "grafana"}}}]),
        )
        .secret("iam", "keycloak-user-passwords", &[("dev", "dev-pw")]);
    let endpoints = vec![
        ep("monitoring", "grafana", "grafana.example.internal", false),
        keycloak_ep(),
    ];
    let res = reveal("oauth-env", &runner, endpoints, 0).await.unwrap();
    assert_eq!(res.groups.len(), 2);
    assert_eq!(res.groups[0].label, "Admin account");
    assert_eq!(res.groups[0].error, Some(LoginErrorCode::NotFound));
    assert_eq!(res.groups[1].label, "SSO (Keycloak realm unknown)");
    assert_eq!(names(&res.groups[1]), ["dev"]);
    assert!(runner
        .calls()
        .iter()
        .all(|c| !c.contains("keycloak-client-secrets")));
}

#[tokio::test]
async fn two_keycloak_backends_attach_no_env_or_probe_evidence() {
    let runner = KcRunner::default()
        .pod_env(
            "portal",
            serde_json::json!([{"name": "OIDC_ENABLED", "value": "true"}]),
        )
        .http(
            "/api/auth/providers",
            http_ok(r#"{"keycloak":{"type":"oidc"}}"#),
        )
        .secret("iam", "keycloak-user-passwords", &[("dev", "dev-pw")]);
    let mut endpoints = portal_endpoints();
    endpoints.push(ep("other", "keycloak", "sso2.example.internal", false));
    let err = reveal("two-kc", &runner, endpoints.clone(), 0)
        .await
        .unwrap_err();
    assert_eq!(err, LoginErrorCode::UnsupportedService);
    assert!(runner.curl_calls.lock().unwrap().is_empty());
    assert!(runner.secret_reads().is_empty());

    let paths = temp_paths("two-kc-opts");
    seed_profile_and_endpoints(&paths, "lab", kubeconfig(), endpoints);
    let opts = endpoint_login_options_with_deps(&runner, &paths, "lab")
        .await
        .unwrap();
    let _ = std::fs::remove_dir_all(&paths.base);
    assert!(opts.iter().all(|o| o.service_name != "portal"));
}

#[tokio::test]
async fn options_probe_the_login_surface_for_endpoints_without_other_evidence() {
    let runner = KcRunner::default().http(
        "/api/auth/providers",
        http_ok(r#"{"keycloak":{"type":"oidc"}}"#),
    );
    let paths = temp_paths("probe-opts");
    seed_profile_and_endpoints(&paths, "lab", kubeconfig(), portal_endpoints());
    let opts = endpoint_login_options_with_deps(&runner, &paths, "lab")
        .await
        .unwrap();
    let _ = std::fs::remove_dir_all(&paths.base);
    let portal = opts.iter().find(|o| o.service_name == "portal").unwrap();
    assert_eq!(portal.sso, Some(SsoHint { realm: None }));
    assert_eq!(runner.curl_calls.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn numeric_looking_hosts_are_never_probed() {
    for host in [
        "127.1",
        "0x7f.1",
        "2130706433",
        "0177.0.0.1",
        "0x7f.0x1",
        "localhost",
    ] {
        let runner = KcRunner::default().http("", http_ok(r#"{"keycloak":{"type":"oidc"}}"#));
        let endpoints = vec![ep("portal", "portal", host, false), keycloak_ep()];
        let err = reveal("numeric-host", &runner, endpoints, 0)
            .await
            .unwrap_err();
        assert_eq!(err, LoginErrorCode::UnsupportedService, "{host}");
        assert!(runner.curl_calls.lock().unwrap().is_empty(), "{host}");
    }
}

fn manager_issuer_runner() -> KcRunner {
    KcRunner::default()
        .pod_env(
            "mgr",
            serde_json::json!([{"name": "OIDC_ISSUER",
                "value": "https://sso.example.internal/realms/beluga"}]),
        )
        .secret("iam", "keycloak-user-passwords", &[("analyst", "an-pw")])
}

#[tokio::test]
async fn same_issuer_host_on_several_keycloak_backends_attaches_nothing() {
    let mut endpoints = vec![
        ep("mgr", "manager", "mgr.example.internal", false),
        keycloak_ep(),
    ];
    // A second Keycloak backend (other namespace) behind the very same host.
    endpoints.push(ep("other", "keycloak", "sso.example.internal", false));
    let runner = manager_issuer_runner();
    let err = reveal("amb-host", &runner, endpoints, 0).await.unwrap_err();
    assert_eq!(err, LoginErrorCode::UnsupportedService);
    assert!(runner.secret_reads().is_empty());
}

#[tokio::test]
async fn literal_issuer_picks_one_of_several_keycloaks_but_weak_sources_do_not() {
    let mut endpoints = vec![
        ep("mgr", "manager", "mgr.example.internal", false),
        keycloak_ep(),
    ];
    endpoints.push(ep("other", "keycloak", "sso2.example.internal", false));
    let runner = manager_issuer_runner();
    let res = reveal("multi-issuer", &runner, endpoints.clone(), 0)
        .await
        .unwrap();
    assert_eq!(res.groups[0].label, "SSO (Keycloak realm beluga)");
    assert_eq!(res.groups[0].namespace, "iam");

    // The options listing agrees.
    let paths = temp_paths("multi-issuer-opts");
    seed_profile_and_endpoints(&paths, "lab", kubeconfig(), endpoints);
    let opts = endpoint_login_options_with_deps(&runner, &paths, "lab")
        .await
        .unwrap();
    let _ = std::fs::remove_dir_all(&paths.base);
    let mgr = opts.iter().find(|o| o.service_name == "manager").unwrap();
    assert_eq!(
        mgr.sso,
        Some(SsoHint {
            realm: Some("beluga".into())
        })
    );
}

#[tokio::test(start_paused = true)]
async fn slow_backend_lookups_hit_the_deadline_but_registry_options_survive() {
    let mut runner = manager_issuer_runner();
    runner.delay = Some(std::time::Duration::from_secs(100));
    let endpoints = vec![
        ep("argocd", "argocd-server", "argo.example.internal", false),
        ep("mgr", "manager", "mgr.example.internal", false),
        keycloak_ep(),
    ];
    let paths = temp_paths("deadline");
    seed_profile_and_endpoints(&paths, "lab", kubeconfig(), endpoints);
    let opts = endpoint_login_options_with_deps(&runner, &paths, "lab")
        .await
        .unwrap();
    let _ = std::fs::remove_dir_all(&paths.base);
    assert!(opts.iter().any(|o| o.service_name == "argocd-server"));
    assert!(opts.iter().any(|o| o.service_name == "keycloak-service"));
    assert!(opts.iter().all(|o| o.service_name != "manager"));
}

#[tokio::test]
async fn sso_username_template_prefixes_realm_users_only() {
    let runner = KcRunner::default()
        .secret(
            "iam",
            "keycloak-admin",
            &[("username", "kcadmin"), ("password", "kc-pw")],
        )
        .secret(
            "iam",
            "keycloak-user-passwords",
            &[("admin", "a-pw"), ("analyst", "b-pw")],
        );
    let res = reveal_with_template(
        "tmpl",
        &runner,
        vec![keycloak_ep()],
        0,
        Some("beluga-{key}"),
    )
    .await
    .unwrap();
    // The admin console account keeps its own username.
    assert_eq!(
        res.groups[0].accounts[0].username.as_deref(),
        Some("kcadmin")
    );
    assert_eq!(res.groups[0].accounts[0].key, None);
    let users = &res.groups[1].accounts;
    assert_eq!(users[0].username.as_deref(), Some("beluga-admin"));
    assert_eq!(users[0].key.as_deref(), Some("admin"));
    assert_eq!(users[1].username.as_deref(), Some("beluga-analyst"));
    assert_eq!(users[1].password.expose_for_test(), "b-pw");
}

#[tokio::test]
async fn sso_username_template_default_keeps_the_key_and_skips_oversized_results() {
    let long_key = "k".repeat(60);
    let runner = KcRunner::default().secret(
        "iam",
        "keycloak-user-passwords",
        &[("admin", "a-pw"), (long_key.as_str(), "b-pw")],
    );
    let res = reveal("tmpl-default", &runner, vec![keycloak_ep()], 0)
        .await
        .unwrap();
    assert_eq!(names(&res.groups[1]), ["admin", long_key.as_str()]);

    // 60-char key + 8-char prefix exceeds the safe-username bound: that account is dropped.
    let res = reveal_with_template(
        "tmpl-long",
        &runner,
        vec![keycloak_ep()],
        0,
        Some("prefix01-{key}"),
    )
    .await
    .unwrap();
    assert_eq!(names(&res.groups[1]), ["prefix01-admin"]);
}

fn issuer_env(url: &str) -> serde_json::Value {
    serde_json::json!([{"name": "OIDC_ISSUER", "value": url}])
}

#[tokio::test]
async fn disagreeing_issuers_across_pods_or_envs_give_no_evidence() {
    let pod = |env: serde_json::Value| serde_json::json!({"spec": {"containers": [{"name": "a", "env": env}]}});
    let a = "https://sso.example.internal/realms/one";
    let b = "https://sso.example.internal/realms/two";
    // Rollout: two pods, two realms.
    assert_eq!(
        keycloak::workload_issuer(&[pod(issuer_env(a)), pod(issuer_env(b))]),
        None
    );
    // One container, two ISSUER envs that disagree.
    let both = serde_json::json!([
        {"name": "OIDC_ISSUER", "value": a},
        {"name": "AUTH_ISSUER_URL", "value": b}]);
    assert_eq!(keycloak::workload_issuer(&[pod(both)]), None);
    // Agreeing pods still resolve.
    assert_eq!(
        keycloak::workload_issuer(&[pod(issuer_env(a)), pod(issuer_env(a))]),
        Some(("sso.example.internal".into(), "one".into()))
    );
}

#[tokio::test(start_paused = true)]
async fn a_late_backend_in_the_same_batch_does_not_discard_the_fast_ones_evidence() {
    let mut runner = KcRunner::default()
        .pod_env(
            "fast",
            issuer_env("https://sso.example.internal/realms/beluga"),
        )
        .pod_env(
            "slow",
            issuer_env("https://sso.example.internal/realms/beluga"),
        );
    runner.slow_ns = Some(("slow".into(), std::time::Duration::from_secs(100)));
    let endpoints = vec![
        ep("fast", "quick", "fast.example.internal", false),
        ep("slow", "laggard", "slow.example.internal", false),
        keycloak_ep(),
    ];
    let paths = temp_paths("partial-batch");
    seed_profile_and_endpoints(&paths, "lab", kubeconfig(), endpoints);
    let opts = endpoint_login_options_with_deps(&runner, &paths, "lab")
        .await
        .unwrap();
    let _ = std::fs::remove_dir_all(&paths.base);
    let fast = opts.iter().find(|o| o.service_name == "quick").unwrap();
    assert_eq!(
        fast.sso,
        Some(SsoHint {
            realm: Some("beluga".into())
        })
    );
    assert!(opts.iter().all(|o| o.service_name != "laggard"));
}
