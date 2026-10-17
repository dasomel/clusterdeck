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
    is_allowed_private_zone, is_reserved_clusterdeck_zone, is_safe_host_domain, is_safe_ip_address,
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
    #[serde(default)]
    pub zone_records: HashMap<String, usize>,
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

pub(crate) fn cache_path(env: &ManagerEnv<'_>) -> PathBuf {
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

#[derive(Debug, Clone)]
pub struct EndpointDiscoveryInput {
    pub endpoints: Vec<DiscoveredEndpoint>,
    pub succeeded_kinds: Option<Vec<String>>,
    pub failed_kinds: Vec<String>,
}

impl From<Vec<DiscoveredEndpoint>> for EndpointDiscoveryInput {
    fn from(endpoints: Vec<DiscoveredEndpoint>) -> Self {
        Self {
            endpoints,
            succeeded_kinds: None,
            failed_kinds: Vec::new(),
        }
    }
}

impl From<&[DiscoveredEndpoint]> for EndpointDiscoveryInput {
    fn from(endpoints: &[DiscoveredEndpoint]) -> Self {
        Self {
            endpoints: endpoints.to_vec(),
            succeeded_kinds: None,
            failed_kinds: Vec::new(),
        }
    }
}

impl From<&Vec<DiscoveredEndpoint>> for EndpointDiscoveryInput {
    fn from(endpoints: &Vec<DiscoveredEndpoint>) -> Self {
        Self {
            endpoints: endpoints.clone(),
            succeeded_kinds: None,
            failed_kinds: Vec::new(),
        }
    }
}

impl From<crate::services::k8s_endpoints::DiscoveryResult> for EndpointDiscoveryInput {
    fn from(res: crate::services::k8s_endpoints::DiscoveryResult) -> Self {
        Self {
            endpoints: res.endpoints,
            succeeded_kinds: Some(res.succeeded_kinds),
            failed_kinds: res.failed_kinds,
        }
    }
}

impl From<&crate::services::k8s_endpoints::DiscoveryResult> for EndpointDiscoveryInput {
    fn from(res: &crate::services::k8s_endpoints::DiscoveryResult) -> Self {
        Self {
            endpoints: res.endpoints.clone(),
            succeeded_kinds: Some(res.succeeded_kinds.clone()),
            failed_kinds: res.failed_kinds.clone(),
        }
    }
}

pub struct EndpointRecordUpdate<'a> {
    pub profile_id: &'a str,
    pub endpoints: &'a [DiscoveredEndpoint],
    pub succeeded_kinds: Option<&'a [String]>,
}

impl<'a> From<(&'a str, &'a [DiscoveredEndpoint])> for EndpointRecordUpdate<'a> {
    fn from((profile_id, endpoints): (&'a str, &'a [DiscoveredEndpoint])) -> Self {
        Self {
            profile_id,
            endpoints,
            succeeded_kinds: None,
        }
    }
}

/// Last-known endpoints of one profile (empty when there is no cache entry).
pub(crate) fn cached_endpoints(cache_file: &Path, profile_id: &str) -> Vec<DiscoveredEndpoint> {
    cache_load(cache_file)
        .remove(profile_id)
        .unwrap_or_default()
}

/// Drops a deleted profile's entry (any mapping: Hosts mode writes the cache too), so a later
/// profile with the same id never inherits its kind attribution.
pub(crate) fn remove_cached_endpoints(cache_file: &Path, profile_id: &str) -> Result<(), String> {
    cache_update(cache_file, |c| {
        c.remove(profile_id);
    })
}

