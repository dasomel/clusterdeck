#![allow(dead_code)]

use async_trait::async_trait;
use std::collections::HashSet;
use std::path::PathBuf;
use tokio::process::Command;

const SEARCH_PATHS: [&str; 4] = ["/opt/homebrew/bin", "/usr/local/bin", "/usr/bin", "/bin"];

/// Parses a `PATH`-style variable into absolute directories only, in order. Relative and empty
/// entries (e.g. a leading/trailing/doubled `:`, historically meaning "cwd") are dropped: this
/// feeds a security-relevant lookup that ends in a `CommandRunner` invocation, and a
/// cwd-relative resolution here would let whatever directory the app happens to be launched
/// from shadow a system binary. Pulled out of `search_dirs` as a pure function so tests can
/// exercise the parsing without mutating the real process's `PATH` (an env var is global,
/// process-wide, shared-mutable state -- unsafe to touch from a test that may run in parallel
/// with others in the same test binary).
fn parse_path_env_dirs(path_var: &std::ffi::OsStr) -> Vec<PathBuf> {
    std::env::split_paths(path_var)
        .filter(|dir| dir.is_absolute())
        .collect()
}

/// All directories `resolve_cli_path` searches, in order: the fixed `SEARCH_PATHS` first, then
/// each absolute directory from the `PATH` environment variable, deduped against both
/// `SEARCH_PATHS` and itself. A GUI-launched macOS app inherits a minimal PATH (or none), so
/// PATH is a supplement here rather than a replacement -- `SEARCH_PATHS` is tried first and
/// still covers the common case with no environment dependency.
fn search_dirs() -> Vec<PathBuf> {
    let mut seen = HashSet::new();
    let mut dirs = Vec::new();
    for dir in SEARCH_PATHS.iter().map(PathBuf::from) {
        if seen.insert(dir.clone()) {
            dirs.push(dir);
        }
    }
    if let Some(path_var) = std::env::var_os("PATH") {
        for dir in parse_path_env_dirs(&path_var) {
            if seen.insert(dir.clone()) {
                dirs.push(dir);
            }
        }
    }
    dirs
}

/// `PATH` handed to every child process: the same directories `resolve_cli_path` searched.
/// Resolving the binary alone is not enough -- tools like `colima` shell out to `limactl`, and
/// kubectl exec plugins / docker credential helpers are PATH lookups too. A GUI-launched app
/// inherits only `/usr/bin:/bin:/usr/sbin:/sbin`, so without this those nested lookups fail
/// even though the top-level binary was found. `None` (a directory containing `:`) leaves the
/// inherited PATH untouched rather than failing the command.
fn child_path_env() -> Option<std::ffi::OsString> {
    std::env::join_paths(search_dirs()).ok()
}

pub fn resolve_cli_path(bin: &str) -> Result<PathBuf, String> {
    let p = std::path::Path::new(bin);
    if p.is_absolute() {
        if p.is_file() {
            return Ok(p.to_path_buf());
        }
        return Err(format!("'{bin}' executable not found"));
    }
    if bin == "vmrun" {
        let mac_vmrun =
            std::path::Path::new("/Applications/VMware Fusion.app/Contents/Library/vmrun");
        if mac_vmrun.is_file() {
            return Ok(mac_vmrun.to_path_buf());
        }
    }
    for dir in search_dirs() {
        let candidate = dir.join(bin);
        if candidate.is_file() {
            return Ok(candidate);
        }
    }

    Err(format!("'{bin}' executable not found"))
}

#[derive(Debug, Clone)]
pub struct CommandOutput {
    pub stdout: String,
    pub stderr: String,
    pub success: bool,
}

#[derive(Debug, Clone)]
pub struct CommandLimits {
    pub timeout: std::time::Duration,
    pub max_output_bytes: usize,
}

impl Default for CommandLimits {
    fn default() -> Self {
        Self {
            timeout: std::time::Duration::from_secs(10),
            max_output_bytes: 2 * 1024 * 1024,
        }
    }
}

