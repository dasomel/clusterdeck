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
    pub api_endpoint: Option<String>,
    pub last_verified: Option<String>,
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

    match runner.run("kubectl", &args).await {
        Ok(output) => {
            let last_verified = Some(Utc::now().to_rfc3339());
            if output.success {
                if let Ok(value) = serde_json::from_str::<serde_json::Value>(&output.stdout) {
                    if let Some(items) = value.get("items").and_then(|v| v.as_array()) {
                        return VerificationResult {
                            ssh: false,
                            kubeconfig: false,
                            kubernetes: true,
                            node_count: Some(items.len() as u32),
                            api_endpoint: None,
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
                api_endpoint: None,
                last_verified,
            }
        }
        Err(_) => VerificationResult {
            ssh: false,
            kubeconfig: false,
            kubernetes: false,
            node_count: None,
            api_endpoint: None,
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
