use super::*;
use crate::services::config::{AuthMode, BootstrapPolicy, Host, KubeconfigSource, Profile};
use crate::services::process::CommandOutput;
use base64::prelude::*;
use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Mutex;

struct FakeLoginRunner {
    calls: Mutex<Vec<(String, Vec<String>)>>,
    can_i_res: Result<CommandOutput, String>,
    get_secret_res: Result<CommandOutput, String>,
    service_res: Result<CommandOutput, String>,
    upstream_res: Result<CommandOutput, String>,
}

impl FakeLoginRunner {
    fn new(
        can_i_res: Result<CommandOutput, String>,
        get_secret_res: Result<CommandOutput, String>,
    ) -> Self {
        Self {
            calls: Mutex::new(Vec::new()),
            can_i_res,
            get_secret_res,
            service_res: json_output(serde_json::json!({"spec": {"type": "ClusterIP"}})),
            upstream_res: json_output(serde_json::json!({"spec": {"externalNodes": [
                {"type": "Domain", "name": "argocd-server.argocd.svc"}
            ]}})),
        }
    }

    fn recorded_calls(&self) -> Vec<(String, Vec<String>)> {
        self.calls.lock().unwrap().clone()
    }
}

#[async_trait::async_trait]
impl CommandRunner for FakeLoginRunner {
    async fn run(&self, bin: &str, args: &[String]) -> Result<CommandOutput, String> {
        self.calls
            .lock()
            .unwrap()
            .push((bin.to_string(), args.to_vec()));
        if args.iter().any(|a| a == "can-i") {
            self.can_i_res.clone()
        } else if args.iter().any(|a| a == "service") {
            self.service_res.clone()
        } else if args
            .iter()
            .any(|a| a == "apisixupstreams.apisix.apache.org")
        {
            self.upstream_res.clone()
        } else {
            self.get_secret_res.clone()
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

pub(super) fn temp_paths(tag: &str) -> ClusterDeckPaths {
    let dir = std::env::temp_dir().join(format!(
        "clusterdeck-service-login-test-{tag}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let paths = ClusterDeckPaths::at(dir);
    paths.ensure_dirs().unwrap();
    paths
}

pub(super) fn seed_profile_and_endpoints(
    paths: &ClusterDeckPaths,
    profile_id: &str,
    kubeconfig_yaml: &str,
    endpoints: Vec<DiscoveredEndpoint>,
) {
    let profile = Profile {
        id: profile_id.to_string(),
        name: "Test Profile".to_string(),
        hosts: vec![Host {
            name: "m1".to_string(),
            address: "192.0.2.10".to_string(),
            port: 22,
            user: "root".to_string(),
            identity_file: None,
            auth: AuthMode::Key,
        }],
        bastion: None,
        bootstrap: BootstrapPolicy::default(),
        kubeconfig: Some(KubeconfigSource {
            remote_path: String::new(),
            control_plane: "m1".to_string(),
            local_path: String::new(),
            context: format!("{profile_id}-ctx"),
        }),
        host_mapping: crate::services::config::HostMapping::Off,
        approved_dns_zones: Vec::new(),
        gateway_address: None,
        sso_username_template: None,
        trusted_cas: Vec::new(),
        local_runtime: None,
    };
    store::save_profiles(paths, &[profile]).unwrap();
    std::fs::write(paths.kubeconfig_file(profile_id), kubeconfig_yaml).unwrap();

    let cache_path = paths.endpoints_cache_file();
    if let Some(parent) = cache_path.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    let mut map = BTreeMap::new();
    map.insert(profile_id.to_string(), endpoints);
    std::fs::write(&cache_path, serde_json::to_string(&map).unwrap()).unwrap();
}

fn clean_kubeconfig(context: &str) -> String {
    format!(
        "apiVersion: v1\ncurrent-context: {context}\ncontexts:\n- name: {context}\n  context:\n    cluster: c\n    user: u\nusers:\n- name: u\n  user:\n    token: fake-test-token\n"
    )
}

pub(super) fn secret_json(entries: &[(&str, &str)]) -> String {
    let mut data = serde_json::Map::new();
    for (k, v) in entries {
        data.insert(
            (*k).to_string(),
            serde_json::Value::String(BASE64_STANDARD.encode(v.as_bytes())),
        );
    }
    serde_json::json!({
        "apiVersion": "v1",
        "kind": "Secret",
        "data": data
    })
    .to_string()
}

#[test]
fn lookup_service_login_matches_the_d2_entries_and_rejects_unknown() {
    let argo = lookup_service_login("argocd-server").unwrap();
    assert_eq!(argo.secret_name, "argocd-initial-admin-secret");
    assert_eq!(argo.username, UsernameSource::Fixed("admin"));
    assert_eq!(argo.password_key, "password");

    for svc in ["grafana", "kube-prom-stack-grafana", "my-release-grafana"] {
        let g = lookup_service_login(svc).unwrap();
        assert_eq!(g.secret_name, svc);
        assert_eq!(g.username, UsernameSource::SecretKey("admin-user"));
        assert_eq!(g.password_key, "admin-password");
    }
    assert!(lookup_service_login("-grafana").is_none());

    // The Keycloak bootstrap admin is temporary and usually deleted, so it is not offered.
    assert!(lookup_service_login("keycloak").is_none());
    assert!(lookup_service_login("keycloak-service").is_none());

    for svc in ["harbor-core", "harbor"] {
        let h = lookup_service_login(svc).unwrap();
        assert_eq!(h.secret_name, "harbor-secrets");
        assert_eq!(h.username, UsernameSource::Fixed("admin"));
        assert_eq!(h.password_key, "HARBOR_ADMIN_PASSWORD");
    }

    let gitea = lookup_service_login("gitea-http").unwrap();
    assert_eq!(gitea.secret_name, "gitea-admin");
    assert_eq!(gitea.username, UsernameSource::SecretKey("username"));
    assert_eq!(gitea.password_key, "admin-password");

    let superset = lookup_service_login("superset").unwrap();
    assert_eq!(superset.secret_name, "superset-credential");
    assert_eq!(superset.username, UsernameSource::Fixed("admin"));
    assert_eq!(superset.password_key, "admin-password");

    assert!(lookup_service_login("trino").is_none());
    assert!(lookup_service_login("harbor-portal").is_none());
    assert!(lookup_service_login("Unknown_Service").is_none());
}

#[tokio::test]
async fn reveal_endpoint_login_decodes_each_d2_service_and_handles_missing_gitea_username() {
    let cases = [
        (
            "argocd",
            "argocd-server",
            vec![("password", "fake-argo-pw")],
            Some("admin"),
            "fake-argo-pw",
        ),
        (
            "monitoring",
            "prom-grafana",
            vec![
                ("admin-user", "graf-admin"),
                ("admin-password", "fake-graf-pw"),
            ],
            Some("graf-admin"),
            "fake-graf-pw",
        ),
        (
            "registry",
            "harbor",
            vec![("HARBOR_ADMIN_PASSWORD", "fake-harbor-pw")],
            Some("admin"),
            "fake-harbor-pw",
        ),
        (
            "registry",
            "harbor-core",
            vec![("HARBOR_ADMIN_PASSWORD", "fake-harbor-pw")],
            Some("admin"),
            "fake-harbor-pw",
        ),
        (
            "git",
            "gitea-http",
            vec![("admin-password", "fake-gitea-pw")],
            None,
            "fake-gitea-pw",
        ),
        (
            "bi",
            "superset",
            vec![("admin-password", "fake-superset-pw")],
            Some("admin"),
            "fake-superset-pw",
        ),
    ];

    for (ns, svc, secret_kv, expected_user, expected_pw) in cases {
        let paths = temp_paths(svc);
        let ep = DiscoveredEndpoint {
            id: DiscoveredEndpoint::compute_id(
                "ingress",
                &format!("{ns}/{svc}-ing"),
                &format!("{svc}.example.internal"),
            ),
            host: format!("{svc}.example.internal"),
            ip: "192.0.2.20".to_string(),
            source: "ingress".to_string(),
            resource_name: format!("{ns}/{svc}-ing"),
            backend_service: Some(format!("{ns}/{svc}")),
            backend_upstream: None,
            sso_oidc: false,
        };
        seed_profile_and_endpoints(
            &paths,
            "lab",
            &clean_kubeconfig("lab-ctx"),
            vec![ep.clone()],
        );
        let runner = FakeLoginRunner::new(
            Ok(CommandOutput {
                stdout: "yes\n".to_string(),
                stderr: String::new(),
                success: true,
            }),
            Ok(CommandOutput {
                stdout: secret_json(&secret_kv),
                stderr: String::new(),
                success: true,
            }),
        );
        let res = reveal_endpoint_login_with_deps(&runner, &paths, "lab", &ep.id)
            .await
            .unwrap();
        assert_eq!(
            res.account().username.as_deref(),
            expected_user,
            "service {svc}"
        );
        assert_eq!(
            res.account().password.expose_for_test(),
            expected_pw,
            "service {svc}"
        );
        let dbg = format!("{res:?}");
        assert!(
            !dbg.contains(expected_pw),
            "Debug output must redact password: {dbg}"
        );
        assert!(dbg.contains("<redacted>"));

        let calls = runner.recorded_calls();
        assert_eq!(calls.len(), 3);
        assert!(calls[0].1.windows(2).any(|w| w == ["--context", "lab-ctx"]));
        assert!(calls[0].1.iter().any(|a| a == "--kubeconfig"));
        assert!(calls[1].1.windows(2).any(|w| w == ["--context", "lab-ctx"]));
        let _ = std::fs::remove_dir_all(&paths.base);
    }
}

#[tokio::test]
async fn impersonation_fields_are_refused_before_any_kubectl_call() {
    let payloads = [
        "apiVersion: v1\ncurrent-context: lab-ctx\ncontexts:\n- name: lab-ctx\n  context:\n    user: u\nusers:\n- name: u\n  user:\n    token: t\n    as: system:admin\n",
        "apiVersion: v1\ncurrent-context: lab-ctx\ncontexts:\n- name: lab-ctx\n  context:\n    user: u\nusers:\n- name: u\n  user:\n    token: t\n    as-groups:\n    - system:masters\n",
        "apiVersion: v1\ncurrent-context: lab-ctx\ncontexts:\n- name: lab-ctx\n  context:\n    user: u\nusers:\n- name: u\n  user:\n    token: t\n    as-user-extra:\n      scopes:\n      - admin\n",
    ];
    for (idx, kc) in payloads.iter().enumerate() {
        let paths = temp_paths(&format!("impersonation-{idx}"));
        let ep = DiscoveredEndpoint {
            id: "ingress:argocd/argocd-ing:argocd.example.internal".to_string(),
            host: "argocd.example.internal".to_string(),
            ip: "192.0.2.20".to_string(),
            source: "ingress".to_string(),
            resource_name: "argocd/argocd-ing".to_string(),
            backend_service: Some("argocd/argocd-server".to_string()),
            backend_upstream: None,
            sso_oidc: false,
        };
        seed_profile_and_endpoints(&paths, "lab", kc, vec![ep.clone()]);
        let runner = FakeLoginRunner::new(
            Ok(CommandOutput {
                stdout: "yes".into(),
                stderr: String::new(),
                success: true,
            }),
            Ok(CommandOutput {
                stdout: "{}".into(),
                stderr: String::new(),
                success: true,
            }),
        );
        let err = reveal_endpoint_login_with_deps(&runner, &paths, "lab", &ep.id)
            .await
            .unwrap_err();
        assert_eq!(err, LoginErrorCode::ImpersonationRefused);
        assert!(runner.recorded_calls().is_empty());
        let _ = std::fs::remove_dir_all(&paths.base);
    }
}

#[tokio::test]
async fn apisix_cross_namespace_reveal_checks_and_reads_service_namespace() {
    let paths = temp_paths("apisix-cross-ns");
    let ep = DiscoveredEndpoint {
        id: "apisix:platform-system/route:admin.example.test".into(),
        host: "admin.example.test".into(),
        ip: "192.0.2.10".into(),
        source: "apisix".into(),
        resource_name: "platform-system/route".into(),
        backend_service: Some("argocd/argocd-server".into()),
        backend_upstream: Some("platform-system/admin-upstream".into()),
        sso_oidc: false,
    };
    seed_profile_and_endpoints(
        &paths,
        "lab",
        &clean_kubeconfig("lab-ctx"),
        vec![ep.clone()],
    );
    for permitted in [false, true] {
        let runner = FakeLoginRunner::new(
            Ok(CommandOutput {
                stdout: if permitted { "yes" } else { "no" }.into(),
                stderr: String::new(),
                success: permitted,
            }),
            Ok(CommandOutput {
                stdout: secret_json(&[("password", "fake-argo-pw")]),
                stderr: String::new(),
                success: true,
            }),
        );
        let result = reveal_endpoint_login_with_deps(&runner, &paths, "lab", &ep.id).await;
        if permitted {
            let revealed = result.unwrap();
            assert_eq!(revealed.namespace, "argocd");
            assert_eq!(revealed.route_namespace, "platform-system");
            assert_eq!(revealed.route_resource, "platform-system/route");
            assert_eq!(revealed.host, "admin.example.test");
        } else {
            assert_eq!(result.unwrap_err(), LoginErrorCode::Forbidden);
        }
        let calls = runner.recorded_calls();
        assert_eq!(calls.len(), if permitted { 4 } else { 3 });
        for (_, args) in calls {
            let ns = if args
                .iter()
                .any(|arg| arg == "apisixupstreams.apisix.apache.org")
            {
                "platform-system"
            } else {
                "argocd"
            };
            assert!(args.windows(2).any(|w| w == ["-n", ns]));
        }
    }
    std::fs::remove_dir_all(&paths.base).unwrap();
}

#[tokio::test]
async fn unknown_endpoint_id_and_cross_namespace_backend_make_zero_kubectl_calls() {
    let paths = temp_paths("forged-id");
    let cross_ns = DiscoveredEndpoint {
        id: "gateway-api:tenant/route:argocd.example.internal".to_string(),
        host: "argocd.example.internal".to_string(),
        ip: "192.0.2.20".to_string(),
        source: "gateway-api".to_string(),
        resource_name: "tenant/route".to_string(),
        backend_service: Some("argocd/argocd-server".to_string()),
        backend_upstream: None,
        sso_oidc: false,
    };
    let bad_label = DiscoveredEndpoint {
        id: "ingress:argocd/ing:bad.example.internal".to_string(),
        host: "bad.example.internal".to_string(),
        ip: "192.0.2.20".to_string(),
        source: "ingress".to_string(),
        resource_name: "argocd/ing".to_string(),
        backend_service: Some("argocd/--evil".to_string()),
        backend_upstream: None,
        sso_oidc: false,
    };
    seed_profile_and_endpoints(
        &paths,
        "lab",
        &clean_kubeconfig("lab-ctx"),
        vec![cross_ns.clone(), bad_label.clone()],
    );
    let runner = FakeLoginRunner::new(
        Ok(CommandOutput {
            stdout: "yes".into(),
            stderr: String::new(),
            success: true,
        }),
        Ok(CommandOutput {
            stdout: "{}".into(),
            stderr: String::new(),
            success: true,
        }),
    );

    assert_eq!(
        reveal_endpoint_login_with_deps(&runner, &paths, "lab", "forged:argocd/secret:host")
            .await
            .unwrap_err(),
        LoginErrorCode::NotFound
    );
    assert_eq!(
        reveal_endpoint_login_with_deps(&runner, &paths, "lab", &cross_ns.id)
            .await
            .unwrap_err(),
        LoginErrorCode::UnsupportedService
    );
    assert_eq!(
        reveal_endpoint_login_with_deps(&runner, &paths, "lab", &bad_label.id)
            .await
            .unwrap_err(),
        LoginErrorCode::UnsupportedService
    );
    assert!(runner.recorded_calls().is_empty());
    let _ = std::fs::remove_dir_all(&paths.base);
}

#[tokio::test]
async fn can_i_no_returns_forbidden_without_invoking_get_secret_and_errors_never_leak_output() {
    let paths = temp_paths("rbac-forbidden");
    let ep = DiscoveredEndpoint {
        id: "ingress:argocd/argocd-ing:argocd.example.internal".to_string(),
        host: "argocd.example.internal".to_string(),
        ip: "192.0.2.20".to_string(),
        source: "ingress".to_string(),
        resource_name: "argocd/argocd-ing".to_string(),
        backend_service: Some("argocd/argocd-server".to_string()),
        backend_upstream: None,
        sso_oidc: false,
    };
    seed_profile_and_endpoints(
        &paths,
        "lab",
        &clean_kubeconfig("lab-ctx"),
        vec![ep.clone()],
    );

    // 1. can-i = "no" -> Forbidden after Service validation (no Secret GET).
    let runner_no = FakeLoginRunner::new(
        Ok(CommandOutput {
            stdout: "no\n".to_string(),
            stderr: "LEAK_MARKER_CAN_I".to_string(),
            success: false,
        }),
        Ok(CommandOutput {
            stdout: secret_json(&[("password", "should-never-read")]),
            stderr: String::new(),
            success: true,
        }),
    );
    let err = reveal_endpoint_login_with_deps(&runner_no, &paths, "lab", &ep.id)
        .await
        .unwrap_err();
    assert_eq!(err, LoginErrorCode::Forbidden);
    assert_eq!(err.as_str(), "forbidden");
    assert_eq!(runner_no.recorded_calls().len(), 2);

    // 2. can-i = "yes", GET = Forbidden -> Forbidden, no stderr leaked.
    let runner_get_forbidden = FakeLoginRunner::new(
        Ok(CommandOutput {
            stdout: "yes\n".to_string(),
            stderr: String::new(),
            success: true,
        }),
        Ok(CommandOutput {
            stdout: "LEAK_MARKER_STDOUT".to_string(),
            stderr: "Error from server (Forbidden): LEAK_MARKER_STDERR".to_string(),
            success: false,
        }),
    );
    let err = reveal_endpoint_login_with_deps(&runner_get_forbidden, &paths, "lab", &ep.id)
        .await
        .unwrap_err();
    assert_eq!(err, LoginErrorCode::Forbidden);
    assert!(!err.to_string().contains("LEAK_MARKER"));

    // 3. Timeout and generic kubectl failure return fixed codes only.
    let runner_timeout = FakeLoginRunner::new(
        Err("command timed out".to_string()),
        Ok(CommandOutput {
            stdout: String::new(),
            stderr: String::new(),
            success: false,
        }),
    );
    let err = reveal_endpoint_login_with_deps(&runner_timeout, &paths, "lab", &ep.id)
        .await
        .unwrap_err();
    assert_eq!(err, LoginErrorCode::Timeout);
    assert!(!err.to_string().contains("LEAK_MARKER"));

    let _ = std::fs::remove_dir_all(&paths.base);
}

#[test]
fn response_debug_redacts_username_and_password() {
    let response = RevealedEndpointLogin {
        endpoint_id: "endpoint".into(),
        route_namespace: "ns".into(),
        route_resource: "ns/route".into(),
        host: "grafana.example.test".into(),
        namespace: "ns".into(),
        service_name: "grafana".into(),
        groups: vec![LoginGroup {
            label: "Admin account".into(),
            namespace: "ns".into(),
            secret_name: "grafana".into(),
            accounts: vec![LoginAccount {
                label: "Admin account".into(),
                key: None,
                username: Some("unique-test-user".into()),
                password: SecretValue::new("unique-test-password".into()),
            }],
            error: None,
        }],
    };
    let debug = format!("{response:?}");
    assert!(!debug.contains("unique-test-user"));
    assert!(!debug.contains("unique-test-password"));
    assert!(debug.contains("<redacted>"));
}

struct SnapshotRunner {
    inner: FakeLoginRunner,
    original: std::path::PathBuf,
    expected: Vec<u8>,
}

#[async_trait::async_trait]
impl CommandRunner for SnapshotRunner {
    async fn run(&self, bin: &str, args: &[String]) -> Result<CommandOutput, String> {
        let path = Path::new(&args[1]);
        assert_eq!(args[0], "--kubeconfig");
        assert_ne!(path, self.original);
        assert_eq!(std::fs::read(path).unwrap(), self.expected);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        // Simulate Connect replacing the source after the impersonation check.
        std::fs::write(
            &self.original,
            "users: [{name: u, user: {as: system:admin}}]",
        )
        .unwrap();
        self.inner.run(bin, args).await
    }
}

#[tokio::test]
async fn kubeconfig_snapshot_is_pinned_and_removed_after_success_and_failures() {
    for outcome in [
        "success",
        "denied",
        "get_failed",
        "can_i_timeout",
        "get_timeout",
    ] {
        let paths = temp_paths(outcome);
        let ep = DiscoveredEndpoint {
            id: "test-endpoint".into(),
            host: "grafana.example.test".into(),
            ip: "192.0.2.10".into(),
            source: "ingress".into(),
            resource_name: "monitoring/route".into(),
            backend_service: Some("monitoring/grafana".into()),
            backend_upstream: None,
            sso_oidc: false,
        };
        let kubeconfig = clean_kubeconfig("lab-ctx");
        seed_profile_and_endpoints(&paths, "lab", &kubeconfig, vec![ep.clone()]);
        let yes = || {
            Ok(CommandOutput {
                stdout: "yes".into(),
                stderr: String::new(),
                success: true,
            })
        };
        let can_i = match outcome {
            "denied" => Ok(CommandOutput {
                stdout: "no".into(),
                stderr: String::new(),
                success: false,
            }),
            "can_i_timeout" => Err("command timed out".into()),
            _ => yes(),
        };
        let get = match outcome {
            "get_failed" => Err("untrusted error".into()),
            "get_timeout" => Err("command timed out".into()),
            _ => Ok(CommandOutput {
                stdout: secret_json(&[
                    ("admin-user", "test-user"),
                    ("admin-password", "test-password"),
                ]),
                stderr: String::new(),
                success: true,
            }),
        };
        let runner = SnapshotRunner {
            inner: FakeLoginRunner::new(can_i, get),
            original: paths.kubeconfig_file("lab"),
            expected: kubeconfig.into_bytes(),
        };
        let result = reveal_endpoint_login_with_deps(&runner, &paths, "lab", &ep.id).await;
        match outcome {
            "success" => assert!(result.is_ok()),
            "denied" => assert_eq!(result.unwrap_err(), LoginErrorCode::Forbidden),
            "get_failed" => assert_eq!(result.unwrap_err(), LoginErrorCode::KubectlFailed),
            _ => assert_eq!(result.unwrap_err(), LoginErrorCode::Timeout),
        }
        let calls = runner.inner.recorded_calls();
        assert!(!calls.is_empty());
        for (_, args) in &calls {
            assert_eq!(args[1], calls[0].1[1]);
            let path = Path::new(&args[1]);
            assert!(path.starts_with(paths.base.join("tmp")));
            assert!(!path.exists(), "snapshot leaked after {outcome}");
        }
        assert_eq!(
            std::fs::read_dir(paths.base.join("tmp")).unwrap().count(),
            0
        );
        std::fs::remove_dir_all(paths.base).unwrap();
    }
}

pub(super) fn json_output(value: serde_json::Value) -> Result<CommandOutput, String> {
    Ok(CommandOutput {
        stdout: value.to_string(),
        stderr: String::new(),
        success: true,
    })
}

#[tokio::test]
async fn live_backend_checks_reject_external_name_missing_and_changed_upstream_before_secret_get() {
    for action_check in [false, true] {
        for scenario in ["external_name", "missing", "changed", "unchanged"] {
            let paths = temp_paths(scenario);
            let ep = DiscoveredEndpoint {
                id: "live-binding".into(),
                source: "apisix".into(),
                resource_name: "platform-system/route".into(),
                backend_service: Some("argocd/argocd-server".into()),
                backend_upstream: Some("platform-system/admin-upstream".into()),
                sso_oidc: false,
                ..Default::default()
            };
            seed_profile_and_endpoints(
                &paths,
                "lab",
                &clean_kubeconfig("lab-ctx"),
                vec![ep.clone()],
            );
            let mut runner = FakeLoginRunner::new(
                Ok(CommandOutput {
                    stdout: "yes".into(),
                    stderr: String::new(),
                    success: true,
                }),
                Ok(CommandOutput {
                    stdout: secret_json(&[("password", "fake-test-password")]),
                    stderr: String::new(),
                    success: true,
                }),
            );
            match scenario {
                "external_name" => {
                    runner.service_res =
                        json_output(serde_json::json!({"spec": {"type": "ExternalName"}}))
                }
                "missing" => {
                    runner.service_res = Ok(CommandOutput {
                        stdout: "LEAK_MARKER".into(),
                        stderr: "NotFound LEAK_MARKER".into(),
                        success: false,
                    })
                }
                "changed" => {
                    runner.upstream_res =
                        json_output(serde_json::json!({"spec": {"externalNodes": [
                            {"type": "Domain", "name": "grafana.monitoring.svc.cluster.local"}
                        ]}}))
                }
                _ => {}
            }
            let result = if action_check {
                check_endpoint_login_with_deps(&runner, &paths, "lab", &ep.id)
                    .await
                    .map(|_| ())
            } else {
                reveal_endpoint_login_with_deps(&runner, &paths, "lab", &ep.id)
                    .await
                    .map(|value| {
                        assert_eq!(
                            value.account().password.expose_for_test(),
                            "fake-test-password"
                        );
                    })
            };
            if scenario == "unchanged" {
                assert_eq!(result, Ok(()));
            } else {
                let error = result.unwrap_err();
                assert_eq!(
                    error.as_str(),
                    if scenario == "missing" {
                        "not_found"
                    } else {
                        "backend_mismatch"
                    }
                );
                assert!(!error.to_string().contains("LEAK_MARKER"));
            }
            let calls = runner.recorded_calls();
            let secret_gets = calls
                .iter()
                .filter(|(_, args)| args.iter().any(|a| a == "secret"))
                .count();
            assert_eq!(
                secret_gets,
                usize::from(scenario == "unchanged" && !action_check)
            );
            for (_, args) in &calls {
                assert_eq!(args[0], "--kubeconfig");
                assert_eq!(args[1], calls[0].1[1]);
                assert_eq!(&args[2..4], ["--context", "lab-ctx"]);
            }
            assert!(calls.iter().any(|(_, args)| args
                .windows(2)
                .any(|w| w == ["apisixupstreams.apisix.apache.org", "admin-upstream"])));
            if scenario != "changed" {
                assert!(calls
                    .iter()
                    .any(|(_, args)| args.windows(2).any(|w| w == ["service", "argocd-server"])));
            }
            std::fs::remove_dir_all(paths.base).unwrap();
        }
    }
}

#[test]
fn options_expose_route_namespace_so_cross_binding_is_detectable() {
    let mk = |route: &str, backend: &str| DiscoveredEndpoint {
        id: format!("apisix:{route}:h.example.test"),
        host: "h.example.test".into(),
        ip: "192.0.2.10".into(),
        source: "apisix".into(),
        resource_name: route.into(),
        backend_service: Some(backend.into()),
        backend_upstream: None,
        sso_oidc: false,
    };
    let opts = endpoint_login_options_for_slice(&[
        mk("platform-system/route", "argocd/argocd-server"),
        mk("argocd/route", "argocd/argocd-server"),
    ]);
    assert_eq!(opts.len(), 2);
    assert_eq!(opts[0].route_namespace, "platform-system");
    assert_eq!(opts[0].route_resource, "platform-system/route");
    assert_eq!(opts[0].host, "h.example.test");
    assert_ne!(opts[0].route_namespace, opts[0].namespace);
    assert_eq!(opts[1].route_namespace, opts[1].namespace);
}
