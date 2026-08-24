#![allow(dead_code)]

use crate::services::config::{Bastion, Host};
use crate::services::process::CommandRunner;
use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
pub struct ProbeResult {
    pub host: String,
    pub reachable: bool,
    pub detail: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct BootstrapResult {
    pub host: String,
    pub key_deployed: bool,
    pub verified: bool,
    pub detail: String,
}

fn is_safe_ssh_identifier(s: &str) -> bool {
    !s.is_empty() && !s.starts_with('-')
}

pub fn build_ssh_target_args(
    host: &Host,
    bastion: Option<&Bastion>,
    extra: &[&str],
) -> Vec<String> {
    let mut args = vec![
        "-o".to_string(),
        "BatchMode=yes".to_string(),
        "-o".to_string(),
        "ConnectTimeout=5".to_string(),
        "-p".to_string(),
        host.port.to_string(),
    ];

    if let Some(identity) = &host.identity_file {
        if !identity.is_empty() {
            args.push("-i".to_string());
            args.push(identity.clone());
        }
    }

    if let Some(b) = bastion {
        args.push("-J".to_string());
        let bastion_target = if b.port == 22 {
            format!("{}@{}", b.user, b.address)
        } else {
            format!("{}@{}:{}", b.user, b.address, b.port)
        };
        args.push(bastion_target);
    }

    args.push("--".to_string());
    args.push(format!("{}@{}", host.user, host.address));

    for arg in extra {
        args.push(arg.to_string());
    }

    args
}

pub async fn probe_key_auth(
    runner: &dyn CommandRunner,
    host: &Host,
    bastion: Option<&Bastion>,
) -> ProbeResult {
    if !is_safe_ssh_identifier(&host.user)
        || !is_safe_ssh_identifier(&host.address)
        || bastion.is_some_and(|b| {
            !is_safe_ssh_identifier(&b.user) || !is_safe_ssh_identifier(&b.address)
        })
    {
        return ProbeResult {
            host: host.name.clone(),
            reachable: false,
            detail: "unsafe SSH identifier".to_string(),
        };
    }

    let args = build_ssh_target_args(host, bastion, &["true"]);
    match runner.run("ssh", &args).await {
        Ok(output) => ProbeResult {
            host: host.name.clone(),
            reachable: output.success,
            detail: if output.success {
                if output.stdout.is_empty() {
                    "SSH key auth succeeded".to_string()
                } else {
                    output.stdout
                }
            } else if !output.stderr.is_empty() {
                output.stderr
            } else {
                output.stdout
            },
        },
        Err(err) => ProbeResult {
            host: host.name.clone(),
            reachable: false,
            detail: err,
        },
    }
}

pub async fn probe_password_auth(
    runner: &dyn CommandRunner,
    host: &Host,
    bastion: Option<&Bastion>,
    password: &str,
) -> ProbeResult {
    if !is_safe_ssh_identifier(&host.user)
        || !is_safe_ssh_identifier(&host.address)
        || bastion.is_some_and(|b| {
            !is_safe_ssh_identifier(&b.user) || !is_safe_ssh_identifier(&b.address)
        })
    {
        return ProbeResult {
            host: host.name.clone(),
            reachable: false,
            detail: "unsafe SSH identifier".to_string(),
        };
    }

    let mut args = vec![
        "-p".to_string(),
        password.to_string(),
        "ssh".to_string(),
        "-o".to_string(),
        "ConnectTimeout=5".to_string(),
        "-o".to_string(),
        "StrictHostKeyChecking=accept-new".to_string(),
        "-p".to_string(),
        host.port.to_string(),
    ];

    if let Some(identity) = &host.identity_file {
        if !identity.is_empty() {
            args.push("-i".to_string());
            args.push(identity.clone());
        }
    }

    if let Some(b) = bastion {
        args.push("-J".to_string());
        let bastion_target = if b.port == 22 {
            format!("{}@{}", b.user, b.address)
        } else {
            format!("{}@{}:{}", b.user, b.address, b.port)
        };
        args.push(bastion_target);
    }

    args.push("--".to_string());
    args.push(format!("{}@{}", host.user, host.address));
    args.push("true".to_string());

    match runner.run("sshpass", &args).await {
        Ok(output) => ProbeResult {
            host: host.name.clone(),
            reachable: output.success,
            detail: if output.success {
                if output.stdout.is_empty() {
                    "SSH password auth succeeded".to_string()
                } else {
                    output.stdout
                }
            } else if !output.stderr.is_empty() {
                output.stderr
            } else {
                output.stdout
            },
        },
        Err(err) => ProbeResult {
            host: host.name.clone(),
            reachable: false,
            detail: err,
        },
    }
}

pub async fn deploy_public_key(
    runner: &dyn CommandRunner,
    host: &Host,
    bastion: Option<&Bastion>,
    password: &str,
) -> Result<(), String> {
    if !is_safe_ssh_identifier(&host.user)
        || !is_safe_ssh_identifier(&host.address)
        || bastion.is_some_and(|b| {
            !is_safe_ssh_identifier(&b.user) || !is_safe_ssh_identifier(&b.address)
        })
    {
        return Err("unsafe SSH identifier".to_string());
    }

    let mut args = vec![
        "-p".to_string(),
        password.to_string(),
        "ssh-copy-id".to_string(),
        "-o".to_string(),
        "ConnectTimeout=5".to_string(),
        "-o".to_string(),
        "StrictHostKeyChecking=accept-new".to_string(),
        "-p".to_string(),
        host.port.to_string(),
    ];

    if let Some(identity) = &host.identity_file {
        if !identity.is_empty() {
            args.push("-i".to_string());
            args.push(identity.clone());
        }
    }

    if let Some(b) = bastion {
        let bastion_target = if b.port == 22 {
            format!("{}@{}", b.user, b.address)
        } else {
            format!("{}@{}:{}", b.user, b.address, b.port)
        };
        args.push("-o".to_string());
        args.push(format!("ProxyJump={bastion_target}"));
    }

    args.push("--".to_string());
    args.push(format!("{}@{}", host.user, host.address));

    let output = runner.run("sshpass", &args).await?;
    if output.success {
        Ok(())
    } else {
        Err(if !output.stderr.is_empty() {
            output.stderr
        } else {
            output.stdout
        })
    }
}

pub async fn probe_with_retry(
    runner: &dyn CommandRunner,
    host: &Host,
    bastion: Option<&Bastion>,
    retries: u32,
    retry_delay: std::time::Duration,
) -> ProbeResult {
    let attempts = std::cmp::max(1, retries);
    let mut last_result = ProbeResult {
        host: host.name.clone(),
        reachable: false,
        detail: "No probe attempted".to_string(),
    };

    for attempt in 1..=attempts {
        last_result = probe_key_auth(runner, host, bastion).await;
        if last_result.reachable {
            return last_result;
        }
        if attempt < attempts {
            tokio::time::sleep(retry_delay).await;
        }
    }

    last_result
}

pub async fn bootstrap_host(
    runner: &dyn CommandRunner,
    host: &Host,
    bastion: Option<&Bastion>,
    password: &str,
    retries: u32,
    retry_delay: std::time::Duration,
) -> BootstrapResult {
    match deploy_public_key(runner, host, bastion, password).await {
        Ok(_) => {
            let probe_res = probe_with_retry(runner, host, bastion, retries, retry_delay).await;
            BootstrapResult {
                host: host.name.clone(),
                key_deployed: true,
                verified: probe_res.reachable,
                detail: probe_res.detail,
            }
        }
        Err(err) => BootstrapResult {
            host: host.name.clone(),
            key_deployed: false,
            verified: false,
            detail: err,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::config::Host;
    use crate::services::process::CommandOutput;
    use async_trait::async_trait;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct FakeRunner {
        // returns success on the Nth call (1-indexed), failure before that
        succeed_on_call: usize,
        calls: AtomicUsize,
    }

    #[async_trait]
    impl CommandRunner for FakeRunner {
        async fn run(&self, _bin: &str, _args: &[String]) -> Result<CommandOutput, String> {
            let n = self.calls.fetch_add(1, Ordering::SeqCst) + 1;
            Ok(CommandOutput {
                stdout: String::new(),
                stderr: if n >= self.succeed_on_call {
                    String::new()
                } else {
                    "Permission denied".into()
                },
                success: n >= self.succeed_on_call,
            })
        }
    }

    fn host() -> Host {
        Host {
            name: "cka-m1".into(),
            address: "192.0.2.10".into(),
            port: 22,
            user: "root".into(),
            identity_file: None,
        }
    }

    #[tokio::test]
    async fn probe_key_auth_reports_reachable_on_success() {
        let runner = FakeRunner {
            succeed_on_call: 1,
            calls: AtomicUsize::new(0),
        };
        let result = probe_key_auth(&runner, &host(), None).await;
        assert!(result.reachable);
    }

    #[tokio::test]
    async fn probe_with_retry_succeeds_after_transient_failures() {
        let runner = FakeRunner {
            succeed_on_call: 3,
            calls: AtomicUsize::new(0),
        };
        let result = probe_with_retry(
            &runner,
            &host(),
            None,
            3,
            std::time::Duration::from_millis(1),
        )
        .await;
        assert!(result.reachable);
    }

    #[tokio::test]
    async fn probe_with_retry_gives_up_after_max_retries() {
        let runner = FakeRunner {
            succeed_on_call: 99,
            calls: AtomicUsize::new(0),
        };
        let result = probe_with_retry(
            &runner,
            &host(),
            None,
            2,
            std::time::Duration::from_millis(1),
        )
        .await;
        assert!(!result.reachable);
    }

    #[tokio::test]
    async fn build_ssh_target_args_includes_proxy_jump_when_bastion_present() {
        use crate::services::config::Bastion;
        let bastion = Bastion {
            name: "b".into(),
            address: "10.0.0.10".into(),
            port: 22,
            user: "ubuntu".into(),
            identity_file: None,
        };
        let args = build_ssh_target_args(&host(), Some(&bastion), &[]);
        assert!(args.iter().any(|a| a == "-J"));
        assert!(args.iter().any(|a| a.contains("ubuntu@10.0.0.10")));
    }

    #[test]
    fn rejects_ssh_identifier_starting_with_dash() {
        assert!(!is_safe_ssh_identifier("-oProxyCommand=evil"));
        assert!(is_safe_ssh_identifier("root"));
    }

    #[tokio::test]
    async fn probe_key_auth_refuses_unsafe_user() {
        let runner = FakeRunner {
            succeed_on_call: 1,
            calls: AtomicUsize::new(0),
        };
        let mut h = host();
        h.user = "-oProxyCommand=evil".into();
        let result = probe_key_auth(&runner, &h, None).await;
        assert!(!result.reachable);
    }
}
