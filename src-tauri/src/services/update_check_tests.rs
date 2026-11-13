use super::*;
use crate::services::process::CommandOutput;
use async_trait::async_trait;
use std::sync::Mutex;

struct FakeRunner {
    output: Result<CommandOutput, String>,
    open_calls: Mutex<Vec<(String, Vec<String>)>>,
    calls: Mutex<Vec<(String, Vec<String>, CommandLimits)>>,
}

impl FakeRunner {
    fn new(output: Result<CommandOutput, String>) -> Self {
        Self {
            output,
            open_calls: Mutex::new(Vec::new()),
            calls: Mutex::new(Vec::new()),
        }
    }
}

#[async_trait]
impl CommandRunner for FakeRunner {
    async fn run(&self, bin: &str, args: &[String]) -> Result<CommandOutput, String> {
        assert_eq!(bin, "open", "only the existing browser helper uses run");
        self.open_calls
            .lock()
            .unwrap()
            .push((bin.into(), args.to_vec()));
        self.output.clone()
    }

    async fn run_bounded(
        &self,
        bin: &str,
        args: &[String],
        limits: CommandLimits,
    ) -> Result<CommandOutput, String> {
        self.calls
            .lock()
            .unwrap()
            .push((bin.into(), args.to_vec(), limits));
        self.output.clone()
    }
}

fn output(stdout: &str, success: bool) -> Result<CommandOutput, String> {
    Ok(CommandOutput {
        stdout: stdout.into(),
        stderr: "remote error".into(),
        success,
    })
}

fn release(tag: &str) -> serde_json::Value {
    serde_json::json!({
        "tag_name": tag,
        "html_url": format!("https://github.com/dasomel/clusterdeck/releases/tag/{tag}"),
        "draft": false,
        "prerelease": false
    })
}

#[test]
fn version_comparator_accepts_only_numeric_triples() {
    for (latest, expected) in [
        ("v0.7.0", true),
        ("v0.6.1", false),
        ("v0.6.0", false),
        ("v1.0.0", true),
        ("garbage", false),
        ("0.10.0", true),
        ("v0.7", false),
        ("v0.7.0.1", false),
        ("v0.7.0-beta", false),
        ("v0.7.+1", false),
        ("v0.7.18446744073709551616", false),
    ] {
        assert_eq!(is_newer("0.6.1", latest), expected, "{latest}");
    }
    assert!(!is_newer("garbage", "v1.0.0"));
}

#[test]
fn parse_latest_json_normal_draft_prerelease_and_malformed_tags() {
    let normal = release("v0.7.0");
    assert_eq!(
        parse_latest("0.6.1", &normal.to_string()),
        UpdateResult::Available {
            version: "0.7.0".into(),
            url: normal["html_url"].as_str().unwrap().into(),
        }
    );
    for field in ["draft", "prerelease"] {
        let mut ignored = normal.clone();
        ignored[field] = true.into();
        assert_eq!(
            parse_latest("0.6.1", &ignored.to_string()),
            UpdateResult::UpToDate {
                current: "0.6.1".into()
            }
        );
    }
    for tag in ["v0.6.1", "v0.6.0"] {
        assert_eq!(
            parse_latest("0.6.1", &release(tag).to_string()),
            UpdateResult::UpToDate {
                current: "0.6.1".into()
            }
        );
    }
}

#[test]
fn parse_latest_json_rejects_bad_urls_and_missing_fields() {
    for url in [
        "https://evil.example/releases/v0.7.0",
        "https://github.com.evil.example/dasomel/clusterdeck/releases/v0.7.0",
        "https://github.com/dasomel/other/releases/v0.7.0",
        "https://github.com/dasomel/clusterdeck/releases/a b",
        "https://github.com/dasomel/clusterdeck/releases/a\nb",
    ] {
        let mut invalid = release("v0.7.0");
        invalid["html_url"] = url.into();
        assert!(matches!(
            parse_latest("0.6.1", &invalid.to_string()),
            UpdateResult::Failed { .. }
        ));
    }
    for tag in ["garbage", "0.7.0", "v0.7", "v0.7.0-beta"] {
        assert!(matches!(
            parse_latest("0.6.1", &release(tag).to_string()),
            UpdateResult::Failed { .. }
        ));
    }
    for field in ["tag_name", "html_url", "draft", "prerelease"] {
        let mut invalid = release("v0.7.0");
        invalid.as_object_mut().unwrap().remove(field);
        assert!(matches!(
            parse_latest("0.6.1", &invalid.to_string()),
            UpdateResult::Failed { .. }
        ));
    }
    assert!(matches!(
        parse_latest("0.6.1", "not json"),
        UpdateResult::Failed { .. }
    ));
}

