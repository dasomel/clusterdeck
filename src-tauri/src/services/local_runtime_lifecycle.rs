#![allow(dead_code)]

//! Phase 2 (issue #14) — lifecycle actions on Colima/Lima instances discovered by
//! `services/local_runtime.rs`. See `docs/adr/0007-local-runtime-lifecycle-actions.md` for the
//! design decisions this module implements.
//!
//! Every action here re-runs `local_runtime::detect_local_hosts` and requires the
//! `(provider, instance_name)` pair to still be present in that fresh listing before doing
//! anything else — the instance name and any docker/kube context string used below is always
//! read off that fresh row, never trusted from a caller-supplied value, closing off a class of
//! sink-trusts-unvalidated-input bug AGENTS.md calls out as a repeat offender in this codebase.

use serde::Serialize;
use std::collections::HashSet;
use std::str::FromStr;
use std::sync::Mutex;
use std::time::Duration;

use crate::services::local_runtime::{self, DiscoveredLocalHost};
use crate::services::process::{self, CommandOutput, CommandRunner};
use crate::services::validate;

/// Lifecycle calls can take minutes for `start` (VM boot, container runtime init), so the
/// timeout is generous, per AGENTS.md's "keep external command execution asynchronous and
/// cancellable where practical" — this is the cancellable-by-timeout half of that; true
/// mid-flight user cancellation is deferred (ADR-0007 D7).
const LIFECYCLE_TIMEOUT: Duration = Duration::from_secs(600);

/// `find_fresh_instance`'s re-discovery re-runs the full `detect_local_hosts` sweep (Colima +
/// Lima + Vagrant + `docker context ls`), which is a handful of local CLI calls, not a
/// minutes-long VM operation — bounded separately from `LIFECYCLE_TIMEOUT` so a hung `vagrant`
/// (the heaviest of the three, per ADR-0005) can't stall a Start/Stop/Restart/Shell/Context call
/// indefinitely while `LifecycleGuard`'s per-instance lock is held.
const DISCOVERY_TIMEOUT: Duration = Duration::from_secs(30);

/// The provider dispatch for lifecycle actions. Deliberately narrower than
/// `DiscoveredLocalHost.provider: String` (which also carries "Vagrant") — Phase 2 only builds
/// start/stop/restart/shell for Colima and Lima (ADR-0007 D1); routing a free string into argv
/// is exactly what AGENTS.md's validate-at-every-sink rule exists to prevent, so the wire value
/// is parsed into this enum and rejected outright if it is anything else.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LocalRuntimeProvider {
    Colima,
    Lima,
}

impl LocalRuntimeProvider {
    /// Matches `DiscoveredLocalHost.provider`'s existing capitalized display string, which
    /// predates this enum (`services/local_runtime.rs`) and is left unchanged here.
    fn display_name(self) -> &'static str {
        match self {
            LocalRuntimeProvider::Colima => "Colima",
            LocalRuntimeProvider::Lima => "Lima",
        }
    }
}

impl FromStr for LocalRuntimeProvider {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "colima" => Ok(LocalRuntimeProvider::Colima),
            "lima" => Ok(LocalRuntimeProvider::Lima),
            other => Err(format!("unknown local runtime provider: {other}")),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct LifecycleActionResult {
    pub success: bool,
    /// Tail of stdout (on success) or stderr (on failure) from the underlying CLI. Never
    /// includes identity_file contents or any other secret — `colima`/`limactl` don't print
    /// those on start/stop/restart.
    pub message: String,
}

/// Per-(provider, instance) in-flight marker, so a second Start/Stop/Restart on the same
/// instance while one is already running is rejected instead of racing two CLI invocations
/// against the same VM. Managed as Tauri app state (`lib.rs`'s `.manage(...)`).
#[derive(Debug, Default)]
pub struct LifecycleGuard(Mutex<HashSet<(LocalRuntimeProvider, String)>>);

#[derive(Debug)]
pub struct LifecycleLock<'a> {
    guard: &'a LifecycleGuard,
    key: (LocalRuntimeProvider, String),
}

