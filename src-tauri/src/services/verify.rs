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

pub async fn verify_cluster(
    runner: &dyn CommandRunner,
    kubeconfig_path: &Path,
    context: &str,
) -> VerificationResult {
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

    match runner.run("kubectl", &args).await {
        Ok(output) => {
            let last_verified = Some(Utc::now().to_rfc3339());
            if output.success {
                if let Ok(value) = serde_json::from_str::<serde_json::Value>(&output.stdout) {
                    if let Some(items) = value.get("items").and_then(|v| v.as_array()) {
                        let kubernetes_version = items
                            .first()
                            .and_then(|item| item.get("status"))
                            .and_then(|status| status.get("nodeInfo"))
                            .and_then(|node_info| node_info.get("kubeletVersion"))
                            .and_then(|v| v.as_str())
                            .map(|s| s.to_string());
                        return VerificationResult {
                            ssh: false,
                            kubeconfig: false,
                            kubernetes: true,
                            node_count: Some(items.len() as u32),
                            kubernetes_version,
                            api_endpoint,
                            last_verified,
                        };
                    }
                }
            }
            VerificationResult {
                ssh: false,
                kubeconfig: false,
                kubernetes: false,
                node_count: None,
                kubernetes_version: None,
                api_endpoint,
                last_verified,
            }
        }
        Err(_) => VerificationResult {
            ssh: false,
            kubeconfig: false,
            kubernetes: false,
            node_count: None,
            kubernetes_version: None,
            api_endpoint,
            last_verified: None,
        },
    }
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
}
