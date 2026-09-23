#![allow(dead_code)]

use crate::services::config::{AuthMode, Bastion, Host};
use crate::services::process::{CommandOutput, CommandRunner};
use crate::services::validate::{is_safe_known_hosts_path, is_safe_ssh_identifier};
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

/// Pushes the SSH options shared by every connection path: a short connect timeout,
/// trust-on-first-use host key acceptance (required because ClusterDeck's whole use case is
/// frequently recreated VMs, which by definition are hosts SSH has never seen before -- see
/// AGENTS.md Security Rules), the target port, and an identity file when one is configured.
/// Deliberately excludes `-o BatchMode=yes`: that flag is not always wanted (see
/// probe_password_auth), so callers that need it push it themselves before calling this.
fn push_connection_options(args: &mut Vec<String>, host: &Host) {
    args.push("-o".to_string());
    args.push("ConnectTimeout=5".to_string());
    args.push("-o".to_string());
    args.push("StrictHostKeyChecking=accept-new".to_string());
    args.push("-p".to_string());
    args.push(host.port.to_string());

    if let Some(identity) = &host.identity_file {
        if !identity.is_empty() {
            args.push("-i".to_string());
            args.push(identity.clone());
        }
    }
}

/// Formats a bastion as an SSH connection target: `user@address`, or `user@address:port` when
/// the bastion isn't reachable on the default port 22.
fn jump_target(b: &Bastion) -> String {
    if b.port == 22 {
        format!("{}@{}", b.user, b.address)
    } else {
        format!("{}@{}:{}", b.user, b.address, b.port)
    }
}

pub fn build_ssh_target_args(
    host: &Host,
    bastion: Option<&Bastion>,
    extra: &[&str],
) -> Vec<String> {
    let mut args = vec!["-o".to_string(), "BatchMode=yes".to_string()];
    push_connection_options(&mut args, host);

    if let Some(b) = bastion {
        args.push("-J".to_string());
        args.push(jump_target(b));
    }

    args.push("--".to_string());
    args.push(format!("{}@{}", host.user, host.address));

    for arg in extra {
        args.push(arg.to_string());
    }

    args
}

/// Checks whether OpenSSH stderr indicates that the remote host key has changed
/// (the classic "WARNING: REMOTE HOST IDENTIFICATION HAS CHANGED!" error,
/// common when frequently recreated VMs reuse IPs).
pub fn is_host_key_changed_error(stderr: &str) -> bool {
    stderr.contains("REMOTE HOST IDENTIFICATION HAS CHANGED")
        || (stderr.contains("Host key for ") && stderr.contains("has changed"))
        || (stderr.contains("Host key verification failed") && stderr.contains("Offending"))
}

/// Parses the offending known_hosts file path from OpenSSH error output if present,
/// e.g. "Offending ED25519 key in /Users/m/.ssh/known_hosts:2" -> "/Users/m/.ssh/known_hosts".
pub fn extract_offending_known_hosts_file(stderr: &str) -> Option<String> {
    for line in stderr.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("Offending ") && trimmed.contains(" key in ") {
            if let Some(pos) = trimmed.find(" key in ") {
                let rest = &trimmed[pos + " key in ".len()..];
                if let Some(colon_pos) = rest.rfind(':') {
                    let path = rest[..colon_pos].trim();
                    if is_safe_known_hosts_path(path) {
                        return Some(path.to_string());
                    }
                }
            }
        }
        if trimmed.starts_with("Add correct host key in ") && trimmed.contains(" to get rid of") {
            let after = &trimmed["Add correct host key in ".len()..];
            if let Some(end_pos) = after.find(" to get rid of") {
                let path = after[..end_pos].trim();
                if is_safe_known_hosts_path(path) {
                    return Some(path.to_string());
                }
            }
        }
    }
    None
}

/// Prunes stale host keys using `ssh-keygen -R` for the given host and bastion (if applicable).
/// Invokes `ssh-keygen -f <offending_file> -R <target>` if an offending file was identified,
/// as well as the default `ssh-keygen -R <target>`.
pub async fn prune_stale_host_keys(
    runner: &dyn CommandRunner,
    host: &Host,
    bastion: Option<&Bastion>,
    stderr: &str,
) {
    let offending_file = extract_offending_known_hosts_file(stderr);

    let mut targets = Vec::new();
    targets.push(host.address.clone());
    if host.port != 22 {
        targets.push(format!("[{}]:{}", host.address, host.port));
    }

    if let Some(b) = bastion {
        targets.push(b.address.clone());
        if b.port != 22 {
            targets.push(format!("[{}]:{}", b.address, b.port));
        }
    }

    for target in targets {
        if let Some(file) = &offending_file {
            let _ = runner
                .run(
                    "ssh-keygen",
                    &[
                        "-f".to_string(),
                        file.clone(),
                        "-R".to_string(),
                        target.clone(),
                    ],
                )
                .await;
        }

        let _ = runner.run("ssh-keygen", &["-R".to_string(), target]).await;
    }
}

