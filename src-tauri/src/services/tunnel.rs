//! App-managed SSH API tunnel for bastion profiles (ADR-0009).
//!
//! A bastion profile's API server (e.g. `https://10.0.0.10:6443`) is only reachable from the
//! control plane, so the Mac forwards `127.0.0.1:<L>` through the profile's own `-F` SSH config
//! (which already carries the ProxyJump) to the server address as seen from the control plane.

use crate::services::config::Profile;
use crate::services::paths::ClusterDeckPaths;
use crate::services::process::{BackgroundProcess, CommandLimits, CommandRunner};
use crate::services::ssh_config::ssh_alias;
use crate::services::validate::is_safe_ssh_identifier;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, LazyLock};
use std::time::Duration;
use tokio::sync::Mutex;

const PORT_RANGE_START: u16 = 16443;
const PORT_RANGE_LEN: u16 = 1000;
const READY_POLL: Duration = Duration::from_millis(200);
const READY_TIMEOUT: Duration = Duration::from_secs(10);

/// API server host/port as seen from the control plane (the `-L` destination).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TunnelTarget {
    pub host: String,
    pub port: u16,
}

/// Local socket checks, injectable so tests need neither real sockets nor real time.
#[async_trait]
pub trait PortProbe: Send + Sync {
    /// Can 127.0.0.1:`port` be bound right now?
    fn is_bindable(&self, port: u16) -> bool;
    /// Does something accept TCP connections on 127.0.0.1:`port`?
    async fn is_open(&self, port: u16) -> bool;
}

pub struct SystemPortProbe;

#[async_trait]
impl PortProbe for SystemPortProbe {
    fn is_bindable(&self, port: u16) -> bool {
        std::net::TcpListener::bind(("127.0.0.1", port)).is_ok()
    }

    async fn is_open(&self, port: u16) -> bool {
        tokio::time::timeout(
            Duration::from_secs(1),
            tokio::net::TcpStream::connect(("127.0.0.1", port)),
        )
        .await
        .is_ok_and(|r| r.is_ok())
    }
}

/// FNV-1a 32-bit. Hand-rolled because `std`'s `DefaultHasher` output is explicitly not stable
/// across releases, and the preferred port must not move when the toolchain is upgraded
/// (the stored kubeconfig and any merged ~/.kube/config embed it).
fn fnv1a_32(data: &[u8]) -> u32 {
    data.iter().fold(0x811c_9dc5u32, |h, b| {
        (h ^ u32::from(*b)).wrapping_mul(0x0100_0193)
    })
}

/// ADR-0009 D4: the deterministic first-choice local port of a profile.
pub fn preferred_port(profile_id: &str) -> u16 {
    PORT_RANGE_START + (fnv1a_32(profile_id.as_bytes()) % u32::from(PORT_RANGE_LEN)) as u16
}

/// First bindable port at or after `preferred_port`, wrapping inside the range.
fn choose_port(profile_id: &str, probe: &dyn PortProbe, exclude: &[u16]) -> Result<u16, String> {
    let offset = preferred_port(profile_id) - PORT_RANGE_START;
    (0..PORT_RANGE_LEN)
        .map(|i| PORT_RANGE_START + (offset + i) % PORT_RANGE_LEN)
        .find(|p| !exclude.contains(p) && probe.is_bindable(*p))
        .ok_or_else(|| {
            format!(
                "no free local port in {PORT_RANGE_START}-{}",
                PORT_RANGE_START + PORT_RANGE_LEN - 1
            )
        })
}

/// Sink validation for the `-L` destination. Deliberately stricter than
/// `is_safe_ssh_identifier`: the value comes from a kubeconfig fetched off a remote machine
/// (untrusted) and is interpolated into a `-L` spec where `:` and `/` carry meaning, so only a
/// hostname/IP shape is accepted.
fn validate_target_host(host: &str) -> Result<(), String> {
    let is_hostname = host
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'));
    let ok =
        is_safe_ssh_identifier(host) && (host.parse::<std::net::Ipv6Addr>().is_ok() || is_hostname);
    if ok {
        Ok(())
    } else {
        Err(format!("unsafe API server host for tunnel: {host:?}"))
    }
}

/// ADR-0009 D2. `alias` is the same `<profile>-<host>` alias the kubeconfig fetch uses with
/// `-F`; the generated config supplies ProxyJump, identity and
/// `StrictHostKeyChecking=accept-new`.
fn tunnel_argv(
    ssh_conf: &std::path::Path,
    local_port: u16,
    target: &TunnelTarget,
    alias: &str,
) -> Result<Vec<String>, String> {
    validate_target_host(&target.host)?;
    if !is_safe_ssh_identifier(alias) {
        return Err(format!("unsafe ssh alias: {alias}"));
    }
    let dest = if target.host.contains(':') {
        format!("[{}]", target.host)
    } else {
        target.host.clone()
    };
    let mut argv: Vec<String> = vec!["-F".into(), ssh_conf.to_string_lossy().to_string()];
    for opt in [
        "BatchMode=yes",
        "ConnectTimeout=5",
        "ExitOnForwardFailure=yes",
        "ServerAliveInterval=15",
        "ServerAliveCountMax=3",
    ] {
        argv.push("-o".into());
        argv.push(opt.into());
    }
    argv.push("-N".into());
    argv.push("-L".into());
    argv.push(format!("127.0.0.1:{local_port}:{dest}:{}", target.port));
    argv.push("--".into());
    argv.push(alias.to_string());
    Ok(argv)
}

struct TunnelEntry {
    port: u16,
    target: TunnelTarget,
    process: Box<dyn BackgroundProcess>,
}

/// One slot per profile. The slot's own lock is what an in-flight `ensure_tunnel` holds across
/// its readiness wait, so a slow profile never blocks another profile or `stop_all`.
type Slot = Arc<Mutex<Option<TunnelEntry>>>;

pub struct TunnelManager {
    slots: Mutex<HashMap<String, Slot>>,
    /// Pids of ssh children still in their readiness wait. Deliberately outside the slot lock:
    /// `stop_all` must be able to kill them while `ensure_tunnel` holds that lock.
    in_flight: std::sync::Mutex<HashMap<String, u32>>,
    probe: Arc<dyn PortProbe>,
    ready_poll: Duration,
    ready_timeout: Duration,
}

/// Why one spawn attempt failed. `bind_failure` marks a local-port conflict that is worth one
/// retry on the next candidate port.
struct StartError {
    message: String,
    bind_failure: bool,
}

/// Is `pid` among the TCP listeners of 127.0.0.1:`port`? `None` when lsof is unavailable or
/// failed, so the caller falls back to the probe-only check instead of failing the tunnel.
async fn owns_port(runner: &dyn CommandRunner, port: u16, pid: u32) -> Option<bool> {
    let args = [
        "-nP".to_string(),
        "-a".to_string(),
        format!("-iTCP:{port}"),
        "-sTCP:LISTEN".to_string(),
        "-t".to_string(),
    ];
    let out = runner
        .run_bounded("lsof", &args, CommandLimits::default())
        .await
        .ok()?;
    // lsof exits 1 with no output when nothing listens; any other failure is "unavailable".
    if !out.success && !(out.stdout.is_empty() && out.stderr.is_empty()) {
        return None;
    }
    Some(
        out.stdout
            .lines()
            .any(|l| l.trim().parse::<u32>() == Ok(pid)),
    )
}

