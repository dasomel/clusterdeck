#![allow(dead_code)]

use crate::services::config::Profile;
use crate::services::paths::ClusterDeckPaths;
use crate::services::validate::{is_safe_profile_id, is_safe_ssh_identifier};

use std::path::{Path, PathBuf};

pub fn ssh_alias(profile_id: &str, host_name: &str) -> String {
    format!("{profile_id}-{host_name}")
}

/// Defense-in-depth: re-validate identifiers at this sink even though
/// `store::upsert_profile` already enforces this at the persistence boundary
/// (AGENTS.md: two prior CRITICAL findings came from a sink trusting
/// unvalidated profile data reaching SSH config generation).
fn validate_profile_identifiers(profile: &Profile) -> Result<(), String> {
    if !is_safe_profile_id(&profile.id) {
        return Err(format!("unsafe profile id: {}", profile.id));
    }
    if let Some(bastion) = &profile.bastion {
        if !is_safe_ssh_identifier(&bastion.address) || !is_safe_ssh_identifier(&bastion.user) {
            return Err("unsafe bastion identifier".to_string());
        }
    }
    for host in &profile.hosts {
        if !is_safe_ssh_identifier(&host.name)
            || !is_safe_ssh_identifier(&host.address)
            || !is_safe_ssh_identifier(&host.user)
        {
            return Err(format!("unsafe host identifier: {}", host.name));
        }
    }
    Ok(())
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
    validate_profile_identifiers(profile)?;
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

    let include_line = format!("Include {}/*.conf", paths.ssh_dir().display());
    let already_present = content.lines().any(|line| line.trim() == include_line);

    if !already_present {
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
    fn ensure_ssh_include_adds_line_when_unrelated_include_exists() {
        let dir =
            std::env::temp_dir().join(format!("clusterdeck-sshcfg-test-d-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let ssh_config = dir.join("config");
        std::fs::write(
            &ssh_config,
            "Include other.conf\nHost existing\n  HostName example.invalid\n",
        )
        .unwrap();
        let paths = crate::services::paths::ClusterDeckPaths::at(dir.join("cdhome"));
        ensure_ssh_include(&ssh_config, &paths).unwrap();
        let content = std::fs::read_to_string(&ssh_config).unwrap();
        assert!(content.contains("Include other.conf"));
        assert!(content.contains(&format!("Include {}/*.conf", paths.ssh_dir().display())));
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

    #[test]
    fn write_profile_config_rejects_unsafe_profile_id() {
        let dir =
            std::env::temp_dir().join(format!("clusterdeck-sshcfg-test-e-{}", std::process::id()));
        let paths = crate::services::paths::ClusterDeckPaths::at(dir);
        let mut profile = profile_with_bastion();
        profile.id = "../../etc".into();
        let result = write_profile_config(&paths, &profile);
        assert!(result.is_err());
    }

    // Real-process regression: proves the actual OpenSSH binary parses ProxyJump/HostName
    // correctly from a file write_profile_config really wrote to disk -- not a mock of
    // what we assume the config format means. `ssh -G` prints effective config without
    // connecting to any network, so this is safe/reproducible/offline.
    #[test]
    #[ignore = "invokes the real `ssh` binary; run manually with `cargo test -- --ignored`"]
    fn real_ssh_binary_parses_proxyjump_and_hostname_from_generated_config() {
        let dir = std::env::temp_dir().join(format!(
            "clusterdeck-sshcfg-real-argv-test-{}",
            std::process::id()
        ));
        let paths = crate::services::paths::ClusterDeckPaths::at(dir.clone());
        let profile = profile_with_bastion();
        let conf_path = write_profile_config(&paths, &profile).unwrap();

        let output = std::process::Command::new("ssh")
            .args(["-F", conf_path.to_str().unwrap(), "-G", "cka-cka-m1"])
            .output()
            .expect("failed to spawn real ssh binary");
        assert!(output.status.success(), "ssh -G failed: {output:?}");
        let effective = String::from_utf8_lossy(&output.stdout).to_lowercase();

        assert!(
            effective.contains("proxyjump cka-bastion"),
            "expected proxyjump directive resolved from generated config, got: {effective}"
        );
        assert!(
            effective.contains("hostname 192.168.56.10"),
            "expected hostname resolved from generated config, got: {effective}"
        );
        assert!(
            effective.contains("user vagrant"),
            "expected user resolved from generated config, got: {effective}"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn write_profile_config_rejects_unsafe_host_identifier() {
        let dir =
            std::env::temp_dir().join(format!("clusterdeck-sshcfg-test-f-{}", std::process::id()));
        let paths = crate::services::paths::ClusterDeckPaths::at(dir);
        let mut profile = profile_with_bastion();
        profile.hosts[0].address = "10.0.0.1\nHost evil".into();
        let result = write_profile_config(&paths, &profile);
        assert!(result.is_err());
    }
}