/// Runs `bin` (via `run_with_env` when `env` is non-empty, otherwise plain `run`) and, if it
/// fails with a changed-host-key error, prunes the stale known_hosts entry and retries exactly
/// once. Recreated VMs frequently reuse addresses, so a changed host key is an expected
/// condition here, not an attack signal.
pub async fn run_with_host_key_retry(
    runner: &dyn CommandRunner,
    bin: &str,
    args: &[String],
    env: &[(String, String)],
    host: &Host,
    bastion: Option<&Bastion>,
) -> Result<CommandOutput, String> {
    let mut output = if env.is_empty() {
        runner.run(bin, args).await
    } else {
        runner.run_with_env(bin, args, env).await
    };

    if let Ok(ref out) = output {
        if !out.success && is_host_key_changed_error(&out.stderr) {
            prune_stale_host_keys(runner, host, bastion, &out.stderr).await;
            output = if env.is_empty() {
                runner.run(bin, args).await
            } else {
                runner.run_with_env(bin, args, env).await
            };
        }
    }

    output
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
    let output = run_with_host_key_retry(runner, "ssh", &args, &[], host, bastion).await;

    match output {
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

    // No BatchMode=yes here: BatchMode disables the interactive password prompt that sshpass
    // answers via SSHPASS, so this probe must allow that prompt through.
    let mut args = vec!["-e".to_string(), "ssh".to_string()];
    push_connection_options(&mut args, host);

    if let Some(b) = bastion {
        args.push("-J".to_string());
        args.push(jump_target(b));
    }

    args.push("--".to_string());
    args.push(format!("{}@{}", host.user, host.address));
    args.push("true".to_string());

    let output = run_with_host_key_retry(
        runner,
        "sshpass",
        &args,
        &[("SSHPASS".to_string(), password.to_string())],
        host,
        bastion,
    )
    .await;

    match output {
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

/// Dispatches to `probe_key_auth` or `probe_password_auth` based on `host.auth` (Issue #26).
/// `password` is only consulted for `AuthMode::Password`; a missing password in that mode is
/// reported as unreachable rather than silently falling back to key auth.
pub async fn probe_auth(
    runner: &dyn CommandRunner,
    host: &Host,
    bastion: Option<&Bastion>,
    password: Option<&str>,
) -> ProbeResult {
    match host.auth {
        AuthMode::Key => probe_key_auth(runner, host, bastion).await,
        AuthMode::Password => match password {
            Some(pwd) => probe_password_auth(runner, host, bastion, pwd).await,
            None => ProbeResult {
                host: host.name.clone(),
                reachable: false,
                detail: "password required for password-auth host".to_string(),
            },
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

    let mut args = vec!["-e".to_string(), "ssh-copy-id".to_string()];
    push_connection_options(&mut args, host);

    if let Some(b) = bastion {
        // ssh-copy-id has no -J flag; -o ProxyJump=<target> is the equivalent it does support.
        args.push("-o".to_string());
        args.push(format!("ProxyJump={}", jump_target(b)));
    }

    args.push("--".to_string());
    args.push(format!("{}@{}", host.user, host.address));

    let output = run_with_host_key_retry(
        runner,
        "sshpass",
        &args,
        &[("SSHPASS".to_string(), password.to_string())],
        host,
        bastion,
    )
    .await?;

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
    password: Option<&str>,
) -> ProbeResult {
    let attempts = std::cmp::max(1, retries);
    let mut last_result = ProbeResult {
        host: host.name.clone(),
        reachable: false,
        detail: "No probe attempted".to_string(),
    };

    for attempt in 1..=attempts {
        last_result = probe_auth(runner, host, bastion, password).await;
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
            // Post-deploy verification is always key auth: bootstrap_host's whole purpose is
            // deploying a public key, so the freshly-deployed key is what we're confirming here.
            let probe_res =
                probe_with_retry(runner, host, bastion, retries, retry_delay, None).await;
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
    use crate::services::config::{AuthMode, Host};
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
            auth: AuthMode::Key,
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
            None,
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
            None,
        )
        .await;
        assert!(!result.reachable);
    }

    #[test]
    fn build_ssh_target_args_accepts_new_host_keys_without_prompting() {
        // Regression: without this, BatchMode connections to a host never seen
        // before (ClusterDeck's whole reason to exist -- frequently recreated
        // VMs) fail outright with "Host key verification failed." instead of
        // trust-on-first-use, because SSH's default StrictHostKeyChecking=ask
        // cannot prompt in BatchMode.
        let args = build_ssh_target_args(&host(), None, &[]);
        let pos = args
            .iter()
            .position(|a| a == "StrictHostKeyChecking=accept-new");
        assert!(
            pos.is_some(),
            "expected StrictHostKeyChecking=accept-new in ssh args"
        );
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

    #[allow(clippy::type_complexity)]
    struct EnvCapturingRunner {
        calls: std::sync::Mutex<Vec<(Vec<String>, Vec<(String, String)>)>>,
    }

    #[async_trait]
    impl CommandRunner for EnvCapturingRunner {
        async fn run(&self, _bin: &str, _args: &[String]) -> Result<CommandOutput, String> {
            Ok(CommandOutput {
                stdout: String::new(),
                stderr: String::new(),
                success: true,
            })
        }

        async fn run_with_env(
            &self,
            bin: &str,
            args: &[String],
            env: &[(String, String)],
        ) -> Result<CommandOutput, String> {
            self.calls
                .lock()
                .unwrap()
                .push((args.to_vec(), env.to_vec()));
            self.run(bin, args).await
        }
    }

    #[tokio::test]
    async fn probe_password_auth_and_deploy_key_pass_password_via_env_not_argv() {
        let runner = EnvCapturingRunner {
            calls: std::sync::Mutex::new(Vec::new()),
        };
        let secret = "super_secret_password_123";

        let _ = probe_password_auth(&runner, &host(), None, secret).await;
        let _ = deploy_public_key(&runner, &host(), None, secret).await;

        let calls = runner.calls.lock().unwrap();
        assert_eq!(calls.len(), 2);
        for (args, env) in calls.iter() {
            assert!(args.contains(&"-e".to_string()));
            assert!(!args.contains(&secret.to_string()));
            assert!(env.contains(&("SSHPASS".to_string(), secret.to_string())));
        }
    }

    #[tokio::test]
    async fn shared_connection_options_present_on_all_three_ssh_argv_paths() {
        // Guards push_connection_options: a future SSH call site that forgets to route through
        // it (or that drops StrictHostKeyChecking/ConnectTimeout from it) fails this test, the
        // same class of regression AGENTS.md documents for probe_key_auth.
        let build_args = build_ssh_target_args(&host(), None, &[]);
        assert!(build_args.contains(&"StrictHostKeyChecking=accept-new".to_string()));
        assert!(build_args.contains(&"ConnectTimeout=5".to_string()));

        let runner = EnvCapturingRunner {
            calls: std::sync::Mutex::new(Vec::new()),
        };
        let _ = probe_password_auth(&runner, &host(), None, "irrelevant").await;
        let _ = deploy_public_key(&runner, &host(), None, "irrelevant").await;

        let calls = runner.calls.lock().unwrap();
        assert_eq!(calls.len(), 2);
        for (args, _env) in calls.iter() {
            assert!(args.contains(&"StrictHostKeyChecking=accept-new".to_string()));
            assert!(args.contains(&"ConnectTimeout=5".to_string()));
        }
    }

    #[tokio::test]
    async fn probe_auth_dispatches_key_hosts_to_key_auth_without_touching_env_runner() {
        // Key mode (the default/unchanged path) must never go through run_with_env: no
        // password exists to carry, so it should behave exactly like probe_key_auth.
        let runner = EnvCapturingRunner {
            calls: std::sync::Mutex::new(Vec::new()),
        };
        let result = probe_auth(&runner, &host(), None, None).await;
        assert!(result.reachable);
        assert_eq!(runner.calls.lock().unwrap().len(), 0);
    }

    #[tokio::test]
    async fn probe_auth_dispatches_password_hosts_to_sshpass_with_no_batchmode() {
        let mut h = host();
        h.auth = AuthMode::Password;
        let runner = EnvCapturingRunner {
            calls: std::sync::Mutex::new(Vec::new()),
        };
        let secret = "super_secret_password_123";
        let result = probe_auth(&runner, &h, None, Some(secret)).await;
        assert!(result.reachable);

        let calls = runner.calls.lock().unwrap();
        assert_eq!(calls.len(), 1);
        let (args, env) = &calls[0];
        // BatchMode=yes would block the interactive password prompt sshpass answers, so the
        // password-mode argv must never include it (see push_connection_options's doc comment).
        assert!(!args.contains(&"BatchMode=yes".to_string()));
        assert!(args.contains(&"StrictHostKeyChecking=accept-new".to_string()));
        assert!(args.contains(&"-e".to_string()));
        assert!(!args.contains(&secret.to_string()));
        assert!(env.contains(&("SSHPASS".to_string(), secret.to_string())));
    }

    #[tokio::test]
    async fn probe_auth_reports_password_hosts_unreachable_without_a_password() {
        let mut h = host();
        h.auth = AuthMode::Password;
        let runner = EnvCapturingRunner {
            calls: std::sync::Mutex::new(Vec::new()),
        };
        let result = probe_auth(&runner, &h, None, None).await;
        assert!(!result.reachable);
        assert!(result.detail.contains("password"));
        // No process should be spawned when there is nothing to authenticate with.
        assert_eq!(runner.calls.lock().unwrap().len(), 0);
    }

    #[test]
    fn host_deserializes_default_auth_mode_as_key_for_legacy_yaml_without_the_field() {
        // Regression: profile YAML persisted before Issue #26 has no `auth` field at all.
        // #[serde(default)] on Host::auth must make that deserialize as Key, not fail.
        let legacy_yaml =
            "name: cka-m1\naddress: 192.0.2.10\nport: 22\nuser: root\nidentity_file: null\n";
        let parsed: Host = serde_yaml::from_str(legacy_yaml).expect("legacy host YAML must parse");
        assert_eq!(parsed.auth, AuthMode::Key);
    }

    #[test]
    fn host_round_trips_password_auth_mode_through_yaml() {
        let mut h = host();
        h.auth = AuthMode::Password;
        let yaml = serde_yaml::to_string(&h).unwrap();
        assert!(yaml.contains("auth: password"));
        let parsed: Host = serde_yaml::from_str(&yaml).unwrap();
        assert_eq!(parsed.auth, AuthMode::Password);
    }

    /// Real-process regression: FakeRunner tests above only prove build_ssh_target_args
    /// returns the right Vec<String>, not that a real spawned process actually receives
    /// those exact argv strings intact (shell/exec quoting, arg splitting, etc. can differ
    /// from a mocked call). Spawns a real OS process (a stub `ssh` script, since
    /// CommandRunner::run's resolve_cli_path only searches fixed system dirs and we must
    /// not shadow the real /usr/bin/ssh) via the same tokio::process::Command mechanism
    /// SystemRunner uses, and reads back the argv the OS actually delivered.
    /// AGENTS.md: this class of bug (missing StrictHostKeyChecking on probe_key_auth) was
    /// only caught by a real end-to-end SSH test, not FakeRunner unit tests.
    #[tokio::test]
    #[ignore = "spawns a real OS process; run manually with `cargo test -- --ignored`"]
    async fn real_process_receives_batchmode_stricthostkeychecking_and_proxyjump_argv() {
        use crate::services::config::Bastion;
        use std::os::unix::fs::PermissionsExt;

        let dir = std::env::temp_dir().join(format!(
            "clusterdeck-real-ssh-argv-test-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let capture_path = dir.join("captured_argv.txt");
        let stub_path = dir.join("ssh");
        std::fs::write(
            &stub_path,
            format!(
                "#!/bin/sh\nprintf '%s\\n' \"$@\" > {}\n",
                capture_path.display()
            ),
        )
        .unwrap();
        let mut perms = std::fs::metadata(&stub_path).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&stub_path, perms).unwrap();

        let bastion = Bastion {
            name: "bastion01".into(),
            address: "10.0.0.10".into(),
            port: 22,
            user: "ubuntu".into(),
            identity_file: None,
        };
        let args = build_ssh_target_args(&host(), Some(&bastion), &["true"]);

        let output = tokio::process::Command::new(&stub_path)
            .args(&args)
            .output()
            .await
            .expect("failed to spawn stub ssh");
        assert!(output.status.success());

        let captured = std::fs::read_to_string(&capture_path).unwrap();
        assert!(
            captured.contains("BatchMode=yes"),
            "missing BatchMode=yes in real argv: {captured}"
        );
        assert!(
            captured.contains("StrictHostKeyChecking=accept-new"),
            "missing StrictHostKeyChecking=accept-new in real argv: {captured}"
        );
        assert!(
            captured.contains("-J"),
            "missing -J (ProxyJump) in real argv: {captured}"
        );
        assert!(
            captured.contains("ubuntu@10.0.0.10"),
            "missing bastion target in real argv: {captured}"
        );
        assert!(
            captured.contains(&format!("{}@{}", host().user, host().address)),
            "missing target host in real argv: {captured}"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn is_host_key_changed_error_detects_all_patterns() {
        let actual_error = r#"
@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@
@ WARNING: REMOTE HOST IDENTIFICATION HAS CHANGED! @
@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@
IT IS POSSIBLE THAT SOMEONE IS DOING SOMETHING NASTY!
Someone could be eavesdropping on you right now (man-in-the-middle attack)!
It is also possible that a host key has just been changed.
The fingerprint for the ED25519 key sent by the remote host is SHA256:VuSZc8Rg1PGTVRvt6IzIShJ0C0ssgZZrFQMVDeMkXns.
Please contact your system administrator.
Add correct host key in /Users/m/.ssh/known_hosts to get rid of this message.
Offending ED25519 key in /Users/m/.ssh/known_hosts:2
Host key for 172.16.221.136 has changed and you have requested strict checking.
Host key verification failed.
"#;
        assert!(is_host_key_changed_error(actual_error));
        assert!(is_host_key_changed_error(
            "Host key for 10.0.0.1 has changed"
        ));
        assert!(!is_host_key_changed_error("Permission denied (publickey)"));
        assert!(!is_host_key_changed_error("Connection timed out"));
    }

    #[test]
    fn extract_offending_known_hosts_file_extracts_path() {
        let stderr1 =
            "Offending ED25519 key in /Users/m/.ssh/known_hosts:2\nHost key verification failed.";
        assert_eq!(
            extract_offending_known_hosts_file(stderr1),
            Some("/Users/m/.ssh/known_hosts".to_string())
        );

        let stderr2 = "Add correct host key in /custom/hosts to get rid of this message.";
        assert_eq!(
            extract_offending_known_hosts_file(stderr2),
            Some("/custom/hosts".to_string())
        );

        let stderr3 = "Permission denied (publickey)";
        assert_eq!(extract_offending_known_hosts_file(stderr3), None);
    }

    struct HostKeyPruningTestRunner {
        calls: std::sync::Mutex<Vec<(String, Vec<String>)>>,
    }

    #[async_trait]
    impl CommandRunner for HostKeyPruningTestRunner {
        async fn run(&self, bin: &str, args: &[String]) -> Result<CommandOutput, String> {
            let mut calls = self.calls.lock().unwrap();
            calls.push((bin.to_string(), args.to_vec()));
            let call_count = calls.len();
            drop(calls);

            if bin == "ssh" && call_count == 1 {
                Ok(CommandOutput {
                    stdout: String::new(),
                    stderr: "WARNING: REMOTE HOST IDENTIFICATION HAS CHANGED!\nOffending ED25519 key in /Users/m/.ssh/known_hosts:2\nHost key verification failed.".into(),
                    success: false,
                })
            } else if bin == "ssh-keygen" {
                Ok(CommandOutput {
                    stdout: "# Host found and updated".into(),
                    stderr: String::new(),
                    success: true,
                })
            } else if bin == "ssh" {
                Ok(CommandOutput {
                    stdout: String::new(),
                    stderr: String::new(),
                    success: true,
                })
            } else {
                Err(format!("unexpected bin: {bin}"))
            }
        }
    }

    #[tokio::test]
    async fn probe_key_auth_auto_prunes_stale_host_key_and_retries_successfully() {
        let runner = HostKeyPruningTestRunner {
            calls: std::sync::Mutex::new(Vec::new()),
        };

        let result = probe_key_auth(&runner, &host(), None).await;
        assert!(result.reachable);
        assert_eq!(result.detail, "SSH key auth succeeded");

        let recorded = runner.calls.lock().unwrap();
        // Call 1: ssh (failed with host key mismatch)
        assert_eq!(recorded[0].0, "ssh");
        // Call 2: ssh-keygen -f /Users/m/.ssh/known_hosts -R 192.0.2.10
        assert_eq!(recorded[1].0, "ssh-keygen");
        assert_eq!(
            recorded[1].1,
            vec![
                "-f".to_string(),
                "/Users/m/.ssh/known_hosts".to_string(),
                "-R".to_string(),
                "192.0.2.10".to_string(),
            ]
        );
        // Call 3: ssh-keygen -R 192.0.2.10 (default known_hosts)
        assert_eq!(recorded[2].0, "ssh-keygen");
        assert_eq!(
            recorded[2].1,
            vec!["-R".to_string(), "192.0.2.10".to_string()]
        );
        // Call 4: ssh retry (succeeded)
        assert_eq!(recorded[3].0, "ssh");
    }
}
