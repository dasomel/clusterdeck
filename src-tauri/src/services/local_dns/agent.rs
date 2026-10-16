#![allow(dead_code)]

use sha2::{Digest, Sha256};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use crate::services::local_dns::zone_file::StatusFile;
use crate::services::process::CommandRunner;

pub const AGENT_LABEL: &str = "com.clusterdeck.dns";
// Apple-installed tools by absolute path (R10): a GUI app's PATH must not decide what runs.
const LAUNCHCTL: &str = "/bin/launchctl";
const LSOF: &str = "/usr/sbin/lsof";
const ID: &str = "/usr/bin/id";

static TMP_SEQ: AtomicU64 = AtomicU64::new(0);

/// Every on-disk location the DNS agent uses, derived from one home directory so tests can
/// point the whole lifecycle at a temp dir.
#[derive(Debug, Clone)]
pub struct AgentPaths {
    pub binary: PathBuf,
    pub dns_dir: PathBuf,
    pub zones: PathBuf,
    pub status: PathBuf,
    pub generation: PathBuf,
    pub stderr_log: PathBuf,
    pub plist: PathBuf,
}

impl AgentPaths {
    pub fn new(home: &Path) -> Self {
        let base = home.join(".clusterdeck");
        let dns_dir = base.join("dns");
        Self {
            binary: base.join("bin").join("clusterdeck-dns"),
            zones: dns_dir.join("zones.json"),
            status: dns_dir.join("status.json"),
            generation: dns_dir.join("generation"),
            stderr_log: dns_dir.join("agent.err.log"),
            plist: home
                .join("Library")
                .join("LaunchAgents")
                .join(format!("{AGENT_LABEL}.plist")),
            dns_dir,
        }
    }

    pub fn resolve() -> Result<Self, String> {
        let home = std::env::var("HOME").map_err(|_| "HOME is not set".to_string())?;
        Ok(Self::new(Path::new(&home)))
    }
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

fn xml_path(path: &Path, label: &str) -> Result<String, String> {
    if !path.is_absolute() {
        return Err(format!(
            "{label} must be an absolute path: {}",
            path.display()
        ));
    }
    let s = path
        .to_str()
        .ok_or_else(|| format!("{label} is not valid UTF-8"))?;
    // XML 1.0 cannot represent control characters at all, escaped or not.
    if s.chars().any(char::is_control) {
        return Err(format!("{label} contains control characters"));
    }
    Ok(xml_escape(s))
}

pub fn render_plist(paths: &AgentPaths, port: u16) -> Result<String, String> {
    if port == 0 {
        return Err("invalid port: 0".to_string());
    }
    let args = [
        xml_path(&paths.binary, "agent binary")?,
        "--dns-responder".to_string(),
        "--zones".to_string(),
        xml_path(&paths.zones, "zones path")?,
        "--status".to_string(),
        xml_path(&paths.status, "status path")?,
        "--port".to_string(),
        port.to_string(),
    ];
    let args_xml: String = args
        .iter()
        .map(|a| format!("    <string>{a}</string>\n"))
        .collect();
    let stderr = xml_path(&paths.stderr_log, "stderr path")?;
    Ok(format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n\
<plist version=\"1.0\">\n\
<dict>\n  <key>Label</key>\n  <string>{AGENT_LABEL}</string>\n\
  <key>ProgramArguments</key>\n  <array>\n{args_xml}  </array>\n\
  <key>RunAtLoad</key>\n  <true/>\n\
  <key>KeepAlive</key>\n  <true/>\n\
  <key>StandardErrorPath</key>\n  <string>{stderr}</string>\n\
</dict>\n</plist>\n"
    ))
}

/// Temp-file + rename in the destination directory, so readers never see a partial file.
pub fn atomic_write(path: &Path, bytes: &[u8], mode: u32) -> Result<(), String> {
    let parent = path
        .parent()
        .ok_or_else(|| format!("no parent directory for {}", path.display()))?;
    std::fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
    let seq = TMP_SEQ.fetch_add(1, Ordering::Relaxed);
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("file");
    let tmp = parent.join(format!(".{name}.tmp.{}.{seq}", std::process::id()));
    let res = std::fs::write(&tmp, bytes)
        .and_then(|_| std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(mode)))
        .and_then(|_| std::fs::rename(&tmp, path));
    if let Err(e) = res {
        let _ = std::fs::remove_file(&tmp);
        return Err(format!("{}: {e}", path.display()));
    }
    Ok(())
}

