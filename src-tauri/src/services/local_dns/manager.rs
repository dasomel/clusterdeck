#![allow(dead_code)]

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;
use tokio::sync::Mutex;

use crate::services::local_dns::agent::{self, AgentPaths};
use crate::services::local_dns::zone_file::{
    load_status, write_zones, StatusFile, ZoneEntry, ZonesFile, ZONES_FILE_VERSION,
};
use crate::services::process::CommandRunner;
use crate::services::resolver_files::{
    build_privileged_script_with, parse_marker, plan_changes_for_port, sha256_hex, Conflict,
    ConflictKind, ResolverPlan, ScriptOptions,
};
use crate::services::validate::is_allowed_private_zone;

pub const STATUS_WAIT: Duration = Duration::from_secs(5);
pub const STATUS_POLL: Duration = Duration::from_millis(100);

static HOSTS_STAGE_SEQ: AtomicU64 = AtomicU64::new(0);

/// R8: `apply`, `teardown` and `startup_sweep` all mutate the same files, the agent and
/// /etc/resolver, so they run one at a time (same pattern as `store::ProfileWriteGuard`).
static MANAGER_MUTEX: Mutex<()> = Mutex::const_new(());

// Design rule for every failure path below: reconcile from what is actually on disk in
// /etc/resolver, never from memory of what we meant to do (`reconcile_entries`).

/// Everything the manager touches, injected so tests run against temp dirs and a fake runner.
pub struct ManagerEnv<'a> {
    pub runner: &'a dyn CommandRunner,
    pub paths: &'a AgentPaths,
    /// `/etc/resolver` in production.
    pub resolver_dir: &'a Path,
    pub current_exe: &'a Path,
    pub port: u16,
    pub status_timeout: Duration,
    pub status_poll: Duration,
}

impl<'a> ManagerEnv<'a> {
    pub fn new(
        runner: &'a dyn CommandRunner,
        paths: &'a AgentPaths,
        resolver_dir: &'a Path,
        current_exe: &'a Path,
        port: u16,
    ) -> Self {
        Self {
            runner,
            paths,
            resolver_dir,
            current_exe,
            port,
            status_timeout: STATUS_WAIT,
            status_poll: STATUS_POLL,
        }
    }
}

/// A pending /etc/hosts change (R11). The manager reads `path` itself immediately before the
/// privileged script, so the new content is always computed from a fresh snapshot (ADR-0004).
pub struct HostsUpdate<'a> {
    /// `/etc/hosts` in production.
    pub path: &'a Path,
    /// Pure: current hosts content -> desired content (return it unchanged for "nothing to do").
    pub compute: &'a (dyn Fn(&str) -> String + Sync),
}

pub struct ApplyRequest<'a> {
    pub zones: ZonesFile,
    /// `(zone, from_owner, to_owner)` as accepted by `plan_changes_for_port`.
    pub approved_switches: &'a [(String, String, String)],
    pub hosts: Option<HostsUpdate<'a>>,
    pub scope_profile: Option<&'a str>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ApplyResult {
    pub generation: u64,
    /// Zones now served by the responder (what is on disk and ours).
    pub published: Vec<String>,
    pub conflicts: Vec<Conflict>,
    pub prompted: bool,
    pub cancelled: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TeardownResult {
    pub removed: Vec<String>,
    pub cancelled: bool,
    pub agent_removed: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SweepResult {
    pub removed: Vec<String>,
    pub cancelled: bool,
    pub skipped_owned: bool,
}

enum ScriptError {
    Cancelled,
    /// Exit 8: /etc/hosts changed after the snapshot. Resolver steps already completed.
    HostsChanged,
    /// Exit 7: the UDP/TCP listener on the port is not the expected agent pid (checked first,
    /// as root, so nothing was changed).
    ListenerMismatch,
    Failed(String),
}

#[derive(Debug, Default)]
pub(crate) struct Listing {
    /// Readable regular files: `(name, content)`, sorted.
    pub files: Vec<(String, String)>,
    /// Names of symlinks, non-regular files and files whose content could not be read.
    pub unreadable: Vec<String>,
}

/// Fails (rather than returning a partial view) when the directory cannot be listed: planning
/// against a guess could remove or overwrite files we cannot see. A missing directory is an
/// empty listing.
pub(crate) fn read_resolver_listing(dir: &Path) -> Result<Listing, String> {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Listing::default()),
        Err(e) => return Err(format!("cannot list {}: {e}", dir.display())),
    };
    let mut listing = Listing::default();
    for entry in entries {
        let entry = entry.map_err(|e| format!("cannot list {}: {e}", dir.display()))?;
        // A non-UTF-8 name can never be a valid zone, so it cannot be ours or desired.
        let Ok(name) = entry.file_name().into_string() else {
            continue;
        };
        let regular = entry
            .file_type()
            .map(|t| t.is_file() && !t.is_symlink())
            .unwrap_or(false);
        match regular
            .then(|| std::fs::read_to_string(entry.path()).ok())
            .flatten()
        {
            Some(content) => listing.files.push((name, content)),
            None => listing.unreadable.push(name),
        }
    }
    listing.files.sort();
    listing.unreadable.sort();
    Ok(listing)
}

fn read_generation_file(path: &Path) -> u64 {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|s| s.trim().parse().ok())
        .unwrap_or(0)
}

fn read_previous_zones(paths: &AgentPaths) -> Option<ZonesFile> {
    let content = std::fs::read_to_string(&paths.zones).ok()?;
    serde_json::from_str(&content).ok()
}

/// R8: max(stored, status.generation, zones.json.generation) + 1, so the generation never goes
/// backwards even if one of the files was deleted or rolled back.
fn next_generation(paths: &AgentPaths) -> u64 {
    let stored = read_generation_file(&paths.generation);
    let status = load_status(&paths.status)
        .map(|s| s.generation)
        .unwrap_or(0);
    let zones = read_previous_zones(paths)
        .map(|z| z.generation)
        .unwrap_or(0);
    stored.max(status).max(zones) + 1
}

/// Persists the generation BEFORE publishing zones.json: a crash in between wastes a number
/// instead of ever reusing one.
fn publish(env: &ManagerEnv<'_>, entries: Vec<ZoneEntry>, generation: u64) -> Result<(), String> {
    agent::atomic_write(
        &env.paths.generation,
        format!("{generation}\n").as_bytes(),
        0o644,
    )?;
    write_zones(
        &env.paths.zones,
        &ZonesFile {
            version: ZONES_FILE_VERSION,
            generation,
            port: env.port,
            zones: entries,
        },
    )
}

/// The zones that SHOULD be served given what is on disk: every zone whose resolver file
/// carries OUR marker for its owner, with the new records when this apply requested that exact
/// (zone, owner), otherwise the records previously served for that (zone, owner). A zone
/// with no file (or a foreign/unknown owner) is not served.
fn reconcile_entries(
    files: &[(String, String)],
    desired: &[ZoneEntry],
    previous: Option<&ZonesFile>,
) -> Vec<ZoneEntry> {
    files
        .iter()
        .filter(|(name, _)| is_allowed_private_zone(name))
        .filter_map(|(name, content)| {
            let owner = parse_marker(content)?;
            let pick = |entries: &[ZoneEntry]| {
                entries
                    .iter()
                    .find(|z| &z.zone == name && z.profile_id == owner)
                    .cloned()
            };
            pick(desired).or_else(|| previous.and_then(|p| pick(&p.zones)))
        })
        .collect()
}

/// Re-reads /etc/resolver and publishes what is really there. Used after every failed,
/// cancelled or partial run. On a listing failure zones.json is left alone (a superset of
/// what is on disk is harmless: serving a zone nothing points at has no effect).
fn reconcile_publish(
    env: &ManagerEnv<'_>,
    desired: &[ZoneEntry],
    previous: &Option<ZonesFile>,
) -> Result<(), String> {
    let listing = read_resolver_listing(env.resolver_dir)?;
    let entries = reconcile_entries(&listing.files, desired, previous.as_ref());
    publish(env, entries, next_generation(env.paths))
}

async fn wait_for_status(env: &ManagerEnv<'_>, generation: u64) -> Result<StatusFile, String> {
    let deadline = tokio::time::Instant::now() + env.status_timeout;
    let mut last_error: Option<String> = None;
    loop {
        if let Ok(status) = load_status(&env.paths.status) {
            if status.generation >= generation {
                match &status.error {
                    None => return Ok(status),
                    Some(e) => last_error = Some(e.clone()),
                }
            }
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(match last_error {
                Some(e) => format!("DNS responder rejected generation {generation}: {e}"),
                None => format!("DNS responder did not load generation {generation} in time"),
            });
        }
        tokio::time::sleep(env.status_poll).await;
    }
}