/// Kinds in `succeeded` are replaced by exactly their fresh results; everything else already
/// known is kept. `None` means "the whole list is fresh".
pub(crate) fn merge_endpoints(
    existing: Vec<DiscoveredEndpoint>,
    discovered: &[DiscoveredEndpoint],
    succeeded_kinds: Option<&[String]>,
) -> Vec<DiscoveredEndpoint> {
    match succeeded_kinds {
        Some(succeeded) => {
            let mut merged: Vec<DiscoveredEndpoint> = existing
                .into_iter()
                .filter(|ep| !succeeded.iter().any(|k| k == &ep.source))
                .collect();
            for ep in discovered {
                if succeeded.iter().any(|k| k == &ep.source)
                    && is_safe_host_domain(&ep.host)
                    && is_safe_ip_address(&ep.ip)
                {
                    merged.push(ep.clone());
                }
            }
            let mut dedup_map = std::collections::BTreeMap::new();
            for ep in merged {
                if !dedup_map.contains_key(&ep.host) {
                    dedup_map.insert(ep.host.clone(), ep);
                }
            }
            dedup_map.into_values().collect()
        }
        None => discovered
            .iter()
            .filter(|ep| is_safe_host_domain(&ep.host) && is_safe_ip_address(&ep.ip))
            .cloned()
            .collect(),
    }
}

pub fn update_endpoint_cache(
    cache_file: &Path,
    profile_id: &str,
    discovered: &[DiscoveredEndpoint],
    succeeded_kinds: Option<&[String]>,
) -> Result<Vec<DiscoveredEndpoint>, String> {
    let _guard = CACHE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let mut cache = cache_load(cache_file);
    let existing = cache.remove(profile_id).unwrap_or_default();
    let updated = merge_endpoints(existing, discovered, succeeded_kinds);

    cache.insert(profile_id.to_string(), updated.clone());
    let json = serde_json::to_vec(&cache).map_err(|e| e.to_string())?;
    agent::atomic_write(cache_file, &json, 0o600)?;
    Ok(updated)
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
    protect_resolver: bool,
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
                scope_profile: None,
                protect_resolver,
            },
        )
        .await?;
        view.prompted = res.prompted;
        if res.cancelled {
            view.cancelled = true;
            return Ok(view);
        }
    }
    let teardown_env;
    let env_ref = if protect_resolver {
        teardown_env = ManagerEnv {
            runner: env.runner,
            paths: env.paths,
            resolver_dir: env.resolver_dir,
            current_exe: env.current_exe,
            port: env.port,
            status_timeout: env.status_timeout,
            status_poll: env.status_poll,
            protect_resolver: true,
        };
        &teardown_env
    } else {
        env
    };
    let torn = manager::teardown(env_ref).await?;
    view.prompted |= !torn.removed.is_empty() || torn.cancelled;
    view.cancelled |= torn.cancelled;
    Ok(view)
}

fn skipped_hosts_view(
    build: &crate::services::local_dns::zones_builder::BuildOutput,
) -> Vec<DnsSkippedHost> {
    build
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
        .collect()
}

fn zone_record_counts(
    build: &crate::services::local_dns::zones_builder::BuildOutput,
) -> HashMap<String, usize> {
    build
        .zones
        .zones
        .iter()
        .map(|z| (z.zone.clone(), z.records.len()))
        .collect()
}

/// Refreshes the records of the zones already installed for every `LocalDns` profile without any
/// /etc/resolver change, so it can never raise a privileged prompt -- in particular not for
/// another profile's pending or cancelled zones. Pending resolver changes stay pending until
/// the user's explicit Sync / approve action.
async fn reapply_records_only_locked(
    paths: &ClusterDeckPaths,
    env: &ManagerEnv<'_>,
) -> Result<DnsApplyView, String> {
    let profiles = store::load_profiles(paths)?;
    let cached = cache_load(&cache_path(env));
    let build = build_zones(&profiles, &cached, 0, env.port);
    manager::apply_records_only(env, &build.zones.zones).await?;
    Ok(DnsApplyView {
        conflicts: build
            .conflicts
            .iter()
            .map(|c| DnsConflict {
                zone: c.zone.clone(),
                kind: "claimed_by_multiple".to_string(),
                profile_ids: c.profile_ids.clone(),
            })
            .collect(),
        skipped_hosts: skipped_hosts_view(&build),
        zone_records: zone_record_counts(&build),
        ..DnsApplyView::default()
    })
}

