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
    // The kubeconfig-credential curl fallback exists because macOS Local Network Privacy exempts
    // Apple's curl; a Homebrew curl earlier in SEARCH_PATHS would be blocked like kubectl is.
    if bin == "curl" {
        let system_curl = std::path::Path::new("/usr/bin/curl");
        if system_curl.is_file() {
            return Ok(system_curl.to_path_buf());
        }
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
    /// Starts a long-lived child (stdin/stdout null, stderr captured). Default is unsupported so
    /// existing fakes compile untouched; only `SystemRunner` and tunnel-aware fakes override it.
    async fn spawn_background(
        &self,
        _bin: &str,
        _args: &[String],
    ) -> Result<Box<dyn BackgroundProcess>, String> {
        Err("background processes not supported".to_string())
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

/// A long-lived child process (e.g. an `ssh -N -L` tunnel) owned by the app, as opposed to the
/// run-to-completion commands `run*` cover. Dropping it must terminate the child.
#[async_trait]
pub trait BackgroundProcess: Send {
    /// `Some(code)` once the process has exited (`-1` when killed by a signal), `None` while
    /// it is still running.
    fn try_exit_status(&mut self) -> Option<i32>;
    /// OS process id, when known.
    fn pid(&self) -> Option<u32> {
        None
    }
    async fn kill(&mut self);
    /// The last `BACKGROUND_STDERR_TAIL_BYTES` of stderr, for error messages. Only meaningful
    /// after the process has exited (the tail is collected when stderr reaches EOF).
    async fn stderr_tail(&mut self) -> String;
}

const BACKGROUND_STDERR_TAIL_BYTES: usize = 4096;

pub struct SystemBackgroundProcess {
    child: tokio::process::Child,
    stderr_task: Option<tokio::task::JoinHandle<Vec<u8>>>,
}

#[async_trait]
impl BackgroundProcess for SystemBackgroundProcess {
    fn try_exit_status(&mut self) -> Option<i32> {
        match self.child.try_wait() {
            Ok(Some(status)) => Some(status.code().unwrap_or(-1)),
            Ok(None) => None,
            // A wait error means the child can no longer be observed; treat it as gone.
            Err(_) => Some(-1),
        }
    }

    fn pid(&self) -> Option<u32> {
        self.child.id()
    }

    async fn kill(&mut self) {
        let _ = self.child.kill().await;
        if let Some(task) = self.stderr_task.take() {
            task.abort();
        }
    }

    async fn stderr_tail(&mut self) -> String {
        let Some(task) = self.stderr_task.take() else {
            return String::new();
        };
        // Bounded wait: a grandchild that inherited stderr could keep the pipe open after the
        // child exited, and an error message is not worth hanging on.
        match tokio::time::timeout(std::time::Duration::from_millis(500), task).await {
            Ok(Ok(bytes)) => String::from_utf8_lossy(&bytes).trim().to_owned(),
            _ => String::new(),
        }
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

    async fn spawn_background(
        &self,
        bin: &str,
        args: &[String],
    ) -> Result<Box<dyn BackgroundProcess>, String> {
        let path = resolve_cli_path(bin)?;
        let mut cmd = Command::new(path);
        cmd.args(args)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true);
        if let Some(child_path) = child_path_env() {
            cmd.env("PATH", child_path);
        }
        let mut child = cmd
            .spawn()
            .map_err(|err| format!("{bin} execution failed: {err}"))?;
        let mut stderr = child
            .stderr
            .take()
            .ok_or_else(|| "stderr unavailable".to_string())?;
        // Drain continuously so a chatty child can never block on a full pipe, keeping only
        // the tail.
        let stderr_task = tokio::spawn(async move {
            use tokio::io::AsyncReadExt;
            let mut tail: Vec<u8> = Vec::new();
            let mut buf = [0u8; 1024];
            while let Ok(n) = stderr.read(&mut buf).await {
                if n == 0 {
                    break;
                }
                tail.extend_from_slice(&buf[..n]);
                if tail.len() > BACKGROUND_STDERR_TAIL_BYTES {
                    let excess = tail.len() - BACKGROUND_STDERR_TAIL_BYTES;
                    tail.drain(..excess);
                }
            }
            tail
        });
        Ok(Box::new(SystemBackgroundProcess {
            child,
            stderr_task: Some(stderr_task),
        }))
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
        // A caller that pins any `VAGRANT_*` variable gets a fully controlled Vagrant environment:
        // inherited ones (VAGRANT_DOTFILE_PATH, VAGRANT_HOME, ...) could redirect which state dir
        // or machine index the child uses (ADR-0012 D17).
        if env.iter().any(|(k, _)| k.starts_with("VAGRANT_")) {
            for (k, _) in std::env::vars_os() {
                if k.to_string_lossy().starts_with("VAGRANT_") {
                    cmd.env_remove(k);
                }
            }
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

fn applescript_escape(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

/// POSIX single-quote a token for the shell inside an AppleScript terminal.
fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// What to run in a new terminal window. A domain enum instead of a joined string so each
/// launch recipe decides how tokens are quoted (D8).
pub enum TerminalCommand {
    /// A program and its arguments; never re-split by a shell on argv terminals.
    Argv(Vec<String>),
    /// Several shell statements (the VM context shell). AppleScript terminals run it in their
    /// login shell; argv terminals get `/bin/sh -c "<script>; exec $SHELL -l"`.
    Script(String),
}

async fn run_osascript(runner: &dyn CommandRunner, script: String) -> Result<(), String> {
    let out = runner.run("osascript", &["-e".to_string(), script]).await?;
    if out.success {
        Ok(())
    } else if out.stderr.is_empty() {
        Err("failed to open terminal".to_string())
    } else {
        Err(out.stderr)
    }
}

/// Opens a new window of the chosen terminal (a bundle id from `app_settings`' catalog; `None` =
/// Terminal.app) running `command`. Goes through `CommandRunner` and surfaces the child's stderr
/// on failure. There is no URL scheme for an arbitrary shell command, so each terminal has its
/// own launch recipe:
/// - Terminal / iTerm2: AppleScript; each Argv token is shell-quoted, then AppleScript-escaped.
///   iTerm2 types the line into the new window's login shell (`write text`) because
///   `create window ... command` execs without a shell.
/// - the rest: `open -n -b <id> --args <flags> <argv...>`, argv passed as separate arguments
///   (`-b` rather than `-a <name>` so the bundle id from settings picks the exact app).
///   LaunchServices does not pass our child `PATH`, so argv[0] is resolved to an absolute path.
///
/// Tokens must already be validated by the caller (see `services/local_runtime_lifecycle.rs`,
/// `commands/connection.rs`); this function additionally rejects control characters and
/// escapes the quoting layers, it does not authorize the content. An unknown `terminal` is
/// rejected rather than passed to `open -b`.
pub async fn open_terminal(
    runner: &dyn CommandRunner,
    terminal: Option<&str>,
    command: &TerminalCommand,
) -> Result<(), String> {
    open_terminal_with(runner, terminal, command, &resolve_cli_path).await
}

pub async fn open_terminal_with(
    runner: &dyn CommandRunner,
    terminal: Option<&str>,
    command: &TerminalCommand,
    resolve: &(dyn Fn(&str) -> Result<PathBuf, String> + Sync),
) -> Result<(), String> {
    let has_control = |s: &String| s.chars().any(|c| c.is_control());
    let bad = match command {
        TerminalCommand::Argv(argv) => argv.is_empty() || argv.iter().any(has_control),
        TerminalCommand::Script(script) => has_control(script),
    };
    if bad {
        return Err("invalid terminal command: empty or contains control characters".to_string());
    }
    let applescript_line = || match command {
        TerminalCommand::Argv(argv) => argv
            .iter()
            .map(|t| shell_quote(t))
            .collect::<Vec<_>>()
            .join(" "),
        TerminalCommand::Script(script) => script.clone(),
    };
    let prefix: &[&str] = match terminal {
        None | Some("com.apple.Terminal") => {
            let escaped = applescript_escape(&applescript_line());
            return run_osascript(
                runner,
                format!(
                    "tell application \"Terminal\"\n  activate\n  do script \"{escaped}\"\nend tell"
                ),
            )
            .await;
        }
        Some("com.googlecode.iterm2") => {
            let escaped = applescript_escape(&applescript_line());
            return run_osascript(
                runner,
                format!(
                    "tell application \"iTerm\"\n  activate\n  set w to (create window with default profile)\n  tell current session of w to write text \"{escaped}\"\nend tell"
                ),
            )
            .await;
        }
        Some("com.mitchellh.ghostty") | Some("org.alacritty") => &["-e"],
        Some("com.github.wez.wezterm") => &["start", "--"],
        Some("net.kovidgoyal.kitty") => &[],
        Some("org.tabby") => &["run", "--"],
        Some(other) => return Err(format!("unsupported terminal: {other}")),
    };
    let argv: Vec<String> = match command {
        TerminalCommand::Argv(argv) => argv.clone(),
        TerminalCommand::Script(script) => vec![
            "/bin/sh".to_string(),
            "-c".to_string(),
            format!("{script}; exec \"$SHELL\" -l"),
        ],
    };
    let exe = resolve(&argv[0])?;
    let id = terminal.unwrap_or_default();
    let mut args: Vec<String> = ["-n", "-b", id, "--args"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    args.extend(prefix.iter().map(|s| s.to_string()));
    args.push(exe.to_string_lossy().into_owned());
    args.extend(argv.into_iter().skip(1));
    open_with_system(runner, &args, "failed to open terminal").await
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

    fn argv(parts: &[&str]) -> Vec<String> {
        parts.iter().map(|s| s.to_string()).collect()
    }

    fn fake(success: bool) -> FakeOsascriptRunner {
        FakeOsascriptRunner {
            success,
            calls: std::sync::Mutex::new(Vec::new()),
        }
    }

    fn cmd(parts: &[&str]) -> TerminalCommand {
        TerminalCommand::Argv(argv(parts))
    }

    /// Deterministic stand-in for `resolve_cli_path` (no real filesystem).
    fn fake_resolve(bin: &str) -> Result<PathBuf, String> {
        if bin.starts_with('/') {
            return Ok(PathBuf::from(bin));
        }
        Ok(PathBuf::from(format!("/abs/bin/{bin}")))
    }

    async fn launch(
        runner: &FakeOsascriptRunner,
        terminal: Option<&str>,
        command: &TerminalCommand,
    ) -> Result<(), String> {
        open_terminal_with(runner, terminal, command, &fake_resolve).await
    }

    #[tokio::test]
    async fn open_terminal_default_quotes_tokens_then_escapes_applescript() {
        for terminal in [None, Some("com.apple.Terminal")] {
            let runner = fake(true);
            launch(&runner, terminal, &cmd(&["echo", "\"hi\"", "it's"]))
                .await
                .unwrap();
            let calls = runner.calls.lock().unwrap();
            assert_eq!(calls.len(), 1);
            let (bin, args) = &calls[0];
            assert_eq!(bin, "osascript");
            assert_eq!(args[0], "-e");
            assert!(args[1].contains("tell application \"Terminal\""));
            // shell: 'echo' '"hi"' 'it'\''s' ; then AppleScript escapes `\` and `"`.
            assert!(
                args[1].contains(r#"do script "'echo' '\"hi\"' 'it'\\''s'""#),
                "{}",
                args[1]
            );
        }
    }

    #[tokio::test]
    async fn open_terminal_iterm_writes_text_into_login_shell() {
        let runner = fake(true);
        launch(
            &runner,
            Some("com.googlecode.iterm2"),
            &cmd(&["ssh", "a-b"]),
        )
        .await
        .unwrap();
        let calls = runner.calls.lock().unwrap();
        let script = &calls[0].1[1];
        assert_eq!(calls[0].0, "osascript");
        assert!(script.contains("tell application \"iTerm\""));
        assert!(script.contains("set w to (create window with default profile)"));
        assert!(script.contains("tell current session of w to write text \"'ssh' 'a-b'\""));
        assert!(!script.contains("default profile command"));
    }

    #[tokio::test]
    async fn open_terminal_script_is_passed_unquoted_to_applescript_terminals() {
        let runner = fake(true);
        let script = TerminalCommand::Script("export A=b; alias k='kubectl'".into());
        launch(&runner, None, &script).await.unwrap();
        assert!(runner.calls.lock().unwrap()[0].1[1]
            .contains(r#"do script "export A=b; alias k='kubectl'""#));
    }

    #[tokio::test]
    async fn open_terminal_argv_recipes_use_absolute_executable() {
        let cases: [(&str, &[&str]); 5] = [
            ("com.mitchellh.ghostty", &["-e"]),
            ("com.github.wez.wezterm", &["start", "--"]),
            ("net.kovidgoyal.kitty", &[]),
            ("org.alacritty", &["-e"]),
            ("org.tabby", &["run", "--"]),
        ];
        for (id, flags) in cases {
            let runner = fake(true);
            launch(
                &runner,
                Some(id),
                &cmd(&["colima", "ssh", "--profile", "d"]),
            )
            .await
            .unwrap();
            let calls = runner.calls.lock().unwrap();
            assert_eq!(calls[0].0, "open");
            let mut expected = argv(&["-n", "-b", id, "--args"]);
            expected.extend(flags.iter().map(|s| s.to_string()));
            expected.extend(argv(&["/abs/bin/colima", "ssh", "--profile", "d"]));
            assert_eq!(calls[0].1, expected, "{id}");
        }
    }

    #[tokio::test]
    async fn open_terminal_script_on_argv_terminal_is_wrapped_in_sh() {
        let runner = fake(true);
        launch(
            &runner,
            Some("org.alacritty"),
            &TerminalCommand::Script("export A=b".into()),
        )
        .await
        .unwrap();
        let calls = runner.calls.lock().unwrap();
        assert_eq!(
            calls[0].1,
            argv(&[
                "-n",
                "-b",
                "org.alacritty",
                "--args",
                "-e",
                "/bin/sh",
                "-c",
                "export A=b; exec \"$SHELL\" -l"
            ])
        );
    }

    #[tokio::test]
    async fn open_terminal_rejects_control_chars_without_running_anything() {
        for terminal in [None, Some("com.googlecode.iterm2"), Some("org.alacritty")] {
            for bad in ["a\nb", "a\rb", "a\tb", "a\0b", "a\x1bb"] {
                let runner = fake(true);
                assert!(launch(&runner, terminal, &cmd(&["ssh", bad]))
                    .await
                    .is_err());
                assert!(
                    launch(&runner, terminal, &TerminalCommand::Script(bad.into()))
                        .await
                        .is_err()
                );
                assert!(runner.calls.lock().unwrap().is_empty());
            }
        }
        let runner = fake(true);
        assert!(launch(&runner, None, &TerminalCommand::Argv(vec![]))
            .await
            .is_err());
    }

    #[tokio::test]
    async fn open_terminal_rejects_unknown_terminal_without_running_anything() {
        let runner = fake(true);
        let err = launch(&runner, Some("-a/evil"), &cmd(&["ssh", "x"]))
            .await
            .unwrap_err();
        assert!(err.contains("unsupported terminal"));
        assert!(runner.calls.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn open_terminal_unresolvable_executable_errors_before_open() {
        let runner = fake(true);
        let err = open_terminal_with(
            &runner,
            Some("org.alacritty"),
            &cmd(&["colima", "ssh"]),
            &|b| Err(format!("'{b}' executable not found")),
        )
        .await
        .unwrap_err();
        assert!(err.contains("not found"));
        assert!(runner.calls.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn open_terminal_surfaces_stderr_on_failure() {
        let runner = fake(false);
        let err = launch(
            &runner,
            None,
            &cmd(&["colima", "ssh", "--profile", "default"]),
        )
        .await
        .unwrap_err();
        assert_eq!(err, "boom");
    }

    #[tokio::test]
    async fn system_runner_spawn_background_reports_exit_and_stderr_tail() {
        let mut proc = SystemRunner
            .spawn_background(
                "sh",
                &["-c".to_string(), "echo boom >&2; exit 3".to_string()],
            )
            .await
            .unwrap();
        let mut code = None;
        for _ in 0..100 {
            code = proc.try_exit_status();
            if code.is_some() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        assert_eq!(code, Some(3));
        assert_eq!(proc.stderr_tail().await, "boom");
    }

    #[tokio::test]
    async fn system_runner_spawn_background_kill_terminates_child() {
        let mut proc = SystemRunner
            .spawn_background("sleep", &["30".to_string()])
            .await
            .unwrap();
        assert_eq!(proc.try_exit_status(), None);
        proc.kill().await;
        assert!(proc.try_exit_status().is_some());
    }

    #[tokio::test]
    async fn run_bounded_with_env_drops_inherited_vagrant_vars_only_when_one_is_pinned() {
        std::env::set_var("VAGRANT_CD_TEST_LEAK", "leaked");
        std::env::set_var("CD_TEST_KEEP", "kept");
        let print = vec![
            "-c".to_string(),
            "echo \"${VAGRANT_CD_TEST_LEAK:-none}/${CD_TEST_KEEP:-none}/${VAGRANT_CWD:-none}\""
                .to_string(),
        ];
        let limits = CommandLimits::default;
        let pinned = vec![("VAGRANT_CWD".to_string(), "/p".to_string())];
        let out = SystemRunner
            .run_bounded_with_env("sh", &print, &pinned, limits())
            .await
            .unwrap();
        assert_eq!(out.stdout, "none/kept//p");
        let out = SystemRunner
            .run_bounded_with_env("sh", &print, &[], limits())
            .await
            .unwrap();
        assert_eq!(out.stdout, "leaked/kept/none");
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
