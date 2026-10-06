#![allow(dead_code)]

use std::path::Path;

use crate::services::config::{AuthMode, Host, LocalRuntimeSource};
pub use crate::services::inventory::apply::{apply, Target};
pub use crate::services::inventory::identity::{
    is_reachable_address, parse_colima_ssh_config, reachable_ip, slug, vagrant_identity,
    vagrant_profile_id,
};
use crate::services::inventory::model::Machine;
use crate::services::inventory::{discover, CreateProfileOutcome};
use crate::services::paths::ClusterDeckPaths;
use crate::services::process::{CommandLimits, CommandRunner};
use crate::services::store;

/// Asks the running VM (over its forwarded SSH endpoint) for its private-network address, using
/// the same scoring as `local_runtime::detect_vagrant`.
async fn probe_private_ip(
    runner: &dyn CommandRunner,
    name: &str,
    address: &str,
    port: u16,
    user: &str,
    identity_file: Option<String>,
) -> Option<String> {
    let safe = crate::services::validate::is_safe_ssh_identifier;
    if !safe(address) || !safe(user) || identity_file.as_deref().is_some_and(|k| !safe(k)) {
        return None;
    }
    let host = Host {
        name: name.to_string(),
        address: address.to_string(),
        port,
        user: user.to_string(),
        identity_file,
        auth: AuthMode::Key,
    };
    let args = crate::services::ssh::build_ssh_target_args(&host, None, &["ip -4 -o addr show"]);
    let out = runner
        .run_bounded_with_env("ssh", &args, &[], CommandLimits::default())
        .await
        .ok()
        .filter(|o| o.success)?;
    crate::services::local_runtime::extract_private_network_ip(&out.stdout, address)
        .filter(|ip| is_reachable_address(ip))
}

pub async fn build_target(
    runner: &dyn CommandRunner,
    environment: &str,
    machines: &[Machine],
) -> Result<Target, String> {
    let group: Vec<&Machine> = machines
        .iter()
        .filter(|m| m.environment == environment)
        .collect();
    let first = group.first().ok_or("environment not found")?;
    let mut hosts = vec![];
    if first.runtime == "colima" {
        if first.state != "running" {
            return Err(format!(
                "Colima '{}' is {}; start it first (colima start -p {}).",
                first.name, first.state, first.name
            ));
        }
        let mut ssh_args = vec!["ssh-config".to_string()];
        if first.name != "default" {
            ssh_args.push("--profile".to_string());
            ssh_args.push(first.name.clone());
        }
        let config = runner
            .run_bounded("colima", &ssh_args, CommandLimits::default())
            .await?;
        if !config.success {
            return Err(format!(
                "could not read Colima SSH settings: {}",
                config.stderr
            ));
        }
        let (address, port, user, identity) =
            parse_colima_ssh_config(&config.stdout).ok_or("could not read Colima SSH settings")?;
        hosts.push(Host {
            name: "colima-vm".into(),
            address,
            port,
            user,
            identity_file: identity,
            auth: AuthMode::Key,
        });
        return Ok(Target {
            id: format!("colima-{}", slug(&first.name)),
            name: format!("Colima {}", first.name),
            hosts,
            local_runtime: Some(LocalRuntimeSource {
                provider: crate::services::local_runtime_lifecycle::LocalRuntimeProvider::Colima,
                instance: first.name.clone(),
            }),
            kubeconfig_path: "/etc/rancher/k3s/k3s.yaml".into(),
            context: "colima".into(),
        });
    }

    if first.orchestrator.as_deref() != Some("vagrant") {
        return Err("ClusterDeck handoff currently supports Vagrant projects and Colima.".into());
    }
    let dir = Path::new(environment);
    let mut has_k3s = false;
    for m in group.iter().filter(|m| m.state == "running") {
        let mut address = reachable_ip(&m.ips).cloned();
        let mut port = 22;
        let mut user = "vagrant".to_string();
        let pdir = dir.to_path_buf();
        let mname = m.name.clone();
        let mut identity = tokio::task::spawn_blocking(move || {
            let home = std::env::var_os("HOME").map(std::path::PathBuf::from);
            vagrant_identity(&pdir, &mname, home.as_deref())
        })
        .await
        .unwrap_or(None);

        let pdir2 = dir.to_path_buf();
        let mname2 = m.name.clone();
        let (static_ip, m_k3s) = tokio::task::spawn_blocking(move || {
            crate::services::local_runtime::find_static_vagrant_info(&pdir2, &mname2)
        })
        .await
        .unwrap_or((None, false));
        if m_k3s {
            has_k3s = true;
        }

        // `vagrant ssh-config` yields the NAT-forwarded loopback endpoint: SSH works through it but
        // the Kubernetes API does not. Prefer a real VM address; a non-loopback ssh-config
        // HostName keeps its own Port.
        let mut forwarded: Option<(String, u16)> = None;
        if let Some(ssh) =
            crate::services::inventory::providers::vagrant_ssh_config(runner, dir, &m.name).await
        {
            user = ssh.user;
            if ssh.identity_file.is_some() {
                identity = ssh.identity_file;
            }
            if is_reachable_address(&ssh.address) {
                address = Some(ssh.address);
                port = ssh.port;
            } else {
                forwarded = Some((ssh.address, ssh.port));
            }
        }
        if address.is_none() {
            // Same real-IP lookups as local_runtime::detect_vagrant: static Vagrantfile IP, then
            // an `ip addr` probe through the forwarded endpoint.
            address = static_ip.filter(|ip| is_reachable_address(ip));
            if address.is_none() {
                if let Some((faddr, fport)) = &forwarded {
                    address =
                        probe_private_ip(runner, &m.name, faddr, *fport, &user, identity.clone())
                            .await;
                }
            }
        }

        let Some(addr) = address else {
            if forwarded.is_some() {
                return Err(format!(
                    "Machine '{}' has no reachable private IP; run `vagrant up` with a private_network or start the VM first",
                    m.name
                ));
            }
            continue;
        };
        hosts.push(Host {
            name: m.name.clone(),
            address: addr,
            port,
            user,
            identity_file: identity,
            auth: AuthMode::Key,
        });
    }
    if hosts.is_empty() {
        return Err(
            "No running machine with a known IP in this environment. Run `vagrant up` first."
                .into(),
        );
    }
    let base = dir
        .file_name()
        .and_then(|s| s.to_str())
        .map(slug)
        .filter(|s| !s.is_empty())
        .ok_or("cannot derive a profile id from the project path")?;
    let pdir = dir.to_path_buf();
    let b = base.clone();
    let id = tokio::task::spawn_blocking(move || vagrant_profile_id(&pdir, &b))
        .await
        .map_err(|e| e.to_string())?;

    let kubeconfig_path = if has_k3s {
        "/etc/rancher/k3s/k3s.yaml".to_string()
    } else {
        "/etc/kubernetes/admin.conf".to_string()
    };

    Ok(Target {
        id,
        name: format!("{base} (Vagrant)"),
        hosts,
        local_runtime: None,
        kubeconfig_path,
        context: base,
    })
}

