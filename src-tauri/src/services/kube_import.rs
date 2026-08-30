#![allow(dead_code)]

use serde::Serialize;
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct LocalKubeContext {
    pub context_name: String,
    pub cluster_name: String,
    pub user_name: String,
    pub server: String,
}

pub fn parse_kube_contexts(raw_yaml: &str) -> Result<Vec<LocalKubeContext>, String> {
    let val: serde_yaml::Value =
        serde_yaml::from_str(raw_yaml).map_err(|e| format!("Invalid YAML format: {e}"))?;

    let contexts_seq = match val.get("contexts") {
        Some(v) => v
            .as_sequence()
            .ok_or_else(|| "contexts is not a sequence".to_string())?,
        None => return Err("contexts field missing or not a sequence".to_string()),
    };

    let clusters_seq = val.get("clusters").and_then(|v| v.as_sequence());
    let users_seq = val.get("users").and_then(|v| v.as_sequence());

    let mut results = Vec::new();

    for ctx_item in contexts_seq {
        let ctx_name = match ctx_item.get("name").and_then(|v| v.as_str()) {
            Some(n) => n,
            None => continue,
        };

        let ctx_inner = match ctx_item.get("context") {
            Some(inner) => inner,
            None => continue,
        };

        let cluster_ref = match ctx_inner.get("cluster").and_then(|v| v.as_str()) {
            Some(c) => c,
            None => continue,
        };

        let user_ref = match ctx_inner.get("user").and_then(|v| v.as_str()) {
            Some(u) => u,
            None => continue,
        };

        let server = match clusters_seq {
            Some(seq) => {
                let found = seq
                    .iter()
                    .find(|item| item.get("name").and_then(|v| v.as_str()) == Some(cluster_ref));
                match found
                    .and_then(|item| item.get("cluster"))
                    .and_then(|v| v.get("server"))
                    .and_then(|v| v.as_str())
                {
                    Some(s) => s,
                    None => continue,
                }
            }
            None => continue,
        };

        let user_found = match users_seq {
            Some(seq) => seq
                .iter()
                .any(|item| item.get("name").and_then(|v| v.as_str()) == Some(user_ref)),
            None => false,
        };

        if !user_found {
            continue;
        }

        results.push(LocalKubeContext {
            context_name: ctx_name.to_string(),
            cluster_name: cluster_ref.to_string(),
            user_name: user_ref.to_string(),
            server: server.to_string(),
        });
    }

    Ok(results)
}

pub fn read_local_kubeconfig_path() -> PathBuf {
    if let Ok(val) = std::env::var("KUBECONFIG") {
        if let Some(first) = val.split(':').next() {
            let trimmed = first.trim();
            if !trimmed.is_empty() {
                return PathBuf::from(trimmed);
            }
        }
    }
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
    PathBuf::from(home).join(".kube").join("config")
}

pub fn list_local_kube_contexts() -> Result<Vec<LocalKubeContext>, String> {
    let path = read_local_kubeconfig_path();
    match std::fs::read_to_string(&path) {
        Ok(raw_yaml) => parse_kube_contexts(&raw_yaml),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(e) => Err(format!(
            "Failed to read kubeconfig file at {}: {e}",
            path.display()
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"
apiVersion: v1
kind: Config
clusters:
  - name: colima
    cluster:
      server: https://127.0.0.1:56993
  - name: prod
    cluster:
      server: https://prod.example.invalid:6443
contexts:
  - name: colima
    context:
      cluster: colima
      user: colima
  - name: prod-ctx
    context:
      cluster: prod
      user: prod-user
  - name: broken-ctx
    context:
      cluster: does-not-exist
      user: nobody
current-context: colima
users:
  - name: colima
    user: {}
  - name: prod-user
    user: {}
"#;

    #[test]
    fn parse_kube_contexts_resolves_cluster_and_user_by_name() {
        let contexts = parse_kube_contexts(SAMPLE).unwrap();
        let colima = contexts
            .iter()
            .find(|c| c.context_name == "colima")
            .unwrap();
        assert_eq!(colima.cluster_name, "colima");
        assert_eq!(colima.user_name, "colima");
        assert_eq!(colima.server, "https://127.0.0.1:56993");
    }

    #[test]
    fn parse_kube_contexts_skips_context_with_unresolvable_cluster() {
        let contexts = parse_kube_contexts(SAMPLE).unwrap();
        assert!(!contexts.iter().any(|c| c.context_name == "broken-ctx"));
        // the two resolvable contexts must still come through
        assert_eq!(contexts.len(), 2);
    }

    #[test]
    fn parse_kube_contexts_errors_on_garbage_input() {
        assert!(parse_kube_contexts("not: [valid, yaml: at all: :::").is_err());
    }

    #[test]
    fn read_local_kubeconfig_path_defaults_to_home_dot_kube_config() {
        // Only assert the fallback shape, don't depend on real $HOME contents.
        let path = read_local_kubeconfig_path();
        assert!(
            path.to_string_lossy().ends_with(".kube/config") || std::env::var("KUBECONFIG").is_ok()
        );
    }
}
