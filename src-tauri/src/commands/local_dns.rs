//! ADR-0011 phase 2 lane E: wires the built-in local DNS (zones builder + agent/manager) into the
//! sync/remove commands, the profile lifecycle and app start.
//!
//! Everything funnels through `reapply`: it rebuilds the zones of ALL `LocalDns` profiles (using
//! each profile's last-known endpoints) and hands them to `manager::apply`, so syncing one
//! profile never drops another profile's records.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex as StdMutex;

use serde::{Deserialize, Serialize};

use crate::commands::connection::SyncHostsResult;
use crate::services::config::{HostMapping, Profile};
use crate::services::hosts_file::{
    compute_updated_hosts_content, get_profile_hosts_block, render_hosts_block_with_endpoints,
    HOSTS_FILE_PATH,
};
use crate::services::k8s_endpoints::DiscoveredEndpoint;
use crate::services::local_dns::agent::{self, AgentPaths};
use crate::services::local_dns::manager::{self, ApplyRequest, HostsUpdate, ManagerEnv};
use crate::services::local_dns::zone_file::{load_status, ZonesFile, ZONES_FILE_VERSION};
use crate::services::local_dns::zones_builder::{build_zones, SkippedReasonKind};
use crate::services::paths::ClusterDeckPaths;
use crate::services::process::SystemRunner;
use crate::services::resolver_files::{
    parse_marker, Conflict, ConflictKind, DEFAULT_DNS_PORT, RESOLVER_DIR,
};
use crate::services::store::{self, ProfileWriteGuard};
use crate::services::validate::{
    is_allowed_private_zone, is_reserved_clusterdeck_zone, is_safe_host_domain,
};

const ENDPOINTS_CACHE_FILE: &str = "endpoints.json";

/// Serializes the cache's read-modify-write cycles (separate from the manager's mutex, which
/// only covers apply/teardown/sweep).
static CACHE_LOCK: StdMutex<()> = StdMutex::new(());

static LOCAL_DNS_LANE_MUTEX: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// Process-wide mutual exclusion for the local DNS lane (ADR-0011).
/// Serializes the entire sequence: endpoints-cache update → load profiles → build_zones → manager::apply / teardown.
pub async fn lock_lane() -> tokio::sync::MutexGuard<'static, ()> {
    LOCAL_DNS_LANE_MUTEX.lock().await
}