fn is_bind_failure(stderr: &str) -> bool {
    [
        "Address already in use",
        "cannot listen to port",
        "Could not request local forwarding",
    ]
    .iter()
    .any(|needle| stderr.contains(needle))
}

/// D5b: one manager per app process. A `static` (like `store.rs`'s `PROFILE_WRITE_MUTEX`)
/// rather than Tauri managed state because the tunnel is needed deep inside
/// `kubeconfig::fetch_and_store`, which is reached from several commands and from
/// `ensure_profile_kubeconfig`; threading `State` through all of them would touch every caller.
static GLOBAL: LazyLock<TunnelManager> =
    LazyLock::new(|| TunnelManager::new(Arc::new(SystemPortProbe)));

pub fn global() -> &'static TunnelManager {
    &GLOBAL
}

impl TunnelManager {
    pub fn new(probe: Arc<dyn PortProbe>) -> Self {
        Self {
            slots: Mutex::new(HashMap::new()),
            in_flight: std::sync::Mutex::new(HashMap::new()),
            probe,
            ready_poll: READY_POLL,
            ready_timeout: READY_TIMEOUT,
        }
    }

    async fn slot(&self, profile_id: &str) -> Slot {
        self.slots
            .lock()
            .await
            .entry(profile_id.to_string())
            .or_default()
            .clone()
    }

    /// Returns the local port forwarding to `orig_host:orig_port` for `profile`, starting (or
    /// restarting) the tunnel as needed. Only this profile's slot is locked across the readiness
    /// wait: two concurrent callers for the same profile must not each spawn an `ssh`, but other
    /// profiles and shutdown proceed.
    pub async fn ensure_tunnel(
        &self,
        runner: &dyn CommandRunner,
        paths: &ClusterDeckPaths,
        profile: &Profile,
        orig_host: &str,
        orig_port: u16,
    ) -> Result<u16, String> {
        let target = TunnelTarget {
            host: orig_host.to_string(),
            port: orig_port,
        };
        // Re-check at the sink (AGENTS.md): the id reaches `paths.ssh_conf`, the host reaches
        // an ssh argv.
        if !crate::services::validate::is_safe_profile_id(&profile.id) {
            return Err("invalid profile id".to_string());
        }
        validate_target_host(&target.host)?;
        let control_plane = &profile
            .kubeconfig
            .as_ref()
            .ok_or_else(|| "Profile has no kubeconfig configuration".to_string())?
            .control_plane;
        let alias = ssh_alias(&profile.id, control_plane);
        let ssh_conf = paths.ssh_conf(&profile.id);

        let slot = self.slot(&profile.id).await;
        let mut entry = slot.lock().await;
        if let Some(mut old) = entry.take() {
            if old.target == target
                && old.process.try_exit_status().is_none()
                && self.probe.is_open(old.port).await
            {
                let port = old.port;
                *entry = Some(old);
                return Ok(port);
            }
            old.process.kill().await;
            let _ = remove_tunnel_record(paths, &profile.id);
        }

        // ADR-0009 / Issue #97: Reap any orphan left behind from a previous crash
        // for this profile so its preferred port is freed before choose_port.
        let _ = reap_profile_orphan(runner, paths, &profile.id).await;

        let mut excluded: Vec<u16> = Vec::new();
        for attempt in 0..2 {
            let port = choose_port(&profile.id, self.probe.as_ref(), &excluded)?;
            let argv = tunnel_argv(&ssh_conf, port, &target, &alias)?;
            let record = TunnelRecord {
                profile_id: profile.id.clone(),
                pid: 0,
                port,
                target: target.clone(),
                ssh_conf: ssh_conf.clone(),
                alias: alias.clone(),
                started_at_epoch_s: std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_secs())
                    .unwrap_or(0),
            };
            let started = self
                .start_once(runner, paths, &profile.id, &argv, port, record)
                .await;
            self.in_flight.lock().unwrap().remove(&profile.id);
            match started {
                Ok(process) => {
                    // Best effort: the tunnel works without it; only a later verify after an
                    // app restart needs the persisted target.
                    let _ = crate::services::state::save_tunnel_target(paths, &profile.id, &target);
                    *entry = Some(TunnelEntry {
                        port,
                        target,
                        process,
                    });
                    return Ok(port);
                }
                // A conflict with a foreign listener that appeared after the bind check: retry
                // once on the next candidate instead of failing the whole connect.
                Err(e) if e.bind_failure && attempt == 0 => excluded.push(port),
                Err(e) => return Err(e.message),
            }
        }
        unreachable!("the second attempt always returns")
    }

    /// Spawns ssh and waits until it owns `port`. A bare "port accepts connections" is not
    /// proof: a foreign listener can appear after the bind check, and ssh only binds after
    /// authentication. Readiness is therefore the port open on two consecutive probes one poll
    /// apart while the child is alive, plus, when `lsof` works, the child's pid being among the
    /// port's listeners. Without lsof the window is only narrowed, not closed: a foreign
    /// listener makes ssh exit (ExitOnForwardFailure), which the final alive check catches.
    async fn start_once(
        &self,
        runner: &dyn CommandRunner,
        paths: &ClusterDeckPaths,
        profile_id: &str,
        argv: &[String],
        port: u16,
        record: TunnelRecord,
    ) -> Result<Box<dyn BackgroundProcess>, StartError> {
        let other = |message: String| StartError {
            message,
            bind_failure: false,
        };
        let mut process = runner.spawn_background("ssh", argv).await.map_err(other)?;

        let pid = process.pid();
        if let Some(pid) = pid {
            self.in_flight
                .lock()
                .unwrap()
                .insert(profile_id.to_string(), pid);
            let mut rec = record;
            rec.pid = pid;
            let _ = write_tunnel_record(paths, &rec);
        }

        let started = tokio::time::Instant::now();
        let mut consecutive = 0u8;
        loop {
            if let Some(code) = process.try_exit_status() {
                let _ = remove_tunnel_record(paths, profile_id);
                return Err(Self::exited(process.as_mut(), code).await);
            }
            if self.probe.is_open(port).await {
                consecutive += 1;
                if consecutive >= 2 {
                    // `Some(false)`: something else listens there; keep waiting (the child
                    // will exit on a forward failure, or we time out) rather than adopt it.
                    match pid {
                        Some(pid) if owns_port(runner, port, pid).await == Some(false) => {
                            consecutive = 0;
                        }
                        _ => break,
                    }
                }
            } else {
                consecutive = 0;
            }
            if started.elapsed() >= self.ready_timeout {
                process.kill().await;
                let _ = remove_tunnel_record(paths, profile_id);
                return Err(other(format!(
                    "ssh tunnel not ready after {}s",
                    self.ready_timeout.as_secs()
                )));
            }
            tokio::time::sleep(self.ready_poll).await;
        }
        if let Some(code) = process.try_exit_status() {
            let _ = remove_tunnel_record(paths, profile_id);
            return Err(Self::exited(process.as_mut(), code).await);
        }
        Ok(process)
    }

    async fn exited(process: &mut dyn BackgroundProcess, code: i32) -> StartError {
        let stderr = process.stderr_tail().await;
        StartError {
            bind_failure: is_bind_failure(&stderr),
            message: format!("ssh tunnel exited with status {code}: {stderr}"),
        }
    }

    /// The live tunnel's target for `profile_id`, if any. A slot busy with an in-flight
    /// `ensure_tunnel` has no live tunnel yet.
    pub async fn live_target(&self, profile_id: &str) -> Option<TunnelTarget> {
        let slot = self.slots.lock().await.get(profile_id)?.clone();
        let mut guard = slot.try_lock().ok()?;
        let entry = guard.as_mut()?;
        entry
            .process
            .try_exit_status()
            .is_none()
            .then(|| entry.target.clone())
    }

    /// Stops the profile's tunnel and forgets its persisted target, for a kubeconfig that no
    /// longer points at the tunnel (ADR-0009 Security), so Verify cannot revive it.
    pub async fn retire(&self, paths: &ClusterDeckPaths, profile_id: &str) {
        self.stop_tunnel(profile_id).await;
        let _ = remove_tunnel_record(paths, profile_id);
        let _ = crate::services::state::clear_tunnel_target(paths, profile_id);
    }

    /// Waits for this profile's in-flight `ensure_tunnel` (if any), then kills its tunnel.
    pub async fn stop_tunnel(&self, profile_id: &str) {
        let slot = self.slots.lock().await.get(profile_id).cloned();
        if let Some(slot) = slot {
            if let Some(mut entry) = slot.lock().await.take() {
                entry.process.kill().await;
            }
        }
        if let Ok(paths) = ClusterDeckPaths::resolve() {
            let _ = remove_tunnel_record(&paths, profile_id);
        }
    }

    /// Kills every stored tunnel without waiting on in-flight `ensure_tunnel` calls (`try_lock`):
    /// this runs from `RunEvent::Exit` under `block_on`, and quitting must not hang for the
    /// readiness timeout. The in-flight attempts' ssh children are SIGKILLed by pid instead
    /// (through the runner; there is no libc dependency), because their tasks are never dropped
    /// before the process exits, so `kill_on_drop` cannot be relied on.
    pub async fn stop_all(&self, runner: &dyn CommandRunner) {
        if let Ok(paths) = ClusterDeckPaths::resolve() {
            self.stop_all_with_paths(runner, &paths).await;
        } else {
            self.stop_all_internal(runner).await;
        }
    }

    pub async fn stop_all_with_paths(&self, runner: &dyn CommandRunner, paths: &ClusterDeckPaths) {
        self.stop_all_internal(runner).await;
        if let Ok(records) = list_tunnel_records(paths) {
            for record in records {
                let _ = remove_tunnel_record(paths, &record.profile_id);
            }
        }
    }

    async fn stop_all_internal(&self, runner: &dyn CommandRunner) {
        let slots: Vec<Slot> = self.slots.lock().await.values().cloned().collect();
        for slot in slots {
            if let Ok(mut guard) = slot.try_lock() {
                if let Some(mut entry) = guard.take() {
                    entry.process.kill().await;
                }
            }
        }
        let pids: Vec<u32> = self
            .in_flight
            .lock()
            .unwrap()
            .drain()
            .map(|(_, p)| p)
            .collect();
        for pid in pids {
            let _ = runner
                .run("kill", &["-9".to_string(), pid.to_string()])
                .await;
        }
    }
}

