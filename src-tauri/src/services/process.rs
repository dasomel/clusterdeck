use std::path::PathBuf;
use tokio::process::Command;

const SEARCH_PATHS: [&str; 4] = [
    "/opt/homebrew/bin",
    "/usr/local/bin",
    "/usr/bin",
    "/bin",
];

pub fn resolve_cli_path(bin: &str) -> Result<PathBuf, String> {
    for dir in SEARCH_PATHS {
        let candidate = PathBuf::from(dir).join(bin);
        if candidate.is_file() {
            return Ok(candidate);
        }
    }

    Err(format!("'{bin}' executable not found"))
}

pub async fn run_cli(bin: &str, args: &[&str]) -> Result<String, String> {
    let path = resolve_cli_path(bin)?;
    let output = Command::new(path)
        .args(args)
        .output()
        .await
        .map_err(|err| format!("{bin} execution failed: {err}"))?;

    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
    } else {
        Err(String::from_utf8_lossy(&output.stderr).trim().to_owned())
    }
}
