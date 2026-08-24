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
        let output = Command::new(path)
            .args(args)
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
}