async fn ensure_agent(env: &ManagerEnv<'_>, uid: u32) -> Result<(), String> {
    let bin_changed = agent::ensure_binary(env.current_exe, &env.paths.binary)?;
    let plist_changed = agent::ensure_plist(env.paths, env.port)?;
    let running = agent::agent_pid(env.runner, uid).await?.is_some();
    // Not running covers both never-loaded and loaded-but-dead; `reload` handles either.
    if !running || bin_changed || plist_changed {
        agent::reload(env.runner, env.paths, uid).await?;
    }
    Ok(())
}

/// Hosts content staged for one privileged copy. The temp file is removed on drop.
struct StagedHosts {
    tmp: PathBuf,
    snapshot_sha256: String,
    /// D-MBB: every line of the snapshot survives in the new content (adds only).
    additive: bool,
    /// Hash of the content that was staged; the script verifies its own copy against it.
    content_sha256: String,
}

/// Multiset comparison of lines: true when `new` keeps every line of `old`. A mode switch
/// either adds this profile's block or removes it, never both, so this is the classification
/// the script ordering needs.
fn hosts_is_additive(old: &str, new: &str) -> bool {
    let mut counts: std::collections::HashMap<&str, i64> = std::collections::HashMap::new();
    for l in new.lines() {
        *counts.entry(l).or_default() += 1;
    }
    old.lines().all(|l| {
        let c = counts.entry(l).or_default();
        *c -= 1;
        *c >= 0
    })
}

impl Drop for StagedHosts {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.tmp);
    }
}

/// Fresh read + compute right before the script is built (ADR-0004 snapshot discipline).
/// `None` when the hosts file already has the desired content.
fn stage_hosts(update: &HostsUpdate<'_>) -> Result<Option<StagedHosts>, String> {
    let snapshot = std::fs::read_to_string(update.path).unwrap_or_default();
    let new_content = (update.compute)(&snapshot);
    if new_content == snapshot {
        return Ok(None);
    }
    let seq = HOSTS_STAGE_SEQ.fetch_add(1, Ordering::Relaxed);
    let tmp = std::env::temp_dir().join(format!(
        "clusterdeck-hosts-{}-{seq}.tmp",
        std::process::id()
    ));
    std::fs::write(&tmp, &new_content)
        .map_err(|e| format!("failed to write temporary hosts file: {e}"))?;
    Ok(Some(StagedHosts {
        tmp,
        snapshot_sha256: sha256_hex(&snapshot),
        additive: hosts_is_additive(&snapshot, &new_content),
        content_sha256: sha256_hex(&new_content),
    }))
}

async fn run_script(
    env: &ManagerEnv<'_>,
    plan: &ResolverPlan,
    expected_pid: Option<u32>,
    hosts: Option<&StagedHosts>,
) -> Result<(), ScriptError> {
    let resolver_dir = env
        .resolver_dir
        .to_str()
        .ok_or_else(|| ScriptError::Failed("resolver dir is not valid UTF-8".to_string()))?;
    let script = build_privileged_script_with(
        resolver_dir,
        plan,
        env.port,
        &ScriptOptions {
            expected_pid,
            hosts_tmp: hosts.map(|h| h.tmp.as_path()),
            hosts_sha256: hosts.map(|h| h.snapshot_sha256.as_str()),
            hosts_additive: hosts.is_some_and(|h| h.additive),
            hosts_content_sha256: hosts.map(|h| h.content_sha256.clone()),
        },
    )
    .map_err(ScriptError::Failed)?;
    let out = env
        .runner
        .run("osascript", &["-e".to_string(), script])
        .await
        .map_err(ScriptError::Failed)?;
    if out.success {
        return Ok(());
    }
    // AppleScript error -128 is "User canceled".
    if out.stderr.contains("User canceled") || out.stderr.contains("(-128)") {
        return Err(ScriptError::Cancelled);
    }
    if expected_pid.is_some() && out.stderr.trim_end().ends_with("(7)") {
        return Err(ScriptError::ListenerMismatch);
    }
    if hosts.is_some() && out.stderr.trim_end().ends_with("(8)") {
        return Err(ScriptError::HostsChanged);
    }
    Err(ScriptError::Failed(if out.stderr.is_empty() {
        "privileged script failed".to_string()
    } else {
        out.stderr
    }))
}

const HOSTS_CHANGED_MSG: &str = "hosts file changed concurrently; press Sync again";

/// Runs the resolver plan plus the optional hosts step in ONE privileged call.
///
/// D-ONEPROMPT: one user action is at most one prompt, so there is no automatic retry. On a
/// hosts snapshot mismatch (exit 8) the script has already applied the steps that precede the
/// hosts step (D-MBB order) and skipped the rest; the caller reconciles from disk and the user
/// presses Sync again.
async fn run_plan_and_hosts(
    env: &ManagerEnv<'_>,
    plan: &ResolverPlan,
    expected_pid: Option<u32>,
    hosts: Option<&HostsUpdate<'_>>,
) -> Result<(), ScriptError> {
    let staged = hosts
        .map(stage_hosts)
        .transpose()
        .map_err(ScriptError::Failed)?
        .flatten();
    let plan_needed = !plan.write.is_empty() || !plan.remove.is_empty();
    if !plan_needed && staged.is_none() {
        return Ok(());
    }
    run_script(env, plan, expected_pid, staged.as_ref()).await
}

/// D-DEAD, the single exit for EVERY "responder unavailable / not the owner" detection at any
/// stage of `apply` (agent start, status timeout, ownership pre-check, script exit 7, the g2
/// status wait, the post-script ownership re-check): nothing may keep pointing at a dead or
/// squatted port, so remove OUR resolver files in one privileged call and return the message to
/// report. A cancelled removal leaves the state as-is, and says so.
///
/// D-DEAD-2: when the main script already ran in this action (exit 7 aside, which runs before
/// any step; or a g2/ownership failure after the writes), this is a SECOND prompt. That is
/// accepted for failure recovery only; the normal path never exceeds one prompt (D-ONEPROMPT).
/// Callers must `return Err(on_responder_unavailable(..).await)`; no `?` may bypass it.
async fn on_responder_unavailable(
    env: &ManagerEnv<'_>,
    previous: &Option<ZonesFile>,
    cause: &str,
) -> String {
    const HEAD: &str = "local DNS responder unavailable";
    let listing = match read_resolver_listing(env.resolver_dir) {
        Ok(l) => l,
        Err(e) => return format!("{HEAD}; {e} ({cause})"),
    };
    let plan = removal_plan(env, &listing);
    if plan.remove.is_empty() {
        let _ = reconcile_publish(env, &[], previous);
        return format!("{HEAD} ({cause})");
    }
    let outcome = run_script(env, &plan, None, None).await;
    let _ = reconcile_publish(env, &[], previous);
    match outcome {
        Ok(()) => format!("{HEAD}; resolver entries removed ({cause})"),
        Err(ScriptError::Cancelled) => format!(
            "{HEAD}; removal of resolver entries was cancelled and they were left in place ({cause})"
        ),
        Err(ScriptError::Failed(e)) => {
            format!("{HEAD}; removing resolver entries failed: {e} ({cause})")
        }
        Err(ScriptError::HostsChanged | ScriptError::ListenerMismatch) => {
            format!("{HEAD}; removing resolver entries failed ({cause})")
        }
    }
}

/// True when the apply needs a privileged prompt at all (hosts change is decided later, from
/// the fresh snapshot, so a pending hosts update counts as "maybe").
fn plan_is_empty(plan: &ResolverPlan, hosts: &Option<HostsUpdate<'_>>) -> bool {
    plan.write.is_empty() && plan.remove.is_empty() && hosts.is_none()
}

/// Plan that removes every ClusterDeck-marked file in the listing and nothing else.
fn removal_plan(env: &ManagerEnv<'_>, listing: &Listing) -> ResolverPlan {
    plan_changes_for_port(&[], &listing.files, &[], env.port)
}

/// Entries that could be ours (valid private-zone name) but whose ownership cannot be read.
fn unreadable_candidates(listing: &Listing) -> Vec<&str> {
    listing
        .unreadable
        .iter()
        .map(String::as_str)
        .filter(|n| is_allowed_private_zone(n))
        .collect()
}