pub fn count_profiles_in_file(path: &Path) -> Result<usize, String> {
    if !path.exists() {
        return Ok(0);
    }
    let content = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
    let val: serde_yaml::Value = serde_yaml::from_str(&content).map_err(|e| e.to_string())?;
    let count = val
        .get("profiles")
        .and_then(|p| p.as_mapping())
        .map(|m| m.len())
        .unwrap_or(0);
    Ok(count)
}

pub async fn create_profile_from_environment(
    runner: &dyn CommandRunner,
    paths: &ClusterDeckPaths,
    environment: &str,
) -> Result<CreateProfileOutcome, String> {
    // R3: store drops invalid profiles and re-saves the file. Refuse to persist if any profile was dropped.
    // Move sync fs calls into spawn_blocking per R8.
    let paths_clone = paths.clone();
    let (raw_count, mut profiles) = tokio::task::spawn_blocking(move || {
        let raw_file = paths_clone.profiles_file();
        let raw_count = count_profiles_in_file(&raw_file)?;
        let profiles = store::load_profiles(&paths_clone)?;
        Ok::<_, String>((raw_count, profiles))
    })
    .await
    .map_err(|e| e.to_string())??;

    if profiles.len() < raw_count {
        let dropped = raw_count - profiles.len();
        return Err(format!(
            "refusing to persist: profiles.yaml contains {dropped} invalid profile(s) that would be lost on save"
        ));
    }

    let inventory = discover(runner, false).await;
    let target = build_target(runner, environment, &inventory.machines).await?;

    let (profile_id, created, updated_hosts) = apply(&mut profiles, &target)?;
    let profile = profiles
        .iter()
        .find(|p| p.id == profile_id)
        .cloned()
        .ok_or_else(|| "profile not found after apply".to_string())?;

    let paths_clone = paths.clone();
    tokio::task::spawn_blocking(move || store::upsert_profile(&paths_clone, profile))
        .await
        .map_err(|e| e.to_string())??;

    Ok(CreateProfileOutcome {
        profile_id,
        created,
        updated_hosts,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::process::CommandOutput;
    use async_trait::async_trait;

    struct FakeCreateRunner {
        global_status: String,
    }

    #[async_trait]
    impl CommandRunner for FakeCreateRunner {
        async fn run(&self, bin: &str, args: &[String]) -> Result<CommandOutput, String> {
            if bin == "vagrant" && args.first().map(String::as_str) == Some("global-status") {
                return Ok(CommandOutput {
                    stdout: self.global_status.clone(),
                    stderr: "".into(),
                    success: true,
                });
            }
            Ok(CommandOutput {
                stdout: "".into(),
                stderr: "".into(),
                success: true,
            })
        }
    }

    #[tokio::test]
    async fn r3_create_profile_refuses_when_dropped_invalid_profiles_exist_and_preserves_bytes() {
        let temp_dir = std::env::temp_dir().join(format!("cd-r3-full-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&temp_dir);
        let paths = ClusterDeckPaths::at(temp_dir.clone());
        let profiles_file = paths.profiles_file();
        std::fs::create_dir_all(profiles_file.parent().unwrap()).unwrap();

        // 1 valid profile, 1 invalid profile (traversal id)
        let yaml = r#"
profiles:
  valid:
    name: "Valid"
    hosts: []
    manage_hosts_file: false
  "../../invalid":
    name: "Invalid"
    hosts: []
    manage_hosts_file: false
"#;
        std::fs::write(&profiles_file, yaml).unwrap();
        let original_bytes = std::fs::read(&profiles_file).unwrap();

        let runner = FakeCreateRunner {
            global_status: "".into(),
        };

        // Attempt creation on environment
        let err = create_profile_from_environment(&runner, &paths, "demo")
            .await
            .unwrap_err();
        assert!(err.contains("refusing to persist: profiles.yaml contains 1 invalid profile(s) that would be lost on save"));

        // Prove file bytes are completely unchanged
        let after_bytes = std::fs::read(&profiles_file).unwrap();
        assert_eq!(
            original_bytes, after_bytes,
            "profiles.yaml file bytes must remain unchanged"
        );

        let _ = std::fs::remove_dir_all(&temp_dir);
    }

    /// `ssh_config` answers `vagrant ssh-config`, `ip_addr` answers the `ssh ... ip addr` probe.
    struct SshConfigRunner {
        ssh_config: &'static str,
        ip_addr: &'static str,
    }

    const NAT_CONFIG: &str =
        "Host n1\n  HostName 127.0.0.1\n  User ops\n  Port 2222\n  IdentityFile \"/keys/n1\"\n";
    const IP_ADDR: &str = "3: eth1    inet 192.168.56.11/24 brd 192.168.56.255 scope global eth1\n";

    #[async_trait]
    impl CommandRunner for SshConfigRunner {
        async fn run(&self, bin: &str, _args: &[String]) -> Result<CommandOutput, String> {
            Err(format!("unbounded run: {bin}"))
        }
        async fn run_bounded_with_env(
            &self,
            bin: &str,
            _args: &[String],
            env: &[(String, String)],
            _limits: CommandLimits,
        ) -> Result<CommandOutput, String> {
            let stdout = if bin == "vagrant" {
                assert!(env.iter().any(|(k, _)| k == "VAGRANT_CWD"));
                self.ssh_config
            } else {
                self.ip_addr
            };
            Ok(CommandOutput {
                stdout: stdout.into(),
                stderr: String::new(),
                success: true,
            })
        }
    }

    async fn target_for(runner: &SshConfigRunner, ips: &[&str]) -> Result<Target, String> {
        let dir = std::env::temp_dir().join(format!("cd-target-lab-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let env = dir.to_string_lossy().into_owned();
        let mut m = Machine::new("virtualbox", "id1", "n1");
        m.orchestrator = Some("vagrant".into());
        m.environment = env.clone();
        m.state = "running".into();
        m.ips = ips.iter().map(|s| s.to_string()).collect();
        build_target(runner, &env, &[m]).await
    }

    #[tokio::test]
    async fn vagrant_loopback_only_is_refused_with_actionable_error() {
        let runner = SshConfigRunner {
            ssh_config: NAT_CONFIG,
            ip_addr: "",
        };
        let err = target_for(&runner, &["127.0.0.1"]).await.unwrap_err();
        assert!(err.contains("has no reachable private IP"), "{err}");
    }

    #[tokio::test]
    async fn vagrant_target_probes_real_ip_instead_of_forwarded_loopback() {
        let runner = SshConfigRunner {
            ssh_config: NAT_CONFIG,
            ip_addr: IP_ADDR,
        };
        let target = target_for(&runner, &["127.0.0.1"]).await.unwrap();
        let host = &target.hosts[0];
        assert_eq!(
            (host.address.as_str(), host.port, host.user.as_str()),
            ("192.168.56.11", 22, "ops")
        );
        assert_eq!(host.identity_file.as_deref(), Some("/keys/n1"));
    }

    #[tokio::test]
    async fn vagrant_target_prefers_private_inventory_ip_over_nat() {
        let runner = SshConfigRunner {
            ssh_config: NAT_CONFIG,
            ip_addr: "",
        };
        let target = target_for(&runner, &["10.0.2.15", "192.168.56.20"])
            .await
            .unwrap();
        assert_eq!(
            (target.hosts[0].address.as_str(), target.hosts[0].port),
            ("192.168.56.20", 22)
        );
    }

    #[tokio::test]
    async fn vagrant_target_keeps_port_of_non_loopback_ssh_config() {
        let runner = SshConfigRunner {
            ssh_config: "Host n1\n  HostName 192.168.56.30\n  User ops\n  Port 2200\n",
            ip_addr: "",
        };
        let target = target_for(&runner, &["192.168.56.30"]).await.unwrap();
        assert_eq!(
            (target.hosts[0].address.as_str(), target.hosts[0].port),
            ("192.168.56.30", 2200)
        );
    }
}