/// A persisted record of a live or in-flight tunnel child process.
/// Stored in `~/.clusterdeck/tunnels/<profile_id>.json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TunnelRecord {
    pub profile_id: String,
    pub pid: u32,
    pub port: u16,
    pub target: TunnelTarget,
    pub ssh_conf: PathBuf,
    pub alias: String,
    pub started_at_epoch_s: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReapOutcome {
    Killed { pid: u32 },
    AlreadyDead { pid: u32 },
    PidRecycled { pid: u32, actual_cmdline: String },
    Failed { pid: u32, reason: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReapResult {
    pub profile_id: String,
    pub outcome: ReapOutcome,
}

pub fn write_tunnel_record(paths: &ClusterDeckPaths, record: &TunnelRecord) -> Result<(), String> {
    if !crate::services::validate::is_safe_profile_id(&record.profile_id) {
        return Err("invalid profile id in tunnel record".to_string());
    }
    paths.ensure_dirs()?;
    let path = paths.tunnel_pid_file(&record.profile_id);
    let json = serde_json::to_string_pretty(record).map_err(|e| e.to_string())?;
    std::fs::write(&path, json).map_err(|e| e.to_string())
}

pub fn remove_tunnel_record(paths: &ClusterDeckPaths, profile_id: &str) -> Result<(), String> {
    if !crate::services::validate::is_safe_profile_id(profile_id) {
        return Err("invalid profile id".to_string());
    }
    let path = paths.tunnel_pid_file(profile_id);
    if path.exists() {
        std::fs::remove_file(path).map_err(|e| e.to_string())?;
    }
    Ok(())
}

pub fn read_tunnel_record(
    paths: &ClusterDeckPaths,
    profile_id: &str,
) -> Result<Option<TunnelRecord>, String> {
    if !crate::services::validate::is_safe_profile_id(profile_id) {
        return Err("invalid profile id".to_string());
    }
    let path = paths.tunnel_pid_file(profile_id);
    if !path.exists() {
        return Ok(None);
    }
    let content = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
    serde_json::from_str(&content)
        .map(Some)
        .map_err(|e| e.to_string())
}

pub fn list_tunnel_records(paths: &ClusterDeckPaths) -> Result<Vec<TunnelRecord>, String> {
    let dir = paths.tunnels_dir();
    if !dir.exists() {
        return Ok(Vec::new());
    }
    let mut records = Vec::new();
    let entries = std::fs::read_dir(dir).map_err(|e| e.to_string())?;
    for entry in entries {
        let entry = entry.map_err(|e| e.to_string())?;
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) == Some("json") {
            if let Ok(content) = std::fs::read_to_string(&path) {
                if let Ok(record) = serde_json::from_str::<TunnelRecord>(&content) {
                    records.push(record);
                }
            }
        }
    }
    Ok(records)
}

/// Strict 5-point signature check verifying whether a process command line matches ClusterDeck's
/// SSH child process. Prevents PID recycling from killing innocent processes.
pub fn is_matching_tunnel_cmdline(cmdline: &str, record: &TunnelRecord) -> bool {
    for line in cmdline.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let tokens: Vec<&str> = trimmed.split_whitespace().collect();
        if tokens.is_empty() {
            continue;
        }
        let bin = tokens[0];
        let is_ssh = bin == "ssh" || bin.ends_with("/ssh");
        if !is_ssh {
            continue;
        }
        if !tokens.contains(&"-N") {
            continue;
        }
        let conf_str = record.ssh_conf.to_string_lossy();
        let conf_filename = format!("{}.conf", record.profile_id);
        let has_conf = (tokens.contains(&"-F")
            && (trimmed.contains(&*conf_str) || trimmed.contains(&conf_filename)))
            || tokens
                .windows(2)
                .any(|w| w[0] == "-F" && (w[1] == conf_str || w[1].ends_with(&conf_filename)));
        if !has_conf {
            continue;
        }
        let port_pattern = format!("127.0.0.1:{}:", record.port);
        let has_port = tokens.contains(&"-L") && trimmed.contains(&port_pattern);
        if !has_port {
            continue;
        }
        if !trimmed.contains(&record.alias) {
            continue;
        }
        return true;
    }
    false
}