pub async fn apply(env: &ManagerEnv<'_>, req: ApplyRequest<'_>) -> Result<ApplyResult, String> {
    let _guard = MANAGER_MUTEX.lock().await;

    // R7: plan first (unprivileged read) so conflicted zones are never published. A listing
    // failure aborts before anything changes.
    let listing = read_resolver_listing(env.resolver_dir)?;
    let unreadable: HashSet<&str> = listing.unreadable.iter().map(String::as_str).collect();
    let desired: Vec<(String, String)> = req
        .zones
        .zones
        .iter()
        .filter(|z| !unreadable.contains(z.zone.as_str()))
        .map(|z| (z.zone.clone(), z.profile_id.clone()))
        .collect();
    let mut plan = plan_changes_for_port(&desired, &listing.files, req.approved_switches, env.port);
    for z in &req.zones.zones {
        if unreadable.contains(z.zone.as_str()) {
            plan.conflicts.push(Conflict {
                zone: z.zone.clone(),
                kind: ConflictKind::Unreadable,
            });
        }
    }

    if let Some(target_id) = req.scope_profile {
        let is_in_scope = |zone: &str, profile: &str| -> bool {
            profile == target_id
                || req
                    .approved_switches
                    .iter()
                    .any(|(z, f, t)| z == zone && (f == target_id || t == target_id))
        };
        plan.write
            .retain(|(zone, winner)| is_in_scope(zone, winner));
        plan.remove.retain(|(zone, owner)| is_in_scope(zone, owner));
    }

    let conflicted: HashSet<&str> = plan.conflicts.iter().map(|c| c.zone.as_str()).collect();
    let on_disk_names: HashSet<&str> = listing.files.iter().map(|(n, _)| n.as_str()).collect();
    let will_write_names: HashSet<&str> = plan.write.iter().map(|(z, _)| z.as_str()).collect();

    let free: Vec<ZoneEntry> = req
        .zones
        .zones
        .iter()
        .filter(|z| !conflicted.contains(z.zone.as_str()))
        .filter(|z| {
            if req.scope_profile.is_some() {
                on_disk_names.contains(z.zone.as_str())
                    || will_write_names.contains(z.zone.as_str())
            } else {
                true
            }
        })
        .cloned()
        .collect();
    let mut result = ApplyResult {
        conflicts: plan.conflicts.clone(),
        ..ApplyResult::default()
    };

    let previous = read_previous_zones(env.paths);
    let free_names: HashSet<&str> = free.iter().map(|z| z.zone.as_str()).collect();
    let removing: HashSet<&str> = plan.remove.iter().map(|(z, _)| z.as_str()).collect();

    // Zones on disk under our marker that this apply does not (re)write keep being served with
    // their previous records: either a conflicted zone (only the requested change is withheld)
    // or one about to lose its file (served until the file is gone, R7).
    let on_disk_ours = reconcile_entries(&listing.files, &[], previous.as_ref());
    let (retained, kept): (Vec<ZoneEntry>, Vec<ZoneEntry>) = on_disk_ours
        .into_iter()
        .filter(|z| !free_names.contains(z.zone.as_str()))
        .partition(|z| removing.contains(z.zone.as_str()));

    if free.is_empty() {
        // Nothing new to serve: no agent is needed, and removing our own files is always safe,
        // so no ownership gate. Serving is reconciled to disk afterwards, whatever happened.
        let mut failure = None;
        if !plan_is_empty(&plan, &req.hosts) {
            result.prompted = true;
            match run_plan_and_hosts(env, &plan, None, req.hosts.as_ref()).await {
                Ok(()) => {}
                Err(ScriptError::Cancelled) => result.cancelled = true,
                Err(ScriptError::HostsChanged) => failure = Some(HOSTS_CHANGED_MSG.to_string()),
                Err(ScriptError::ListenerMismatch) => {
                    failure = Some("listener pid mismatch".to_string())
                }
                Err(ScriptError::Failed(e)) => failure = Some(e),
            }
        }
        if previous.is_some() {
            reconcile_publish(env, &[], &previous)?;
            result.generation = served_generation(env).unwrap_or(0);
        }
        result.published = served_names(env);
        return failure.map_or(Ok(result), Err);
    }

    // Gate: nothing privileged happens before the responder proves it loaded this generation
    // and owns the port. EVERY failure here (agent start included) ends in D-DEAD.
    let gate: Result<(u32, u64, StatusFile), String> = async {
        let uid = agent::current_uid(env.runner).await?;
        ensure_agent(env, uid).await?;
        let g1 = next_generation(env.paths);
        let mut first = free.clone();
        first.extend(kept.iter().cloned());
        first.extend(retained.iter().cloned());
        publish(env, first, g1)?;
        let status = wait_for_status(env, g1).await?;
        if !agent::ownership_ok(env.runner, uid, env.port, &status).await? {
            return Err(format!(
                "port {} is not owned by the ClusterDeck DNS agent",
                env.port
            ));
        }
        Ok((uid, g1, status))
    }
    .await;
    let (uid, g1, status) = match gate {
        Ok(v) => v,
        Err(cause) => return Err(on_responder_unavailable(env, &previous, &cause).await),
    };

    result.generation = g1;
    result.published = free
        .iter()
        .chain(kept.iter())
        .chain(retained.iter())
        .map(|z| z.zone.clone())
        .collect();
    if !plan_is_empty(&plan, &req.hosts) {
        result.prompted = true;
        // Only writes point the OS at the port; removals are safe regardless of who owns it.
        let expected_pid = (!plan.write.is_empty()).then_some(status.pid);
        let mut script_err: Option<String> = None;
        match run_plan_and_hosts(env, &plan, expected_pid, req.hosts.as_ref()).await {
            Ok(()) => {}
            Err(ScriptError::Cancelled) => result.cancelled = true,
            Err(ScriptError::HostsChanged) => script_err = Some(HOSTS_CHANGED_MSG.to_string()),
            Err(ScriptError::Failed(e)) => script_err = Some(e),
            Err(ScriptError::ListenerMismatch) => {
                let cause = "listener on the port changed before the privileged script ran";
                return Err(on_responder_unavailable(env, &previous, cause).await);
            }
        }
        if result.cancelled || script_err.is_some() {
            // The script may have stopped half-way: publish exactly what is on disk now.
            let reconciled = reconcile_publish(env, &free, &previous);
            result.generation = served_generation(env).unwrap_or(g1);
            result.published = served_names(env);
            if let Some(e) = script_err {
                return Err(match reconciled {
                    Ok(()) => e,
                    Err(r) => format!("{e}; additionally {r}"),
                });
            }
            reconciled?;
            return Ok(result);
        }
    }

    if !retained.is_empty() {
        reconcile_publish(env, &free, &previous)?;
        let g2 = served_generation(env).unwrap_or(g1);
        // The resolver files may now point at the port, so a g2 failure or a lost ownership
        // is D-DEAD territory, not a plain error.
        let verified: Result<(), String> = async {
            let st = wait_for_status(env, g2).await?;
            if !agent::ownership_ok(env.runner, uid, env.port, &st).await? {
                return Err(format!(
                    "port {} is no longer owned by the ClusterDeck DNS agent",
                    env.port
                ));
            }
            Ok(())
        }
        .await;
        if let Err(cause) = verified {
            return Err(on_responder_unavailable(env, &previous, &cause).await);
        }
        result.generation = g2;
        result.published = served_names(env);
    }
    Ok(result)
}

/// Rebuilds zones.json for the currently installed resolver zones (ours on disk) with no
/// changes to /etc/resolver and no privileged prompts. Used during Connect to refresh records.
///
/// While any of our resolver files exists the OS points at the responder, so a dead agent, a
/// responder that does not load the new generation, or a foreign listener is reported as an
/// error. Nothing is removed here: that needs privilege and is the explicit Sync/apply path's
/// D-DEAD cleanup.
pub async fn apply_records_only(env: &ManagerEnv<'_>, desired: &[ZoneEntry]) -> Result<(), String> {
    const UNAVAILABLE: &str = "Local DNS responder unavailable; press Sync to local DNS";
    let _guard = MANAGER_MUTEX.lock().await;
    let listing = read_resolver_listing(env.resolver_dir)?;
    let previous = read_previous_zones(env.paths);
    let entries = reconcile_entries(&listing.files, desired, previous.as_ref());
    let generation = next_generation(env.paths);
    publish(env, entries, generation)?;
    if remaining_marker_files(&listing).is_empty() {
        return Ok(());
    }
    let uid = agent::current_uid(env.runner)
        .await
        .map_err(|e| format!("{UNAVAILABLE} ({e})"))?;
    let running = agent::agent_pid(env.runner, uid)
        .await
        .map_err(|e| format!("{UNAVAILABLE} ({e})"))?
        .is_some();
    if !running {
        return Err(format!("{UNAVAILABLE} (agent is not running)"));
    }
    let status = wait_for_status(env, generation)
        .await
        .map_err(|e| format!("{UNAVAILABLE} ({e})"))?;
    match agent::ownership_ok(env.runner, uid, env.port, &status).await {
        Ok(true) => Ok(()),
        Ok(false) => Err(format!(
            "{UNAVAILABLE} (port {} is not owned by the ClusterDeck DNS agent)",
            env.port
        )),
        Err(e) => Err(format!("{UNAVAILABLE} ({e})")),
    }
}

fn served_generation(env: &ManagerEnv<'_>) -> Option<u64> {
    read_previous_zones(env.paths).map(|z| z.generation)
}