/// Copies `current_exe` to the stable agent path only when its SHA-256 differs (R10), so app
/// moves/updates/dev rebuilds never leave the LaunchAgent pointing at a vanished binary.
/// Returns whether the destination changed (the caller then reloads the agent).
pub fn ensure_binary(current_exe: &Path, dest: &Path) -> Result<bool, String> {
    let src = std::fs::read(current_exe)
        .map_err(|e| format!("failed to read {}: {e}", current_exe.display()))?;
    if let Ok(existing) = std::fs::read(dest) {
        if Sha256::digest(&existing) == Sha256::digest(&src) {
            // Contents match; still make sure it stays executable.
            let _ = std::fs::set_permissions(dest, std::fs::Permissions::from_mode(0o755));
            return Ok(false);
        }
    }
    atomic_write(dest, &src, 0o755)?;
    Ok(true)
}

/// Writes the plist when its content differs; returns whether it changed.
pub fn ensure_plist(paths: &AgentPaths, port: u16) -> Result<bool, String> {
    let rendered = render_plist(paths, port)?;
    if std::fs::read_to_string(&paths.plist).ok().as_deref() == Some(rendered.as_str()) {
        return Ok(false);
    }
    atomic_write(&paths.plist, rendered.as_bytes(), 0o644)?;
    Ok(true)
}

pub async fn current_uid(runner: &dyn CommandRunner) -> Result<u32, String> {
    let out = runner.run(ID, &["-u".to_string()]).await?;
    if !out.success {
        return Err(format!("id -u failed: {}", out.stderr));
    }
    out.stdout
        .trim()
        .parse()
        .map_err(|_| format!("unexpected `id -u` output: {}", out.stdout))
}

fn domain(uid: u32) -> String {
    format!("gui/{uid}")
}

async fn launchctl(runner: &dyn CommandRunner, args: Vec<String>) -> Result<(), String> {
    let out = runner.run(LAUNCHCTL, &args).await?;
    if out.success {
        Ok(())
    } else {
        Err(format!(
            "launchctl {} failed: {}",
            args.first().map(String::as_str).unwrap_or(""),
            out.stderr
        ))
    }
}

pub async fn install(
    runner: &dyn CommandRunner,
    paths: &AgentPaths,
    uid: u32,
) -> Result<(), String> {
    launchctl(
        runner,
        vec![
            "bootstrap".to_string(),
            domain(uid),
            paths.plist.to_string_lossy().into_owned(),
        ],
    )
    .await
}

/// Picks up a changed binary or plist: bootout (a not-loaded agent is fine) then bootstrap.
pub async fn reload(
    runner: &dyn CommandRunner,
    paths: &AgentPaths,
    uid: u32,
) -> Result<(), String> {
    let _ = bootout(runner, paths, uid).await;
    install(runner, paths, uid).await
}

async fn bootout(runner: &dyn CommandRunner, paths: &AgentPaths, uid: u32) -> Result<(), String> {
    launchctl(
        runner,
        vec![
            "bootout".to_string(),
            domain(uid),
            paths.plist.to_string_lossy().into_owned(),
        ],
    )
    .await
}

/// Boots the agent out and deletes the plist and the copied binary (R10). A bootout failure
/// for an agent that is not loaded is tolerated; the files are removed regardless.
pub async fn uninstall(
    runner: &dyn CommandRunner,
    paths: &AgentPaths,
    uid: u32,
) -> Result<(), String> {
    // Loaded, not merely running: a loaded job without a pid would otherwise come back after
    // its plist and binary are deleted.
    if is_loaded(runner, uid).await? {
        bootout(runner, paths, uid).await?;
    }
    for f in [&paths.plist, &paths.binary] {
        match std::fs::remove_file(f) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(format!("{}: {e}", f.display())),
        }
    }
    Ok(())
}

/// `launchctl print` output when the job is loaded (even if it has no pid right now, e.g.
/// throttled after a crash); `None` when it is not loaded.
async fn print_job(runner: &dyn CommandRunner, uid: u32) -> Result<Option<String>, String> {
    let out = runner
        .run(
            LAUNCHCTL,
            &[
                "print".to_string(),
                format!("{}/{AGENT_LABEL}", domain(uid)),
            ],
        )
        .await?;
    Ok(out.success.then_some(out.stdout))
}