impl LifecycleGuard {
    pub fn try_acquire(
        &self,
        provider: LocalRuntimeProvider,
        instance_name: &str,
    ) -> Result<LifecycleLock<'_>, String> {
        let mut set = self
            .0
            .lock()
            .map_err(|_| "lifecycle guard lock poisoned".to_string())?;
        let key = (provider, instance_name.to_string());
        if !set.insert(key.clone()) {
            return Err(format!(
                "another operation is already in progress for {instance_name}"
            ));
        }
        Ok(LifecycleLock { guard: self, key })
    }
}

impl Drop for LifecycleLock<'_> {
    fn drop(&mut self) {
        if let Ok(mut set) = self.guard.0.lock() {
            set.remove(&self.key);
        }
    }
}

/// Validates `instance_name`'s charset, then confirms the `(provider, instance_name)` pair is
/// present in a fresh discovery listing, returning the freshly discovered row. This is the
/// re-check ADR-0007 D6 requires before any lifecycle/shell/context action.
async fn find_fresh_instance(
    runner: &dyn CommandRunner,
    provider: LocalRuntimeProvider,
    instance_name: &str,
) -> Result<DiscoveredLocalHost, String> {
    if !validate::is_safe_local_runtime_instance_name(instance_name) {
        return Err(format!("invalid instance name: {instance_name}"));
    }
    let hosts =
        match tokio::time::timeout(DISCOVERY_TIMEOUT, local_runtime::detect_local_hosts(runner))
            .await
        {
            Ok(res) => res?,
            Err(_) => {
                return Err(format!(
                    "local runtime discovery timed out after {}s",
                    DISCOVERY_TIMEOUT.as_secs()
                ))
            }
        };
    hosts
        .into_iter()
        .find(|h| h.provider == provider.display_name() && h.instance_name == instance_name)
        .ok_or_else(|| {
            format!(
                "{} instance not found in current discovery listing: {instance_name}",
                provider.display_name()
            )
        })
}

async fn run_with_timeout(
    runner: &dyn CommandRunner,
    bin: &str,
    args: &[String],
) -> Result<CommandOutput, String> {
    match tokio::time::timeout(LIFECYCLE_TIMEOUT, runner.run(bin, args)).await {
        Ok(res) => res,
        Err(_) => Err(format!(
            "{bin} timed out after {}s",
            LIFECYCLE_TIMEOUT.as_secs()
        )),
    }
}

/// Last 20 lines of stdout (success) or stderr (failure) — enough to show the user what
/// happened without dumping a full VM boot log into the UI.
fn tail_message(output: &CommandOutput) -> String {
    let text = if output.success {
        &output.stdout
    } else {
        &output.stderr
    };
    let mut lines: Vec<&str> = text.lines().rev().take(20).collect();
    lines.reverse();
    if lines.is_empty() {
        return if output.success {
            "OK".to_string()
        } else {
            "failed".to_string()
        };
    }
    lines.join("\n")
}

/// Colima argv for starting `instance_name`. Always passes `--activate=false`: `colima start`
/// defaults to `--activate=true` ("set as active Docker/Kubernetes/Incus context on startup",
/// confirmed via `colima start --help` on 0.10.3), which would silently switch the user's
/// *global* `docker context`/kube current-context out from under them — exactly what ADR-0007 D3
/// and ADR-0002 forbid. `--activate=false` is a boolean flag and must be passed as a single
/// `--activate=false` token, not `--activate false` — pflag/cobra bool flags do not consume a
/// following bare argument as their value.
fn colima_start_args(instance_name: &str) -> Vec<String> {
    vec![
        "start".to_string(),
        "--activate=false".to_string(),
        "--profile".to_string(),
        instance_name.to_string(),
    ]
}