fn served_names(env: &ManagerEnv<'_>) -> Vec<String> {
    read_previous_zones(env.paths)
        .map(|z| z.zones.into_iter().map(|e| e.zone).collect())
        .unwrap_or_default()
}

fn remaining_marker_files(listing: &Listing) -> Vec<String> {
    listing
        .files
        .iter()
        .filter(|(name, content)| is_allowed_private_zone(name) && parse_marker(content).is_some())
        .map(|(name, _)| name.clone())
        .collect()
}

/// Removes our resolver files, verifies they are gone, and only then uninstalls the agent. A
/// cancelled prompt, or any entry whose ownership cannot be read, leaves the agent running.
pub async fn teardown(env: &ManagerEnv<'_>) -> Result<TeardownResult, String> {
    let _guard = MANAGER_MUTEX.lock().await;
    let listing = read_resolver_listing(env.resolver_dir)?;
    let blockers = unreadable_candidates(&listing);
    if !blockers.is_empty() {
        return Err(format!(
            "cannot verify ownership of {} in {}; DNS agent left running",
            blockers.join(", "),
            env.resolver_dir.display()
        ));
    }
    let plan = removal_plan(env, &listing);
    let mut result = TeardownResult {
        removed: plan.remove.iter().map(|(z, _)| z.clone()).collect(),
        ..TeardownResult::default()
    };

    if !plan.remove.is_empty() {
        match run_script(env, &plan, None, None).await {
            Ok(()) => {}
            Err(ScriptError::Cancelled) => {
                result.removed.clear();
                result.cancelled = true;
                return Ok(result);
            }
            Err(ScriptError::HostsChanged | ScriptError::ListenerMismatch) => {
                unreachable!("no hosts/pid step in teardown")
            }
            Err(ScriptError::Failed(e)) => return Err(e),
        }
    }

    // Verify from disk; any doubt keeps the agent.
    let after = read_resolver_listing(env.resolver_dir)
        .map_err(|e| format!("{e}; DNS agent left running"))?;
    let doubt: Vec<String> = remaining_marker_files(&after)
        .into_iter()
        .chain(unreadable_candidates(&after).into_iter().map(String::from))
        .collect();
    if !doubt.is_empty() {
        return Err(format!(
            "resolver files still present or unreadable after removal ({}); DNS agent left running",
            doubt.join(", ")
        ));
    }

    let uid = agent::current_uid(env.runner).await?;
    agent::uninstall(env.runner, env.paths, uid).await?;
    // zones.json/status.json describe a dead agent; `generation` is kept so numbers never
    // restart if local DNS is enabled again.
    for f in [&env.paths.zones, &env.paths.status] {
        match std::fs::remove_file(f) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(format!("{}: {e}", f.display())),
        }
    }
    result.agent_removed = true;
    Ok(result)
}

/// App-start refresh of the stable binary copy (R10): copies `current_exe` over it when the
/// SHA-256 differs and reloads the agent only if it is currently loaded (never starts one).
/// Returns whether the copy changed.
pub async fn refresh_binary(env: &ManagerEnv<'_>) -> Result<bool, String> {
    let _guard = MANAGER_MUTEX.lock().await;
    let changed = agent::ensure_binary(env.current_exe, &env.paths.binary)?;
    if changed {
        let uid = agent::current_uid(env.runner).await?;
        if agent::is_loaded(env.runner, uid).await? {
            agent::reload(env.runner, env.paths, uid).await?;
        }
    }
    Ok(changed)
}

