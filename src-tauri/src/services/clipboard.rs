use crate::services::process::CommandRunner;

// D4 (amended): WKWebView denies `navigator.clipboard.readText()`, so the webview cannot check
// "is the clipboard still what I copied". NSPasteboard's changeCount answers that without the
// secret ever leaving the webview. Cost: one `osascript` spawn per call; escape: drop the
// commands and let the UI say the clipboard is not auto-cleared.

const COUNT_SCRIPT: [&str; 2] = [
    "use framework \"AppKit\"",
    "return (current application's NSPasteboard's generalPasteboard()'s changeCount()) as integer",
];

fn osascript_args(lines: &[String]) -> Vec<String> {
    lines
        .iter()
        .flat_map(|line| ["-e".to_string(), line.clone()])
        .collect()
}

pub async fn change_count(runner: &dyn CommandRunner) -> Result<i64, String> {
    let lines = COUNT_SCRIPT.map(str::to_string);
    let out = runner.run("osascript", &osascript_args(&lines)).await?;
    if !out.success {
        return Err("clipboard_unavailable".to_string());
    }
    out.stdout
        .trim()
        .parse::<i64>()
        .map_err(|_| "clipboard_unavailable".to_string())
}

/// Clears the general pasteboard only while its change count still equals `expected`.
/// `expected` is the only variable part of the script and is formatted from an `i64`.
pub async fn clear_if_change_count(
    runner: &dyn CommandRunner,
    expected: i64,
) -> Result<bool, String> {
    if expected < 0 {
        return Err("clipboard_unavailable".to_string());
    }
    let lines = [
        "use framework \"AppKit\"".to_string(),
        "set pb to current application's NSPasteboard's generalPasteboard()".to_string(),
        format!("if (pb's changeCount() as integer) is {expected} then"),
        "pb's clearContents()".to_string(),
        "return \"true\"".to_string(),
        "end if".to_string(),
        "return \"false\"".to_string(),
    ];
    let out = runner.run("osascript", &osascript_args(&lines)).await?;
    if !out.success {
        return Err("clipboard_unavailable".to_string());
    }
    match out.stdout.trim() {
        "true" => Ok(true),
        "false" => Ok(false),
        _ => Err("clipboard_unavailable".to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::process::CommandOutput;
    use std::sync::Mutex;

    struct FakeRunner {
        calls: Mutex<Vec<(String, Vec<String>)>>,
        reply: Result<CommandOutput, String>,
    }

    impl FakeRunner {
        fn replying(stdout: &str, success: bool) -> Self {
            Self {
                calls: Mutex::new(Vec::new()),
                reply: Ok(CommandOutput {
                    stdout: stdout.to_string(),
                    stderr: String::new(),
                    success,
                }),
            }
        }
    }

    #[async_trait::async_trait]
    impl CommandRunner for FakeRunner {
        async fn run(&self, bin: &str, args: &[String]) -> Result<CommandOutput, String> {
            self.calls
                .lock()
                .unwrap()
                .push((bin.to_string(), args.to_vec()));
            self.reply.clone()
        }
    }

    #[tokio::test]
    async fn change_count_parses_the_integer_and_uses_a_fixed_script() {
        let runner = FakeRunner::replying("42\n", true);
        assert_eq!(change_count(&runner).await, Ok(42));
        let calls = runner.calls.lock().unwrap();
        assert_eq!(calls[0].0, "osascript");
        assert!(calls[0].1.iter().any(|a| a.contains("changeCount")));
    }

    #[tokio::test]
    async fn change_count_rejects_non_integer_output_and_failures() {
        for (stdout, success) in [("oops", true), ("", true), ("7", false)] {
            let runner = FakeRunner::replying(stdout, success);
            assert!(change_count(&runner).await.is_err());
        }
    }

    #[tokio::test]
    async fn clear_if_unchanged_embeds_only_the_validated_integer() {
        let runner = FakeRunner::replying("true", true);
        assert_eq!(clear_if_change_count(&runner, 1234).await, Ok(true));
        let calls = runner.calls.lock().unwrap();
        let script = calls[0].1.join("\n");
        assert!(script.contains("is 1234 then"));
        assert!(script.contains("clearContents"));
        let fixed: String = clear_script_without_number();
        assert_eq!(script.replace("-e\n", "").replace("1234", "N"), fixed);
    }

    fn clear_script_without_number() -> String {
        [
            "use framework \"AppKit\"",
            "set pb to current application's NSPasteboard's generalPasteboard()",
            "if (pb's changeCount() as integer) is N then",
            "pb's clearContents()",
            "return \"true\"",
            "end if",
            "return \"false\"",
        ]
        .join("\n")
    }

    #[tokio::test]
    async fn clear_if_unchanged_reports_false_and_rejects_bad_input() {
        let runner = FakeRunner::replying("false\n", true);
        assert_eq!(clear_if_change_count(&runner, 5).await, Ok(false));
        let garbage = FakeRunner::replying("maybe", true);
        assert!(clear_if_change_count(&garbage, 5).await.is_err());
        let untouched = FakeRunner::replying("true", true);
        assert!(clear_if_change_count(&untouched, -1).await.is_err());
        assert!(untouched.calls.lock().unwrap().is_empty());
    }
}