#[async_trait]
pub trait CommandRunner: Send + Sync {
    async fn run(&self, bin: &str, args: &[String]) -> Result<CommandOutput, String>;
    async fn run_with_env(
        &self,
        bin: &str,
        args: &[String],
        _env: &[(String, String)],
    ) -> Result<CommandOutput, String> {
        self.run(bin, args).await
    }
    async fn run_bounded(
        &self,
        bin: &str,
        args: &[String],
        _limits: CommandLimits,
    ) -> Result<CommandOutput, String> {
        self.run(bin, args).await
    }
    /// `run_bounded` plus extra environment variables (e.g. `VAGRANT_CWD`: the runner has no
    /// cwd support). Default delegates to `run_with_env`, so fakes need no change.
    async fn run_bounded_with_env(
        &self,
        bin: &str,
        args: &[String],
        env: &[(String, String)],
        _limits: CommandLimits,
    ) -> Result<CommandOutput, String> {
        self.run_with_env(bin, args, env).await
    }
}

pub struct SystemRunner;

#[async_trait]
impl CommandRunner for SystemRunner {
    async fn run(&self, bin: &str, args: &[String]) -> Result<CommandOutput, String> {
        let path = resolve_cli_path(bin)?;
        // kill_on_drop: without it, dropping this future (e.g. a tokio::time::timeout firing on
        // a hung password-auth prompt) leaves the child running instead of terminating it.
        let mut cmd = Command::new(path);
        cmd.args(args).kill_on_drop(true);
        if let Some(child_path) = child_path_env() {
            cmd.env("PATH", child_path);
        }
        let output = cmd
            .output()
            .await
            .map_err(|err| format!("{bin} execution failed: {err}"))?;
        Ok(CommandOutput {
            stdout: String::from_utf8_lossy(&output.stdout).trim().to_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).trim().to_owned(),
            success: output.status.success(),
        })
    }

    async fn run_with_env(
        &self,
        bin: &str,
        args: &[String],
        env: &[(String, String)],
    ) -> Result<CommandOutput, String> {
        let path = resolve_cli_path(bin)?;
        let mut cmd = Command::new(path);
        cmd.args(args);
        cmd.kill_on_drop(true);
        if let Some(child_path) = child_path_env() {
            cmd.env("PATH", child_path);
        }
        for (k, v) in env {
            cmd.env(k, v);
        }
        let output = cmd
            .output()
            .await
            .map_err(|err| format!("{bin} execution failed: {err}"))?;
        Ok(CommandOutput {
            stdout: String::from_utf8_lossy(&output.stdout).trim().to_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).trim().to_owned(),
            success: output.status.success(),
        })
    }

    async fn run_bounded(
        &self,
        bin: &str,
        args: &[String],
        limits: CommandLimits,
    ) -> Result<CommandOutput, String> {
        self.run_bounded_with_env(bin, args, &[], limits).await
    }

    async fn run_bounded_with_env(
        &self,
        bin: &str,
        args: &[String],
        env: &[(String, String)],
        limits: CommandLimits,
    ) -> Result<CommandOutput, String> {
        let path = resolve_cli_path(bin)?;
        let mut cmd = Command::new(path);
        cmd.args(args)
            .env("LC_ALL", "C")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true);

        if let Some(child_path) = child_path_env() {
            cmd.env("PATH", child_path);
        }
        for (k, v) in env {
            cmd.env(k, v);
        }

        let op = async {
            let mut child = cmd
                .spawn()
                .map_err(|err| format!("{bin} execution failed: {err}"))?;

            let stdout = child
                .stdout
                .take()
                .ok_or_else(|| "stdout unavailable".to_string())?;
            let stderr = child
                .stderr
                .take()
                .ok_or_else(|| "stderr unavailable".to_string())?;

            let max_read = (limits.max_output_bytes + 1) as u64;

            let out_fut = async {
                let mut data = Vec::new();
                use tokio::io::AsyncReadExt;
                stdout
                    .take(max_read)
                    .read_to_end(&mut data)
                    .await
                    .map_err(|e| e.to_string())?;
                Ok::<_, String>(data)
            };
            let err_fut = async {
                let mut data = Vec::new();
                use tokio::io::AsyncReadExt;
                stderr
                    .take(max_read)
                    .read_to_end(&mut data)
                    .await
                    .map_err(|e| e.to_string())?;
                Ok::<_, String>(data)
            };

            let (out_bytes, err_bytes) = tokio::try_join!(out_fut, err_fut)?;
            if out_bytes.len() > limits.max_output_bytes
                || err_bytes.len() > limits.max_output_bytes
            {
                return Err("command output limit exceeded".to_string());
            }

            let status = child
                .wait()
                .await
                .map_err(|err| format!("{bin} wait failed: {err}"))?;

            Ok(CommandOutput {
                stdout: String::from_utf8_lossy(&out_bytes).trim().to_owned(),
                stderr: String::from_utf8_lossy(&err_bytes).trim().to_owned(),
                success: status.success(),
            })
        };

        tokio::time::timeout(limits.timeout, op)
            .await
            .map_err(|_| "command timed out".to_string())?
    }
}