pub(crate) async fn reapply_locked<'a>(
    paths: &ClusterDeckPaths,
    env: &ManagerEnv<'_>,
    exclude: Option<&str>,
    hosts: Option<HostsUpdate<'_>>,
    record: Option<EndpointRecordUpdate<'a>>,
    scope: Option<&str>,
) -> Result<(DnsApplyView, EndpointCache), String> {
    let doc = store::load_document(paths)?;
    let protect_resolver = doc.protects_resolver();
    let profiles: Vec<Profile> = doc
        .profiles()
        .into_iter()
        .filter(|p| Some(p.id.as_str()) != exclude)
        .collect();

    let cache_file = cache_path(env);
    if let Some(rec) = record {
        // Under the lane mutex, only write cache entries for profiles that still exist.
        if profiles.iter().any(|p| p.id == rec.profile_id) {
            let _ = update_endpoint_cache(
                &cache_file,
                rec.profile_id,
                rec.endpoints,
                rec.succeeded_kinds,
            )?;
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
        let view = disable(env, hosts, protect_resolver).await?;
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
            scope_profile: scope,
            protect_resolver,
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

    let skipped_hosts = skipped_hosts_view(&build);
    let zone_records = zone_record_counts(&build);

    Ok((
        DnsApplyView {
            conflicts,
            skipped_hosts,
            prompted: result.prompted,
            cancelled: result.cancelled,
            zone_records,
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
    let rec = record.map(|(id, endpoints)| EndpointRecordUpdate {
        profile_id: id,
        endpoints,
        succeeded_kinds: None,
    });
    let (view, _) = reapply_locked(paths, env, exclude, hosts, rec, exclude).await?;
    Ok(view)
}

/// Whether /etc/hosts still has this profile's block, so a hosts step (and its prompt) is only
/// requested when there is something to strip.
pub fn stale_block_present(hosts_path: &Path, profile_id: &str) -> bool {
    let existing = std::fs::read_to_string(hosts_path).unwrap_or_default();
    get_profile_hosts_block(&existing, profile_id).is_some()
}

/// Whether a save of `latest` must also strip a leftover /etc/hosts block. A `Hosts` profile
/// owns its block, so its mere presence is not stale (D-STALE: counting it made every Hosts
/// save run a full reapply).
pub fn stale_block_after_save(hosts_path: &Path, latest: &Profile) -> bool {
    latest.host_mapping != HostMapping::Hosts && stale_block_present(hosts_path, &latest.id)
}

/// Removes only this profile's hosts block (one prompt), leaving resolver files and every
/// other profile's zones alone.
async fn strip_stale_block(
    env: &ManagerEnv<'_>,
    hosts_path: &Path,
    profile_id: &str,
) -> Result<DnsApplyView, String> {
    let mut view = DnsApplyView {
        prompted: true,
        ..DnsApplyView::default()
    };
    match crate::services::hosts_file::remove_hosts_block_at(env.runner, hosts_path, profile_id)
        .await
    {
        Ok(()) => Ok(view),
        // AppleScript error -128 is "User canceled" (same test as the manager's script runner).
        Err(e) if e.contains("User canceled") || e.contains("(-128)") => {
            view.cancelled = true;
            Ok(view)
        }
        Err(e) => Err(e),
    }
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
#[allow(dead_code)]
pub async fn sync_local_dns_profile(
    paths: &ClusterDeckPaths,
    env: &ManagerEnv<'_>,
    hosts_path: &Path,
    profile: &Profile,
    endpoints: Result<Vec<DiscoveredEndpoint>, &str>,
) -> Result<SyncHostsResult, String> {
    let (input, err) = match endpoints {
        Ok(eps) => (
            Some(EndpointDiscoveryInput {
                endpoints: eps,
                succeeded_kinds: None,
                failed_kinds: Vec::new(),
            }),
            None,
        ),
        Err(e) => (None, Some(e)),
    };
    sync_local_dns_profile_impl(paths, env, hosts_path, profile, input, err).await
}

pub async fn sync_local_dns_profile_detailed(
    paths: &ClusterDeckPaths,
    env: &ManagerEnv<'_>,
    hosts_path: &Path,
    profile: &Profile,
    discovery: Result<crate::services::k8s_endpoints::DiscoveryResult, &str>,
) -> Result<SyncHostsResult, String> {
    let (input, err) = match discovery {
        Ok(res) => (
            Some(EndpointDiscoveryInput {
                endpoints: res.endpoints,
                succeeded_kinds: Some(res.succeeded_kinds),
                failed_kinds: res.failed_kinds,
            }),
            None,
        ),
        Err(e) => (None, Some(e)),
    };
    sync_local_dns_profile_impl(paths, env, hosts_path, profile, input, err).await
}

async fn sync_local_dns_profile_impl(
    paths: &ClusterDeckPaths,
    env: &ManagerEnv<'_>,
    hosts_path: &Path,
    profile: &Profile,
    input_opt: Option<EndpointDiscoveryInput>,
    discovery_err: Option<&str>,
) -> Result<SyncHostsResult, String> {
    let _lane_guard = lock_lane().await;
    let profile = store::get_profile(paths, &profile.id)?;
    if profile.host_mapping != HostMapping::LocalDns {
        return Err("Host mapping changed or is off; refresh the profile before syncing".into());
    }
    let id = profile.id.clone();
    let strip = move |existing: &str| compute_updated_hosts_content(existing, &id, None);
    let hosts = stale_block_present(hosts_path, &profile.id).then_some(HostsUpdate {
        path: hosts_path,
        compute: &strip,
    });

    let record = input_opt.as_ref().map(|inp| EndpointRecordUpdate {
        profile_id: profile.id.as_str(),
        endpoints: &inp.endpoints,
        succeeded_kinds: inp.succeeded_kinds.as_deref(),
    });

    let failed_kinds = input_opt
        .as_ref()
        .map(|inp| inp.failed_kinds.clone())
        .unwrap_or_default();

    let (view, cached) = reapply_locked(paths, env, None, hosts, record, Some(&profile.id)).await?;
    let view = view.for_profile(&profile.id);

    let served_endpoints = cached.get(&profile.id).cloned().unwrap_or_default();
    let hosts_count = profile.hosts.len() + usize::from(profile.bastion.is_some());
    let endpoints_count = served_endpoints.len();

    let warning_msg = if !failed_kinds.is_empty() {
        let names: Vec<&str> = failed_kinds
            .iter()
            .map(|k| crate::services::k8s_endpoints::display_kind_name(k))
            .collect();
        Some(format!(
            "Partial endpoint discovery: failed querying {}; keeping cached records.",
            names.join(", ")
        ))
    } else {
        discovery_err.map(|e| {
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
        })
    };

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
#[allow(dead_code)]
pub async fn refresh_local_dns_on_connect(
    paths: &ClusterDeckPaths,
    env: &ManagerEnv<'_>,
    profile_id: &str,
    discovery: Result<&[DiscoveredEndpoint], &str>,
    errors: &mut Vec<String>,
) {
    let (input, err) = match discovery {
        Ok(eps) => (
            Some(EndpointDiscoveryInput {
                endpoints: eps.to_vec(),
                succeeded_kinds: None,
                failed_kinds: Vec::new(),
            }),
            None,
        ),
        Err(e) => (None, Some(e)),
    };
    refresh_local_dns_on_connect_impl(paths, env, profile_id, input, err, errors).await;
}

pub async fn refresh_local_dns_on_connect_detailed(
    paths: &ClusterDeckPaths,
    env: &ManagerEnv<'_>,
    profile_id: &str,
    discovery: Result<&crate::services::k8s_endpoints::DiscoveryResult, &str>,
    errors: &mut Vec<String>,
) {
    let (input, err) = match discovery {
        Ok(res) => (
            Some(EndpointDiscoveryInput {
                endpoints: res.endpoints.clone(),
                succeeded_kinds: Some(res.succeeded_kinds.clone()),
                failed_kinds: res.failed_kinds.clone(),
            }),
            None,
        ),
        Err(e) => (None, Some(e)),
    };
    refresh_local_dns_on_connect_impl(paths, env, profile_id, input, err, errors).await;
}

async fn refresh_local_dns_on_connect_impl(
    paths: &ClusterDeckPaths,
    env: &ManagerEnv<'_>,
    profile_id: &str,
    input_opt: Option<EndpointDiscoveryInput>,
    discovery_err: Option<&str>,
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
    let Some(current) = profiles.iter().find(|p| p.id == profile_id) else {
        return;
    };
    if current.host_mapping != HostMapping::LocalDns {
        return;
    }

    let cache_file = cache_path(env);
    if let Some(inp) = input_opt {
        if let Err(e) = update_endpoint_cache(
            &cache_file,
            profile_id,
            &inp.endpoints,
            inp.succeeded_kinds.as_deref(),
        ) {
            errors.push(format!("Could not update endpoint cache: {e}"));
        }
        if !inp.failed_kinds.is_empty() {
            let names: Vec<&str> = inp
                .failed_kinds
                .iter()
                .map(|k| crate::services::k8s_endpoints::display_kind_name(k))
                .collect();
            errors.push(format!(
                "Partial endpoint discovery: failed querying {}; keeping cached records.",
                names.join(", ")
            ));
        }
    } else if let Some(err) = discovery_err {
        let sanitized = sanitize_discovery_error(err);
        if sanitized.is_empty() {
            errors
                .push("Could not discover endpoints; serving the last known records.".to_string());
        } else {
            errors.push(format!(
                "Could not discover endpoints; serving the last known records: {sanitized}"
            ));
        }
    }

    let listing = match manager::read_resolver_listing(env.resolver_dir) {
        Ok(l) => l,
        Err(e) => {
            errors.push(format!("Could not inspect resolver files: {e}"));
            return;
        }
    };

    let cached = cache_load(&cache_file);
    let build = build_zones(&profiles, &cached, 0, env.port);

    let desired_for_profile: std::collections::HashSet<String> = build
        .zones
        .zones
        .iter()
        .filter(|z| z.profile_id == profile_id)
        .map(|z| z.zone.clone())
        .collect();

    let installed_for_profile: std::collections::HashSet<String> = listing
        .files
        .iter()
        .filter(|(_, content)| parse_marker(content).as_deref() == Some(profile_id))
        .map(|(name, _)| name.clone())
        .collect();

    if desired_for_profile != installed_for_profile {
        errors.push("Local DNS changes pending: press Sync to local DNS".to_string());
    }

    // Connect's refresh updates records only for currently installed resolver zones (ours on disk),
    // with NO /etc/resolver change and NO privileged prompt.
    if let Err(e) = manager::apply_records_only(env, &build.zones.zones).await {
        errors.push(format!("Local DNS update failed: {e}"));
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
#[allow(dead_code)]
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
#[allow(dead_code)]
pub async fn reapply_after_save(
    paths: &ClusterDeckPaths,
    env: &ManagerEnv<'_>,
    hosts_path: &Path,
    old: Option<&Profile>,
    new: &Profile,
) -> Result<Option<DnsApplyView>, String> {
    let _lane_guard = lock_lane().await;
    reapply_after_save_locked(paths, env, hosts_path, old, new).await
}

pub(crate) async fn reapply_after_save_locked(
    paths: &ClusterDeckPaths,
    env: &ManagerEnv<'_>,
    hosts_path: &Path,
    old: Option<&Profile>,
    new: &Profile,
) -> Result<Option<DnsApplyView>, String> {
    // Inside the lock, fetch the LATEST stored profile.
    let latest = store::get_profile(paths, &new.id).unwrap_or_else(|_| new.clone());

    let needs_dns = needs_reapply_after_save(old, &latest);
    let stale = stale_block_after_save(hosts_path, &latest);
    if !needs_dns && !stale {
        return Ok(None);
    }
    if !needs_dns {
        return strip_stale_block(env, hosts_path, &latest.id)
            .await
            .map(Some);
    }

    let old_mapping = old.map_or(HostMapping::Off, |p| p.host_mapping);
    // Same mapping and same approved zones: only records changed (aliases, gateway), so the
    // resolver files stay as they are and no prompt can involve another profile's zones.
    if old_mapping == HostMapping::LocalDns
        && latest.host_mapping == HostMapping::LocalDns
        && old.is_some_and(|p| p.approved_dns_zones == latest.approved_dns_zones)
    {
        let mut view = reapply_records_only_locked(paths, env).await?;
        if stale {
            let stripped = strip_stale_block(env, hosts_path, &latest.id).await?;
            view.prompted |= stripped.prompted;
            view.cancelled |= stripped.cancelled;
        }
        return Ok(Some(view));
    }
    let id = latest.id.clone();
    let strip = {
        let id = id.clone();
        move |existing: &str| compute_updated_hosts_content(existing, &id, None)
    };
    let block = if old_mapping == HostMapping::LocalDns && latest.host_mapping == HostMapping::Hosts
    {
        let cached = cache_load(&cache_path(env)).remove(&id).unwrap_or_default();
        Some(render_hosts_block_with_endpoints(&latest, &cached)?)
    } else {
        None
    };
    let add = move |existing: &str| compute_updated_hosts_content(existing, &id, block.as_deref());
    let hosts = match (old_mapping, latest.host_mapping) {
        (HostMapping::Hosts, HostMapping::LocalDns)
            if stale_block_present(hosts_path, &latest.id) =>
        {
            Some(HostsUpdate {
                path: hosts_path,
                compute: &strip,
            })
        }
        (HostMapping::LocalDns, HostMapping::Hosts) => Some(HostsUpdate {
            path: hosts_path,
            compute: &add,
        }),
        (_, HostMapping::Off) if stale_block_present(hosts_path, &latest.id) => Some(HostsUpdate {
            path: hosts_path,
            compute: &strip,
        }),
        _ => None,
    };
    let (view, _) = reapply_locked(paths, env, None, hosts, None, Some(&latest.id)).await?;
    Ok(Some(view))
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
    let _write_lock = guard.lock().await;
    let _lane_lock = lock_lane().await;
    mutate(&paths, profile_id)?;
    let profile = store::get_profile(&paths, profile_id)?;
    if profile.host_mapping != HostMapping::LocalDns {
        return Ok(DnsApplyView::default());
    }
    let dns = SystemDns::resolve()?;
    let (view, _) = reapply_locked(&paths, &dns.env(), None, None, None, Some(profile_id)).await?;
    Ok(view)
}

#[tauri::command]
pub async fn approve_dns_zone(
    guard: tauri::State<'_, ProfileWriteGuard>,
    profile_id: String,
    zone: String,
) -> Result<DnsApplyView, String> {
    mutate_then_apply(&guard, &profile_id, |paths, id| {
        store::patch_profile(paths, id, |m| {
            crate::services::profile_document::patch::add_zone(m, &zone)
        })
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
        store::patch_profile(paths, id, |m| {
            crate::services::profile_document::patch::remove_zone(m, &zone)
        })
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
    let paths = ClusterDeckPaths::resolve()?;
    let doc = store::load_document(&paths).ok();
    let protect_resolver = doc.as_ref().is_some_and(|d| d.protects_resolver());

    let dns = SystemDns::resolve()?;
    let mut env = dns.env();
    env.protect_resolver = protect_resolver;
    let torn = manager::teardown(&env).await?;
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
