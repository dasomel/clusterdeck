#![allow(dead_code)]

use crate::services::config::Profile;
use crate::services::paths::ClusterDeckPaths;
use crate::services::process::CommandRunner;
use serde::Serialize;
#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;

#[derive(Debug, Clone, Serialize)]
pub struct KubeconfigSummary {
    pub cluster_name: String,
    pub context_name: String,
    pub local_path: String,
}

pub fn normalize(raw_yaml: &str, profile_id: &str) -> Result<String, String> {
    let mut val: serde_yaml::Value =
        serde_yaml::from_str(raw_yaml).map_err(|e| format!("Invalid YAML format: {e}"))?;

    let clusters_len = val
        .get("clusters")
        .and_then(|v| v.as_sequence())
        .map(|s| s.len());
    let contexts_len = val
        .get("contexts")
        .and_then(|v| v.as_sequence())
        .map(|s| s.len());
    let users_len = val
        .get("users")
        .and_then(|v| v.as_sequence())
        .map(|s| s.len());

    if clusters_len != Some(1) || contexts_len != Some(1) || users_len != Some(1) {
        return Err("multi-cluster kubeconfig is not supported in MVP".to_string());
    }

    let profile_val = serde_yaml::Value::String(profile_id.to_string());

    if let Some(cluster_item) = val.get_mut("clusters").and_then(|v| v.get_mut(0)) {
        if let Some(map) = cluster_item.as_mapping_mut() {
            map.insert(
                serde_yaml::Value::String("name".to_string()),
                profile_val.clone(),
            );
        }
    }

    if let Some(ctx_item) = val.get_mut("contexts").and_then(|v| v.get_mut(0)) {
        if let Some(map) = ctx_item.as_mapping_mut() {
            map.insert(
                serde_yaml::Value::String("name".to_string()),
                profile_val.clone(),
            );
            if let Some(inner) = map.get_mut("context").and_then(|v| v.as_mapping_mut()) {
                inner.insert(
                    serde_yaml::Value::String("cluster".to_string()),
                    profile_val.clone(),
                );
                inner.insert(
                    serde_yaml::Value::String("user".to_string()),
                    profile_val.clone(),
                );
            }
        }
    }

    if let Some(user_item) = val.get_mut("users").and_then(|v| v.get_mut(0)) {
        if let Some(map) = user_item.as_mapping_mut() {
            map.insert(
                serde_yaml::Value::String("name".to_string()),
                profile_val.clone(),
            );
        }
    }

    if let Some(map) = val.as_mapping_mut() {
        map.insert(
            serde_yaml::Value::String("current-context".to_string()),
            profile_val,
        );
    }

    serde_yaml::to_string(&val).map_err(|e| format!("Failed to serialize normalized YAML: {e}"))
}

struct TempFileCleaner<'a>(&'a std::path::Path);
impl<'a> Drop for TempFileCleaner<'a> {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(self.0);
    }
}