pub async fn start_instance(
    runner: &dyn CommandRunner,
    provider: LocalRuntimeProvider,
    instance_name: &str,
) -> Result<LifecycleActionResult, String> {
    find_fresh_instance(runner, provider, instance_name).await?;
    let output = match provider {
        LocalRuntimeProvider::Colima => {
            run_with_timeout(runner, "colima", &colima_start_args(instance_name)).await?
        }
        LocalRuntimeProvider::Lima => {
            // limactl has no context-switching flag or concept (confirmed: no "context" mention
            // anywhere in `limactl --help`/`limactl start --help` on 2.2.0) — nothing here can
            // mutate global Docker/kube state the way Colima's default `--activate=true` did.
            run_with_timeout(
                runner,
                "limactl",
                &["start".to_string(), instance_name.to_string()],
            )
            .await?
        }
    };
    Ok(LifecycleActionResult {
        success: output.success,
        message: tail_message(&output),
    })
}

pub async fn stop_instance(
    runner: &dyn CommandRunner,
    provider: LocalRuntimeProvider,
    instance_name: &str,
) -> Result<LifecycleActionResult, String> {
    find_fresh_instance(runner, provider, instance_name).await?;
    let output = match provider {
        LocalRuntimeProvider::Colima => {
            run_with_timeout(
                runner,
                "colima",
                &[
                    "stop".to_string(),
                    "--profile".to_string(),
                    instance_name.to_string(),
                ],
            )
            .await?
        }
        LocalRuntimeProvider::Lima => {
            run_with_timeout(
                runner,
                "limactl",
                &["stop".to_string(), instance_name.to_string()],
            )
            .await?
        }
    };
    Ok(LifecycleActionResult {
        success: output.success,
        message: tail_message(&output),
    })
}

/// Restart is stop-then-start for both providers, run under a single combined
/// `LIFECYCLE_TIMEOUT` deadline (not one per call, so the worst case stays 10 minutes instead of
/// 20). If stop fails, start is not attempted.
///
/// Neither provider uses its own native restart subcommand: `colima restart` has **no**
/// `--activate` flag at all (confirmed via `colima restart --help` on 0.10.3), so it cannot be
/// made to skip switching the global Docker/kube context the way `colima start --activate=false`
/// can — stop-then-start-with-`--activate=false` is the only way to restart a Colima instance
/// without that side effect. Lima's `limactl restart` was available but is not used, to keep both
/// providers' restart implemented the same way rather than a third CLI surface to reason about
/// (ADR-0007 D1); this is a low-cost consistency choice since Lima's start/stop are already
/// idempotent and side-effect-free with respect to global context (see `start_instance`).
pub async fn restart_instance(
    runner: &dyn CommandRunner,
    provider: LocalRuntimeProvider,
    instance_name: &str,
) -> Result<LifecycleActionResult, String> {
    find_fresh_instance(runner, provider, instance_name).await?;
    let (stop_bin, stop_args, start_bin, start_args) = match provider {
        LocalRuntimeProvider::Colima => (
            "colima",
            vec![
                "stop".to_string(),
                "--profile".to_string(),
                instance_name.to_string(),
            ],
            "colima",
            colima_start_args(instance_name),
        ),
        LocalRuntimeProvider::Lima => (
            "limactl",
            vec!["stop".to_string(), instance_name.to_string()],
            "limactl",
            vec!["start".to_string(), instance_name.to_string()],
        ),
    };

    let combined = tokio::time::timeout(LIFECYCLE_TIMEOUT, async {
        let stop_out = runner.run(stop_bin, &stop_args).await?;
        if !stop_out.success {
            return Ok(LifecycleActionResult {
                success: false,
                message: format!("stop failed: {}", tail_message(&stop_out)),
            });
        }
        let start_out = runner.run(start_bin, &start_args).await?;
        Ok(LifecycleActionResult {
            success: start_out.success,
            message: tail_message(&start_out),
        })
    })
    .await;

    match combined {
        Ok(result) => result,
        Err(_) => Err(format!(
            "{stop_bin} restart timed out after {}s",
            LIFECYCLE_TIMEOUT.as_secs()
        )),
    }
}

