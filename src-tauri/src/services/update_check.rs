use serde::Deserialize;
use std::time::Duration;

use super::process::{CommandLimits, CommandRunner};
use super::validate::is_safe_open_url;

const LATEST_URL: &str = "https://api.github.com/repos/dasomel/clusterdeck/releases/latest";
const RELEASE_PREFIX: &str = "https://github.com/dasomel/clusterdeck/releases/tag/";

#[derive(Debug, PartialEq, Eq)]
pub enum UpdateResult {
    UpToDate { current: String },
    Available { version: String, url: String },
    Failed { reason: String },
}

fn numeric_version(value: &str) -> Option<[u64; 3]> {
    let mut parts = value.strip_prefix('v').unwrap_or(value).split('.');
    let mut version = [0; 3];
    for number in &mut version {
        let part = parts.next()?;
        if part.is_empty() || !part.bytes().all(|c| c.is_ascii_digit()) {
            return None;
        }
        *number = part.parse().ok()?;
    }
    parts.next().is_none().then_some(version)
}

fn is_newer(current: &str, latest: &str) -> bool {
    match (numeric_version(current), numeric_version(latest)) {
        (Some(current), Some(latest)) => latest > current,
        _ => false,
    }
}

#[derive(Deserialize)]
struct Release {
    tag_name: String,
    html_url: String,
    draft: bool,
    prerelease: bool,
}

pub fn parse_latest(current: &str, json: &str) -> UpdateResult {
    let release: Release = match serde_json::from_str(json) {
        Ok(release) => release,
        Err(_) => return failed("GitHub returned an invalid release response."),
    };
    if !is_release_url(&release.html_url)
        || release.html_url != format!("{RELEASE_PREFIX}{}", release.tag_name)
    {
        return failed("GitHub returned an invalid release URL.");
    }
    // D1: stable numeric releases only; costs pre-release awareness, replace with a semver
    // parser if release policy expands (ADR-0013). Never offer a malformed version.
    if !release.draft && !release.prerelease && is_newer(current, &release.tag_name) {
        UpdateResult::Available {
            version: release.tag_name.trim_start_matches('v').to_string(),
            url: release.html_url,
        }
    } else {
        UpdateResult::UpToDate {
            current: current.to_string(),
        }
    }
}

fn is_release_url(url: &str) -> bool {
    // D6: allow only numeric release tag pages; costs excluding other GitHub URLs.
    // Expand this check only when the release policy changes.
    url.strip_prefix(RELEASE_PREFIX)
        .and_then(|tag| tag.strip_prefix('v'))
        .is_some_and(|version| !version.starts_with('v') && numeric_version(version).is_some())
}

fn failed(reason: &str) -> UpdateResult {
    UpdateResult::Failed {
        reason: reason.to_string(),
    }
}

pub async fn check(runner: &dyn CommandRunner) -> UpdateResult {
    let current = env!("CARGO_PKG_VERSION");
    // D2: use the existing runner and system curl, avoiding an HTTP dependency. Costs one
    // bounded unauthenticated GET per launch; remove the startup task to make it manual-only.
    let args = [
        "-fsSL".to_string(),
        "--max-time".to_string(),
        "8".to_string(),
        "-H".to_string(),
        "Accept: application/vnd.github+json".to_string(),
        "-H".to_string(),
        format!("User-Agent: ClusterDeck/{current}"),
        LATEST_URL.to_string(),
    ];
    match runner
        .run_bounded(
            "/usr/bin/curl",
            &args,
            CommandLimits {
                timeout: Duration::from_secs(10),
                max_output_bytes: 256 * 1024,
            },
        )
        .await
    {
        Ok(output) if output.success => parse_latest(current, &output.stdout),
        // Do not reflect arbitrary server/proxy output into a native dialog.
        _ => failed("Could not reach GitHub. Check your connection and try again."),
    }
}

pub async fn open_release(runner: &dyn CommandRunner, url: &str) -> Result<(), String> {
    if !is_release_url(url) || !is_safe_open_url(url) {
        return Err("Invalid release URL.".to_string());
    }
    crate::services::process::open_with_system(
        runner,
        &[url.to_string()],
        "Could not open the release page.",
    )
    .await
}

pub async fn show_alert(
    runner: &dyn CommandRunner,
    title: &str,
    message: &str,
) -> Result<(), String> {
    // D3: argv keeps remote text out of AppleScript source; costs a small fixed script.
    // A native dialog plugin can replace this if more dialog interactions are needed.
    let script = "on run argv\n display alert (item 1 of argv) message (item 2 of argv) giving up after 60\nend run";
    let output = runner
        .run_bounded(
            "/usr/bin/osascript",
            &[
                "-e".into(),
                script.into(),
                "--".into(),
                title.into(),
                message.into(),
            ],
            CommandLimits {
                timeout: Duration::from_secs(65),
                max_output_bytes: 4096,
            },
        )
        .await?;
    if output.success {
        Ok(())
    } else {
        Err("Could not display update alert.".to_string())
    }
}

#[cfg(test)]
#[path = "update_check_tests.rs"]
mod tests;
