#![allow(dead_code)]

use async_trait::async_trait;
use std::path::PathBuf;
use tokio::process::Command;

const SEARCH_PATHS: [&str; 4] = ["/opt/homebrew/bin", "/usr/local/bin", "/usr/bin", "/bin"];

pub fn resolve_cli_path(bin: &str) -> Result<PathBuf, String> {
    for dir in SEARCH_PATHS {
        let candidate = PathBuf::from(dir).join(bin);
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
}

pub struct SystemRunner;

#[async_trait]
impl CommandRunner for SystemRunner {
    async fn run(&self, bin: &str, args: &[String]) -> Result<CommandOutput, String> {
        let path = resolve_cli_path(bin)?;
        // kill_on_drop: without it, dropping this future (e.g. a tokio::time::timeout firing on
        // a hung password-auth prompt) leaves the child running instead of terminating it.
        let output = Command::new(path)
            .args(args)
            .kill_on_drop(true)
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
}