/// Runs macOS `open` with `args`: Ok on success, else Err(stderr) if non-empty, otherwise
/// Err(fallback_err). Shared by the Finder/browser/SSH-session "reveal externally" call sites.
pub async fn open_with_system(
    runner: &dyn CommandRunner,
    args: &[String],
    fallback_err: &str,
) -> Result<(), String> {
    let out = runner.run("open", args).await?;
    if out.success {
        Ok(())
    } else if out.stderr.is_empty() {
        Err(fallback_err.to_string())
    } else {
        Err(out.stderr)
    }
}

/// Opens a new Terminal.app window running `command_line`, via osascript's `do script`. Mirrors
/// `open_with_system`'s call shape (goes through `CommandRunner`, returns `Result<(), String>`
/// with the child's stderr surfaced on failure) but drives Terminal directly instead of the
/// `open <url>` scheme `open_ssh_session` uses, because there is no URL scheme for an arbitrary
/// shell command the way `ssh://` covers a plain SSH session.
///
/// `command_line` must already be built from validated tokens by the caller (see
/// `services/local_runtime_lifecycle.rs`, which validates instance/context names via
/// `services/validate.rs` before composing it) — this function only escapes the AppleScript
/// string layer (backslash and double-quote), it does not authorize the content.
pub async fn open_terminal_with_command(
    runner: &dyn CommandRunner,
    command_line: &str,
) -> Result<(), String> {
    let escaped = command_line.replace('\\', "\\\\").replace('"', "\\\"");
    let script =
        format!("tell application \"Terminal\"\n  activate\n  do script \"{escaped}\"\nend tell");
    let out = runner.run("osascript", &["-e".to_string(), script]).await?;
    if out.success {
        Ok(())
    } else if out.stderr.is_empty() {
        Err("failed to open terminal".to_string())
    } else {
        Err(out.stderr)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn system_runner_reports_failure_without_erroring() {
        let runner = SystemRunner;
        let result = runner.run("true", &[]).await;
        // `true` exists on macOS default PATH search dirs
        assert!(result.is_ok());
    }

    #[test]
    fn resolve_cli_path_errors_on_unknown_binary() {
        let err = resolve_cli_path("definitely-not-a-real-binary-xyz").unwrap_err();
        assert!(err.contains("not found"));
    }

    #[test]
    fn parse_path_env_dirs_keeps_only_absolute_entries() {
        let dirs = parse_path_env_dirs(std::ffi::OsStr::new(
            "/usr/bin:relative/path::/opt/tool/bin:",
        ));
        assert_eq!(
            dirs,
            vec![PathBuf::from("/usr/bin"), PathBuf::from("/opt/tool/bin")],
            "relative and empty PATH entries must be dropped, absolute ones kept in order"
        );
    }

    #[test]
    fn parse_path_env_dirs_returns_empty_for_all_relative_or_empty() {
        assert!(parse_path_env_dirs(std::ffi::OsStr::new("relative:another/path:")).is_empty());
        assert!(parse_path_env_dirs(std::ffi::OsStr::new("")).is_empty());
    }

    #[test]
    fn search_dirs_puts_fixed_paths_first_with_no_duplicates() {
        let dirs = search_dirs();
        let fixed: Vec<PathBuf> = SEARCH_PATHS.iter().map(PathBuf::from).collect();
        assert_eq!(
            &dirs[..fixed.len()],
            &fixed[..],
            "SEARCH_PATHS must be tried before any $PATH-supplied directory"
        );
        let unique: std::collections::HashSet<&PathBuf> = dirs.iter().collect();
        assert_eq!(
            unique.len(),
            dirs.len(),
            "search_dirs must not contain duplicate directories: {dirs:?}"
        );
    }

    #[tokio::test]
    async fn system_runner_gives_children_the_search_dirs_as_path() {
        let out = SystemRunner
            .run("sh", &["-c".to_string(), "printf %s \"$PATH\"".to_string()])
            .await
            .unwrap();
        let expected = std::env::join_paths(search_dirs()).unwrap();
        assert_eq!(out.stdout, expected.to_string_lossy());
    }

    struct FakeOsascriptRunner {
        success: bool,
        calls: std::sync::Mutex<Vec<(String, Vec<String>)>>,
    }

    #[async_trait]
    impl CommandRunner for FakeOsascriptRunner {
        async fn run(&self, bin: &str, args: &[String]) -> Result<CommandOutput, String> {
            self.calls
                .lock()
                .unwrap()
                .push((bin.to_string(), args.to_vec()));
            Ok(CommandOutput {
                stdout: String::new(),
                stderr: if self.success {
                    String::new()
                } else {
                    "boom".to_string()
                },
                success: self.success,
            })
        }
    }

    #[tokio::test]
    async fn open_terminal_with_command_builds_do_script_and_escapes_quotes() {
        let runner = FakeOsascriptRunner {
            success: true,
            calls: std::sync::Mutex::new(Vec::new()),
        };
        open_terminal_with_command(&runner, r#"echo "hi""#)
            .await
            .unwrap();

        let calls = runner.calls.lock().unwrap();
        assert_eq!(calls.len(), 1);
        let (bin, args) = &calls[0];
        assert_eq!(bin, "osascript");
        assert_eq!(args[0], "-e");
        assert!(args[1].contains("tell application \"Terminal\""));
        assert!(args[1].contains("do script \"echo \\\"hi\\\"\""));
    }

    #[tokio::test]
    async fn open_terminal_with_command_surfaces_stderr_on_failure() {
        let runner = FakeOsascriptRunner {
            success: false,
            calls: std::sync::Mutex::new(Vec::new()),
        };
        let err = open_terminal_with_command(&runner, "colima ssh --profile default")
            .await
            .unwrap_err();
        assert_eq!(err, "boom");
    }

    #[tokio::test]
    async fn system_runner_run_bounded_enforces_output_cap() {
        let runner = SystemRunner;
        let limits = CommandLimits {
            timeout: std::time::Duration::from_secs(5),
            max_output_bytes: 100,
        };
        let res = runner
            .run_bounded(
                "sh",
                &["-c".to_string(), "yes abcdefghij | head -n 50".to_string()],
                limits,
            )
            .await;
        assert!(res.is_err());
        assert_eq!(res.unwrap_err(), "command output limit exceeded");
    }

    #[tokio::test(start_paused = true)]
    async fn system_runner_run_bounded_times_out() {
        let runner = SystemRunner;
        let limits = CommandLimits {
            timeout: std::time::Duration::from_millis(50),
            max_output_bytes: 1024,
        };
        let res = runner
            .run_bounded("sleep", &["10".to_string()], limits)
            .await;
        assert!(res.is_err());
        assert_eq!(res.unwrap_err(), "command timed out");
    }

    #[tokio::test]
    async fn system_runner_run_bounded_terminates_hanging_child_on_timeout() {
        let runner = SystemRunner;
        let tmp_dir = std::env::temp_dir();
        let pid_file = tmp_dir.join(format!(
            "clusterdeck_test_child_pid_{}_{}.pid",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        let _ = std::fs::remove_file(&pid_file);

        let limits = CommandLimits {
            timeout: std::time::Duration::from_millis(100),
            max_output_bytes: 1024,
        };
        // sh replaces itself with sleep 60 via exec; the pid file records the pid.
        let script = format!("echo $$ > '{}' && exec sleep 60", pid_file.display());
        let res = runner
            .run_bounded("sh", &["-c".to_string(), script], limits)
            .await;
        assert!(res.is_err());
        assert_eq!(res.unwrap_err(), "command timed out");

        let pid_str = std::fs::read_to_string(&pid_file).expect("pid file should exist");
        let pid = pid_str.trim();
        assert!(!pid.is_empty(), "pid should not be empty");

        // Wait briefly for SIGKILL/cleanup to finish and confirm process is dead
        let mut alive = true;
        for _ in 0..20 {
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            let status = std::process::Command::new("kill")
                .args(["-0", pid])
                .stderr(std::process::Stdio::null())
                .status();
            match status {
                Ok(s) if !s.success() => {
                    alive = false;
                    break;
                }
                Err(_) => {
                    alive = false;
                    break;
                }
                _ => {}
            }
        }
        let _ = std::fs::remove_file(&pid_file);
        assert!(
            !alive,
            "child process with pid {pid} was not terminated on timeout"
        );
    }
}
