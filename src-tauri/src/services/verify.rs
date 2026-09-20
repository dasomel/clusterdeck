#![allow(dead_code)]

use crate::services::process::CommandRunner;
use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VerificationResult {
    pub ssh: bool,
    pub kubeconfig: bool,
    pub kubernetes: bool,
    pub node_count: Option<u32>,
    pub kubernetes_version: Option<String>,
    pub api_endpoint: Option<String>,
    pub last_verified: Option<String>,
}

fn read_api_endpoint(kubeconfig_path: &Path) -> Option<String> {
    let raw = std::fs::read_to_string(kubeconfig_path).ok()?;
    let value: serde_yaml::Value = serde_yaml::from_str(&raw).ok()?;
    value
        .get("clusters")?
        .get(0)?
        .get("cluster")?
        .get("server")?
        .as_str()
        .map(|s| s.to_string())
}

fn parse_nodes_json(stdout: &str, api_endpoint: Option<String>) -> Option<VerificationResult> {
    let value: serde_json::Value = serde_json::from_str(stdout).ok()?;
    let items = value.get("items")?.as_array()?;
    let kubernetes_version = items
        .first()
        .and_then(|item| item.get("status"))
        .and_then(|status| status.get("nodeInfo"))
        .and_then(|node_info| node_info.get("kubeletVersion"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    Some(VerificationResult {
        ssh: false,
        kubeconfig: false,
        kubernetes: true,
        node_count: Some(items.len() as u32),
        kubernetes_version,
        api_endpoint,
        last_verified: Some(Utc::now().to_rfc3339()),
    })
}

pub async fn verify_cluster_detailed(
    runner: &dyn CommandRunner,
    kubeconfig_path: &Path,
    context: &str,
) -> (VerificationResult, Option<String>) {
    let args = vec![
        "--kubeconfig".to_string(),
        kubeconfig_path.to_string_lossy().to_string(),
        "--context".to_string(),
        context.to_string(),
        "get".to_string(),
        "nodes".to_string(),
        "-o".to_string(),
        "json".to_string(),
    ];

    let api_endpoint = read_api_endpoint(kubeconfig_path);

    // 1. Try kubectl first
    let kubectl_result = runner.run("kubectl", &args).await;
    match &kubectl_result {
        Ok(output) if output.success => {
            if let Some(res) = parse_nodes_json(&output.stdout, api_endpoint.clone()) {
                return (res, None);
            }
        }
        _ => {}
    }

    // 2. If kubectl fails (e.g. macOS Sequoia Local Network Privacy blocks third-party socket,
    // or kubectl is not installed), fall back to system curl which bypasses LNP restrictions
    if let Ok(stdout) =
        crate::services::k8s_endpoints::curl_k8s_api(runner, kubeconfig_path, "/api/v1/nodes").await
    {
        if let Some(res) = parse_nodes_json(&stdout, api_endpoint.clone()) {
            return (res, None);
        }
    }

    // 3. If both failed, report the step-1 kubectl error
    let err_msg = match kubectl_result {
        Ok(output) if !output.stderr.trim().is_empty() => {
            let raw = output.stderr.trim();
            if raw.contains("no route to host") {
                format!("{raw} (macOS Sequoia: Check System Settings > Privacy & Security > Local Network)")
            } else {
                raw.to_string()
            }
        }
        Ok(_) => "kubectl exited with non-zero status".to_string(),
        Err(e) => format!("Failed to execute kubectl: {e}"),
    };

    (
        VerificationResult {
            ssh: false,
            kubeconfig: false,
            kubernetes: false,
            node_count: None,
            kubernetes_version: None,
            api_endpoint,
            last_verified: None,
        },
        Some(err_msg),
    )
}

pub async fn verify_cluster(
    runner: &dyn CommandRunner,
    kubeconfig_path: &Path,
    context: &str,
) -> VerificationResult {
    verify_cluster_detailed(runner, kubeconfig_path, context)
        .await
        .0
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::process::CommandOutput;
    use async_trait::async_trait;

    struct FakeRunner {
        nodes_json: &'static str,
        success: bool,
    }

    #[async_trait]
    impl CommandRunner for FakeRunner {
        async fn run(&self, _bin: &str, _args: &[String]) -> Result<CommandOutput, String> {
            Ok(CommandOutput {
                stdout: self.nodes_json.into(),
                stderr: String::new(),
                success: self.success,
            })
        }
    }

    #[tokio::test]
    async fn verify_cluster_counts_nodes_on_success() {
        let runner = FakeRunner {
            nodes_json: r#"{"items":[{},{},{}]}"#,
            success: true,
        };
        let result = verify_cluster(&runner, Path::new("/tmp/kc.yaml"), "cka").await;
        assert!(result.kubernetes);
        assert_eq!(result.node_count, Some(3));
        assert!(result.last_verified.is_some());
    }

    #[tokio::test]
    async fn verify_cluster_parses_kubernetes_version_from_node_info() {
        let runner = FakeRunner {
            nodes_json: r#"{"items":[{"status":{"nodeInfo":{"kubeletVersion":"v1.35.2"}}}]}"#,
            success: true,
        };
        let result = verify_cluster(&runner, Path::new("/tmp/kc.yaml"), "cka").await;
        assert_eq!(result.kubernetes_version, Some("v1.35.2".to_string()));
    }

    #[tokio::test]
    async fn verify_cluster_reports_no_kubernetes_version_when_items_empty() {
        let runner = FakeRunner {
            nodes_json: r#"{"items":[]}"#,
            success: true,
        };
        let result = verify_cluster(&runner, Path::new("/tmp/kc.yaml"), "cka").await;
        assert_eq!(result.kubernetes_version, None);
    }

    #[tokio::test]
    async fn verify_cluster_populates_api_endpoint_from_kubeconfig_file() {
        let temp_dir =
            std::env::temp_dir().join(format!("clusterdeck-verify-test-{}", std::process::id()));
        std::fs::create_dir_all(&temp_dir).unwrap();
        let kubeconfig_path = temp_dir.join("kubeconfig.yaml");
        std::fs::write(
            &kubeconfig_path,
            "clusters:\n  - name: cka\n    cluster:\n      server: https://192.0.2.10:6443\n",
        )
        .unwrap();

        let runner = FakeRunner {
            nodes_json: r#"{"items":[]}"#,
            success: true,
        };
        let result = verify_cluster(&runner, &kubeconfig_path, "cka").await;
        assert_eq!(
            result.api_endpoint,
            Some("https://192.0.2.10:6443".to_string())
        );

        std::fs::remove_dir_all(&temp_dir).ok();
    }

    #[tokio::test]
    async fn verify_cluster_reports_false_on_kubectl_failure() {
        let runner = FakeRunner {
            nodes_json: "",
            success: false,
        };
        let result = verify_cluster(&runner, Path::new("/tmp/kc.yaml"), "cka").await;
        assert!(!result.kubernetes);
        assert_eq!(result.node_count, None);
    }

    struct FakeErrorRunner {
        stderr: &'static str,
    }

    #[async_trait]
    impl CommandRunner for FakeErrorRunner {
        async fn run(&self, _bin: &str, _args: &[String]) -> Result<CommandOutput, String> {
            Ok(CommandOutput {
                stdout: String::new(),
                stderr: self.stderr.to_string(),
                success: false,
            })
        }
    }

    #[tokio::test]
    async fn verify_cluster_detailed_captures_kubectl_stderr() {
        let runner = FakeErrorRunner {
            stderr: "dial tcp 172.16.221.133:6443: connect: no route to host\n",
        };
        let (result, err) =
            verify_cluster_detailed(&runner, Path::new("/tmp/kc.yaml"), "dev").await;
        assert!(!result.kubernetes);
        assert_eq!(
            err,
            Some("dial tcp 172.16.221.133:6443: connect: no route to host (macOS Sequoia: Check System Settings > Privacy & Security > Local Network)".to_string())
        );
    }

    struct FakeRunnerWithCurlFallback {
        kubectl_stderr: &'static str,
        curl_nodes_json: &'static str,
    }

    #[async_trait]
    impl CommandRunner for FakeRunnerWithCurlFallback {
        async fn run(&self, bin: &str, _args: &[String]) -> Result<CommandOutput, String> {
            if bin == "kubectl" {
                Ok(CommandOutput {
                    stdout: String::new(),
                    stderr: self.kubectl_stderr.to_string(),
                    success: false,
                })
            } else if bin == "curl" {
                Ok(CommandOutput {
                    stdout: self.curl_nodes_json.to_string(),
                    stderr: String::new(),
                    success: true,
                })
            } else {
                Err(format!("unknown binary {bin}"))
            }
        }
    }

    #[tokio::test]
    async fn verify_cluster_detailed_falls_back_to_curl_on_kubectl_failure() {
        let temp_dir = std::env::temp_dir().join(format!(
            "cd_verify_curl_{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&temp_dir).unwrap();
        let kc_path = temp_dir.join("config.yaml");
        let kc_content = r#"
apiVersion: v1
clusters:
- cluster:
    server: https://192.168.77.10:6443
  name: dev
contexts:
- context:
    cluster: dev
    user: dev
  name: dev
current-context: dev
users:
- name: dev
  user:
    token: mytoken
"#;
        std::fs::write(&kc_path, kc_content).unwrap();

        let runner = FakeRunnerWithCurlFallback {
            kubectl_stderr: "dial tcp 192.168.77.10:6443: connect: no route to host\n",
            curl_nodes_json: r#"{"items":[{"status":{"nodeInfo":{"kubeletVersion":"v1.36.4+k3s1"}}},{"status":{"nodeInfo":{"kubeletVersion":"v1.36.4+k3s1"}}}]}"#,
        };

        let (result, err) = verify_cluster_detailed(&runner, &kc_path, "dev").await;
        assert!(result.kubernetes);
        assert_eq!(result.node_count, Some(2));
        assert_eq!(result.kubernetes_version.as_deref(), Some("v1.36.4+k3s1"));
        assert_eq!(err, None);

        let _ = std::fs::remove_dir_all(temp_dir);
    }
}
