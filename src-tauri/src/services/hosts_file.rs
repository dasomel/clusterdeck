#![allow(dead_code)]

pub const HOSTS_FILE_PATH: &str = "/etc/hosts";

pub fn render_hosts_block(profile: &crate::services::config::Profile) -> Result<String, String> {
    if !crate::services::validate::is_safe_profile_id(&profile.id) {
        return Err(format!("invalid profile id: {}", profile.id));
    }

    for host in &profile.hosts {
        if !crate::services::validate::is_safe_ssh_identifier(&host.name)
            || !crate::services::validate::is_safe_ssh_identifier(&host.address)
        {
            return Err(format!("invalid host name/address for host {}", host.name));
        }
    }

    if let Some(bastion) = &profile.bastion {
        if !crate::services::validate::is_safe_ssh_identifier(&bastion.name)
            || !crate::services::validate::is_safe_ssh_identifier(&bastion.address)
        {
            return Err(format!(
                "invalid bastion name/address for bastion {}",
                bastion.name
            ));
        }
    }

    let mut block = String::new();
    block.push_str(&format!(
        "# >>> ClusterDeck BEGIN (profile: {}) >>>\n",
        profile.id
    ));

    for host in &profile.hosts {
        block.push_str(&format!(
            "{} {}.{}.clusterdeck.local\n",
            host.address, host.name, profile.id
        ));
    }

    if let Some(bastion) = &profile.bastion {
        block.push_str(&format!(
            "{} {}.{}.clusterdeck.local\n",
            bastion.address, bastion.name, profile.id
        ));
    }

    block.push_str(&format!(
        "# <<< ClusterDeck END (profile: {}) <<<\n",
        profile.id
    ));

    Ok(block)
}