pub async fn reap_single_orphan(
    runner: &dyn CommandRunner,
    paths: &ClusterDeckPaths,
    record: &TunnelRecord,
) -> ReapOutcome {
    let ps_res = runner
        .run(
            "ps",
            &[
                "-p".to_string(),
                record.pid.to_string(),
                "-o".to_string(),
                "args=".to_string(),
            ],
        )
        .await;

    match ps_res {
        Err(e) => ReapOutcome::Failed {
            pid: record.pid,
            reason: format!("failed to inspect process with ps: {e}"),
        },
        Ok(out) if !out.success || out.stdout.trim().is_empty() => {
            // Process does not exist (already exited). Stale record cleaned up.
            let _ = remove_tunnel_record(paths, &record.profile_id);
            ReapOutcome::AlreadyDead { pid: record.pid }
        }
        Ok(out) => {
            let cmdline = out.stdout.trim();
            if is_matching_tunnel_cmdline(cmdline, record) {
                // Verified ClusterDeck SSH child. Safe to kill.
                let kill_res = runner
                    .run("kill", &["-9".to_string(), record.pid.to_string()])
                    .await;
                let _ = remove_tunnel_record(paths, &record.profile_id);
                match kill_res {
                    Ok(k) if k.success => ReapOutcome::Killed { pid: record.pid },
                    Ok(k) => ReapOutcome::Failed {
                        pid: record.pid,
                        reason: format!("kill exited non-zero: {}", k.stderr),
                    },
                    Err(e) => ReapOutcome::Failed {
                        pid: record.pid,
                        reason: format!("kill failed: {e}"),
                    },
                }
            } else {
                // PID recycled by OS! Spared from termination.
                let _ = remove_tunnel_record(paths, &record.profile_id);
                ReapOutcome::PidRecycled {
                    pid: record.pid,
                    actual_cmdline: cmdline.to_string(),
                }
            }
        }
    }
}

pub async fn reap_profile_orphan(
    runner: &dyn CommandRunner,
    paths: &ClusterDeckPaths,
    profile_id: &str,
) -> Option<ReapOutcome> {
    let record = read_tunnel_record(paths, profile_id).ok()??;
    Some(reap_single_orphan(runner, paths, &record).await)
}

