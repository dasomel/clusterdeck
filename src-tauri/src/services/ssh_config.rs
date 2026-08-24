#![allow(dead_code)]

use crate::services::config::Profile;
use crate::services::paths::ClusterDeckPaths;

use std::path::{Path, PathBuf};

pub fn ssh_alias(profile_id: &str, host_name: &str) -> String {
    format!("{profile_id}-{host_name}")
}

pub fn render_profile_config(profile: &Profile) -> String {
    let mut blocks = Vec::new();

    if let Some(bastion) = &profile.bastion {
        let mut lines = Vec::new();
        lines.push(format!("Host {}-bastion", profile.id));
        lines.push(format!("  HostName {}", bastion.address));
        lines.push(format!("  User {}", bastion.user));
        lines.push(format!("  Port {}", bastion.port));
        if let Some(identity) = &bastion.identity_file {
            lines.push(format!("  IdentityFile {identity}"));
        }
        blocks.push(lines.join("\n"));
    }

    for host in &profile.hosts {
        let mut lines = Vec::new();
        lines.push(format!("Host {}", ssh_alias(&profile.id, &host.name)));
        lines.push(format!("  HostName {}", host.address));
        lines.push(format!("  User {}", host.user));
        lines.push(format!("  Port {}", host.port));
        if let Some(identity) = &host.identity_file {
            lines.push(format!("  IdentityFile {identity}"));
        }
        if profile.bastion.is_some() {
            lines.push(format!("  ProxyJump {}-bastion", profile.id));
        }
        blocks.push(lines.join("\n"));
    }

    if blocks.is_empty() {
        String::new()
    } else {
        format!("{}\n", blocks.join("\n\n"))
    }
}

pub fn write_profile_config(
    paths: &ClusterDeckPaths,
    profile: &Profile,
) -> Result<PathBuf, String> {
    paths.ensure_dirs()?;
    let conf_path = paths.ssh_conf(&profile.id);
    let content = render_profile_config(profile);
    std::fs::write(&conf_path, content).map_err(|e| e.to_string())?;
    Ok(conf_path)
}

pub fn ensure_ssh_include(
    home_ssh_config_path: &Path,
    paths: &ClusterDeckPaths,
) -> Result<(), String> {
    if let Some(parent) = home_ssh_config_path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }

    let content = if home_ssh_config_path.exists() {
        std::fs::read_to_string(home_ssh_config_path).map_err(|e| e.to_string())?
    } else {
        String::new()
    };

    if !content.contains("Include") {
        let include_line = format!("Include {}/*.conf", paths.ssh_dir().display());
        let new_content = if content.is_empty() {
            format!("{include_line}\n")
        } else {
            format!("{include_line}\n\n{content}")
        };
        std::fs::write(home_ssh_config_path, new_content).map_err(|e| e.to_string())?;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::config::{Bastion, BootstrapPolicy, Host, Profile};

    fn profile_with_bastion() -> Profile {
        Profile {
            id: "cka".into(),
            name: "CKA Lab".into(),
            hosts: vec![Host {
                name: "cka-m1".into(),
                address: "192.168.56.10".into(),
                port: 22,
                user: "vagrant".into(),
                identity_file: Some("~/.ssh/cka".into()),
            }],
            bastion: Some(Bastion {
                name: "bastion01".into(),
                address: "10.0.0.10".into(),
                port: 22,
                user: "ubuntu".into(),
                identity_file: Some("~/.ssh/lab".into()),
            }),
            bootstrap: BootstrapPolicy::default(),
            kubeconfig: None,
            manage_hosts_file: false,
        }
    }

    #[test]
    fn render_includes_proxy_jump_for_target_hosts() {
        let rendered = render_profile_config(&profile_with_bastion());
        assert!(rendered.contains("Host cka-bastion"));
        assert!(rendered.contains("Host cka-cka-m1"));
        assert!(rendered.contains("ProxyJump cka-bastion"));
        assert!(rendered.contains("HostName 10.0.0.10"));
    }

    #[test]
    fn ssh_alias_formats_profile_and_host() {
        assert_eq!(ssh_alias("cka", "cka-m1"), "cka-cka-m1");
    }

    #[test]
    fn ensure_ssh_include_creates_file_when_missing() {
        let dir =
            std::env::temp_dir().join(format!("clusterdeck-sshcfg-test-a-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let ssh_config = dir.join("config");
        let paths = crate::services::paths::ClusterDeckPaths::at(dir.join("cdhome"));
        ensure_ssh_include(&ssh_config, &paths).unwrap();
        let content = std::fs::read_to_string(&ssh_config).unwrap();
        assert!(content.contains("Include"));
        assert!(content.contains("ssh/*.conf"));
    }

    #[test]
    fn ensure_ssh_include_is_idempotent_and_preserves_existing_content() {
        let dir =
            std::env::temp_dir().join(format!("clusterdeck-sshcfg-test-b-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let ssh_config = dir.join("config");
        std::fs::write(&ssh_config, "Host existing\n  HostName example.invalid\n").unwrap();
        let paths = crate::services::paths::ClusterDeckPaths::at(dir.join("cdhome"));
        ensure_ssh_include(&ssh_config, &paths).unwrap();
        ensure_ssh_include(&ssh_config, &paths).unwrap();
        let content = std::fs::read_to_string(&ssh_config).unwrap();
        assert_eq!(content.matches("Include").count(), 1);
        assert!(content.contains("Host existing"));
    }

    #[test]
    fn write_profile_config_writes_file_to_ssh_dir() {
        let dir =
            std::env::temp_dir().join(format!("clusterdeck-sshcfg-test-c-{}", std::process::id()));
        let paths = crate::services::paths::ClusterDeckPaths::at(dir);
        let profile = profile_with_bastion();
        let file_path = write_profile_config(&paths, &profile).unwrap();
        assert!(file_path.exists());
        let content = std::fs::read_to_string(file_path).unwrap();
        assert!(content.contains("Host cka-bastion"));
    }
}