pub fn compute_updated_hosts_content(
    existing: &str,
    profile_id: &str,
    block: Option<&str>,
) -> String {
    let begin_marker = format!("# >>> ClusterDeck BEGIN (profile: {profile_id}) >>>");
    let end_marker = format!("# <<< ClusterDeck END (profile: {profile_id}) <<<");

    let lines: Vec<&str> = existing.lines().collect();

    let begin_idx = lines.iter().position(|l| l.trim() == begin_marker);
    let end_idx = lines.iter().position(|l| l.trim() == end_marker);

    let mut filtered = Vec::new();
    if let (Some(b_idx), Some(e_idx)) = (begin_idx, end_idx) {
        if b_idx <= e_idx {
            for (i, line) in lines.iter().enumerate() {
                if i < b_idx || i > e_idx {
                    filtered.push(*line);
                }
            }
        } else {
            filtered.extend(lines);
        }
    } else {
        filtered.extend(lines);
    }

    let mut out = if filtered.is_empty() {
        String::new()
    } else {
        let mut s = filtered.join("\n");
        if existing.ends_with('\n') || !s.is_empty() {
            s.push('\n');
        }
        s
    };

    if let Some(b) = block {
        if out.is_empty() {
            out.push_str(b);
        } else {
            if !out.ends_with("\n\n") {
                if out.ends_with('\n') {
                    out.push('\n');
                } else {
                    out.push_str("\n\n");
                }
            }
            out.push_str(b);
        }
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::config::{Bastion, BootstrapPolicy, Host, Profile};

    fn profile() -> Profile {
        Profile {
            id: "cka-lab".into(),
            name: "CKA Lab".into(),
            hosts: vec![Host {
                name: "cka-m1".into(),
                address: "192.0.2.10".into(),
                port: 22,
                user: "root".into(),
                identity_file: None,
            }],
            bastion: Some(Bastion {
                name: "bastion01".into(),
                address: "198.51.100.1".into(),
                port: 22,
                user: "ubuntu".into(),
                identity_file: None,
            }),
            bootstrap: BootstrapPolicy::default(),
            kubeconfig: None,
            manage_hosts_file: true,
        }
    }

    #[test]
    fn render_hosts_block_includes_hosts_and_bastion_with_namespaced_names() {
        let block = render_hosts_block(&profile()).unwrap();
        assert!(block.contains("# >>> ClusterDeck BEGIN (profile: cka-lab) >>>"));
        assert!(block.contains("192.0.2.10 cka-m1.cka-lab.clusterdeck.local"));
        assert!(block.contains("198.51.100.1 bastion01.cka-lab.clusterdeck.local"));
        assert!(block.contains("# <<< ClusterDeck END (profile: cka-lab) <<<"));
    }

    #[test]
    fn render_hosts_block_rejects_unsafe_profile_id() {
        let mut p = profile();
        p.id = "../evil".into();
        assert!(render_hosts_block(&p).is_err());
    }

    #[test]
    fn compute_updated_hosts_content_appends_block_to_empty_file() {
        let result = compute_updated_hosts_content(
            "",
            "cka-lab",
            Some(
                "# >>> ClusterDeck BEGIN (profile: cka-lab) >>>\n192.0.2.10 cka-m1.cka-lab.clusterdeck.local\n# <<< ClusterDeck END (profile: cka-lab) <<<\n",
            ),
        );
        assert!(result.contains("cka-m1.cka-lab.clusterdeck.local"));
    }

    #[test]
    fn compute_updated_hosts_content_preserves_unrelated_lines() {
        let existing = "127.0.0.1 localhost\n255.255.255.255 broadcasthost\n";
        let result = compute_updated_hosts_content(
            existing,
            "cka-lab",
            Some(
                "# >>> ClusterDeck BEGIN (profile: cka-lab) >>>\n192.0.2.10 cka-m1.cka-lab.clusterdeck.local\n# <<< ClusterDeck END (profile: cka-lab) <<<\n",
            ),
        );
        assert!(result.contains("127.0.0.1 localhost"));
        assert!(result.contains("255.255.255.255 broadcasthost"));
        assert!(result.contains("cka-m1.cka-lab.clusterdeck.local"));
    }

    #[test]
    fn compute_updated_hosts_content_replaces_only_matching_profile_block_leaving_others() {
        let existing = "127.0.0.1 localhost\n\n# >>> ClusterDeck BEGIN (profile: cka-lab) >>>\n192.0.2.99 stale.cka-lab.clusterdeck.local\n# <<< ClusterDeck END (profile: cka-lab) <<<\n\n# >>> ClusterDeck BEGIN (profile: dev-cluster) >>>\n198.51.100.20 dev-m1.dev-cluster.clusterdeck.local\n# <<< ClusterDeck END (profile: dev-cluster) <<<\n";
        let new_block = "# >>> ClusterDeck BEGIN (profile: cka-lab) >>>\n192.0.2.10 cka-m1.cka-lab.clusterdeck.local\n# <<< ClusterDeck END (profile: cka-lab) <<<\n";
        let result = compute_updated_hosts_content(existing, "cka-lab", Some(new_block));
        assert!(
            !result.contains("stale.cka-lab.clusterdeck.local"),
            "old cka-lab entry must be gone"
        );
        assert!(
            result.contains("cka-m1.cka-lab.clusterdeck.local"),
            "new cka-lab entry must be present"
        );
        assert!(
            result.contains("dev-m1.dev-cluster.clusterdeck.local"),
            "unrelated profile's block must survive untouched"
        );
        assert!(
            result.contains("127.0.0.1 localhost"),
            "non-ClusterDeck line must survive untouched"
        );
    }

    #[test]
    fn compute_updated_hosts_content_removes_block_when_none_given() {
        let existing = "127.0.0.1 localhost\n# >>> ClusterDeck BEGIN (profile: cka-lab) >>>\n192.0.2.10 cka-m1.cka-lab.clusterdeck.local\n# <<< ClusterDeck END (profile: cka-lab) <<<\n";
        let result = compute_updated_hosts_content(existing, "cka-lab", None);
        assert!(!result.contains("cka-lab.clusterdeck.local"));
        assert!(result.contains("127.0.0.1 localhost"));
    }
}

pub async fn write_hosts_file(
    runner: &dyn crate::services::process::CommandRunner,
    new_content: &str,
) -> Result<(), String> {
    let tmp_path =
        std::env::temp_dir().join(format!("clusterdeck-hosts-{}.tmp", std::process::id()));
    if let Err(e) = std::fs::write(&tmp_path, new_content) {
        let _ = std::fs::remove_file(&tmp_path);
        return Err(format!("failed to write temporary hosts file: {e}"));
    }

    let tmp_path_str = tmp_path.to_string_lossy();
    let script = format!(
        "do shell script \"cp '{tmp_path_str}' {HOSTS_FILE_PATH}\" with administrator privileges"
    );

    let res = runner.run("osascript", &["-e".to_string(), script]).await;

    let _ = std::fs::remove_file(&tmp_path);

    let output = res?;
    if output.success {
        Ok(())
    } else {
        Err(output.stderr)
    }
}

pub async fn upsert_hosts_block(
    runner: &dyn crate::services::process::CommandRunner,
    profile: &crate::services::config::Profile,
) -> Result<(), String> {
    let existing = std::fs::read_to_string(HOSTS_FILE_PATH).unwrap_or_default();
    let block = render_hosts_block(profile)?;
    let new_content = compute_updated_hosts_content(&existing, &profile.id, Some(&block));
    write_hosts_file(runner, &new_content).await
}

pub async fn remove_hosts_block(
    runner: &dyn crate::services::process::CommandRunner,
    profile_id: &str,
) -> Result<(), String> {
    let existing = std::fs::read_to_string(HOSTS_FILE_PATH).unwrap_or_default();
    let new_content = compute_updated_hosts_content(&existing, profile_id, None);
    write_hosts_file(runner, &new_content).await
}

#[cfg(test)]
mod write_tests {
    use super::*;
    use crate::services::process::CommandOutput;
    use async_trait::async_trait;

    struct DenyingRunner;

    #[async_trait]
    impl crate::services::process::CommandRunner for DenyingRunner {
        async fn run(&self, _bin: &str, _args: &[String]) -> Result<CommandOutput, String> {
            Ok(CommandOutput {
                stdout: String::new(),
                stderr: "User canceled.".into(),
                success: false,
            })
        }
    }

    #[tokio::test]
    async fn write_hosts_file_reports_error_when_admin_prompt_is_cancelled() {
        let runner = DenyingRunner;
        let result = write_hosts_file(&runner, "127.0.0.1 localhost\n").await;
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("User canceled"));
    }
}