#[test]
fn parse_latest_rejects_query_fragment_and_different_tag_path() {
    for url in [
        "https://github.com/dasomel/clusterdeck/releases/tag/v0.7.0?download=1",
        "https://github.com/dasomel/clusterdeck/releases/tag/v0.7.0#assets",
        "https://github.com/dasomel/clusterdeck/releases/tag/v0.8.0",
    ] {
        let mut invalid = release("v0.7.0");
        invalid["html_url"] = url.into();
        assert!(
            matches!(
                parse_latest("0.6.1", &invalid.to_string()),
                UpdateResult::Failed { .. }
            ),
            "{url}"
        );
    }
}

#[tokio::test]
async fn curl_uses_exact_url_headers_timeout_and_output_limit() {
    let runner = FakeRunner::new(output(&release("v999.0.0").to_string(), true));
    assert!(matches!(
        check(&runner).await,
        UpdateResult::Available { .. }
    ));
    let calls = runner.calls.lock().unwrap();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].0, "/usr/bin/curl");
    assert_eq!(
        calls[0].1,
        [
            "-fsSL",
            "--max-time",
            "8",
            "-H",
            "Accept: application/vnd.github+json",
            "-H",
            &format!("User-Agent: ClusterDeck/{}", env!("CARGO_PKG_VERSION")),
            "https://api.github.com/repos/dasomel/clusterdeck/releases/latest",
        ]
    );
    assert_eq!(calls[0].2.timeout, Duration::from_secs(10));
    assert_eq!(calls[0].2.max_output_bytes, 256 * 1024);
}

#[tokio::test]
async fn curl_failure_returns_short_failed_reason() {
    for response in [Err("timeout".into()), output("", false)] {
        let runner = FakeRunner::new(response);
        assert_eq!(
            check(&runner).await,
            failed("Could not reach GitHub. Check your connection and try again.")
        );
    }
}

#[tokio::test]
async fn osascript_passes_alert_values_as_argv() {
    let runner = FakeRunner::new(output("", true));
    let title = "Couldn't check for updates";
    let message = "\" & do shell script \"touch /tmp/unwanted\"\n雪";
    show_alert(&runner, title, message).await.unwrap();
    let calls = runner.calls.lock().unwrap();
    let (bin, args, limits) = &calls[0];
    assert_eq!(bin, "/usr/bin/osascript");
    assert_eq!(args.len(), 5);
    assert_eq!(args[0], "-e");
    assert!(args[1].starts_with("on run argv\n"));
    assert!(args[1].contains("display alert (item 1 of argv) message (item 2 of argv)"));
    assert!(!args[1].contains(title));
    assert!(!args[1].contains(message));
    assert_eq!(&args[2..], &["--", title, message]);
    assert_eq!(limits.timeout, Duration::from_secs(65));
    assert_eq!(limits.max_output_bytes, 4096);
}

#[tokio::test]
async fn alert_failure_is_reported() {
    let runner = FakeRunner::new(output("", false));
    assert!(show_alert(&runner, "title", "message").await.is_err());
}

#[tokio::test]
async fn open_release_revalidates_url_and_passes_single_arg_to_open() {
    let runner = FakeRunner::new(output("", true));
    for url in [
        "https://evil.example/",
        "https://github.com/dasomel/clusterdeck/releases/latest",
        "https://github.com/dasomel/clusterdeck/releases/tag/0.7.0",
        "https://github.com/dasomel/clusterdeck/releases/tag/v0.7.0?download=1",
        "https://github.com/dasomel/clusterdeck/releases/tag/v0.7.0#assets",
        "https://github.com/dasomel/clusterdeck/releases/tag/v0.7.0\n",
    ] {
        assert!(open_release(&runner, url).await.is_err(), "{url}");
    }
    assert!(runner.open_calls.lock().unwrap().is_empty());
    assert!(runner.calls.lock().unwrap().is_empty());
    let url = "https://github.com/dasomel/clusterdeck/releases/tag/v0.7.0";
    open_release(&runner, url).await.unwrap();
    let calls = runner.open_calls.lock().unwrap();
    assert_eq!(*calls, vec![("open".into(), vec![url.into()])]);
    assert!(runner.calls.lock().unwrap().is_empty());
}