/// Opens a Terminal window running an interactive shell inside the VM (D2):
/// `colima ssh --profile <name>` / `limactl shell <name>`. `instance_name` is re-validated and
/// re-confirmed against a fresh discovery listing by `find_fresh_instance` before it is embedded
/// in the osascript string.
pub async fn open_shell(
    runner: &dyn CommandRunner,
    provider: LocalRuntimeProvider,
    instance_name: &str,
) -> Result<(), String> {
    find_fresh_instance(runner, provider, instance_name).await?;
    let command_line = match provider {
        LocalRuntimeProvider::Colima => format!("colima ssh --profile {instance_name}"),
        LocalRuntimeProvider::Lima => format!("limactl shell {instance_name}"),
    };
    process::open_terminal_with_command(runner, &command_line).await
}

/// Builds the D3 Terminal script (`export DOCKER_CONTEXT=...` / `alias kubectl=...`) from an
/// already-freshly-discovered row. Split out from `open_context_shell` as a pure, synchronous
/// seam so the "a context that fails validation is omitted" behavior can be unit-tested directly
/// against a crafted `DiscoveredLocalHost`, without needing a `CommandRunner` or a live
/// discovery pass.
fn build_context_script(instance: &DiscoveredLocalHost) -> Result<String, String> {
    let mut lines = Vec::new();
    if let Some(ctx) = instance.docker_context.as_deref() {
        if validate::is_safe_shell_context_name(ctx) {
            lines.push(format!("export DOCKER_CONTEXT={ctx}"));
        }
    }
    if let Some(ctx) = instance.kube_context.as_deref() {
        if validate::is_safe_shell_context_name(ctx) {
            lines.push(format!("alias kubectl='kubectl --context {ctx}'"));
        }
    }

    if lines.is_empty() {
        return Err(format!(
            "no docker/kube context available for {}",
            instance.instance_name
        ));
    }

    Ok(lines.join("; "))
}