// ---------- DTOs (serialized to the frontend) ----------

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DnsConflict {
    pub zone: String,
    /// `unowned`, `owned_by_other_profile`, `invalid_zone`, `unreadable` or `claimed_by_multiple`.
    pub kind: String,
    /// Claimant profile(s) of the zone; for `owned_by_other_profile` the on-disk owner is last.
    pub profile_ids: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DnsSkippedHost {
    pub profile_id: String,
    pub host: String,
    pub reason: String,
    pub reason_kind: SkippedReasonKind,
    /// Parent domains the user could approve to cover this host (only for hosts that fell
    /// outside every approved zone).
    pub suggested_zones: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct DnsApplyView {
    pub conflicts: Vec<DnsConflict>,
    pub skipped_hosts: Vec<DnsSkippedHost>,
    pub prompted: bool,
    pub cancelled: bool,
}

impl DnsApplyView {
    /// Only what concerns `profile_id` (the build covers every profile).
    fn for_profile(mut self, profile_id: &str) -> Self {
        self.conflicts
            .retain(|c| c.profile_ids.iter().any(|p| p == profile_id));
        self.skipped_hosts.retain(|s| s.profile_id == profile_id);
        self
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct LocalDnsZoneView {
    pub zone: String,
    pub owner: String,
    pub records: usize,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct LocalDnsStatus {
    pub agent_loaded: bool,
    pub pid: Option<u32>,
    /// Generation of the zones.json currently served.
    pub generation: u64,
    /// Generation the responder reports as loaded (`None` without a status file).
    pub loaded_generation: Option<u64>,
    pub last_error: Option<String>,
    pub port: u16,
    pub zones: Vec<LocalDnsZoneView>,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct LocalDnsDisableResult {
    pub removed: Vec<String>,
    pub cancelled: bool,
    pub agent_removed: bool,
}

// ---------- real-system environment ----------

/// Owns what `ManagerEnv` borrows, for the production commands.
pub struct SystemDns {
    runner: SystemRunner,
    paths: AgentPaths,
    exe: PathBuf,
}

impl SystemDns {
    pub fn resolve() -> Result<Self, String> {
        Ok(Self {
            runner: SystemRunner,
            paths: AgentPaths::resolve()?,
            exe: std::env::current_exe().map_err(|e| format!("cannot locate executable: {e}"))?,
        })
    }

    pub fn env(&self) -> ManagerEnv<'_> {
        ManagerEnv::new(
            &self.runner,
            &self.paths,
            Path::new(RESOLVER_DIR),
            &self.exe,
            DEFAULT_DNS_PORT,
        )
    }
}

// ---------- last-known endpoints cache ----------

type EndpointCache = HashMap<String, Vec<DiscoveredEndpoint>>;

fn cache_path(env: &ManagerEnv<'_>) -> PathBuf {
    env.paths.dns_dir.join(ENDPOINTS_CACHE_FILE)
}

/// A missing or unreadable cache is an empty one: records reappear on the next sync.
fn cache_load(path: &Path) -> EndpointCache {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

fn cache_update(path: &Path, f: impl FnOnce(&mut EndpointCache)) -> Result<(), String> {
    let _guard = CACHE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let mut cache = cache_load(path);
    f(&mut cache);
    let json = serde_json::to_vec(&cache).map_err(|e| e.to_string())?;
    agent::atomic_write(path, &json, 0o600)
}

// ---------- core ----------

fn conflict_kind(kind: &ConflictKind) -> (&'static str, Option<&str>) {
    match kind {
        ConflictKind::Unowned => ("unowned", None),
        ConflictKind::OwnedByOtherProfile { owner } => {
            ("owned_by_other_profile", Some(owner.as_str()))
        }
        ConflictKind::InvalidZone => ("invalid_zone", None),
        ConflictKind::Unreadable => ("unreadable", None),
    }
}

/// `(zone, on-disk owner, new owner)` for every zone whose resolver file carries another
/// profile's marker. The owner change itself was an explicit user action (`switch_dns_zone_owner`
/// moved it in profiles.yaml; zones have a single owner there), so the file marker disagreeing
/// with the config is exactly "an approved switch" (R11).
fn approved_switches(resolver_dir: &Path, zones: &ZonesFile) -> Vec<(String, String, String)> {
    zones
        .zones
        .iter()
        .filter(|z| is_allowed_private_zone(&z.zone))
        .filter_map(|z| {
            let path = resolver_dir.join(&z.zone);
            if !std::fs::symlink_metadata(&path).ok()?.is_file() {
                return None;
            }
            let owner = parse_marker(&std::fs::read_to_string(&path).ok()?)?;
            (owner != z.profile_id).then(|| (z.zone.clone(), owner, z.profile_id.clone()))
        })
        .collect()
}

/// Parent domains of `host` that the user could approve (private-zone rules, never the reserved
/// alias zone), most specific first.
fn suggested_zones(host: &str) -> Vec<String> {
    let host = host.to_ascii_lowercase();
    let mut out = Vec::new();
    let mut rest = host.as_str();
    while let Some((_, parent)) = rest.split_once('.') {
        if is_allowed_private_zone(parent) && !is_reserved_clusterdeck_zone(parent) {
            out.push(parent.to_string());
        }
        rest = parent;
    }
    out
}

async fn disable(
    env: &ManagerEnv<'_>,
    hosts: Option<HostsUpdate<'_>>,
) -> Result<DnsApplyView, String> {
    let mut view = DnsApplyView::default();
    if hosts.is_some() {
        // The hosts change and the removal of our resolver files share one script (one prompt).
        let empty = ZonesFile {
            version: ZONES_FILE_VERSION,
            generation: 0,
            port: env.port,
            zones: Vec::new(),
        };
        let res = manager::apply(
            env,
            ApplyRequest {
                zones: empty,
                approved_switches: &[],
                hosts,
            },
        )
        .await?;
        view.prompted = res.prompted;
        if res.cancelled {
            view.cancelled = true;
            return Ok(view);
        }
    }
    let torn = manager::teardown(env).await?;
    view.prompted |= !torn.removed.is_empty() || torn.cancelled;
    view.cancelled |= torn.cancelled;
    Ok(view)
}

async fn reapply_locked(
    paths: &ClusterDeckPaths,
    env: &ManagerEnv<'_>,
    exclude: Option<&str>,
    hosts: Option<HostsUpdate<'_>>,
    record: Option<(&str, &[DiscoveredEndpoint])>,
) -> Result<(DnsApplyView, EndpointCache), String> {
    let profiles: Vec<Profile> = store::load_profiles(paths)?
        .into_iter()
        .filter(|p| Some(p.id.as_str()) != exclude)
        .collect();

    let cache_file = cache_path(env);
    if let Some((id, endpoints)) = record {
        // Under the lane mutex, only write cache entries for profiles that still exist.
        if profiles.iter().any(|p| p.id == id) {
            cache_update(&cache_file, |c| {
                c.insert(id.to_string(), endpoints.to_vec());
            })?;
        }
    }
    if let Some(id) = exclude {
        cache_update(&cache_file, |c| {
            c.remove(id);
        })?;
    }

    let cached = cache_load(&cache_file);

    if !profiles
        .iter()
        .any(|p| p.host_mapping == HostMapping::LocalDns)
    {
        let view = disable(env, hosts).await?;
        return Ok((view, cached));
    }

    // The generation is assigned by the manager; 0 here is ignored.
    let build = build_zones(&profiles, &cached, 0, env.port);
    let switches = approved_switches(env.resolver_dir, &build.zones);
    let owner_of: HashMap<&str, &str> = build
        .zones
        .zones
        .iter()
        .map(|z| (z.zone.as_str(), z.profile_id.as_str()))
        .collect();

    let result = manager::apply(
        env,
        ApplyRequest {
            zones: build.zones.clone(),
            approved_switches: &switches,
            hosts,
        },
    )
    .await?;

    let mut conflicts: Vec<DnsConflict> = build
        .conflicts
        .iter()
        .map(|c| DnsConflict {
            zone: c.zone.clone(),
            kind: "claimed_by_multiple".to_string(),
            profile_ids: c.profile_ids.clone(),
        })
        .collect();
    conflicts.extend(result.conflicts.iter().map(|c: &Conflict| {
        let (kind, other) = conflict_kind(&c.kind);
        let mut ids: Vec<String> = owner_of
            .get(c.zone.as_str())
            .map(|o| o.to_string())
            .into_iter()
            .collect();
        ids.extend(other.map(String::from));
        DnsConflict {
            zone: c.zone.clone(),
            kind: kind.to_string(),
            profile_ids: ids,
        }
    }));

    let skipped_hosts = build
        .skipped
        .iter()
        .map(|s| DnsSkippedHost {
            profile_id: s.profile_id.clone(),
            host: s.host.clone(),
            reason: s.reason.clone(),
            reason_kind: s.reason_kind,
            suggested_zones: if s.reason_kind == SkippedReasonKind::OutsideZones {
                suggested_zones(&s.host)
            } else {
                Vec::new()
            },
        })
        .collect();

    Ok((
        DnsApplyView {
            conflicts,
            skipped_hosts,
            prompted: result.prompted,
            cancelled: result.cancelled,
        },
        cached,
    ))
}

/// Rebuilds and applies the zones of every `LocalDns` profile. `exclude` drops one profile from
/// the set (removal/deletion); `record` stores that profile's freshly discovered endpoints first.
/// With no `LocalDns` profile left the resolver files and the agent are torn down.
pub async fn reapply(
    paths: &ClusterDeckPaths,
    env: &ManagerEnv<'_>,
    exclude: Option<&str>,
    hosts: Option<HostsUpdate<'_>>,
    record: Option<(&str, &[DiscoveredEndpoint])>,
) -> Result<DnsApplyView, String> {
    let _lane_guard = lock_lane().await;
    let (view, _) = reapply_locked(paths, env, exclude, hosts, record).await?;
    Ok(view)
}

/// Whether /etc/hosts still has this profile's block, so a hosts step (and its prompt) is only
/// requested when there is something to strip.
fn stale_block_present(hosts_path: &Path, profile_id: &str) -> bool {
    let existing = std::fs::read_to_string(hosts_path).unwrap_or_default();
    get_profile_hosts_block(&existing, profile_id).is_some()
}

fn sanitize_discovery_error(err: &str) -> String {
    let first = err
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("");
    if first.chars().count() > 120 {
        let truncated: String = first.chars().take(117).collect();
        format!("{truncated}...")
    } else {
        first.to_string()
    }
}

fn is_connection_or_tunnel_down(err: &str) -> bool {
    let lower = err.to_ascii_lowercase();
    lower.contains("connect first")
        || lower.contains("kubeconfig not found")
        || lower.contains("connection refused")
        || lower.contains("unable to connect")
        || lower.contains("failed to connect")
        || lower.contains("connection reset")
        || lower.contains("connection closed")
        || lower.contains("broken pipe")
        || lower.contains("network is unreachable")
        || lower.contains("no route to host")
        || lower.contains("timed out")
        || lower.contains("timeout")
        || lower.contains("i/o timeout")
        || lower.contains("unreachable")
        || lower.contains("could not resolve host")
        || lower.contains("dial tcp")
}

pub const DISCOVERY_FAILED_WARNING: &str =
    "Could not discover endpoints (Connect first); serving the last known records.";

/// `sync_hosts_file_cmd` for a `LocalDns` profile. A leftover /etc/hosts block from the Hosts
/// mode is removed in the same privileged script as the resolver changes.
pub async fn sync_local_dns_profile(
    paths: &ClusterDeckPaths,
    env: &ManagerEnv<'_>,
    hosts_path: &Path,
    profile: &Profile,
    endpoints: Result<Vec<DiscoveredEndpoint>, &str>,
) -> Result<SyncHostsResult, String> {
    let id = profile.id.clone();
    let strip = move |existing: &str| compute_updated_hosts_content(existing, &id, None);
    let hosts = stale_block_present(hosts_path, &profile.id).then_some(HostsUpdate {
        path: hosts_path,
        compute: &strip,
    });

    let (record, discovery_err) = match &endpoints {
        Ok(eps) => (Some((profile.id.as_str(), eps.as_slice())), None),
        Err(e) => (None, Some(*e)),
    };

    let _lane_guard = lock_lane().await;
    let (view, cached) = reapply_locked(paths, env, None, hosts, record).await?;
    let view = view.for_profile(&profile.id);

    let served_endpoints = match endpoints {
        Ok(eps) => eps,
        Err(_) => cached.get(&profile.id).cloned().unwrap_or_default(),
    };

    let hosts_count = profile.hosts.len() + usize::from(profile.bastion.is_some());
    let endpoints_count = served_endpoints.len();

    let warning_msg = discovery_err.map(|e| {
        let sanitized = sanitize_discovery_error(e);
        let down = is_connection_or_tunnel_down(e) || sanitized.is_empty();
        if down {
            if sanitized.is_empty() {
                DISCOVERY_FAILED_WARNING.to_string()
            } else {
                format!("Could not discover endpoints (Connect first); serving the last known records: {sanitized}")
            }
        } else {
            format!("Endpoint discovery failed: {sanitized}; serving the last known records.")
        }
    });

    let message = if view.cancelled {
        "Admin approval was cancelled; local DNS was not changed".to_string()
    } else if let Some(w) = &warning_msg {
        w.clone()
    } else {
        format!(
            "Local DNS updated: {hosts_count} node alias(es), {endpoints_count} cluster endpoint(s) \
             discovered, {} skipped, {} conflict(s)",
            view.skipped_hosts.len(),
            view.conflicts.len()
        )
    };

    Ok(SyncHostsResult {
        success: !view.cancelled && warning_msg.is_none(),
        endpoints_count,
        hosts_count,
        endpoints: served_endpoints,
        message,
        warning: warning_msg,
        conflicts: vec![],
        dns_conflicts: view.conflicts,
        skipped_hosts: view.skipped_hosts,
        dns_prompted: view.prompted,
    })
}

/// Refreshes local DNS after endpoint discovery in `connect_profile`.
/// Reloads the profile inside the lane mutex: does nothing (and touches no cache) unless
/// the current profile is LocalDns.
/// Non-fatal on cancel, conflicts or errors: reports them into `errors` and never fails.
pub async fn refresh_local_dns_on_connect(
    paths: &ClusterDeckPaths,
    env: &ManagerEnv<'_>,
    profile_id: &str,
    discovery: Result<&[DiscoveredEndpoint], &str>,
    errors: &mut Vec<String>,
) {
    let _lane_guard = lock_lane().await;
    let profiles = match store::load_profiles(paths) {
        Ok(ps) => ps,
        Err(e) => {
            errors.push(format!("Could not load profiles to refresh local DNS: {e}"));
            return;
        }
    };
    let Some(current) = profiles.into_iter().find(|p| p.id == profile_id) else {
        return;
    };
    if current.host_mapping != HostMapping::LocalDns {
        return;
    }

    let record = match discovery {
        Ok(eps) => Some((profile_id, eps)),
        Err(err) => {
            let sanitized = sanitize_discovery_error(err);
            if sanitized.is_empty() {
                errors.push(
                    "Could not discover endpoints; serving the last known records.".to_string(),
                );
            } else {
                errors.push(format!(
                    "Could not discover endpoints; serving the last known records: {sanitized}"
                ));
            }
            None
        }
    };

    match reapply_locked(paths, env, None, None, record).await {
        Ok((view, _)) => {
            let view = view.for_profile(profile_id);
            if view.cancelled {
                errors.push("Admin approval was cancelled; local DNS was not changed".to_string());
            }
            for conflict in view.conflicts {
                errors.push(format!(
                    "Local DNS conflict on zone {}: {}",
                    conflict.zone, conflict.kind
                ));
            }
        }
        Err(e) => {
            errors.push(format!("Local DNS update failed: {e}"));
        }
    }
}

/// `remove_hosts_file_cmd` for a `LocalDns` profile: its zones leave local DNS (the profile
/// stays configured; the next sync restores them) and any stale hosts block goes with them.
pub async fn remove_local_dns_profile(
    paths: &ClusterDeckPaths,
    env: &ManagerEnv<'_>,
    hosts_path: &Path,
    profile_id: &str,
) -> Result<SyncHostsResult, String> {
    let id = profile_id.to_string();
    let strip = move |existing: &str| compute_updated_hosts_content(existing, &id, None);
    let hosts = stale_block_present(hosts_path, profile_id).then_some(HostsUpdate {
        path: hosts_path,
        compute: &strip,
    });
    let view = reapply(paths, env, Some(profile_id), hosts, None).await?;
    Ok(SyncHostsResult {
        success: !view.cancelled,
        endpoints_count: 0,
        hosts_count: 0,
        endpoints: vec![],
        message: if view.cancelled {
            "Admin approval was cancelled; local DNS was not changed".to_string()
        } else {
            format!("Removed profile '{profile_id}' from local DNS")
        },
        warning: None,
        conflicts: vec![],
        dns_conflicts: vec![],
        skipped_hosts: vec![],
        dns_prompted: view.prompted,
    })
}

/// Called after a profile was removed from the store. Its zones leave local DNS; the last
/// `LocalDns` profile takes the agent with it.
pub async fn reapply_after_delete(
    paths: &ClusterDeckPaths,
    env: &ManagerEnv<'_>,
    profile_id: &str,
) -> Result<DnsApplyView, String> {
    reapply(paths, env, Some(profile_id), None, None).await
}

fn node_aliases_eq(a: Option<&Profile>, b: &Profile) -> bool {
    let Some(a) = a else { return false };
    let a_hosts = a.hosts.iter().map(|h| (&h.name, &h.address));
    let b_hosts = b.hosts.iter().map(|h| (&h.name, &h.address));
    let a_bastion = a.bastion.as_ref().map(|b| (&b.name, &b.address));
    let b_bastion = b.bastion.as_ref().map(|b| (&b.name, &b.address));
    a_hosts.eq(b_hosts) && a_bastion == b_bastion
}

/// Whether saving `new` over `old` requires a local DNS reapply.
pub fn needs_reapply_after_save(old: Option<&Profile>, new: &Profile) -> bool {
    let old_mapping = old.map_or(HostMapping::Off, |p| p.host_mapping);
    if old_mapping != HostMapping::LocalDns && new.host_mapping != HostMapping::LocalDns {
        return false;
    }
    let unchanged = old_mapping == new.host_mapping
        && old.map_or(Vec::new(), |p| p.approved_dns_zones.clone()) == new.approved_dns_zones
        && old.and_then(|p| p.gateway_address.clone()) == new.gateway_address
        && node_aliases_eq(old, new);
    !unchanged
}

/// Called after a profile was saved. Re-applies only when a `LocalDns` profile's DNS-relevant
/// settings changed; a Hosts<->LocalDns switch carries the matching hosts change into the same
/// privileged script (one prompt, never a window with neither mapping, R11).
pub async fn reapply_after_save(
    paths: &ClusterDeckPaths,
    env: &ManagerEnv<'_>,
    hosts_path: &Path,
    old: Option<&Profile>,
    new: &Profile,
) -> Result<Option<DnsApplyView>, String> {
    if !needs_reapply_after_save(old, new) {
        return Ok(None);
    }

    let old_mapping = old.map_or(HostMapping::Off, |p| p.host_mapping);
    let id = new.id.clone();
    let strip = {
        let id = id.clone();
        move |existing: &str| compute_updated_hosts_content(existing, &id, None)
    };
    let block = if old_mapping == HostMapping::LocalDns && new.host_mapping == HostMapping::Hosts {
        let cached = cache_load(&cache_path(env)).remove(&id).unwrap_or_default();
        Some(render_hosts_block_with_endpoints(new, &cached)?)
    } else {
        None
    };
    let add = move |existing: &str| compute_updated_hosts_content(existing, &id, block.as_deref());
    let hosts = match (old_mapping, new.host_mapping) {
        (HostMapping::Hosts, HostMapping::LocalDns) if stale_block_present(hosts_path, &new.id) => {
            Some(HostsUpdate {
                path: hosts_path,
                compute: &strip,
            })
        }
        (HostMapping::LocalDns, HostMapping::Hosts) => Some(HostsUpdate {
            path: hosts_path,
            compute: &add,
        }),
        _ => None,
    };
    reapply(paths, env, None, hosts, None).await.map(Some)
}

pub async fn status_with(env: &ManagerEnv<'_>) -> LocalDnsStatus {
    let mut out = LocalDnsStatus {
        port: env.port,
        ..LocalDnsStatus::default()
    };
    match agent::current_uid(env.runner).await {
        Ok(uid) => {
            out.agent_loaded = agent::is_loaded(env.runner, uid).await.unwrap_or(false);
            out.pid = agent::agent_pid(env.runner, uid).await.ok().flatten();
        }
        Err(e) => out.last_error = Some(e),
    }
    if let Ok(status) = load_status(&env.paths.status) {
        out.loaded_generation = Some(status.generation);
        if status.error.is_some() {
            out.last_error = status.error;
        }
    }
    if let Some(zones) = std::fs::read_to_string(&env.paths.zones)
        .ok()
        .and_then(|s| serde_json::from_str::<ZonesFile>(&s).ok())
    {
        out.generation = zones.generation;
        out.zones = zones
            .zones
            .into_iter()
            .map(|z| LocalDnsZoneView {
                zone: z.zone,
                owner: z.profile_id,
                records: z.records.len(),
            })
            .collect();
    }
    out
}

/// App start (D4): remove our resolver files if nothing uses local DNS or the agent does not own
/// the port (no prompt unless such files exist), then keep the agent's binary copy current.
pub async fn startup_with(paths: &ClusterDeckPaths, env: &ManagerEnv<'_>) -> Result<(), String> {
    let _lane_guard = lock_lane().await;
    let any_local_dns = store::load_profiles(paths)?
        .iter()
        .any(|p| p.host_mapping == HostMapping::LocalDns);
    let sweep = manager::startup_sweep(env, any_local_dns).await;
    let refresh = if any_local_dns {
        manager::refresh_binary(env).await.map(|_| ())
    } else {
        Ok(())
    };
    sweep.map(|_| ()).and(refresh)
}

/// Spawned from `lib.rs` setup; never blocks startup and only logs failures.
pub async fn startup_task() {
    let result = async {
        let paths = ClusterDeckPaths::resolve()?;
        let dns = SystemDns::resolve()?;
        startup_with(&paths, &dns.env()).await
    }
    .await;
    if let Err(e) = result {
        eprintln!("[ClusterDeck] local DNS startup check failed: {e}");
    }
}

// ---------- commands ----------

/// Adds/removes a zone on a profile under the write lock, then re-applies outside it (the
/// admin prompt must not hold up other profile writes).
async fn mutate_then_apply(
    guard: &ProfileWriteGuard,
    profile_id: &str,
    mutate: impl FnOnce(&ClusterDeckPaths, &str) -> Result<(), String>,
) -> Result<DnsApplyView, String> {
    let paths = ClusterDeckPaths::resolve()?;
    let profile = {
        let _lock = guard.lock().await;
        mutate(&paths, profile_id)?;
        store::get_profile(&paths, profile_id)?
    };
    if profile.host_mapping != HostMapping::LocalDns {
        return Ok(DnsApplyView::default());
    }
    let dns = SystemDns::resolve()?;
    reapply(&paths, &dns.env(), None, None, None).await
}

#[tauri::command]
pub async fn approve_dns_zone(
    guard: tauri::State<'_, ProfileWriteGuard>,
    profile_id: String,
    zone: String,
) -> Result<DnsApplyView, String> {
    mutate_then_apply(&guard, &profile_id, |paths, id| {
        let mut profile = store::get_profile(paths, id)?;
        if !profile.approved_dns_zones.contains(&zone) {
            profile.approved_dns_zones.push(zone.clone());
        }
        // Validates the zone and rejects one another profile owns (that is a switch).
        store::upsert_profile(paths, profile)
    })
    .await
}

#[tauri::command]
pub async fn revoke_dns_zone(
    guard: tauri::State<'_, ProfileWriteGuard>,
    profile_id: String,
    zone: String,
) -> Result<DnsApplyView, String> {
    mutate_then_apply(&guard, &profile_id, |paths, id| {
        let mut profile = store::get_profile(paths, id)?;
        profile.approved_dns_zones.retain(|z| z != &zone);
        store::upsert_profile(paths, profile)
    })
    .await
}

#[tauri::command]
pub async fn switch_dns_zone_owner(
    guard: tauri::State<'_, ProfileWriteGuard>,
    zone: String,
    to_profile: String,
) -> Result<DnsApplyView, String> {
    mutate_then_apply(&guard, &to_profile, |paths, id| {
        store::switch_dns_zone_owner(paths, &zone, id)
    })
    .await
}

#[tauri::command]
pub async fn local_dns_status() -> Result<LocalDnsStatus, String> {
    let dns = SystemDns::resolve()?;
    Ok(status_with(&dns.env()).await)
}

#[tauri::command]
pub async fn local_dns_disable() -> Result<LocalDnsDisableResult, String> {
    let _lane_guard = lock_lane().await;
    let dns = SystemDns::resolve()?;
    let torn = manager::teardown(&dns.env()).await?;
    Ok(LocalDnsDisableResult {
        removed: torn.removed,
        cancelled: torn.cancelled,
        agent_removed: torn.agent_removed,
    })
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SuggestedDnsZones {
    pub zones: Vec<String>,
    pub by_host: HashMap<String, Vec<String>>,
}

const MAX_SUGGEST_HOSTS: usize = 1000;

#[tauri::command]
pub fn suggest_dns_zones(hosts: Vec<String>) -> SuggestedDnsZones {
    let mut zones = Vec::new();
    let mut by_host = HashMap::new();
    for host in hosts.into_iter().take(MAX_SUGGEST_HOSTS) {
        if !is_safe_host_domain(&host) {
            continue;
        }
        let suggested = suggested_zones(&host);
        for z in &suggested {
            if !zones.contains(z) {
                zones.push(z.clone());
            }
        }
        by_host.insert(host, suggested);
    }
    SuggestedDnsZones { zones, by_host }
}

pub fn hosts_path() -> &'static Path {
    Path::new(HOSTS_FILE_PATH)
}

#[cfg(test)]
mod tests;