/// App-start cleanup: removes only OUR marker files when no profile uses local DNS or when the
/// agent does not own the port. Never starts the agent; unowned files are never touched.
pub async fn startup_sweep(
    env: &ManagerEnv<'_>,
    any_profile_uses_local_dns: bool,
) -> Result<SweepResult, String> {
    let _guard = MANAGER_MUTEX.lock().await;
    let mut result = SweepResult::default();
    // Abort before any change when the directory cannot be listed.
    let listing = read_resolver_listing(env.resolver_dir)?;

    if any_profile_uses_local_dns {
        let owned = match load_status(&env.paths.status) {
            Ok(status) => match agent::current_uid(env.runner).await {
                Ok(uid) => agent::ownership_ok(env.runner, uid, env.port, &status)
                    .await
                    .unwrap_or(false),
                Err(_) => false,
            },
            Err(_) => false,
        };
        if owned {
            result.skipped_owned = true;
            return Ok(result);
        }
    }

    let plan = removal_plan(env, &listing);
    if plan.remove.is_empty() {
        return Ok(result);
    }
    result.removed = plan.remove.iter().map(|(z, _)| z.clone()).collect();
    match run_script(env, &plan, None, None).await {
        Ok(()) => Ok(result),
        Err(ScriptError::Cancelled) => {
            result.removed.clear();
            result.cancelled = true;
            Ok(result)
        }
        Err(ScriptError::HostsChanged | ScriptError::ListenerMismatch) => {
            unreachable!("no hosts/pid step in sweep")
        }
        Err(ScriptError::Failed(e)) => Err(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::local_dns::zone_file::{write_status, RecordEntry};
    use crate::services::process::CommandOutput;
    use crate::services::resolver_files::render_resolver_file;
    use async_trait::async_trait;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::{Arc, Mutex as StdMutex};

    const PORT: u16 = 15353;
    const PID: u32 = 4242;
    static SEQ: AtomicU64 = AtomicU64::new(0);

    #[derive(Clone, Copy, PartialEq)]
    enum Osa {
        Ok,
        Cancel,
        Fail,
        /// Applies the first N queued writes, then fails (a script that dies half-way).
        Partial(usize),
        /// Applies everything, then reports exit 8 (hosts snapshot mismatch) N more times.
        HostsChanged(usize),
        /// Exit 7 once (nothing applied), then behaves as `Ok`.
        ListenerMismatch,
    }

    struct Behaviour {
        loaded: bool,
        responder_alive: bool,
        responder_pid: u32,
        launchctl_pid: u32,
        udp_pid: u32,
        tcp_pid: u32,
        osa: Osa,
        /// `(zone, owner)` files the next successful script run "writes" to the resolver dir.
        will_write: Vec<(String, String)>,
        /// After a successful script the responder dies (drives the g2 timeout).
        kill_responder_on_script: bool,
        /// Fail the Nth non-osascript runner call (0-based) with a runner error.
        fail_at: Option<usize>,
        ncalls: usize,
    }

    /// Scripted stand-in for launchctl/lsof/id/osascript plus a simulated responder that
    /// mirrors zones.json into status.json while the agent is "loaded".
    struct Fake {
        home: PathBuf,
        paths: AgentPaths,
        resolver: PathBuf,
        exe: PathBuf,
        log: StdMutex<Vec<String>>,
        scripts: StdMutex<Vec<String>>,
        b: StdMutex<Behaviour>,
    }

    impl Fake {
        fn new() -> Arc<Self> {
            let seq = SEQ.fetch_add(1, Ordering::Relaxed);
            let home = std::env::temp_dir()
                .join(format!("clusterdeck-mgr-test-{}-{seq}", std::process::id()));
            let _ = std::fs::remove_dir_all(&home);
            let resolver = home.join("resolver");
            std::fs::create_dir_all(&resolver).unwrap();
            let exe = home.join("current-exe");
            std::fs::write(&exe, b"binary-v1").unwrap();
            Arc::new(Self {
                paths: AgentPaths::new(&home),
                home,
                resolver,
                exe,
                log: StdMutex::new(Vec::new()),
                scripts: StdMutex::new(Vec::new()),
                b: StdMutex::new(Behaviour {
                    loaded: false,
                    responder_alive: true,
                    responder_pid: PID,
                    launchctl_pid: PID,
                    udp_pid: PID,
                    tcp_pid: PID,
                    osa: Osa::Ok,
                    will_write: Vec::new(),
                    kill_responder_on_script: false,
                    fail_at: None,
                    ncalls: 0,
                }),
            })
        }

        fn env(&self) -> ManagerEnv<'_> {
            ManagerEnv::new(self, &self.paths, &self.resolver, &self.exe, PORT)
        }

        fn calls(&self) -> Vec<String> {
            self.log.lock().unwrap().clone()
        }

        fn index(&self, prefix: &str) -> Option<usize> {
            self.calls().iter().position(|c| c.starts_with(prefix))
        }

        fn count(&self, prefix: &str) -> usize {
            self.calls()
                .iter()
                .filter(|c| c.starts_with(prefix))
                .count()
        }

        fn set(&self, f: impl FnOnce(&mut Behaviour)) {
            f(&mut self.b.lock().unwrap());
        }

        fn sync_responder(&self) {
            let b = self.b.lock().unwrap();
            if !b.loaded || !b.responder_alive {
                return;
            }
            if let Some(z) = read_previous_zones(&self.paths) {
                let _ = write_status(
                    &self.paths.status,
                    StatusFile {
                        generation: z.generation,
                        loaded_at: "t".into(),
                        error: None,
                        pid: b.responder_pid,
                        instance: "inst".into(),
                    },
                );
            }
        }

        fn spawn_responder(self: &Arc<Self>) {
            let f = Arc::clone(self);
            tokio::spawn(async move {
                loop {
                    tokio::time::sleep(Duration::from_millis(30)).await;
                    f.sync_responder();
                }
            });
        }

        fn served_zones(&self) -> Vec<String> {
            read_previous_zones(&self.paths)
                .map(|z| z.zones.into_iter().map(|e| e.zone).collect())
                .unwrap_or_default()
        }

        fn writes(&self, items: &[(&str, &str)]) {
            self.b.lock().unwrap().will_write = items
                .iter()
                .map(|(z, o)| (z.to_string(), o.to_string()))
                .collect();
        }

        /// Simulates what the privileged script does to the resolver dir.
        fn script_effects(&self, write_limit: usize, removals: bool) {
            let writes = std::mem::take(&mut self.b.lock().unwrap().will_write);
            // A removal-only script (no mkdir step) cannot write anything.
            let has_writes = self
                .scripts
                .lock()
                .unwrap()
                .last()
                .is_some_and(|s| s.contains("/bin/mkdir"));
            let write_limit = if has_writes { write_limit } else { 0 };
            for (zone, owner) in writes.into_iter().take(write_limit) {
                self.put_marker_file(&zone, &owner);
            }
            if !removals {
                return;
            }
            for (name, content) in read_resolver_listing(&self.resolver).unwrap().files {
                if parse_marker(&content).is_some() {
                    let rm = format!("/bin/rm -f '{}/{name}'", self.resolver.display());
                    let removed_by_script = self
                        .scripts
                        .lock()
                        .unwrap()
                        .last()
                        .is_some_and(|s| s.contains(&rm));
                    if removed_by_script {
                        let _ = std::fs::remove_file(self.resolver.join(name));
                    }
                }
            }
        }

        fn put_marker_file(&self, zone: &str, profile: &str) {
            std::fs::write(
                self.resolver.join(zone),
                render_resolver_file(profile, PORT).unwrap(),
            )
            .unwrap();
        }
    }

    fn ok(stdout: &str) -> Result<CommandOutput, String> {
        Ok(CommandOutput {
            stdout: stdout.into(),
            stderr: String::new(),
            success: true,
        })
    }

    fn fail(stderr: &str) -> Result<CommandOutput, String> {
        Ok(CommandOutput {
            stdout: String::new(),
            stderr: stderr.into(),
            success: false,
        })
    }

    #[async_trait]
    impl CommandRunner for Fake {
        async fn run(&self, bin: &str, args: &[String]) -> Result<CommandOutput, String> {
            if bin != "osascript" {
                let mut b = self.b.lock().unwrap();
                let idx = b.ncalls;
                b.ncalls += 1;
                if b.fail_at == Some(idx) {
                    return Err("injected runner failure".to_string());
                }
            }
            match (bin, args.first().map(String::as_str)) {
                ("/usr/bin/id", _) => {
                    self.log.lock().unwrap().push("id".into());
                    ok("501")
                }
                ("/bin/launchctl", Some(sub)) => {
                    self.log.lock().unwrap().push(format!("launchctl {sub}"));
                    let mut b = self.b.lock().unwrap();
                    match sub {
                        "bootstrap" => {
                            b.loaded = true;
                            ok("")
                        }
                        "bootout" => {
                            if b.loaded {
                                b.loaded = false;
                                ok("")
                            } else {
                                fail("not loaded")
                            }
                        }
                        "print" if b.loaded => ok(&format!("{{\n\tpid = {}\n}}", b.launchctl_pid)),
                        "print" => fail("not found"),
                        other => panic!("unexpected launchctl {other}"),
                    }
                }
                ("/usr/sbin/lsof", _) => {
                    let proto = if args.iter().any(|a| a.contains("UDP")) {
                        "udp"
                    } else {
                        "tcp"
                    };
                    self.log.lock().unwrap().push(format!("lsof {proto}"));
                    let b = self.b.lock().unwrap();
                    let pid = if proto == "udp" { b.udp_pid } else { b.tcp_pid };
                    if pid == 0 {
                        fail("")
                    } else {
                        ok(&pid.to_string())
                    }
                }
                ("osascript", _) => {
                    let zones = self.served_zones().join(",");
                    let status_gen = load_status(&self.paths.status)
                        .map(|s| s.generation)
                        .unwrap_or(0);
                    let zones_gen = read_previous_zones(&self.paths)
                        .map(|z| z.generation)
                        .unwrap_or(0);
                    self.log.lock().unwrap().push(format!(
                        "osascript zones_gen={zones_gen} status_gen={status_gen} served=[{zones}]"
                    ));
                    self.scripts.lock().unwrap().push(args[1].clone());
                    let mode = self.b.lock().unwrap().osa;
                    match mode {
                        Osa::Cancel => fail("execution error: User canceled. (-128)"),
                        Osa::Fail => fail("execution error: boom (9)"),
                        Osa::Partial(n) => {
                            self.script_effects(n, false);
                            fail("execution error: boom (9)")
                        }
                        Osa::ListenerMismatch => {
                            // exit 7 runs before any step: nothing lands.
                            self.b.lock().unwrap().will_write.clear();
                            self.set(|b| b.osa = Osa::Ok);
                            fail("execution error: listener mismatch (7)")
                        }
                        Osa::HostsChanged(n) => {
                            // Additive hosts step sits before the removals (D-MBB): writes
                            // landed, removals were skipped by the exit-8 abort.
                            self.script_effects(usize::MAX, false);
                            self.set(|b| {
                                b.osa = if n > 1 {
                                    Osa::HostsChanged(n - 1)
                                } else {
                                    Osa::Ok
                                }
                            });
                            fail("execution error: hosts changed (8)")
                        }
                        Osa::Ok => {
                            self.script_effects(usize::MAX, true);
                            self.set(|b| {
                                if b.kill_responder_on_script {
                                    b.responder_alive = false;
                                }
                            });
                            ok("")
                        }
                    }
                }
                other => panic!("unexpected command {other:?}"),
            }
        }
    }

    fn zones(gen: u64, names: &[(&str, &str)]) -> ZonesFile {
        ZonesFile {
            version: 1,
            generation: gen,
            port: PORT,
            zones: names
                .iter()
                .map(|(zone, profile)| ZoneEntry {
                    zone: zone.to_string(),
                    profile_id: profile.to_string(),
                    records: vec![RecordEntry {
                        name: format!("api.{zone}"),
                        ipv4: "192.0.2.10".into(),
                    }],
                })
                .collect(),
        }
    }

    fn req(z: ZonesFile) -> ApplyRequest<'static> {
        ApplyRequest {
            zones: z,
            approved_switches: &[],
            hosts: None,
            scope_profile: None,
        }
    }

    fn zones_ip(names: &[(&str, &str)], ip: &str) -> ZonesFile {
        let mut z = zones(0, names);
        for e in &mut z.zones {
            e.records[0].ipv4 = ip.to_string();
        }
        z
    }

    fn served_entry(f: &Fake, zone: &str) -> Option<ZoneEntry> {
        read_previous_zones(&f.paths)?
            .zones
            .into_iter()
            .find(|e| e.zone == zone)
    }

    fn started() -> Arc<Fake> {
        let f = Fake::new();
        f.spawn_responder();
        f
    }

    #[tokio::test(start_paused = true)]
    async fn privileged_call_only_after_verified_generation_and_ownership() {
        let f = started();
        f.writes(&[("lab.example.internal", "prof-a")]);
        let res = apply(
            &f.env(),
            req(zones(0, &[("lab.example.internal", "prof-a")])),
        )
        .await
        .unwrap();
        assert!(res.prompted && !res.cancelled);

        let calls = f.calls();
        let osa = f.index("osascript").expect("prompt expected");
        for needed in [
            "launchctl print",
            "lsof udp",
            "lsof tcp",
            "launchctl bootstrap",
        ] {
            assert!(
                f.index(needed).unwrap() < osa,
                "{needed} must precede osascript: {calls:?}"
            );
        }
        assert_eq!(f.count("osascript"), 1, "exactly one prompt: {calls:?}");
        assert!(
            calls[osa].contains("zones_gen=1 status_gen=1"),
            "status generation must equal the written one at prompt time: {}",
            calls[osa]
        );
        let script = f.scripts.lock().unwrap()[0].clone();
        assert!(
            script.contains("-iUDP:15353")
                && script.contains(&format!("{PID}"))
                && script.contains("exit 7")
        );
        assert_eq!(f.served_zones(), vec!["lab.example.internal"]);
        // ensure_agent installed the stable binary copy.
        assert_eq!(std::fs::read(&f.paths.binary).unwrap(), b"binary-v1");
    }

    #[tokio::test(start_paused = true)]
    async fn removal_happens_before_zone_is_unpublished_and_final_generation_drops_it() {
        let f = started();
        f.put_marker_file("old.example.internal", "prof-a");
        f.writes(&[("new.example.internal", "prof-a")]);
        write_zones(
            &f.paths.zones,
            &zones(3, &[("old.example.internal", "prof-a")]),
        )
        .unwrap();

        let res = apply(
            &f.env(),
            req(zones(0, &[("new.example.internal", "prof-a")])),
        )
        .await
        .unwrap();

        let osa_call = f
            .calls()
            .into_iter()
            .find(|c| c.starts_with("osascript"))
            .unwrap();
        assert!(
            osa_call.contains("old.example.internal") && osa_call.contains("new.example.internal"),
            "old zone must still be served while its file is removed: {osa_call}"
        );
        assert_eq!(f.served_zones(), vec!["new.example.internal"]);
        assert!(
            res.generation >= 5,
            "two generations after prior 3: {}",
            res.generation
        );
        assert!(!f.resolver.join("old.example.internal").exists());
    }

    #[tokio::test(start_paused = true)]
    async fn cancelled_prompt_restores_previous_zones_json() {
        let f = started();
        f.set(|b| b.osa = Osa::Cancel);
        let prev = zones(7, &[("keep.example.internal", "prof-a")]);
        write_zones(&f.paths.zones, &prev).unwrap();
        f.put_marker_file("keep.example.internal", "prof-a");

        let res = apply(
            &f.env(),
            req(zones(0, &[("new.example.internal", "prof-a")])),
        )
        .await
        .unwrap();
        assert!(res.cancelled);
        assert_eq!(res.published, vec!["keep.example.internal"]);
        assert_eq!(f.served_zones(), vec!["keep.example.internal"]);
        let now = read_previous_zones(&f.paths).unwrap();
        assert!(now.generation > 7, "restore must use a higher generation");

        // A hard failure restores too and surfaces the error.
        f.set(|b| b.osa = Osa::Fail);
        let err = apply(
            &f.env(),
            req(zones(0, &[("new.example.internal", "prof-a")])),
        )
        .await
        .unwrap_err();
        assert!(err.contains("boom"));
        assert_eq!(f.served_zones(), vec!["keep.example.internal"]);
    }

    #[tokio::test(start_paused = true)]
    async fn empty_plan_makes_no_prompt() {
        let f = started();
        f.put_marker_file("lab.example.internal", "prof-a");
        let res = apply(
            &f.env(),
            req(zones(0, &[("lab.example.internal", "prof-a")])),
        )
        .await
        .unwrap();
        assert!(!res.prompted);
        assert_eq!(f.count("osascript"), 0);
        assert_eq!(f.served_zones(), vec!["lab.example.internal"]);
    }

    #[tokio::test(start_paused = true)]
    async fn stale_status_generation_removes_our_resolver_entries_once() {
        let f = Fake::new(); // responder deliberately not running
        f.set(|b| b.loaded = true);
        f.put_marker_file("keep.example.internal", "prof-a");
        write_status(
            &f.paths.status,
            StatusFile {
                generation: 1,
                loaded_at: "t".into(),
                error: None,
                pid: PID,
                instance: "old".into(),
            },
        )
        .unwrap();
        write_zones(
            &f.paths.zones,
            &zones(1, &[("keep.example.internal", "prof-a")]),
        )
        .unwrap();

        let err = apply(
            &f.env(),
            req(zones(0, &[("lab.example.internal", "prof-a")])),
        )
        .await
        .unwrap_err();
        assert!(err.contains("did not load generation"), "{err}");
        assert!(
            err.contains("local DNS responder unavailable; resolver entries removed"),
            "{err}"
        );
        // D-DEAD: one removal prompt (and no write prompt), nothing left pointing at the port.
        assert_eq!(f.count("osascript"), 1);
        assert!(!f.resolver.join("keep.example.internal").exists());
        assert!(f.served_zones().is_empty());
    }

    #[tokio::test(start_paused = true)]
    async fn ownership_mismatch_removes_entries_and_cancel_leaves_them_reported() {
        // Squatter on UDP while one of our files exists.
        let f = started();
        f.put_marker_file("keep.example.internal", "prof-a");
        f.set(|b| b.udp_pid = 999);
        let err = apply(
            &f.env(),
            req(zones(0, &[("lab.example.internal", "prof-a")])),
        )
        .await
        .unwrap_err();
        assert!(
            err.contains("local DNS responder unavailable; resolver entries removed")
                && err.contains("not owned"),
            "{err}"
        );
        assert_eq!(f.count("osascript"), 1);
        assert!(!f.resolver.join("keep.example.internal").exists());
        assert!(!f.resolver.join("lab.example.internal").exists());
        let script = f.scripts.lock().unwrap()[0].clone();
        assert!(!script.contains("exit 7"), "removal-only: no pid check");

        // Cancelled removal: state as-is, clearly reported, still one prompt.
        let f = started();
        f.put_marker_file("keep.example.internal", "prof-a");
        write_zones(
            &f.paths.zones,
            &zones(1, &[("keep.example.internal", "prof-a")]),
        )
        .unwrap();
        f.set(|b| {
            b.tcp_pid = 999;
            b.osa = Osa::Cancel;
        });
        let err = apply(
            &f.env(),
            req(zones(0, &[("lab.example.internal", "prof-a")])),
        )
        .await
        .unwrap_err();
        assert!(
            err.contains("was cancelled") && err.contains("left in place"),
            "{err}"
        );
        assert_eq!(f.count("osascript"), 1);
        assert!(f.resolver.join("keep.example.internal").exists());
        assert_eq!(f.served_zones(), vec!["keep.example.internal"]);
    }

    #[tokio::test(start_paused = true)]
    async fn different_pid_or_port_squatter_aborts() {
        for squat in 0..3 {
            let f = started();
            f.set(|b| match squat {
                0 => b.responder_pid = 111, // status.pid != launchctl pid
                1 => b.udp_pid = 999,
                _ => b.tcp_pid = 999,
            });
            let err = apply(
                &f.env(),
                req(zones(0, &[("lab.example.internal", "prof-a")])),
            )
            .await
            .unwrap_err();
            assert!(err.contains("not owned"), "case {squat}: {err}");
            assert_eq!(f.count("osascript"), 0, "case {squat}");
            assert!(!f.resolver.join("lab.example.internal").exists());
        }
    }

    #[tokio::test(start_paused = true)]
    async fn generation_never_decreases() {
        let f = started();
        std::fs::create_dir_all(&f.paths.dns_dir).unwrap();
        std::fs::write(&f.paths.generation, "50\n").unwrap();
        write_status(
            &f.paths.status,
            StatusFile {
                generation: 70,
                loaded_at: "t".into(),
                error: None,
                pid: PID,
                instance: "x".into(),
            },
        )
        .unwrap();

        let mut last = 0;
        for _ in 0..3 {
            apply(
                &f.env(),
                req(zones(0, &[("lab.example.internal", "prof-a")])),
            )
            .await
            .unwrap();
            let g = read_previous_zones(&f.paths).unwrap().generation;
            assert!(g > last, "{g} <= {last}");
            assert_eq!(read_generation_file(&f.paths.generation), g);
            last = g;
        }
        assert_eq!(last, 73, "max(stored 50, status 70) + 1, then +1 per apply");
    }

    #[tokio::test(start_paused = true)]
    async fn conflicted_zones_are_never_published() {
        let f = started();
        f.writes(&[("ok.example.internal", "prof-a")]);
        std::fs::write(
            f.resolver.join("blocked.example.internal"),
            "nameserver 192.0.2.53\n",
        )
        .unwrap();
        let res = apply(
            &f.env(),
            req(zones(
                0,
                &[
                    ("blocked.example.internal", "prof-a"),
                    ("ok.example.internal", "prof-a"),
                ],
            )),
        )
        .await
        .unwrap();
        assert_eq!(f.served_zones(), vec!["ok.example.internal"]);
        assert_eq!(res.conflicts.len(), 1);
        assert_eq!(
            std::fs::read_to_string(f.resolver.join("blocked.example.internal")).unwrap(),
            "nameserver 192.0.2.53\n"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn teardown_removes_files_before_uninstalling_agent() {
        let f = started();
        apply(
            &f.env(),
            req(zones(0, &[("lab.example.internal", "prof-a")])),
        )
        .await
        .unwrap();
        f.put_marker_file("lab.example.internal", "prof-a");
        assert!(f.paths.plist.exists() && f.paths.binary.exists());

        let res = teardown(&f.env()).await.unwrap();
        assert!(res.agent_removed && !res.cancelled);
        assert_eq!(res.removed, vec!["lab.example.internal"]);
        let osa = f
            .calls()
            .iter()
            .rposition(|c| c.starts_with("osascript"))
            .unwrap();
        // `rposition`: ensure_agent's reload during apply also boots out (a no-op then).
        let out = f
            .calls()
            .iter()
            .rposition(|c| c.starts_with("launchctl bootout"))
            .unwrap();
        assert!(osa < out, "removal must precede uninstall: {:?}", f.calls());
        assert!(!f.paths.plist.exists() && !f.paths.binary.exists());
        assert!(!f.resolver.join("lab.example.internal").exists());
        assert!(f.paths.generation.exists(), "generation survives teardown");
    }

    #[tokio::test(start_paused = true)]
    async fn teardown_cancel_keeps_agent_running() {
        let f = started();
        apply(
            &f.env(),
            req(zones(0, &[("lab.example.internal", "prof-a")])),
        )
        .await
        .unwrap();
        f.put_marker_file("lab.example.internal", "prof-a");
        f.set(|b| b.osa = Osa::Cancel);
        let bootouts = f.count("launchctl bootout");

        let res = teardown(&f.env()).await.unwrap();
        assert!(res.cancelled && !res.agent_removed);
        assert_eq!(f.count("launchctl bootout"), bootouts, "{:?}", f.calls());
        assert!(f.paths.plist.exists() && f.paths.binary.exists());
    }

    #[tokio::test(start_paused = true)]
    async fn sweep_removes_only_our_files_and_never_starts_the_agent() {
        let f = Fake::new();
        f.put_marker_file("ours.example.internal", "prof-a");
        std::fs::write(
            f.resolver.join("theirs.example.internal"),
            "nameserver 192.0.2.53\n",
        )
        .unwrap();

        let res = startup_sweep(&f.env(), false).await.unwrap();
        assert_eq!(res.removed, vec!["ours.example.internal"]);
        assert_eq!(f.count("osascript"), 1);
        assert_eq!(f.count("launchctl bootstrap"), 0);
        assert!(f.resolver.join("theirs.example.internal").exists());
        assert!(!f.resolver.join("ours.example.internal").exists());

        // Nothing of ours left: no further prompt.
        startup_sweep(&f.env(), false).await.unwrap();
        assert_eq!(f.count("osascript"), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn sweep_with_local_dns_profile_keeps_files_only_when_ownership_holds() {
        // Owned: agent loaded, status matches, listeners match.
        let f = started();
        apply(
            &f.env(),
            req(zones(0, &[("lab.example.internal", "prof-a")])),
        )
        .await
        .unwrap();
        f.put_marker_file("lab.example.internal", "prof-a");
        let prompts = f.count("osascript");
        let res = startup_sweep(&f.env(), true).await.unwrap();
        assert!(res.skipped_owned && res.removed.is_empty());
        assert_eq!(f.count("osascript"), prompts);

        // Squatter on UDP: our file is removed, agent is not (re)started.
        f.set(|b| b.udp_pid = 999);
        let boots = f.count("launchctl bootstrap");
        let res = startup_sweep(&f.env(), true).await.unwrap();
        assert_eq!(res.removed, vec!["lab.example.internal"]);
        assert_eq!(f.count("launchctl bootstrap"), boots);
    }

    #[tokio::test(start_paused = true)]
    async fn partial_script_failure_publishes_exactly_what_is_on_disk() {
        let f = started();
        f.writes(&[
            ("a.example.internal", "prof-a"),
            ("b.example.internal", "prof-a"),
        ]);
        f.set(|b| b.osa = Osa::Partial(1));

        let err = apply(
            &f.env(),
            req(zones_ip(
                &[
                    ("a.example.internal", "prof-a"),
                    ("b.example.internal", "prof-a"),
                ],
                "192.0.2.77",
            )),
        )
        .await
        .unwrap_err();
        assert!(err.contains("boom"), "{err}");
        assert!(f.resolver.join("a.example.internal").exists());
        assert!(!f.resolver.join("b.example.internal").exists());
        assert_eq!(f.served_zones(), vec!["a.example.internal"]);
        assert_eq!(
            served_entry(&f, "a.example.internal").unwrap().records[0].ipv4,
            "192.0.2.77",
            "the zone that did land serves the NEW records"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn conflicted_zone_stays_served_with_its_current_owners_previous_records() {
        let f = started();
        f.put_marker_file("c.example.internal", "prof-a");
        write_zones(
            &f.paths.zones,
            &zones_ip(&[("c.example.internal", "prof-a")], "192.0.2.99"),
        )
        .unwrap();
        f.writes(&[("h.example.internal", "prof-b")]);

        let res = apply(
            &f.env(),
            req(zones_ip(
                &[
                    ("c.example.internal", "prof-b"),
                    ("h.example.internal", "prof-b"),
                ],
                "192.0.2.10",
            )),
        )
        .await
        .unwrap();
        assert_eq!(res.conflicts.len(), 1);
        let mut served = f.served_zones();
        served.sort();
        assert_eq!(served, vec!["c.example.internal", "h.example.internal"]);
        assert_eq!(
            served_entry(&f, "c.example.internal").unwrap().records[0].ipv4,
            "192.0.2.99",
            "withheld change must not alter what the current owner serves"
        );
        assert_eq!(
            served_entry(&f, "h.example.internal").unwrap().records[0].ipv4,
            "192.0.2.10"
        );
        assert!(
            std::fs::read_to_string(f.resolver.join("c.example.internal"))
                .unwrap()
                .contains("prof-a")
        );
    }

    fn make_listing_fail(f: &Fake) {
        std::fs::remove_dir_all(&f.resolver).unwrap();
        std::fs::write(&f.resolver, "not a directory").unwrap();
    }

    #[tokio::test(start_paused = true)]
    async fn listing_failure_aborts_apply_teardown_and_sweep_before_any_change() {
        let f = started();
        make_listing_fail(&f);
        let e1 = apply(
            &f.env(),
            req(zones(0, &[("lab.example.internal", "prof-a")])),
        )
        .await
        .unwrap_err();
        let e2 = teardown(&f.env()).await.unwrap_err();
        let e3 = startup_sweep(&f.env(), false).await.unwrap_err();
        for e in [e1, e2, e3] {
            assert!(e.contains("cannot list"), "{e}");
        }
        assert!(f.calls().is_empty(), "nothing may run: {:?}", f.calls());
        assert!(!f.paths.zones.exists());
    }

    #[tokio::test(start_paused = true)]
    async fn symlink_or_unreadable_desired_zone_is_a_conflict_never_written_or_served() {
        use std::os::unix::fs::PermissionsExt;
        let f = started();
        let target = f.home.join("elsewhere");
        std::fs::write(&target, "nameserver 192.0.2.53\n").unwrap();
        std::os::unix::fs::symlink(&target, f.resolver.join("link.example.internal")).unwrap();
        let locked = f.resolver.join("locked.example.internal");
        std::fs::write(&locked, "x").unwrap();
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap();
        f.writes(&[("ok.example.internal", "prof-a")]);

        let res = apply(
            &f.env(),
            req(zones(
                0,
                &[
                    ("link.example.internal", "prof-a"),
                    ("locked.example.internal", "prof-a"),
                    ("ok.example.internal", "prof-a"),
                ],
            )),
        )
        .await
        .unwrap();
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o644)).unwrap();

        let unreadable = res
            .conflicts
            .iter()
            .filter(|c| c.kind == ConflictKind::Unreadable)
            .count();
        assert_eq!(unreadable, 2, "{:?}", res.conflicts);
        assert_eq!(f.served_zones(), vec!["ok.example.internal"]);
        let script = f.scripts.lock().unwrap()[0].clone();
        assert!(!script.contains("link.example.internal") && !script.contains("locked.example"));
        assert!(
            std::fs::symlink_metadata(f.resolver.join("link.example.internal"))
                .unwrap()
                .file_type()
                .is_symlink()
        );
    }

    #[tokio::test(start_paused = true)]
    async fn teardown_aborts_and_keeps_agent_when_a_candidate_entry_is_unreadable() {
        let f = started();
        apply(
            &f.env(),
            req(zones(0, &[("lab.example.internal", "prof-a")])),
        )
        .await
        .unwrap();
        f.put_marker_file("lab.example.internal", "prof-a");
        let target = f.home.join("elsewhere");
        std::fs::write(&target, "x").unwrap();
        std::os::unix::fs::symlink(&target, f.resolver.join("sneaky.example.internal")).unwrap();
        let (prompts, bootouts) = (f.count("osascript"), f.count("launchctl bootout"));

        let err = teardown(&f.env()).await.unwrap_err();
        assert!(
            err.contains("sneaky.example.internal") && err.contains("left running"),
            "{err}"
        );
        assert_eq!(f.count("osascript"), prompts);
        assert_eq!(f.count("launchctl bootout"), bootouts);
        assert!(f.paths.plist.exists() && f.paths.binary.exists());
        assert!(f.resolver.join("lab.example.internal").exists());
    }

    fn hosts_fixture(f: &Fake, content: &str) -> PathBuf {
        let p = f.home.join("hosts.live");
        std::fs::write(&p, content).unwrap();
        p
    }

    #[tokio::test(start_paused = true)]
    async fn exit_8_makes_one_call_skips_removals_and_reconciles_from_disk() {
        let f = started();
        f.put_marker_file("old.example.internal", "prof-a");
        write_zones(
            &f.paths.zones,
            &zones(3, &[("old.example.internal", "prof-a")]),
        )
        .unwrap();
        f.writes(&[("lab.example.internal", "prof-a")]);
        f.set(|b| b.osa = Osa::HostsChanged(1));
        let hosts = hosts_fixture(&f, "orig\n");
        let computed = std::sync::atomic::AtomicUsize::new(0);
        let compute = |s: &str| {
            computed.fetch_add(1, Ordering::Relaxed);
            format!("{s}192.0.2.1 x.example.internal\n")
        };

        let err = apply(
            &f.env(),
            ApplyRequest {
                zones: zones(0, &[("lab.example.internal", "prof-a")]),
                approved_switches: &[],
                hosts: Some(HostsUpdate {
                    path: &hosts,
                    compute: &compute,
                }),
                scope_profile: None,
            },
        )
        .await
        .unwrap_err();
        assert!(
            err.contains("hosts file changed concurrently; press Sync again"),
            "{err}"
        );
        // D-ONEPROMPT: one privileged call, no retry, hosts computed once.
        assert_eq!(f.count("osascript"), 1);
        assert_eq!(computed.load(Ordering::Relaxed), 1);
        let script = f.scripts.lock().unwrap()[0].clone();
        assert!(script.contains("shasum") && script.contains("exit 8"));
        // D-MBB: additive hosts step sits between the resolver writes and removals.
        let mkdir = script.find("/bin/mkdir").unwrap();
        let cp = script.find("/bin/cp -P '").unwrap();
        let rm = script.find("/bin/rm -f '").unwrap();
        assert!(mkdir < cp && cp < rm, "{script}");
        // Reconciled from disk: the new file landed, the removal never ran.
        assert!(f.resolver.join("old.example.internal").exists());
        let mut served = f.served_zones();
        served.sort();
        assert_eq!(served, vec!["lab.example.internal", "old.example.internal"]);
    }

    #[test]
    fn hosts_additive_classification_compares_snapshot_and_new_content() {
        assert!(hosts_is_additive("a\nb\n", "a\nb\nc\n"));
        assert!(hosts_is_additive("a\n", "a\n"));
        assert!(!hosts_is_additive("a\nb\n", "a\n"));
        assert!(!hosts_is_additive("a\na\n", "a\nb\n"), "multiset, not set");
    }

    #[tokio::test(start_paused = true)]
    async fn hosts_step_failure_after_resolver_success_is_reported_with_disk_reconciled() {
        let f = started();
        f.writes(&[
            ("a.example.internal", "prof-a"),
            ("b.example.internal", "prof-a"),
        ]);
        f.set(|b| b.osa = Osa::Partial(2)); // both resolver files land, the script then fails
        let hosts = hosts_fixture(&f, "orig\n");
        let compute = |s: &str| format!("{s}192.0.2.1 x.example.internal\n");

        let err = apply(
            &f.env(),
            ApplyRequest {
                zones: zones(
                    0,
                    &[
                        ("a.example.internal", "prof-a"),
                        ("b.example.internal", "prof-a"),
                    ],
                ),
                approved_switches: &[],
                hosts: Some(HostsUpdate {
                    path: &hosts,
                    compute: &compute,
                }),
                scope_profile: None,
            },
        )
        .await
        .unwrap_err();
        assert!(err.contains("boom"), "{err}");
        let mut served = f.served_zones();
        served.sort();
        assert_eq!(served, vec!["a.example.internal", "b.example.internal"]);
    }

    #[tokio::test(start_paused = true)]
    async fn unchanged_hosts_and_empty_plan_make_no_prompt() {
        let f = started();
        f.put_marker_file("lab.example.internal", "prof-a");
        let hosts = hosts_fixture(&f, "same\n");
        let compute = |s: &str| s.to_string();
        apply(
            &f.env(),
            ApplyRequest {
                zones: zones(0, &[("lab.example.internal", "prof-a")]),
                approved_switches: &[],
                hosts: Some(HostsUpdate {
                    path: &hosts,
                    compute: &compute,
                }),
                scope_profile: None,
            },
        )
        .await
        .unwrap();
        assert_eq!(f.count("osascript"), 0, "a no-op snapshot must not prompt");
    }

    #[tokio::test(start_paused = true)]
    async fn script_exit_7_runs_the_d_dead_cleanup() {
        let f = started();
        f.put_marker_file("keep.example.internal", "prof-a");
        f.writes(&[("lab.example.internal", "prof-a")]);
        f.set(|b| b.osa = Osa::ListenerMismatch);

        let err = apply(
            &f.env(),
            req(zones(0, &[("lab.example.internal", "prof-a")])),
        )
        .await
        .unwrap_err();
        assert!(
            err.contains("local DNS responder unavailable; resolver entries removed"),
            "{err}"
        );
        // D-DEAD-2: the failed script plus the recovery removal.
        assert_eq!(f.count("osascript"), 2);
        assert!(!f.resolver.join("keep.example.internal").exists());
        assert!(!f.resolver.join("lab.example.internal").exists());
        assert!(f.served_zones().is_empty());
    }

    #[tokio::test(start_paused = true)]
    async fn g2_status_timeout_after_writes_runs_the_d_dead_cleanup() {
        let f = started();
        f.put_marker_file("old.example.internal", "prof-a");
        write_zones(
            &f.paths.zones,
            &zones(3, &[("old.example.internal", "prof-a")]),
        )
        .unwrap();
        f.writes(&[("lab.example.internal", "prof-a")]);
        f.set(|b| b.kill_responder_on_script = true);

        let err = apply(
            &f.env(),
            req(zones(0, &[("lab.example.internal", "prof-a")])),
        )
        .await
        .unwrap_err();
        assert!(
            err.contains("resolver entries removed") && err.contains("did not load generation"),
            "{err}"
        );
        assert_eq!(
            f.count("osascript"),
            2,
            "D-DEAD-2: recovery is a 2nd prompt"
        );
        assert!(!f.resolver.join("lab.example.internal").exists());
        assert!(f.served_zones().is_empty());
    }

    /// Fails each runner call in turn (every await point of the gate and of the post-script
    /// verification). Whatever fails, the outcome is either success or the D-DEAD cleanup; no
    /// `?` path may leave our resolver files pointing at an unverified responder.
    #[tokio::test(start_paused = true)]
    async fn every_failing_await_point_ends_in_success_or_the_d_dead_cleanup() {
        let mut cleanups = 0;
        for with_retained in [false, true] {
            for k in 0..16 {
                let f = started();
                f.put_marker_file("keep.example.internal", "prof-a");
                if with_retained {
                    write_zones(
                        &f.paths.zones,
                        &zones(3, &[("keep.example.internal", "prof-a")]),
                    )
                    .unwrap();
                    // keep is not desired, so it is removed -> exercises the g2 phase.
                } else {
                    // Desired too: no removal, single generation.
                    f.writes(&[]);
                }
                let desired: &[(&str, &str)] = if with_retained {
                    &[("lab.example.internal", "prof-a")]
                } else {
                    &[
                        ("keep.example.internal", "prof-a"),
                        ("lab.example.internal", "prof-a"),
                    ]
                };
                f.writes(&[("lab.example.internal", "prof-a")]);
                f.set(|b| b.fail_at = Some(k));

                match apply(&f.env(), req(zones(0, desired))).await {
                    Ok(_) => {}
                    Err(e) => {
                        assert!(
                            e.contains("local DNS responder unavailable"),
                            "retained={with_retained} k={k}: {e}"
                        );
                        assert!(
                            f.count("osascript") >= 1,
                            "retained={with_retained} k={k}: no cleanup prompt: {e}"
                        );
                        assert!(
                            !f.resolver.join("keep.example.internal").exists()
                                && !f.resolver.join("lab.example.internal").exists(),
                            "retained={with_retained} k={k}: entries left behind: {e}"
                        );
                        cleanups += 1;
                    }
                }
            }
        }
        assert!(cleanups >= 6, "injection should hit many paths: {cleanups}");
    }
}