/// Opens a Terminal window on the host (not the VM) with a session-scoped `DOCKER_CONTEXT`
/// export and a `kubectl` alias bound to `--context` (D3). Never runs `docker context use` or
/// `kubectl config use-context` — nothing here mutates the user's global Docker/kube state.
/// Docker/kube context strings are read off the fresh discovery row, not any caller-supplied
/// value, and each is independently re-checked by `validate::is_safe_shell_context_name`; a
/// context that fails that check is omitted rather than quoted defensively (ADR-0007 D6).
pub async fn open_context_shell(
    runner: &dyn CommandRunner,
    provider: LocalRuntimeProvider,
    instance_name: &str,
) -> Result<(), String> {
    let instance = find_fresh_instance(runner, provider, instance_name).await?;
    let script = build_context_script(&instance)?;
    process::open_terminal_with_command(runner, &script).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use std::sync::Mutex as StdMutex;

    struct FakeRunner {
        colima_list: &'static str,
        lima_list: &'static str,
        docker_contexts: &'static str,
        calls: StdMutex<Vec<(String, Vec<String>)>>,
    }

    impl FakeRunner {
        fn new(colima_list: &'static str, lima_list: &'static str) -> Self {
            Self {
                colima_list,
                lima_list,
                docker_contexts: "",
                calls: StdMutex::new(Vec::new()),
            }
        }

        fn with_docker_contexts(mut self, docker_contexts: &'static str) -> Self {
            self.docker_contexts = docker_contexts;
            self
        }
    }

    #[async_trait]
    impl CommandRunner for FakeRunner {
        async fn run(&self, bin: &str, args: &[String]) -> Result<CommandOutput, String> {
            self.calls
                .lock()
                .unwrap()
                .push((bin.to_string(), args.to_vec()));
            match bin {
                "colima" if args.first().map(String::as_str) == Some("list") => Ok(CommandOutput {
                    stdout: self.colima_list.to_string(),
                    stderr: String::new(),
                    success: true,
                }),
                "colima" if args.first().map(String::as_str) == Some("ssh-config") => {
                    Ok(CommandOutput {
                        stdout: String::new(),
                        stderr: String::new(),
                        success: true,
                    })
                }
                "colima" => Ok(CommandOutput {
                    stdout: "colima ok".to_string(),
                    stderr: String::new(),
                    success: true,
                }),
                "limactl" if args.first().map(String::as_str) == Some("list") => {
                    Ok(CommandOutput {
                        stdout: self.lima_list.to_string(),
                        stderr: String::new(),
                        success: true,
                    })
                }
                "limactl" => Ok(CommandOutput {
                    stdout: "limactl ok".to_string(),
                    stderr: String::new(),
                    success: true,
                }),
                "osascript" => Ok(CommandOutput {
                    stdout: String::new(),
                    stderr: String::new(),
                    success: true,
                }),
                "docker" => Ok(CommandOutput {
                    stdout: self.docker_contexts.to_string(),
                    stderr: String::new(),
                    success: true,
                }),
                _ => Err(format!("unexpected command {bin}")),
            }
        }
    }

    const COLIMA_LIST: &str = r#"{"name":"default","status":"Running","runtime":"docker+k3s"}"#;
    const LIMA_LIST: &str =
        r#"{"name":"work","status":"Running","sshAddress":"127.0.0.1","sshLocalPort":50000}"#;

    fn args_for(calls: &[(String, Vec<String>)], bin: &str) -> Vec<Vec<String>> {
        calls
            .iter()
            .filter(|(b, _)| b == bin)
            .map(|(_, a)| a.clone())
            .collect()
    }

    #[tokio::test]
    async fn start_colima_builds_expected_argv() {
        let runner = FakeRunner::new(COLIMA_LIST, "");
        let result = start_instance(&runner, LocalRuntimeProvider::Colima, "default")
            .await
            .unwrap();
        assert!(result.success);

        let calls = runner.calls.lock().unwrap();
        let colima_calls = args_for(&calls, "colima");
        assert_eq!(
            colima_calls.last().unwrap(),
            &vec![
                "start".to_string(),
                "--activate=false".to_string(),
                "--profile".to_string(),
                "default".to_string()
            ]
        );
    }

    #[tokio::test]
    async fn stop_lima_builds_expected_argv() {
        let runner = FakeRunner::new("", LIMA_LIST);
        let result = stop_instance(&runner, LocalRuntimeProvider::Lima, "work")
            .await
            .unwrap();
        assert!(result.success);

        let calls = runner.calls.lock().unwrap();
        let lima_calls = args_for(&calls, "limactl");
        assert_eq!(
            lima_calls.last().unwrap(),
            &vec!["stop".to_string(), "work".to_string()]
        );
    }

    #[tokio::test]
    async fn restart_colima_calls_stop_then_start_with_activate_false_in_order() {
        let runner = FakeRunner::new(COLIMA_LIST, "");
        restart_instance(&runner, LocalRuntimeProvider::Colima, "default")
            .await
            .unwrap();

        let calls = runner.calls.lock().unwrap();
        let colima_calls = args_for(&calls, "colima");
        // Preceding calls are the fresh-discovery `list`/`ssh-config`; find the stop/start pair
        // by their own first argv token rather than a fixed index, since the discovery call
        // shape is local_runtime.rs's concern, not this test's. `colima restart` has no
        // `--activate` flag (ADR-0007), so restart cannot use it and must go through the same
        // stop+start path `start_instance` uses, stop strictly before start.
        let stop_pos = colima_calls
            .iter()
            .position(|a| a.first().map(String::as_str) == Some("stop"))
            .expect("expected a colima stop call");
        let start_pos = colima_calls
            .iter()
            .position(|a| a.first().map(String::as_str) == Some("start"))
            .expect("expected a colima start call");
        assert!(stop_pos < start_pos, "stop must run before start");

        assert_eq!(
            colima_calls[stop_pos],
            vec![
                "stop".to_string(),
                "--profile".to_string(),
                "default".to_string()
            ]
        );
        assert_eq!(
            colima_calls[start_pos],
            vec![
                "start".to_string(),
                "--activate=false".to_string(),
                "--profile".to_string(),
                "default".to_string()
            ]
        );
    }

    #[tokio::test]
    async fn restart_lima_calls_stop_then_start_in_order() {
        let runner = FakeRunner::new("", LIMA_LIST);
        restart_instance(&runner, LocalRuntimeProvider::Lima, "work")
            .await
            .unwrap();

        let calls = runner.calls.lock().unwrap();
        let lima_calls = args_for(&calls, "limactl");
        // First call is the fresh-discovery `list`; the next two must be stop then start, in
        // that order, both scoped to "work".
        assert_eq!(lima_calls[1], vec!["stop".to_string(), "work".to_string()]);
        assert_eq!(lima_calls[2], vec!["start".to_string(), "work".to_string()]);
    }

    #[tokio::test]
    async fn unknown_instance_rejected() {
        let runner = FakeRunner::new(COLIMA_LIST, "");
        let err = start_instance(&runner, LocalRuntimeProvider::Colima, "does-not-exist")
            .await
            .unwrap_err();
        assert!(err.contains("not found"));

        // No lifecycle argv was issued for the rejected instance.
        let calls = runner.calls.lock().unwrap();
        for (_, args) in calls.iter() {
            assert!(!args.iter().any(|a| a == "does-not-exist"));
        }
    }

    #[tokio::test]
    async fn invalid_instance_name_rejected_before_any_command() {
        let runner = FakeRunner::new(COLIMA_LIST, "");
        let err = start_instance(&runner, LocalRuntimeProvider::Colima, "-oProxyCommand=evil")
            .await
            .unwrap_err();
        assert!(err.contains("invalid instance name"));

        // Rejected before even the fresh-discovery re-check ran.
        assert!(runner.calls.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn open_shell_builds_terminal_script_for_colima() {
        let runner = FakeRunner::new(COLIMA_LIST, "");
        open_shell(&runner, LocalRuntimeProvider::Colima, "default")
            .await
            .unwrap();

        let calls = runner.calls.lock().unwrap();
        let osascript_calls = args_for(&calls, "osascript");
        assert_eq!(osascript_calls.len(), 1);
        assert!(osascript_calls[0][1].contains("colima ssh --profile default"));
    }

    #[tokio::test]
    async fn open_shell_builds_terminal_script_for_lima() {
        let runner = FakeRunner::new("", LIMA_LIST);
        open_shell(&runner, LocalRuntimeProvider::Lima, "work")
            .await
            .unwrap();

        let calls = runner.calls.lock().unwrap();
        let osascript_calls = args_for(&calls, "osascript");
        assert_eq!(osascript_calls.len(), 1);
        assert!(osascript_calls[0][1].contains("limactl shell work"));
    }

    #[tokio::test]
    async fn open_context_shell_includes_both_exports_when_present() {
        let colima_list = r#"{"name":"default","status":"Running","runtime":"docker+k3s"}"#;
        // `detect_colima` only sets `docker_context` when the discovered name (`colima`, for the
        // default profile) is actually present in `docker context ls`'s output.
        let docker_contexts = r#"{"Name":"colima"}"#;
        let runner = FakeRunner::new(colima_list, "").with_docker_contexts(docker_contexts);

        open_context_shell(&runner, LocalRuntimeProvider::Colima, "default")
            .await
            .unwrap();

        let calls = runner.calls.lock().unwrap();
        let osascript_calls = args_for(&calls, "osascript");
        assert_eq!(osascript_calls.len(), 1);
        assert!(osascript_calls[0][1].contains("export DOCKER_CONTEXT=colima"));
        assert!(osascript_calls[0][1].contains("alias kubectl='kubectl --context colima'"));
    }

    #[tokio::test]
    async fn open_context_shell_omits_only_kube_context_when_docker_context_absent() {
        let colima_list = r#"{"name":"default","status":"Running","runtime":"docker+k3s"}"#;
        // No `docker` context reported -> docker_context stays None; kube_context is still
        // derived from the `runtime` field's k3s marker, so only the kubectl alias is emitted.
        let runner = FakeRunner::new(colima_list, "");

        open_context_shell(&runner, LocalRuntimeProvider::Colima, "default")
            .await
            .unwrap();

        let calls = runner.calls.lock().unwrap();
        let osascript_calls = args_for(&calls, "osascript");
        assert_eq!(osascript_calls.len(), 1);
        assert!(!osascript_calls[0][1].contains("DOCKER_CONTEXT"));
        assert!(osascript_calls[0][1].contains("alias kubectl='kubectl --context colima'"));
    }

    #[tokio::test]
    async fn open_context_shell_errors_when_no_context_available() {
        let colima_list = r#"{"name":"default","status":"Running","runtime":"docker"}"#;
        let runner = FakeRunner::new(colima_list, "");
        let err = open_context_shell(&runner, LocalRuntimeProvider::Colima, "default")
            .await
            .unwrap_err();
        assert!(err.contains("no docker/kube context available"));

        // No Terminal window was opened.
        let calls = runner.calls.lock().unwrap();
        assert!(args_for(&calls, "osascript").is_empty());
    }

    fn fake_discovered_host(
        docker_context: Option<&str>,
        kube_context: Option<&str>,
    ) -> DiscoveredLocalHost {
        DiscoveredLocalHost {
            provider: "Colima".to_string(),
            instance_name: "default".to_string(),
            status: "Running".to_string(),
            host_name: "colima-vm".to_string(),
            address: "127.0.0.1".to_string(),
            port: 22,
            user: "root".to_string(),
            identity_file: None,
            runtime: None,
            kube_context: kube_context.map(str::to_string),
            kube_remote_path: None,
            arch: None,
            cpus: None,
            memory_bytes: None,
            disk_bytes: None,
            docker_context: docker_context.map(str::to_string),
        }
    }

    #[test]
    fn build_context_script_omits_context_that_fails_validation() {
        // kube_context has a space and a semicolon, so it fails is_safe_shell_context_name and
        // must be dropped rather than quoted defensively; docker_context is valid and kept.
        let instance = fake_discovered_host(Some("colima"), Some("bad context; rm -rf /"));
        let script = build_context_script(&instance).unwrap();
        assert!(script.contains("export DOCKER_CONTEXT=colima"));
        assert!(!script.contains("kubectl"));
    }

    #[test]
    fn build_context_script_errors_when_all_contexts_fail_validation() {
        let instance = fake_discovered_host(Some("bad docker"), Some("bad kube"));
        let err = build_context_script(&instance).unwrap_err();
        assert!(err.contains("no docker/kube context available"));
    }

    #[test]
    fn lifecycle_guard_rejects_concurrent_op_on_same_instance() {
        let guard = LifecycleGuard::default();
        let lock1 = guard
            .try_acquire(LocalRuntimeProvider::Colima, "default")
            .unwrap();
        let err = guard
            .try_acquire(LocalRuntimeProvider::Colima, "default")
            .unwrap_err();
        assert!(err.contains("already in progress"));

        // A different instance is unaffected.
        assert!(guard
            .try_acquire(LocalRuntimeProvider::Colima, "other")
            .is_ok());

        drop(lock1);
        // Released after drop, so the same key can be acquired again.
        assert!(guard
            .try_acquire(LocalRuntimeProvider::Colima, "default")
            .is_ok());
    }

    #[test]
    fn provider_from_str_rejects_unknown_values() {
        assert_eq!(
            LocalRuntimeProvider::from_str("colima"),
            Ok(LocalRuntimeProvider::Colima)
        );
        assert_eq!(
            LocalRuntimeProvider::from_str("lima"),
            Ok(LocalRuntimeProvider::Lima)
        );
        assert!(LocalRuntimeProvider::from_str("vagrant").is_err());
        assert!(LocalRuntimeProvider::from_str("Colima").is_err());
    }
}