pub async fn fetch_and_store(
    runner: &dyn CommandRunner,
    paths: &ClusterDeckPaths,
    profile: &Profile,
) -> Result<KubeconfigSummary, String> {
    if !crate::services::validate::is_safe_profile_id(&profile.id) {
        return Err("invalid profile id".to_string());
    }

    let kube_source = profile
        .kubeconfig
        .as_ref()
        .ok_or_else(|| "Profile has no kubeconfig configuration".to_string())?;

    let host = profile
        .hosts
        .iter()
        .find(|h| h.name == kube_source.control_plane)
        .ok_or_else(|| {
            format!(
                "Control plane host '{}' not found in profile hosts",
                kube_source.control_plane
            )
        })?;

    let alias = crate::services::ssh_config::ssh_alias(&profile.id, &host.name);
    let ssh_conf_path = paths.ssh_conf(&profile.id);

    let tmp_filename = format!("clusterdeck-kc-{}-{}.tmp", profile.id, std::process::id());
    let tmp_path = std::env::temp_dir().join(tmp_filename);
    let _cleaner = TempFileCleaner(&tmp_path);

    let scp_args = vec![
        "-F".to_string(),
        ssh_conf_path.to_string_lossy().to_string(),
        format!("{alias}:{}", kube_source.remote_path),
        tmp_path.to_string_lossy().to_string(),
    ];

    #[cfg(unix)]
    {
        use std::fs::OpenOptions;
        OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&tmp_path)
            .map_err(|e| format!("failed to create temp kubeconfig file: {e}"))?;
    }

    let output = runner.run("scp", &scp_args).await?;
    if !output.success {
        let err_msg = if !output.stderr.is_empty() {
            output.stderr
        } else {
            output.stdout
        };
        return Err(format!("kubeconfig fetch failed: {err_msg}"));
    }

    let raw_yaml = std::fs::read_to_string(&tmp_path)
        .map_err(|e| format!("failed to read fetched kubeconfig file: {e}"))?;

    let normalized_yaml = normalize(&raw_yaml, &profile.id)?;

    paths
        .ensure_dirs()
        .map_err(|e| format!("failed to create kubeconfigs directory: {e}"))?;
    let dest_path = paths.kubeconfig_file(&profile.id);

    #[cfg(unix)]
    {
        use std::fs::OpenOptions;
        use std::io::Write;
        let mut file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&dest_path)
            .map_err(|e| format!("failed to create kubeconfig file: {e}"))?;
        file.write_all(normalized_yaml.as_bytes())
            .map_err(|e| format!("failed to write kubeconfig file: {e}"))?;
    }
    #[cfg(not(unix))]
    {
        std::fs::write(&dest_path, normalized_yaml)
            .map_err(|e| format!("failed to write kubeconfig file: {e}"))?;
    }

    Ok(KubeconfigSummary {
        cluster_name: profile.id.clone(),
        context_name: profile.id.clone(),
        local_path: dest_path.to_string_lossy().to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::config::{BootstrapPolicy, Host, KubeconfigSource};
    use crate::services::process::CommandOutput;
    use async_trait::async_trait;
    use std::sync::atomic::{AtomicBool, Ordering};

    struct FakeScpRunner {
        should_succeed: bool,
        sample_yaml: String,
        scp_called: AtomicBool,
    }

    #[async_trait]
    impl CommandRunner for FakeScpRunner {
        async fn run(&self, bin: &str, args: &[String]) -> Result<CommandOutput, String> {
            if bin == "scp" {
                self.scp_called.store(true, Ordering::SeqCst);
                if self.should_succeed {
                    let dest = args.last().unwrap();
                    std::fs::write(dest, &self.sample_yaml).unwrap();
                    return Ok(CommandOutput {
                        stdout: String::new(),
                        stderr: String::new(),
                        success: true,
                    });
                } else {
                    return Ok(CommandOutput {
                        stdout: String::new(),
                        stderr: "Permission denied (publickey)".to_string(),
                        success: false,
                    });
                }
            }
            Err(format!("unexpected command {bin}"))
        }
    }

    const SAMPLE: &str = r#"
apiVersion: v1
kind: Config
clusters:
  - name: original-cluster
    cluster:
      server: https://192.0.2.10:6443
      certificate-authority-data: ZmFrZS1jYQ==
contexts:
  - name: original-context
    context:
      cluster: original-cluster
      user: original-user
current-context: original-context
users:
  - name: original-user
    user:
      client-certificate-data: ZmFrZS1jZXJ0
      client-key-data: ZmFrZS1rZXk=
"#;

    #[test]
    fn normalize_renames_cluster_context_and_user_to_profile_id() {
        let normalized = normalize(SAMPLE, "cka").unwrap();
        let value: serde_yaml::Value = serde_yaml::from_str(&normalized).unwrap();
        assert_eq!(value["current-context"].as_str().unwrap(), "cka");
        assert_eq!(value["clusters"][0]["name"].as_str().unwrap(), "cka");
        assert_eq!(value["contexts"][0]["name"].as_str().unwrap(), "cka");
        assert_eq!(
            value["contexts"][0]["context"]["cluster"].as_str().unwrap(),
            "cka"
        );
        assert_eq!(
            value["contexts"][0]["context"]["user"].as_str().unwrap(),
            "cka"
        );
        assert_eq!(value["users"][0]["name"].as_str().unwrap(), "cka");
        // certificate data must survive untouched
        assert_eq!(
            value["clusters"][0]["cluster"]["certificate-authority-data"]
                .as_str()
                .unwrap(),
            "ZmFrZS1jYQ=="
        );
    }

    #[test]
    fn normalize_rejects_multi_cluster_kubeconfig() {
        let multi = SAMPLE.replace(
            "clusters:\n  - name: original-cluster",
            "clusters:\n  - name: original-cluster\n    cluster:\n      server: https://x\n  - name: second",
        );
        assert!(normalize(&multi, "cka").is_err());
    }

    #[tokio::test]
    async fn fetch_and_store_fetches_normalizes_and_writes_kubeconfig() {
        let temp_dir =
            std::env::temp_dir().join(format!("clusterdeck-kc-test-{}", std::process::id()));
        let paths = ClusterDeckPaths::at(temp_dir.clone());
        let profile = Profile {
            id: "cka".to_string(),
            name: "CKA Lab".to_string(),
            hosts: vec![Host {
                name: "m1".to_string(),
                address: "192.0.2.10".to_string(),
                port: 22,
                user: "root".to_string(),
                identity_file: None,
            }],
            bastion: None,
            bootstrap: BootstrapPolicy::default(),
            kubeconfig: Some(KubeconfigSource {
                remote_path: "/etc/kubernetes/admin.conf".to_string(),
                control_plane: "m1".to_string(),
                local_path: "".to_string(),
                context: "cka".to_string(),
            }),
            manage_hosts_file: false,
        };

        let runner = FakeScpRunner {
            should_succeed: true,
            sample_yaml: SAMPLE.to_string(),
            scp_called: AtomicBool::new(false),
        };

        let summary = fetch_and_store(&runner, &paths, &profile).await.unwrap();
        assert_eq!(summary.cluster_name, "cka");
        assert_eq!(summary.context_name, "cka");
        assert!(runner.scp_called.load(Ordering::SeqCst));

        let stored_yaml = std::fs::read_to_string(paths.kubeconfig_file("cka")).unwrap();
        let value: serde_yaml::Value = serde_yaml::from_str(&stored_yaml).unwrap();
        assert_eq!(value["current-context"].as_str().unwrap(), "cka");

        std::fs::remove_dir_all(&temp_dir).ok();
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn fetch_and_store_creates_destination_with_0600_permissions() {
        use std::os::unix::fs::PermissionsExt;
        let temp_dir =
            std::env::temp_dir().join(format!("clusterdeck-kc-test-perms-{}", std::process::id()));
        let paths = ClusterDeckPaths::at(temp_dir.clone());
        let profile = Profile {
            id: "cka-perms".to_string(),
            name: "CKA Lab".to_string(),
            hosts: vec![Host {
                name: "m1".to_string(),
                address: "192.0.2.10".to_string(),
                port: 22,
                user: "root".to_string(),
                identity_file: None,
            }],
            bastion: None,
            bootstrap: BootstrapPolicy::default(),
            kubeconfig: Some(KubeconfigSource {
                remote_path: "/etc/kubernetes/admin.conf".to_string(),
                control_plane: "m1".to_string(),
                local_path: "".to_string(),
                context: "cka-perms".to_string(),
            }),
            manage_hosts_file: false,
        };

        let runner = FakeScpRunner {
            should_succeed: true,
            sample_yaml: SAMPLE.to_string(),
            scp_called: AtomicBool::new(false),
        };

        fetch_and_store(&runner, &paths, &profile).await.unwrap();

        let dest_path = paths.kubeconfig_file("cka-perms");
        let mode = std::fs::metadata(&dest_path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);

        std::fs::remove_dir_all(&temp_dir).ok();
    }
}