pub async fn reap_orphans(runner: &dyn CommandRunner, paths: &ClusterDeckPaths) -> Vec<ReapResult> {
    let records = match list_tunnel_records(paths) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("[ClusterDeck] failed to list tunnel records for reaping: {e}");
            return Vec::new();
        }
    };

    let mut results = Vec::new();
    for record in records {
        let outcome = reap_single_orphan(runner, paths, &record).await;
        results.push(ReapResult {
            profile_id: record.profile_id.clone(),
            outcome,
        });
    }
    results
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::config::{
        AuthMode, Bastion, BootstrapPolicy, Host, KubeconfigSource, Profile,
    };
    use crate::services::process::CommandOutput;
    use std::collections::HashSet;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::Mutex as StdMutex;

    struct FakeProbe {
        busy: StdMutex<HashSet<u16>>,
        /// Ports that never accept connections; every other port is open.
        closed: StdMutex<HashSet<u16>>,
        open: AtomicBool,
    }

    impl FakeProbe {
        fn new(open: bool) -> Arc<Self> {
            Arc::new(Self {
                busy: StdMutex::new(HashSet::new()),
                closed: StdMutex::new(HashSet::new()),
                open: AtomicBool::new(open),
            })
        }
    }

    #[async_trait]
    impl PortProbe for FakeProbe {
        fn is_bindable(&self, port: u16) -> bool {
            !self.busy.lock().unwrap().contains(&port)
        }
        async fn is_open(&self, port: u16) -> bool {
            self.open.load(Ordering::SeqCst) && !self.closed.lock().unwrap().contains(&port)
        }
    }

    /// Exits with `code` once `try_exit_status` has been called `exit_after` times
    /// (`Some(0)` = already exited); never exits when `None`.
    #[derive(Clone)]
    struct Script {
        exit_after: Option<usize>,
        code: i32,
        stderr: String,
        pid: Option<u32>,
    }

    impl Script {
        fn alive() -> Self {
            Self {
                exit_after: None,
                code: 0,
                stderr: String::new(),
                pid: Some(4242),
            }
        }
        fn exits(after: usize, code: i32, stderr: &str) -> Self {
            Self {
                exit_after: Some(after),
                code,
                stderr: stderr.to_string(),
                pid: Some(4242),
            }
        }
    }

    struct FakeProcess {
        script: Script,
        polls: usize,
    }

    #[async_trait]
    impl BackgroundProcess for FakeProcess {
        fn pid(&self) -> Option<u32> {
            self.script.pid
        }
        fn try_exit_status(&mut self) -> Option<i32> {
            if self.script.exit_after.is_some_and(|n| self.polls >= n) {
                return Some(self.script.code);
            }
            self.polls += 1;
            None
        }
        async fn kill(&mut self) {
            self.script.exit_after = Some(0);
            self.script.code = -1;
        }
        async fn stderr_tail(&mut self) -> String {
            self.script.stderr.clone()
        }
    }

    /// Hands out one `Script` per spawn; the last one repeats.
    struct FakeRunner {
        spawns: AtomicUsize,
        argv: StdMutex<Vec<Vec<String>>>,
        scripts: Vec<Script>,
        /// Stdout of `lsof`; `None` makes lsof unavailable.
        lsof: Option<String>,
        killed: StdMutex<Vec<String>>,
        ps_map: StdMutex<HashMap<u32, CommandOutput>>,
    }

    impl FakeRunner {
        fn new(scripts: Vec<Script>) -> Self {
            Self {
                spawns: AtomicUsize::new(0),
                argv: StdMutex::new(Vec::new()),
                scripts,
                lsof: None,
                killed: StdMutex::new(Vec::new()),
                ps_map: StdMutex::new(HashMap::new()),
            }
        }
        fn with_lsof(mut self, stdout: &str) -> Self {
            self.lsof = Some(stdout.to_string());
            self
        }
        fn with_ps(self, pid: u32, stdout: &str, success: bool) -> Self {
            self.ps_map.lock().unwrap().insert(
                pid,
                CommandOutput {
                    stdout: stdout.to_string(),
                    stderr: if success {
                        String::new()
                    } else {
                        "No such process".to_string()
                    },
                    success,
                },
            );
            self
        }
    }

    #[async_trait]
    impl CommandRunner for FakeRunner {
        async fn run(&self, bin: &str, args: &[String]) -> Result<CommandOutput, String> {
            match bin {
                "lsof" => match &self.lsof {
                    Some(stdout) => Ok(CommandOutput {
                        stdout: stdout.clone(),
                        stderr: String::new(),
                        success: !stdout.is_empty(),
                    }),
                    None => Err("'lsof' executable not found".to_string()),
                },
                "kill" => {
                    self.killed.lock().unwrap().push(args.join(" "));
                    Ok(CommandOutput {
                        stdout: String::new(),
                        stderr: String::new(),
                        success: true,
                    })
                }
                "ps" => {
                    let pid: u32 = args
                        .windows(2)
                        .find(|w| w[0] == "-p")
                        .and_then(|w| w[1].parse().ok())
                        .unwrap_or(0);
                    if let Some(out) = self.ps_map.lock().unwrap().get(&pid) {
                        Ok(out.clone())
                    } else {
                        Ok(CommandOutput {
                            stdout: String::new(),
                            stderr: "process not found".to_string(),
                            success: false,
                        })
                    }
                }
                _ => Err(format!("unexpected run {bin}")),
            }
        }
        async fn spawn_background(
            &self,
            _bin: &str,
            args: &[String],
        ) -> Result<Box<dyn BackgroundProcess>, String> {
            let n = self.spawns.fetch_add(1, Ordering::SeqCst);
            self.argv.lock().unwrap().push(args.to_vec());
            let script = self.scripts[n.min(self.scripts.len() - 1)].clone();
            Ok(Box::new(FakeProcess { script, polls: 0 }))
        }
    }

    fn profile() -> Profile {
        Profile {
            id: "lab".to_string(),
            name: "Lab".to_string(),
            hosts: vec![Host {
                name: "cp1".to_string(),
                address: "10.0.0.10".to_string(),
                port: 22,
                user: "root".to_string(),
                identity_file: None,
                auth: AuthMode::Key,
            }],
            bastion: Some(Bastion {
                name: "bastion".to_string(),
                address: "203.0.113.5".to_string(),
                port: 22,
                user: "ops".to_string(),
                identity_file: None,
            }),
            bootstrap: BootstrapPolicy::default(),
            kubeconfig: Some(KubeconfigSource {
                remote_path: "/etc/rancher/k3s/k3s.yaml".to_string(),
                control_plane: "cp1".to_string(),
                local_path: String::new(),
                context: "lab".to_string(),
            }),
            host_mapping: crate::services::config::HostMapping::Off,
            approved_dns_zones: Vec::new(),
            gateway_address: None,
            trusted_cas: Vec::new(),
            local_runtime: None,
        }
    }

    fn temp_paths(tag: &str) -> ClusterDeckPaths {
        let dir = std::env::temp_dir().join(format!(
            "clusterdeck-tunnel-test-{tag}-{}",
            std::process::id()
        ));
        std::fs::remove_dir_all(&dir).ok();
        ClusterDeckPaths::at(dir)
    }

    #[test]
    fn preferred_port_is_deterministic_and_in_range() {
        assert_eq!(preferred_port("lab"), preferred_port("lab"));
        // Pins the hash: a change here moves every existing profile's port.
        assert_eq!(fnv1a_32(b"a"), 0xe40c_292c);
        for id in ["lab", "cka", "colima-default", "x", "a_b-c"] {
            let p = preferred_port(id);
            assert!((16443..17443).contains(&p), "{id} -> {p}");
        }
    }

    #[test]
    fn choose_port_skips_a_port_bound_by_a_real_listener() {
        let real = SystemPortProbe;
        let preferred = preferred_port("lab");
        // Hold the preferred port for real; skip the test body only if another process
        // already owns it (then it is occupied anyway and the assertion still holds).
        let _held = std::net::TcpListener::bind(("127.0.0.1", preferred));
        let chosen = choose_port("lab", &real, &[]).unwrap();
        assert_ne!(chosen, preferred);
    }

    #[test]
    fn choose_port_wraps_inside_the_range() {
        let probe = FakeProbe::new(true);
        let preferred = preferred_port("lab");
        {
            let mut busy = probe.busy.lock().unwrap();
            for i in 0..PORT_RANGE_LEN - 1 {
                busy.insert(PORT_RANGE_START + (preferred - PORT_RANGE_START + i) % PORT_RANGE_LEN);
            }
        }
        let last =
            PORT_RANGE_START + (preferred - PORT_RANGE_START + PORT_RANGE_LEN - 1) % PORT_RANGE_LEN;
        assert_eq!(choose_port("lab", probe.as_ref(), &[]).unwrap(), last);
        probe.busy.lock().unwrap().insert(last);
        assert!(choose_port("lab", probe.as_ref(), &[]).is_err());
    }

    #[test]
    fn argv_has_forward_options_and_double_dash_before_alias() {
        let target = TunnelTarget {
            host: "127.0.0.1".into(),
            port: 6443,
        };
        let argv = tunnel_argv(
            std::path::Path::new("/h/ssh/lab.conf"),
            16500,
            &target,
            "lab-cp1",
        )
        .unwrap();
        let joined = argv.join(" ");
        assert!(joined.starts_with("-F /h/ssh/lab.conf "));
        for want in [
            "-N",
            "ExitOnForwardFailure=yes",
            "BatchMode=yes",
            "ServerAliveInterval=15",
        ] {
            assert!(argv.iter().any(|a| a == want), "missing {want}: {joined}");
        }
        assert!(joined.contains("-L 127.0.0.1:16500:127.0.0.1:6443"));
        assert!(joined.ends_with("-- lab-cp1"));
    }

    #[test]
    fn argv_brackets_ipv6_destination() {
        let target = TunnelTarget {
            host: "fd00::10".into(),
            port: 6443,
        };
        let argv = tunnel_argv(std::path::Path::new("/c"), 16500, &target, "lab-cp1").unwrap();
        assert!(argv.contains(&"127.0.0.1:16500:[fd00::10]:6443".to_string()));
    }

    #[tokio::test]
    async fn unsafe_host_is_rejected_without_calling_the_runner() {
        let runner = FakeRunner::new(vec![Script::alive()]);
        let manager = TunnelManager::new(FakeProbe::new(true));
        for bad in ["a b", "a/b", "-oProxyCommand=x", "a:b", "x\ny", ""] {
            let err = manager
                .ensure_tunnel(&runner, &temp_paths("bad"), &profile(), bad, 6443)
                .await
                .unwrap_err();
            assert!(err.contains("unsafe"), "{bad:?}: {err}");
        }
        assert_eq!(runner.spawns.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn validate_target_host_rejects_leading_dash() {
        assert!(validate_target_host("-host").is_err());
        assert!(validate_target_host("-oProxyCommand=x").is_err());
        assert!(validate_target_host("10.0.0.10").is_ok());
        assert!(validate_target_host("fd00::10").is_ok());
    }

    #[tokio::test(start_paused = true)]
    async fn exit_before_ready_returns_stderr() {
        let runner = FakeRunner::new(vec![Script::exits(0, 255, "Permission denied")]);
        let manager = TunnelManager::new(FakeProbe::new(false));
        let err = manager
            .ensure_tunnel(&runner, &temp_paths("exit"), &profile(), "127.0.0.1", 6443)
            .await
            .unwrap_err();
        assert!(
            err.contains("255") && err.contains("Permission denied"),
            "{err}"
        );
        assert_eq!(
            runner.spawns.load(Ordering::SeqCst),
            1,
            "no retry for non-bind errors"
        );
        assert!(manager.live_target("lab").await.is_none());
    }

    #[tokio::test(start_paused = true)]
    async fn never_ready_times_out_and_kills() {
        let runner = FakeRunner::new(vec![Script::alive()]);
        let manager = TunnelManager::new(FakeProbe::new(false));
        let err = manager
            .ensure_tunnel(
                &runner,
                &temp_paths("timeout"),
                &profile(),
                "127.0.0.1",
                6443,
            )
            .await
            .unwrap_err();
        assert!(err.contains("not ready"), "{err}");
    }

    #[tokio::test(start_paused = true)]
    async fn foreign_listener_with_dying_child_is_never_reported_ready() {
        // The probe says "open" the whole time (a foreign listener), but ssh dies on
        // ExitOnForwardFailure right after the readiness probes would have passed.
        let runner = FakeRunner::new(vec![Script::exits(2, 255, "Permission denied")]);
        let manager = TunnelManager::new(FakeProbe::new(true));
        let res = manager
            .ensure_tunnel(
                &runner,
                &temp_paths("foreign"),
                &profile(),
                "127.0.0.1",
                6443,
            )
            .await;
        assert!(res.is_err(), "must not point at a foreign port: {res:?}");
        assert!(manager.live_target("lab").await.is_none());
    }

    #[tokio::test(start_paused = true)]
    async fn bind_failure_is_retried_once_on_the_next_port() {
        let runner = FakeRunner::new(vec![
            Script::exits(0, 255, "bind [127.0.0.1]:1: Address already in use"),
            Script::alive(),
        ]);
        let manager = TunnelManager::new(FakeProbe::new(true));
        let port = manager
            .ensure_tunnel(&runner, &temp_paths("retry"), &profile(), "127.0.0.1", 6443)
            .await
            .unwrap();
        assert_eq!(runner.spawns.load(Ordering::SeqCst), 2);
        let first = preferred_port("lab");
        let expected = PORT_RANGE_START + (first - PORT_RANGE_START + 1) % PORT_RANGE_LEN;
        assert_eq!(port, expected);
        let argv = runner.argv.lock().unwrap();
        assert!(argv[1]
            .join(" ")
            .contains(&format!("-L 127.0.0.1:{expected}:")));
    }

    #[tokio::test(start_paused = true)]
    async fn bind_failure_is_retried_only_once() {
        let runner = FakeRunner::new(vec![Script::exits(0, 255, "cannot listen to port: 1")]);
        let manager = TunnelManager::new(FakeProbe::new(true));
        let err = manager
            .ensure_tunnel(
                &runner,
                &temp_paths("retry2"),
                &profile(),
                "127.0.0.1",
                6443,
            )
            .await
            .unwrap_err();
        assert!(err.contains("cannot listen"), "{err}");
        assert_eq!(runner.spawns.load(Ordering::SeqCst), 2);
    }

    #[tokio::test(start_paused = true)]
    async fn live_entry_with_same_target_is_reused() {
        let runner = FakeRunner::new(vec![Script::alive()]);
        let manager = TunnelManager::new(FakeProbe::new(true));
        let paths = temp_paths("reuse");
        let first = manager
            .ensure_tunnel(&runner, &paths, &profile(), "127.0.0.1", 6443)
            .await
            .unwrap();
        let second = manager
            .ensure_tunnel(&runner, &paths, &profile(), "127.0.0.1", 6443)
            .await
            .unwrap();
        assert_eq!(first, second);
        assert_eq!(runner.spawns.load(Ordering::SeqCst), 1);
        let persisted = crate::services::state::get_tunnel_target(&paths, "lab").unwrap();
        assert_eq!(persisted.unwrap().port, 6443);

        // A different target replaces the tunnel instead of reusing it.
        manager
            .ensure_tunnel(&runner, &paths, &profile(), "10.0.0.10", 6443)
            .await
            .unwrap();
        assert_eq!(runner.spawns.load(Ordering::SeqCst), 2);
        manager.stop_all(&runner).await;
        assert!(manager.live_target("lab").await.is_none());
        std::fs::remove_dir_all(&paths.base).ok();
    }

    #[tokio::test(start_paused = true)]
    async fn a_stuck_profile_does_not_block_other_profiles_or_stop_all() {
        let mut other = profile();
        other.id = "other".to_string();
        assert_ne!(preferred_port("lab"), preferred_port("other"));

        let probe = FakeProbe::new(true);
        probe.closed.lock().unwrap().insert(preferred_port("lab"));
        let manager = TunnelManager::new(probe);
        let runner = FakeRunner::new(vec![Script::alive()]);
        let paths = temp_paths("isolation");

        let lab = profile();
        let stuck = manager.ensure_tunnel(&runner, &paths, &lab, "127.0.0.1", 6443);
        let others = async {
            let port = manager
                .ensure_tunnel(&runner, &paths, &other, "127.0.0.1", 6443)
                .await
                .unwrap();
            assert_eq!(port, preferred_port("other"));
            // Shutdown while "lab" is mid-readiness must return, not wait ~10s.
            manager.stop_all(&runner).await;
            assert!(manager.live_target("other").await.is_none());
            manager.stop_tunnel("other").await;
        };
        tokio::select! {
            res = stuck => panic!("stuck profile finished first: {res:?}"),
            () = others => {}
        }
        std::fs::remove_dir_all(&paths.base).ok();
    }

    #[tokio::test(start_paused = true)]
    async fn stop_all_kills_the_child_of_an_in_flight_ensure() {
        let probe = FakeProbe::new(true);
        probe.closed.lock().unwrap().insert(preferred_port("lab"));
        let manager = TunnelManager::new(probe);
        let runner = FakeRunner::new(vec![Script::alive()]);
        let paths = temp_paths("inflight");
        let lab = profile();
        let stuck = manager.ensure_tunnel(&runner, &paths, &lab, "127.0.0.1", 6443);
        let quit = async {
            tokio::time::sleep(Duration::from_secs(1)).await;
            manager.stop_all(&runner).await;
        };
        tokio::select! {
            res = stuck => panic!("stuck profile finished first: {res:?}"),
            () = quit => {}
        }
        assert_eq!(*runner.killed.lock().unwrap(), vec!["-9 4242".to_string()]);
        std::fs::remove_dir_all(&paths.base).ok();
    }

    #[tokio::test(start_paused = true)]
    async fn foreign_lsof_owner_is_never_adopted() {
        let runner = FakeRunner::new(vec![Script::alive()]).with_lsof("9999\n");
        let manager = TunnelManager::new(FakeProbe::new(true));
        let res = manager
            .ensure_tunnel(
                &runner,
                &temp_paths("lsof-foreign"),
                &profile(),
                "127.0.0.1",
                6443,
            )
            .await;
        assert!(res.unwrap_err().contains("not ready"));
        assert!(manager.live_target("lab").await.is_none());
    }

    #[tokio::test(start_paused = true)]
    async fn lsof_confirming_our_pid_makes_the_tunnel_ready() {
        let runner = FakeRunner::new(vec![Script::alive()]).with_lsof("1\n4242\n");
        let manager = TunnelManager::new(FakeProbe::new(true));
        let port = manager
            .ensure_tunnel(
                &runner,
                &temp_paths("lsof-own"),
                &profile(),
                "127.0.0.1",
                6443,
            )
            .await
            .unwrap();
        assert_eq!(port, preferred_port("lab"));
    }

    #[tokio::test(start_paused = true)]
    async fn foreign_listener_after_bind_check_then_ssh_bind_error_retries_next_port() {
        // The probe reports the port open (foreign listener) while ssh is still authenticating;
        // ssh then fails to bind and exits.
        let runner = FakeRunner::new(vec![
            Script::exits(1, 255, "bind: Address already in use"),
            Script::alive(),
        ]);
        let manager = TunnelManager::new(FakeProbe::new(true));
        let port = manager
            .ensure_tunnel(
                &runner,
                &temp_paths("late-foreign"),
                &profile(),
                "127.0.0.1",
                6443,
            )
            .await
            .unwrap();
        let first = preferred_port("lab");
        assert_eq!(
            port,
            PORT_RANGE_START + (first - PORT_RANGE_START + 1) % PORT_RANGE_LEN
        );
        assert_eq!(runner.spawns.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn reap_orphans_kills_verified_ssh_process() {
        let paths = temp_paths("reap-kill");
        let prof = profile();
        let port = preferred_port(&prof.id);
        let pid = 4242;
        let ssh_conf = paths.ssh_conf(&prof.id);
        let alias = "lab-cp1";
        let target = TunnelTarget {
            host: "10.0.0.10".into(),
            port: 6443,
        };
        let record = TunnelRecord {
            profile_id: prof.id.clone(),
            pid,
            port,
            target: target.clone(),
            ssh_conf: ssh_conf.clone(),
            alias: alias.into(),
            started_at_epoch_s: 1700000000,
        };
        write_tunnel_record(&paths, &record).unwrap();
        assert!(read_tunnel_record(&paths, &prof.id).unwrap().is_some());

        let cmdline = format!(
            "ssh -F {} -o BatchMode=yes -o ConnectTimeout=5 -o ExitOnForwardFailure=yes -o ServerAliveInterval=15 -o ServerAliveCountMax=3 -N -L 127.0.0.1:{port}:10.0.0.10:6443 -- {alias}",
            ssh_conf.display()
        );
        let runner = FakeRunner::new(vec![]).with_ps(pid, &cmdline, true);

        let reaped = reap_orphans(&runner, &paths).await;
        assert_eq!(reaped.len(), 1);
        assert_eq!(reaped[0].profile_id, "lab");
        assert_eq!(reaped[0].outcome, ReapOutcome::Killed { pid });
        assert_eq!(*runner.killed.lock().unwrap(), vec![format!("-9 {pid}")]);
        assert!(read_tunnel_record(&paths, &prof.id).unwrap().is_none());
        std::fs::remove_dir_all(&paths.base).ok();
    }

    #[tokio::test]
    async fn reap_orphans_spares_recycled_pid_process() {
        let paths = temp_paths("reap-recycle");
        let prof = profile();
        let port = preferred_port(&prof.id);
        let pid = 4242;
        let ssh_conf = paths.ssh_conf(&prof.id);
        let record = TunnelRecord {
            profile_id: prof.id.clone(),
            pid,
            port,
            target: TunnelTarget {
                host: "10.0.0.10".into(),
                port: 6443,
            },
            ssh_conf,
            alias: "lab-cp1".into(),
            started_at_epoch_s: 1700000000,
        };
        write_tunnel_record(&paths, &record).unwrap();

        // PID 4242 was recycled by macOS for another innocent process (e.g. Google Chrome or personal SSH)
        let innocent_cmdline =
            "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome --type=renderer";
        let runner = FakeRunner::new(vec![]).with_ps(pid, innocent_cmdline, true);

        let reaped = reap_orphans(&runner, &paths).await;
        assert_eq!(reaped.len(), 1);
        assert_eq!(reaped[0].profile_id, "lab");
        assert!(matches!(
            reaped[0].outcome,
            ReapOutcome::PidRecycled { pid: 4242, .. }
        ));
        // CRITICAL: The innocent process must NOT have been killed!
        assert!(runner.killed.lock().unwrap().is_empty());
        // Stale record file must have been cleaned up
        assert!(read_tunnel_record(&paths, &prof.id).unwrap().is_none());
        std::fs::remove_dir_all(&paths.base).ok();
    }

    #[tokio::test]
    async fn reap_orphans_cleans_up_already_dead_process() {
        let paths = temp_paths("reap-dead");
        let prof = profile();
        let pid = 4242;
        let record = TunnelRecord {
            profile_id: prof.id.clone(),
            pid,
            port: preferred_port(&prof.id),
            target: TunnelTarget {
                host: "10.0.0.10".into(),
                port: 6443,
            },
            ssh_conf: paths.ssh_conf(&prof.id),
            alias: "lab-cp1".into(),
            started_at_epoch_s: 1700000000,
        };
        write_tunnel_record(&paths, &record).unwrap();

        // ps exits with error/not found
        let runner = FakeRunner::new(vec![]).with_ps(pid, "", false);

        let reaped = reap_orphans(&runner, &paths).await;
        assert_eq!(reaped.len(), 1);
        assert_eq!(reaped[0].profile_id, "lab");
        assert_eq!(reaped[0].outcome, ReapOutcome::AlreadyDead { pid });
        assert!(runner.killed.lock().unwrap().is_empty());
        assert!(read_tunnel_record(&paths, &prof.id).unwrap().is_none());
        std::fs::remove_dir_all(&paths.base).ok();
    }

    #[tokio::test(start_paused = true)]
    async fn ensure_tunnel_reaps_orphan_and_reclaims_preferred_port() {
        let paths = temp_paths("reclaim-preferred");
        let prof = profile();
        let preferred = preferred_port(&prof.id);
        let orphan_pid = 7777;
        let ssh_conf = paths.ssh_conf(&prof.id);
        let alias = "lab-cp1";

        // Seed an orphan record from a previous crash holding the preferred port
        let orphan_record = TunnelRecord {
            profile_id: prof.id.clone(),
            pid: orphan_pid,
            port: preferred,
            target: TunnelTarget {
                host: "10.0.0.10".into(),
                port: 6443,
            },
            ssh_conf: ssh_conf.clone(),
            alias: alias.into(),
            started_at_epoch_s: 1700000000,
        };
        write_tunnel_record(&paths, &orphan_record).unwrap();

        let cmdline = format!(
            "ssh -F {} -o BatchMode=yes -o ConnectTimeout=5 -o ExitOnForwardFailure=yes -o ServerAliveInterval=15 -o ServerAliveCountMax=3 -N -L 127.0.0.1:{preferred}:10.0.0.10:6443 -- {alias}",
            ssh_conf.display()
        );
        let mut script = Script::alive();
        script.pid = Some(8888);
        let runner = FakeRunner::new(vec![script]).with_ps(orphan_pid, &cmdline, true);
        let manager = TunnelManager::new(FakeProbe::new(true));

        let port = manager
            .ensure_tunnel(&runner, &paths, &prof, "10.0.0.10", 6443)
            .await
            .unwrap();

        // 1. The orphan was reaped and killed before selecting the port
        assert_eq!(
            *runner.killed.lock().unwrap(),
            vec![format!("-9 {orphan_pid}")]
        );
        // 2. Preferred port was successfully reclaimed
        assert_eq!(port, preferred);
        // 3. New record written for the active tunnel
        let current_rec = read_tunnel_record(&paths, &prof.id).unwrap().unwrap();
        assert_eq!(current_rec.pid, 8888);
        assert_eq!(current_rec.port, preferred);

        std::fs::remove_dir_all(&paths.base).ok();
    }

    #[tokio::test]
    async fn crash_during_readiness_leaves_record_for_startup_reap() {
        let paths = temp_paths("crash-readiness");
        let prof = profile();
        let port = preferred_port(&prof.id);
        let pid = 9999;
        let ssh_conf = paths.ssh_conf(&prof.id);

        // Simulate crash: an in-flight tunnel was spawned, wrote record, then app died
        let record = TunnelRecord {
            profile_id: prof.id.clone(),
            pid,
            port,
            target: TunnelTarget {
                host: "10.0.0.10".into(),
                port: 6443,
            },
            ssh_conf: ssh_conf.clone(),
            alias: "lab-cp1".into(),
            started_at_epoch_s: 1700000000,
        };
        write_tunnel_record(&paths, &record).unwrap();

        let cmdline = format!(
            "/usr/bin/ssh -F {} -N -L 127.0.0.1:{port}:10.0.0.10:6443 -- lab-cp1",
            ssh_conf.display()
        );
        let runner = FakeRunner::new(vec![]).with_ps(pid, &cmdline, true);

        // Next app startup reaps the crashed child
        let reaped = reap_orphans(&runner, &paths).await;
        assert_eq!(reaped.len(), 1);
        assert_eq!(reaped[0].outcome, ReapOutcome::Killed { pid });
        assert_eq!(*runner.killed.lock().unwrap(), vec![format!("-9 {pid}")]);
        assert!(read_tunnel_record(&paths, &prof.id).unwrap().is_none());

        std::fs::remove_dir_all(&paths.base).ok();
    }

    #[test]
    fn is_matching_tunnel_cmdline_edge_cases() {
        let record = TunnelRecord {
            profile_id: "lab".into(),
            pid: 1234,
            port: 16443,
            target: TunnelTarget {
                host: "10.0.0.10".into(),
                port: 6443,
            },
            ssh_conf: PathBuf::from("/Users/test/.clusterdeck/ssh/lab.conf"),
            alias: "lab-cp1".into(),
            started_at_epoch_s: 1700000000,
        };

        // 1. Valid exact line -> true
        let valid = "ssh -F /Users/test/.clusterdeck/ssh/lab.conf -o BatchMode=yes -N -L 127.0.0.1:16443:10.0.0.10:6443 -- lab-cp1";
        assert!(is_matching_tunnel_cmdline(valid, &record));

        // 2. /usr/bin/ssh path -> true
        let valid_path = "/usr/bin/ssh -F /Users/test/.clusterdeck/ssh/lab.conf -N -L 127.0.0.1:16443:10.0.0.10:6443 -- lab-cp1";
        assert!(is_matching_tunnel_cmdline(valid_path, &record));

        // 3. Multiline with ps header -> true
        let multiline = "COMMAND\nssh -F /Users/test/.clusterdeck/ssh/lab.conf -N -L 127.0.0.1:16443:10.0.0.10:6443 -- lab-cp1\n";
        assert!(is_matching_tunnel_cmdline(multiline, &record));

        // 4. Non-ssh binary -> false
        let python = "python3 -F /Users/test/.clusterdeck/ssh/lab.conf -N -L 127.0.0.1:16443:10.0.0.10:6443 -- lab-cp1";
        assert!(!is_matching_tunnel_cmdline(python, &record));

        // 5. Missing -N flag -> false
        let missing_n = "ssh -F /Users/test/.clusterdeck/ssh/lab.conf -L 127.0.0.1:16443:10.0.0.10:6443 -- lab-cp1";
        assert!(!is_matching_tunnel_cmdline(missing_n, &record));

        // 6. Mismatched config file -> false
        let wrong_conf = "ssh -F /Users/test/.clusterdeck/ssh/other.conf -N -L 127.0.0.1:16443:10.0.0.10:6443 -- lab-cp1";
        assert!(!is_matching_tunnel_cmdline(wrong_conf, &record));

        // 7. Mismatched port -> false
        let wrong_port = "ssh -F /Users/test/.clusterdeck/ssh/lab.conf -N -L 127.0.0.1:16444:10.0.0.10:6443 -- lab-cp1";
        assert!(!is_matching_tunnel_cmdline(wrong_port, &record));

        // 8. Mismatched alias -> false
        let wrong_alias = "ssh -F /Users/test/.clusterdeck/ssh/lab.conf -N -L 127.0.0.1:16443:10.0.0.10:6443 -- other-cp1";
        assert!(!is_matching_tunnel_cmdline(wrong_alias, &record));

        // 9. Empty or whitespace -> false
        assert!(!is_matching_tunnel_cmdline("", &record));
        assert!(!is_matching_tunnel_cmdline("   \n\t  ", &record));
    }
}