pub async fn is_loaded(runner: &dyn CommandRunner, uid: u32) -> Result<bool, String> {
    Ok(print_job(runner, uid).await?.is_some())
}

/// Pid from `launchctl print gui/<uid>/com.clusterdeck.dns`; `None` when not loaded or not
/// running (no `pid =` line).
pub async fn agent_pid(runner: &dyn CommandRunner, uid: u32) -> Result<Option<u32>, String> {
    Ok(print_job(runner, uid)
        .await?
        .and_then(|stdout| parse_launchctl_pid(&stdout)))
}

fn parse_launchctl_pid(stdout: &str) -> Option<u32> {
    stdout
        .lines()
        .filter_map(|l| l.trim().strip_prefix("pid = "))
        .find_map(|v| v.trim().parse().ok())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Proto {
    Udp,
    TcpListen,
}

/// Pids bound to `port` on the given protocol (`lsof` exits 1 with no output when none).
pub async fn listener_pids(
    runner: &dyn CommandRunner,
    port: u16,
    proto: Proto,
) -> Result<Vec<u32>, String> {
    let args: Vec<String> = match proto {
        Proto::Udp => vec!["-nP".into(), format!("-iUDP:{port}"), "-t".into()],
        Proto::TcpListen => vec![
            "-nP".into(),
            format!("-iTCP:{port}"),
            "-sTCP:LISTEN".into(),
            "-t".into(),
        ],
    };
    let out = runner.run(LSOF, &args).await?;
    let mut pids: Vec<u32> = out
        .stdout
        .lines()
        .filter_map(|l| l.trim().parse().ok())
        .collect();
    pids.sort_unstable();
    pids.dedup();
    Ok(pids)
}

/// Pure core of the R8 gate: launchctl pid, status.pid and BOTH listener sets must name the
/// same single process, and the responder must report no error.
pub fn ownership_decision(
    launchctl_pid: Option<u32>,
    status: &StatusFile,
    udp: &[u32],
    tcp: &[u32],
) -> bool {
    let Some(pid) = launchctl_pid else {
        return false;
    };
    status.error.is_none() && status.pid == pid && udp == [pid] && tcp == [pid]
}

pub async fn ownership_ok(
    runner: &dyn CommandRunner,
    uid: u32,
    port: u16,
    status: &StatusFile,
) -> Result<bool, String> {
    let pid = agent_pid(runner, uid).await?;
    if pid.is_none() {
        return Ok(false);
    }
    let udp = listener_pids(runner, port, Proto::Udp).await?;
    let tcp = listener_pids(runner, port, Proto::TcpListen).await?;
    Ok(ownership_decision(pid, status, &udp, &tcp))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_home(tag: &str) -> PathBuf {
        let seq = TMP_SEQ.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "clusterdeck-agent-test-{tag}-{}-{seq}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn status(pid: u32, error: Option<&str>) -> StatusFile {
        StatusFile {
            generation: 1,
            loaded_at: "t".into(),
            error: error.map(String::from),
            pid,
            instance: "i".into(),
        }
    }

    #[test]
    fn plist_has_label_arguments_keepalive_and_escapes_xml() {
        let paths = AgentPaths::new(Path::new("/Users/a&b<c>\"d'"));
        let plist = render_plist(&paths, 15353).unwrap();
        assert!(plist.contains("<string>com.clusterdeck.dns</string>"));
        assert!(plist.contains("<string>--dns-responder</string>"));
        assert!(plist.contains("<string>15353</string>"));
        assert!(plist.contains("<key>KeepAlive</key>\n  <true/>"));
        assert!(plist.contains("<key>RunAtLoad</key>\n  <true/>"));
        assert!(
            plist.contains("/Users/a&amp;b&lt;c&gt;&quot;d&apos;/.clusterdeck/bin/clusterdeck-dns")
        );
        assert!(plist.contains("<key>StandardErrorPath</key>"));
        assert!(!plist.contains("a&b"), "raw ampersand leaked: {plist}");
    }

    #[test]
    fn plist_rejects_relative_paths_control_chars_and_port_zero() {
        assert!(render_plist(&AgentPaths::new(Path::new("relative/home")), 15353).is_err());
        assert!(render_plist(&AgentPaths::new(Path::new("/Users/a\nb")), 15353).is_err());
        assert!(render_plist(&AgentPaths::new(Path::new("/Users/a")), 0).is_err());
    }

    #[test]
    fn ensure_binary_copies_only_when_hash_differs_and_sets_0755() {
        let home = tmp_home("bin");
        let src = home.join("src-exe");
        let dest = home.join("bin").join("clusterdeck-dns");
        std::fs::write(&src, b"v1").unwrap();

        assert!(ensure_binary(&src, &dest).unwrap(), "first copy");
        assert_eq!(std::fs::read(&dest).unwrap(), b"v1");
        let mode = std::fs::metadata(&dest).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o755);

        assert!(!ensure_binary(&src, &dest).unwrap(), "identical: no copy");

        std::fs::write(&src, b"v2").unwrap();
        assert!(ensure_binary(&src, &dest).unwrap());
        assert_eq!(std::fs::read(&dest).unwrap(), b"v2");
        let leftovers = std::fs::read_dir(dest.parent().unwrap())
            .unwrap()
            .filter_map(Result::ok)
            .count();
        assert_eq!(leftovers, 1, "temp file left behind");
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn launchctl_pid_parsing_ignores_other_lines() {
        let out =
            "com.clusterdeck.dns = {\n\tactive count = 1\n\tpid = 4321\n\tlast exit code = 0\n}";
        assert_eq!(parse_launchctl_pid(out), Some(4321));
        assert_eq!(parse_launchctl_pid("state = waiting\n"), None);
    }

    #[test]
    fn ownership_requires_every_pid_to_agree_and_no_error() {
        let ok = status(10, None);
        assert!(ownership_decision(Some(10), &ok, &[10], &[10]));
        assert!(!ownership_decision(None, &ok, &[10], &[10]));
        assert!(
            !ownership_decision(Some(11), &ok, &[10], &[10]),
            "status.pid differs"
        );
        assert!(
            !ownership_decision(Some(10), &ok, &[99], &[10]),
            "udp squatter"
        );
        assert!(
            !ownership_decision(Some(10), &ok, &[10], &[99]),
            "tcp squatter"
        );
        assert!(
            !ownership_decision(Some(10), &ok, &[10, 99], &[10]),
            "shared udp"
        );
        assert!(!ownership_decision(Some(10), &ok, &[], &[10]));
        assert!(!ownership_decision(
            Some(10),
            &status(10, Some("boom")),
            &[10],
            &[10]
        ));
    }

    struct PrintOnly {
        loaded: bool,
        calls: std::sync::Mutex<Vec<String>>,
    }

    #[async_trait::async_trait]
    impl CommandRunner for PrintOnly {
        async fn run(
            &self,
            _bin: &str,
            args: &[String],
        ) -> Result<crate::services::process::CommandOutput, String> {
            self.calls.lock().unwrap().push(args[0].clone());
            let ok = args[0] != "print" || self.loaded;
            Ok(crate::services::process::CommandOutput {
                // Loaded but with no `pid =` line (throttled / between restarts).
                stdout: if args[0] == "print" {
                    "state = waiting".into()
                } else {
                    String::new()
                },
                stderr: String::new(),
                success: ok,
            })
        }
    }

    #[tokio::test]
    async fn uninstall_boots_out_a_loaded_job_that_has_no_pid_before_deleting_files() {
        let home = tmp_home("uninstall");
        let paths = AgentPaths::new(&home);
        std::fs::create_dir_all(paths.binary.parent().unwrap()).unwrap();
        std::fs::create_dir_all(paths.plist.parent().unwrap()).unwrap();
        std::fs::write(&paths.binary, b"x").unwrap();
        std::fs::write(&paths.plist, b"x").unwrap();

        let runner = PrintOnly {
            loaded: true,
            calls: std::sync::Mutex::new(Vec::new()),
        };
        assert_eq!(agent_pid(&runner, 501).await.unwrap(), None);
        uninstall(&runner, &paths, 501).await.unwrap();
        assert!(runner
            .calls
            .lock()
            .unwrap()
            .contains(&"bootout".to_string()));
        assert!(!paths.plist.exists() && !paths.binary.exists());

        // Not loaded: no bootout, files still removed.
        std::fs::write(&paths.binary, b"x").unwrap();
        let runner = PrintOnly {
            loaded: false,
            calls: std::sync::Mutex::new(Vec::new()),
        };
        uninstall(&runner, &paths, 501).await.unwrap();
        assert!(!runner
            .calls
            .lock()
            .unwrap()
            .contains(&"bootout".to_string()));
        assert!(!paths.binary.exists());
        let _ = std::fs::remove_dir_all(&home);
    }
}
