use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::services::config::{AuthMode, Profile};
use crate::services::k8s_endpoints::DiscoveredEndpoint;
use crate::services::kubeconfig::{CurrentContext, KubeconfigSummary};
use crate::services::paths::ClusterDeckPaths;
use crate::services::process::{CommandRunner, SystemRunner};
use crate::services::ssh::{self, BootstrapResult};
use crate::services::ssh_config;
use crate::services::state;
use crate::services::store;
use crate::services::verify::{self, VerificationResult};

#[derive(Debug, Clone, Serialize)]
pub struct HostStageResult {
    pub host: String,
    pub reachable: bool,
    pub detail: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ConnectionResult {
    pub hosts: Vec<HostStageResult>,
    pub aliases_written: bool,
    pub kubeconfig: Option<KubeconfigSummary>,
    pub verification: VerificationResult,
    pub endpoints: Vec<DiscoveredEndpoint>,
    pub errors: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SyncHostsResult {
    pub success: bool,
    pub endpoints_count: usize,
    pub hosts_count: usize,
    pub endpoints: Vec<DiscoveredEndpoint>,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub warning: Option<String>,
    pub conflicts: Vec<crate::services::hosts_file::HostnameConflict>,
    /// Local-DNS (ADR-0011) outcome; empty/false for Hosts profiles.
    #[serde(default)]
    pub dns_conflicts: Vec<crate::commands::local_dns::DnsConflict>,
    #[serde(default)]
    pub skipped_hosts: Vec<crate::commands::local_dns::DnsSkippedHost>,
    #[serde(default)]
    pub dns_prompted: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct HostsFileStatus {
    pub managed_by_profile: bool,
    pub is_synced: bool,
    pub active_entries: Vec<String>,
    pub pending_entries: Vec<String>,
    pub conflicts: Vec<crate::services::hosts_file::HostnameConflict>,
}

/// Loads a profile and, if it was created from a detected Colima/Lima instance
/// (`Profile.local_runtime`), re-resolves its SSH endpoint against a fresh discovery pass before
/// any SSH-using command relies on it (D4) -- Colima/Lima forward a new SSH port on every VM
/// restart, so the address/port saved at profile-creation time can go stale. A changed endpoint
/// is persisted via `store::upsert_profile` (which re-validates) so it survives past this one
/// call. A profile with no `local_runtime` source is returned unchanged, with no command run.
async fn load_profile_fresh(
    runner: &dyn CommandRunner,
    paths: &ClusterDeckPaths,
    profile_id: &str,
) -> Result<Profile, String> {
    let mut profile = store::get_profile(paths, profile_id)?;
    if profile.local_runtime.is_none() {
        return Ok(profile);
    }

    let snapshot = profile.clone();
    if crate::services::local_runtime::refresh_local_runtime_endpoint(runner, &mut profile).await {
        // Re-acquire the write lock and reload fresh from disk before saving, so concurrent
        // editor saves / CA trust updates are preserved, and deleted profiles are not resurrected.
        let _lock = crate::services::store::ProfileWriteGuard::lock_process().await;
        let mut fresh = store::get_profile(paths, profile_id)?;
        if merge_refreshed_endpoints(&mut fresh, &snapshot, &profile) {
            store::upsert_profile(paths, fresh.clone())?;
        }
        return Ok(fresh);
    }
    Ok(profile)
}

/// Copies onto `fresh` (re-read under the write lock) only the endpoint fields (address/port/
/// identity_file) of hosts whose endpoint the refresh actually changed (`refreshed` vs the
/// pre-detect `snapshot`), leaving every other host and field untouched so a concurrent edit
/// survives. Hosts match by name; a lone host on both sides matches regardless of name (the
/// refresh may have renamed it). If `local_runtime` was removed or changed since detection, the
/// detected endpoint belongs to a different VM, so nothing is copied. Returns whether `fresh` changed.
fn merge_refreshed_endpoints(fresh: &mut Profile, snapshot: &Profile, refreshed: &Profile) -> bool {
    let same_source = match (&fresh.local_runtime, &snapshot.local_runtime) {
        (Some(f), Some(s)) => f.provider == s.provider && f.instance == s.instance,
        _ => false,
    };
    if !same_source {
        return false;
    }
    let single = fresh.hosts.len() == 1 && snapshot.hosts.len() == 1;
    let mut changed = false;
    for (before, after) in snapshot.hosts.iter().zip(&refreshed.hosts) {
        if before.address == after.address
            && before.port == after.port
            && before.identity_file == after.identity_file
        {
            continue;
        }
        let idx = fresh
            .hosts
            .iter()
            .position(|h| h.name == before.name)
            .or_else(|| single.then_some(0));
        if let Some(h) = idx.map(|i| &mut fresh.hosts[i]) {
            h.address = after.address.clone();
            h.port = after.port;
            if after.identity_file.is_some() {
                h.identity_file = after.identity_file.clone();
            }
            changed = true;
        }
    }
    changed
}

#[tauri::command]
pub async fn probe_profile_hosts(
    profile_id: String,
    password: Option<String>,
) -> Result<Vec<HostStageResult>, String> {
    let paths = ClusterDeckPaths::resolve()?;
    let runner = SystemRunner;
    let profile = load_profile_fresh(&runner, &paths, &profile_id).await?;
    let pwd = password.as_deref();

    let host_futures = profile.hosts.iter().map(|host| async {
        let probe = ssh::probe_with_retry(
            &runner,
            host,
            profile.bastion.as_ref(),
            1,
            Duration::from_secs(1),
            pwd,
        )
        .await;
        HostStageResult {
            host: host.name.clone(),
            reachable: probe.reachable,
            detail: probe.detail,
        }
    });

    let results = futures::future::join_all(host_futures).await;
    Ok(results)
}

// Split out of `bootstrap_profile` so the password-mode-host skip can be unit tested against a
// FakeRunner, the same way commands/profiles.rs's save_profile_with_paths is -- the tauri
// command itself hardcodes SystemRunner/ClusterDeckPaths::resolve(), which a test should not
// invoke for real.
async fn bootstrap_profile_hosts(
    runner: &dyn CommandRunner,
    profile: &Profile,
    password: &str,
) -> Vec<BootstrapResult> {
    let mut results = Vec::new();
    for host in &profile.hosts {
        // Bootstrap deploys a public key via a one-time password; that's meaningless for a host
        // permanently configured for password auth (it already has a working, ongoing auth
        // method), so skip it rather than pointlessly deploying an unused key. Same rule as
        // connect_profile's bootstrap-to-key fallback.
        if host.auth != AuthMode::Key {
            results.push(BootstrapResult {
                host: host.name.clone(),
                key_deployed: false,
                verified: false,
                detail: "skipped: host uses password authentication".to_string(),
            });
            continue;
        }
        let boot = ssh::bootstrap_host(
            runner,
            host,
            profile.bastion.as_ref(),
            password,
            profile.bootstrap.retries,
            Duration::from_secs(profile.bootstrap.retry_delay_secs),
        )
        .await;
        results.push(boot);
    }
    results
}

#[tauri::command]
pub async fn bootstrap_profile(
    profile_id: String,
    password: String,
) -> Result<Vec<BootstrapResult>, String> {
    let paths = ClusterDeckPaths::resolve()?;
    let runner = SystemRunner;
    let profile = load_profile_fresh(&runner, &paths, &profile_id).await?;

    Ok(bootstrap_profile_hosts(&runner, &profile, &password).await)
}

#[tauri::command]
pub async fn generate_aliases(profile_id: String) -> Result<(), String> {
    let paths = ClusterDeckPaths::resolve()?;
    let runner = SystemRunner;
    let profile = load_profile_fresh(&runner, &paths, &profile_id).await?;

    ssh_config::write_profile_config(&paths, &profile)?;
    let home = std::env::var("HOME").map_err(|e| e.to_string())?;
    let home_ssh_config = PathBuf::from(home).join(".ssh").join("config");
    ssh_config::ensure_ssh_include(&home_ssh_config, &paths)?;
    Ok(())
}

#[tauri::command]
pub async fn fetch_kubeconfig(
    profile_id: String,
    password: Option<String>,
) -> Result<KubeconfigSummary, String> {
    let paths = ClusterDeckPaths::resolve()?;
    let runner = SystemRunner;
    let profile = load_profile_fresh(&runner, &paths, &profile_id).await?;

    crate::services::kubeconfig::fetch_and_store(&runner, &paths, &profile, password.as_deref())
        .await
}

fn resolve_verify_context(profile: &Profile, kubeconfig_path: &Path) -> String {
    if let Some(ctx) = crate::services::kubeconfig::read_current_context(kubeconfig_path) {
        return ctx;
    }
    if let Some(kc) = &profile.kubeconfig {
        if !kc.context.is_empty() {
            return kc.context.clone();
        }
    }
    profile.id.clone()
}

fn has_kubeconfig_to_verify(profile: &Profile, fetched: bool, path: &Path) -> bool {
    if profile.kubeconfig.is_some() {
        fetched
    } else {
        path.exists()
    }
}

// No password parameter on this command: it's a read-only/polling status check, so
// password-auth hosts can't be probed here. Only probe key-auth hosts; if the profile has none
// (all hosts are password-mode), keep whatever a prior Connect/Test Connection call (which do
// take a password) last verified instead of overwriting it with a false "unreachable" just
// because this command has nothing it can check. Split out for unit testing (same rationale as
// bootstrap_profile_hosts above).
async fn compute_ssh_reachability(
    runner: &dyn CommandRunner,
    profile: &Profile,
    cached_ssh: bool,
) -> bool {
    let key_hosts: Vec<_> = profile
        .hosts
        .iter()
        .filter(|h| h.auth == AuthMode::Key)
        .collect();

    if key_hosts.is_empty() {
        return cached_ssh;
    }

    for host in key_hosts {
        let probe = ssh::probe_with_retry(
            runner,
            host,
            profile.bastion.as_ref(),
            1,
            Duration::from_secs(1),
            None,
        )
        .await;
        if probe.reachable {
            return true;
        }
    }
    false
}

/// Re-merges the profile kubeconfig into `~/.kube/config` only if the profile is already
/// merged there. Returns the error message on failure.
async fn auto_merge_if_present(
    runner: &dyn CommandRunner,
    paths: &ClusterDeckPaths,
    profile: &Profile,
    context: CurrentContext,
) -> Option<String> {
    if !crate::services::kubeconfig::is_profile_present_in_user_config(&profile.id) {
        return None;
    }
    crate::services::kubeconfig::merge_profile_kubeconfig_to_user_config(
        runner, paths, profile, false, context,
    )
    .await
    .err()
    .map(|e| format!("automatic ~/.kube/config update failed: {e}"))
}

/// Issue #116: after a successful tunnel restore/verify that reported no port change, the
/// retarget may still have happened on an earlier run whose `~/.kube/config` merge failed or never
/// ran. Re-merges (only this profile's entries, never the whole file) when the user config holds
/// this profile's cluster with a server that differs from the stored kubeconfig's.
async fn reconcile_user_kubeconfig(paths: &ClusterDeckPaths, profile: &Profile) -> Option<String> {
    let user_path = crate::services::kubeconfig::resolve_user_kubeconfig_path().ok()?;
    let bak_dir = crate::services::kubeconfig::resolve_user_kube_bak_dir().ok()?;
    reconcile_user_kubeconfig_at(paths, profile, &user_path, &bak_dir).await
}

/// Path-injected body of [`reconcile_user_kubeconfig`] so tests never touch the real `$HOME`.
async fn reconcile_user_kubeconfig_at(
    paths: &ClusterDeckPaths,
    profile: &Profile,
    user_path: &Path,
    bak_dir: &Path,
) -> Option<String> {
    match crate::services::kubeconfig::user_config_server_is_stale(
        user_path,
        &paths.kubeconfig_file(&profile.id),
    ) {
        Ok(true) => {}
        Ok(false) => return None,
        Err(e) => return Some(format!("cannot check ~/.kube/config: {e}")),
    }
    crate::services::kubeconfig::merge_profile_kubeconfig_to_file(
        paths,
        &profile.id,
        user_path,
        Some(bak_dir),
        false,
        CurrentContext::Preserve,
    )
    .await
    .err()
    .map(|e| format!("automatic ~/.kube/config update failed: {e}"))
}

/// Brings the API tunnel of a bastion profile up for verification, from the live tunnel's target
/// or the persisted one, and points the stored kubeconfig at its port. `Ok(true)` when the
/// kubeconfig's port changed (the preferred port was taken after a restart).
async fn ensure_verify_tunnel(
    tunnels: &crate::services::tunnel::TunnelManager,
    runner: &dyn CommandRunner,
    paths: &ClusterDeckPaths,
    profile: &Profile,
    kubeconfig_path: &Path,
) -> Result<bool, String> {
    let target = match tunnels.live_target(&profile.id).await {
        Some(t) => t,
        None => state::get_tunnel_target(paths, &profile.id)?.ok_or_else(|| {
            "no API tunnel target recorded; reconnect to re-establish the API tunnel".to_string()
        })?,
    };
    // ADR-0009 Security: never (re)start a tunnel for a stored kubeconfig that would not be
    // pointed at it -- fail closed when the file cannot be read.
    let raw = std::fs::read_to_string(kubeconfig_path)
        .map_err(|e| format!("API tunnel refused: cannot read kubeconfig: {e}"))?;
    if let Some(reason) = crate::services::kubeconfig::tunnel_refusal_for_yaml(&raw) {
        return Err(reason.to_string());
    }
    let port = tunnels
        .ensure_tunnel(runner, paths, profile, &target.host, target.port)
        .await
        .map_err(|e| format!("API tunnel failed: {e}"))?;
    // A concurrent Connect may have replaced the file with a refused (direct) kubeconfig while
    // the tunnel was starting; re-check so the tunnel and its target don't outlive it.
    let still_allowed = std::fs::read_to_string(kubeconfig_path)
        .ok()
        .is_some_and(|raw| crate::services::kubeconfig::tunnel_refusal_for_yaml(&raw).is_none());
    if !still_allowed {
        tunnels.retire(paths, &profile.id).await;
        return Err("API tunnel refused: kubeconfig changed while the tunnel was starting".into());
    }
    crate::services::kubeconfig::retarget_tunnel_port(kubeconfig_path, port)
}

/// Issue #111: tunnels die with the app (ADR-0009), so after a restart every bastion profile
/// that had one would sit at `connection refused` until it is individually verified. Re-opens
/// each such profile's tunnel through the same guarded path Verify uses; a profile with no
/// persisted target, no kubeconfig on disk, or no bastion is left alone. One profile's failure
/// never stops the others. When the tunnel came back on a different port the merged
/// `~/.kube/config` is refreshed, as `verify_profile` does -- a later Verify would see the
/// kubeconfig already retargeted and skip the merge.
pub async fn restore_tunnels(
    tunnels: &crate::services::tunnel::TunnelManager,
    runner: &dyn CommandRunner,
    paths: &ClusterDeckPaths,
) -> Vec<(String, Result<bool, String>)> {
    // A missing profiles.yaml loads as empty; an Err is a real read/parse failure.
    let profiles = match store::load_profiles(paths) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("[ClusterDeck] tunnel restore skipped: cannot load profiles: {e}");
            return Vec::new();
        }
    };
    let mut results = Vec::new();
    for profile in profiles {
        if tunnels.is_shutting_down() {
            break;
        }
        let kubeconfig_path = paths.kubeconfig_file(&profile.id);
        if profile.bastion.is_none() || profile.kubeconfig.is_none() || !kubeconfig_path.exists() {
            continue;
        }
        // A missing state.json or entry is "no tunnel"; an unreadable one is logged, not hidden.
        match state::get_tunnel_target(paths, &profile.id) {
            Ok(Some(_)) => {}
            Ok(None) => continue,
            Err(e) => {
                eprintln!(
                    "[ClusterDeck] tunnel restore skipped for '{}': cannot read state: {e}",
                    profile.id
                );
                continue;
            }
        }
        let outcome =
            ensure_verify_tunnel(tunnels, runner, paths, &profile, &kubeconfig_path).await;
        // Merge per profile, not after the loop: the kubeconfig is already retargeted, so if the
        // app quits mid-restore a later run sees `Ok(false)` and would never merge this one.
        let merge_err = match outcome {
            Ok(true) => {
                auto_merge_if_present(runner, paths, &profile, CurrentContext::Preserve).await
            }
            // #116: an earlier retarget's merge may have failed; compare instead of trusting it.
            Ok(false) => reconcile_user_kubeconfig(paths, &profile).await,
            Err(_) => None,
        };
        if let Some(e) = merge_err {
            eprintln!("profile '{}': {e}", profile.id);
        }
        results.push((profile.id, outcome));
    }
    results
}

#[tauri::command]
pub async fn verify_profile(profile_id: String) -> Result<VerificationResult, String> {
    let paths = ClusterDeckPaths::resolve()?;
    let runner = SystemRunner;
    let profile = load_profile_fresh(&runner, &paths, &profile_id).await?;

    let kubeconfig_path = paths.kubeconfig_file(&profile_id);

    // ADR-0009 D5b: the tunnel dies with the app, so a bastion profile verified after a restart
    // must bring it back up first. A failure must not abort verification (SSH reachability and
    // the saved status still matter); VerificationResult has no detail field, so the reason is
    // logged and verification then fails on its own as it did before tunnels existed.
    if profile.bastion.is_some() && profile.kubeconfig.is_some() && kubeconfig_path.exists() {
        match ensure_verify_tunnel(
            crate::services::tunnel::global(),
            &runner,
            &paths,
            &profile,
            &kubeconfig_path,
        )
        .await
        {
            Ok(true) => {
                if let Some(e) =
                    auto_merge_if_present(&runner, &paths, &profile, CurrentContext::Preserve).await
                {
                    eprintln!("profile '{profile_id}': {e}");
                }
            }
            Ok(false) => {
                if let Some(e) = reconcile_user_kubeconfig(&paths, &profile).await {
                    eprintln!("profile '{profile_id}': {e}");
                }
            }
            Err(e) => eprintln!("profile '{profile_id}': {e}"),
        }
    }

    let target_context = resolve_verify_context(&profile, &kubeconfig_path);
    let mut result = if kubeconfig_path.exists() {
        verify::verify_cluster(&runner, &kubeconfig_path, &target_context).await
    } else {
        VerificationResult {
            ssh: false,
            kubeconfig: false,
            kubernetes: false,
            node_count: None,
            kubernetes_version: None,
            api_endpoint: None,
            last_verified: None,
        }
    };

    result.kubeconfig = kubeconfig_path.exists();

    let cached_ssh = state::get_status(&paths, &profile_id)
        .ok()
        .flatten()
        .map(|c| c.ssh)
        .unwrap_or(false);
    result.ssh = compute_ssh_reachability(&runner, &profile, cached_ssh).await;

    state::save_status(&paths, &profile_id, result.clone())?;
    Ok(result)
}

#[tauri::command]
pub async fn get_profile_status(profile_id: String) -> Result<Option<VerificationResult>, String> {
    let paths = ClusterDeckPaths::resolve()?;
    state::get_status(&paths, &profile_id)
}

#[tauri::command]
pub async fn connect_profile(
    profile_id: String,
    bootstrap_password: Option<String>,
    password: Option<String>,
) -> Result<ConnectionResult, String> {
    let paths = ClusterDeckPaths::resolve()?;
    let runner = SystemRunner;
    let profile = load_profile_fresh(&runner, &paths, &profile_id).await?;
    let pwd = password.as_deref();

    let host_futures = profile.hosts.iter().map(|host| async {
        let mut probe = ssh::probe_with_retry(
            &runner,
            host,
            profile.bastion.as_ref(),
            profile.bootstrap.retries,
            Duration::from_secs(profile.bootstrap.retry_delay_secs),
            pwd,
        )
        .await;

        // The bootstrap-to-key flow only applies to key-auth hosts: it deploys a public key via
        // a one-time password, which makes no sense for a host permanently configured for
        // password auth (it already has a working, ongoing auth method).
        if !probe.reachable && profile.bootstrap.enabled && host.auth == AuthMode::Key {
            if let Some(bpwd) = bootstrap_password.as_ref() {
                let boot_res = ssh::bootstrap_host(
                    &runner,
                    host,
                    profile.bastion.as_ref(),
                    bpwd,
                    profile.bootstrap.retries,
                    Duration::from_secs(profile.bootstrap.retry_delay_secs),
                )
                .await;

                if boot_res.verified {
                    probe.reachable = true;
                    probe.detail = boot_res.detail;
                } else if !boot_res.detail.is_empty() {
                    probe.detail = format!("Bootstrap failed: {}", boot_res.detail);
                }
            }
        }

        HostStageResult {
            host: host.name.clone(),
            reachable: probe.reachable,
            detail: probe.detail,
        }
    });

    let host_stage_results = futures::future::join_all(host_futures).await;

    let mut errors = Vec::new();

    let mut aliases_written = false;
    match ssh_config::write_profile_config(&paths, &profile) {
        Ok(_) => match std::env::var("HOME") {
            Ok(home) => {
                let home_ssh_config = PathBuf::from(home).join(".ssh").join("config");
                match ssh_config::ensure_ssh_include(&home_ssh_config, &paths) {
                    Ok(_) => {
                        aliases_written = true;
                    }
                    Err(e) => {
                        errors.push(format!("alias include generation failed: {e}"));
                    }
                }
            }
            Err(e) => {
                errors.push(format!("HOME environment variable not set: {e}"));
            }
        },
        Err(e) => {
            errors.push(format!("alias write failed: {e}"));
        }
    }

    let any_host_reachable = host_stage_results.iter().any(|h| h.reachable);
    let mut kubeconfig_summary = None;

    if profile.kubeconfig.is_some() && any_host_reachable {
        match crate::services::kubeconfig::fetch_and_store(&runner, &paths, &profile, pwd).await {
            Ok(mut summary) => {
                if let Some(e) = summary.tunnel_error.take() {
                    errors.push(e);
                }
                kubeconfig_summary = Some(summary);
            }
            Err(e) => {
                errors.push(format!("kubeconfig fetch failed: {e}"));
            }
        }
    }

    let kubeconfig_path = paths.kubeconfig_file(&profile_id);
    // A previous merge may have created an unauthenticated placeholder. When this Connect
    // attempt cannot fetch credentials, never verify that stale file as if it were fresh.
    let kubeconfig_exists =
        has_kubeconfig_to_verify(&profile, kubeconfig_summary.is_some(), &kubeconfig_path);
    let target_context = resolve_verify_context(&profile, &kubeconfig_path);

    let (mut verification, verify_err) = if kubeconfig_exists {
        verify::verify_cluster_detailed(&runner, &kubeconfig_path, &target_context).await
    } else {
        (
            VerificationResult {
                ssh: false,
                kubeconfig: false,
                kubernetes: false,
                node_count: None,
                kubernetes_version: None,
                api_endpoint: None,
                last_verified: None,
            },
            None,
        )
    };

    if let Some(err) = verify_err {
        errors.push(format!("kubernetes verification failed: {err}"));
    }

    verification.ssh = any_host_reachable;
    verification.kubeconfig = kubeconfig_exists;

    // Discover endpoints (Ingress, APISIX, Istio, Gateway API, Services)
    let mut endpoints = Vec::new();
    let mut discovery_res: Result<crate::services::k8s_endpoints::DiscoveryResult, String> =
        Err("kubernetes verification failed or kubeconfig missing".to_string());
    if verification.kubernetes && kubeconfig_exists {
        let default_ip = profile.hosts.first().map(|h| h.address.as_str());
        match crate::services::k8s_endpoints::discover_cluster_endpoints_detailed(
            &runner,
            &kubeconfig_path,
            default_ip,
            profile.gateway_address.as_deref(),
        )
        .await
        {
            Ok(discovered) => {
                endpoints = discovered.endpoints.clone();
                discovery_res = Ok(discovered);
            }
            Err(e) => {
                errors.push(format!("endpoint discovery warning: {e}"));
                discovery_res = Err(e);
            }
        }
    } else if profile.kubeconfig.is_none() {
        discovery_res = Ok(crate::services::k8s_endpoints::DiscoveryResult {
            endpoints: Vec::new(),
            succeeded_kinds: Vec::new(),
            failed_kinds: Vec::new(),
            errors_by_kind: Vec::new(),
        });
    }

    let current_profile = store::load_profiles(&paths)
        .ok()
        .and_then(|ps| ps.into_iter().find(|p| p.id == profile_id))
        .unwrap_or_else(|| profile.clone());

    if let Err(e) = write_connect_hosts_block(
        &runner,
        &paths,
        Path::new(crate::services::hosts_file::HOSTS_FILE_PATH),
        &current_profile,
        &discovery_res,
    )
    .await
    {
        errors.push(e);
    }

    match crate::commands::local_dns::SystemDns::resolve() {
        Ok(dns) => {
            let discovery = discovery_res.as_ref().map_err(|e| e.as_str());
            crate::commands::local_dns::refresh_local_dns_on_connect_detailed(
                &paths,
                &dns.env(),
                &profile_id,
                discovery,
                &mut errors,
            )
            .await;
        }
        Err(e) => {
            if current_profile.host_mapping == crate::services::config::HostMapping::LocalDns {
                errors.push(format!("local DNS resolution failed: {e}"));
            }
        }
    }

    // If this profile was previously merged into ~/.kube/config, auto-sync fresh credentials
    if verification.kubernetes && kubeconfig_exists {
        if let Some(e) =
            auto_merge_if_present(&runner, &paths, &profile, CurrentContext::Select).await
        {
            errors.push(e);
        }
    }

    let _ = state::save_status(&paths, &profile_id, verification.clone());

    Ok(ConnectionResult {
        hosts: host_stage_results,
        aliases_written,
        kubeconfig: kubeconfig_summary,
        verification,
        endpoints,
        errors,
    })
}

/// Every discovery kind as owned strings: a total discovery failure is "all kinds failed".
fn all_discovery_kinds() -> Vec<String> {
    crate::services::k8s_endpoints::DISCOVERY_KINDS
        .iter()
        .map(|k| k.to_string())
        .collect()
}

/// Endpoints for a Hosts-mode block write: a total discovery failure keeps every previous record
/// exactly like a partial one keeps its failed kinds' records.
fn hosts_endpoints_for(
    cache_path: &Path,
    hosts_path: &Path,
    profile_id: &str,
    discovery: &Result<crate::services::k8s_endpoints::DiscoveryResult, String>,
) -> Vec<DiscoveredEndpoint> {
    match discovery {
        Ok(res) => merge_discovered_endpoints(
            cache_path,
            hosts_path,
            profile_id,
            &res.endpoints,
            &res.succeeded_kinds,
            &res.failed_kinds,
        ),
        Err(_) => merge_discovered_endpoints(
            cache_path,
            hosts_path,
            profile_id,
            &[],
            &[],
            &all_discovery_kinds(),
        ),
    }
}

/// Hosts-mode part of Connect: writes the profile's /etc/hosts block from this discovery.
/// Runs inside the local DNS lane and re-reads the profile there (D-LANE): a save to Off or
/// LocalDns that completed after Connect loaded its snapshot must not get its block back.
/// Lock order stays profile write lock -> lane -> MANAGER_MUTEX -> tunnel slots (only the lane
/// is taken here).
pub(crate) async fn write_connect_hosts_block(
    runner: &dyn CommandRunner,
    paths: &ClusterDeckPaths,
    hosts_path: &Path,
    snapshot: &Profile,
    discovery_res: &Result<crate::services::k8s_endpoints::DiscoveryResult, String>,
) -> Result<(), String> {
    let _lane_lock = crate::commands::local_dns::lock_lane().await;
    let Ok(current) = store::get_profile(paths, &snapshot.id) else {
        return Ok(());
    };
    if current.host_mapping != crate::services::config::HostMapping::Hosts {
        return Ok(());
    }
    let hosts_endpoints = hosts_endpoints_for(
        &paths.endpoints_cache_file(),
        hosts_path,
        &current.id,
        discovery_res,
    );
    crate::services::hosts_file::upsert_hosts_block_with_endpoints_at(
        runner,
        hosts_path,
        &current,
        &hosts_endpoints,
    )
    .await
    .map_err(|e| format!("hosts file update failed: {e}"))
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScanEndpointsResult {
    pub endpoints: Vec<DiscoveredEndpoint>,
    pub warning: Option<String>,
}

#[tauri::command]
pub async fn discover_cluster_endpoints_cmd(
    profile_id: String,
) -> Result<ScanEndpointsResult, String> {
    let paths = ClusterDeckPaths::resolve()?;
    let runner = SystemRunner;
    let cache_res = crate::commands::local_dns::SystemDns::resolve()
        .map(|dns| crate::commands::local_dns::cache_path(&dns.env()));
    discover_cluster_endpoints_internal(&runner, &paths, cache_res, &profile_id).await
}

pub async fn discover_cluster_endpoints_internal(
    runner: &dyn CommandRunner,
    paths: &ClusterDeckPaths,
    cache_path_res: Result<PathBuf, String>,
    profile_id: &str,
) -> Result<ScanEndpointsResult, String> {
    let profile = load_profile_fresh(runner, paths, profile_id).await?;
    let kubeconfig_path = paths.kubeconfig_file(profile_id);

    if !kubeconfig_path.exists() {
        return Err("kubeconfig not found for profile; please connect first".to_string());
    }

    let default_ip = profile.hosts.first().map(|h| h.address.as_str());
    let discovery = crate::services::k8s_endpoints::discover_cluster_endpoints_detailed(
        runner,
        &kubeconfig_path,
        default_ip,
        profile.gateway_address.as_deref(),
    )
    .await?;

    // D4 (#102): retain discovery errors alongside cache errors; a partial scan is not
    // authoritative. One combined warning keeps the IPC stable; split it if UI needs categories.
    let mut warnings: Vec<String> = discovery
        .errors_by_kind
        .iter()
        .map(|(kind, error)| {
            format!(
                "{}: {error}",
                crate::services::k8s_endpoints::display_kind_name(kind)
            )
        })
        .collect();
    match cache_path_res {
        Ok(cache_file) => {
            // Discovery ran without any lock: a delete may have completed meanwhile, and an
            // unguarded write would resurrect the deleted profile's cache entry.
            let _lane_lock = crate::commands::local_dns::lock_lane().await;
            if store::get_profile(paths, profile_id).is_ok() {
                if let Err(e) = crate::commands::local_dns::update_endpoint_cache(
                    &cache_file,
                    profile_id,
                    &discovery.endpoints,
                    Some(&discovery.succeeded_kinds),
                ) {
                    warnings.push(format!("failed to update endpoint cache: {e}"));
                }
            }
        }
        Err(e) => {
            warnings.push(format!("failed to initialize DNS environment: {e}"));
        }
    }

    Ok(ScanEndpointsResult {
        endpoints: discovery.endpoints,
        warning: (!warnings.is_empty()).then(|| warnings.join("; ")),
    })
}

/// Endpoints to write for a Hosts profile. Succeeded kinds contribute exactly their fresh
/// results (a host a succeeded kind no longer reports is gone); only FAILED kinds keep their
/// previous records. Attribution comes from the endpoints cache as it was BEFORE this update;
/// an old /etc/hosts line is re-added only when the cache ties it to a failed kind. When the
/// cache cannot attribute a host, preserve it during partial failure and persist that unknown
/// attribution until a fully successful discovery can replace it.
pub fn merge_discovered_endpoints(
    cache_path: &Path,
    hosts_path: &Path,
    profile_id: &str,
    discovered: &[DiscoveredEndpoint],
    succeeded_kinds: &[String],
    failed_kinds: &[String],
) -> Vec<DiscoveredEndpoint> {
    if failed_kinds.is_empty() {
        let _ = crate::commands::local_dns::update_endpoint_cache(
            cache_path, profile_id, discovered, None,
        );
        return discovered.to_vec();
    }

    let previous = crate::commands::local_dns::cached_endpoints(cache_path, profile_id);
    let is_known_kind = |src: &str| crate::services::k8s_endpoints::DISCOVERY_KINDS.contains(&src);
    let kind_of: std::collections::HashMap<&str, &str> = previous
        .iter()
        .filter(|ep| is_known_kind(&ep.source))
        .map(|ep| (ep.host.as_str(), ep.source.as_str()))
        .collect();

    let mut merged = crate::commands::local_dns::merge_endpoints(
        previous.clone(),
        discovered,
        Some(succeeded_kinds),
    );

    if let Ok(hosts_content) = std::fs::read_to_string(hosts_path) {
        if let Some(lines) =
            crate::services::hosts_file::get_profile_hosts_block(&hosts_content, profile_id)
        {
            let node_suffix = format!(".{profile_id}.clusterdeck.local");
            for line in lines {
                let parts: Vec<&str> = line.split_whitespace().collect();
                if parts.len() < 2 {
                    continue;
                }
                let (ip, host) = (parts[0], parts[1]);
                let keep = match kind_of.get(host) {
                    Some(kind) => failed_kinds.iter().any(|k| k == kind),
                    None => true,
                };
                if keep
                    && !host.ends_with(&node_suffix)
                    && crate::services::validate::is_safe_host_domain(host)
                    && crate::services::validate::is_safe_ip_address(ip)
                    && !merged.iter().any(|ep| ep.host == host)
                {
                    merged.push(crate::services::k8s_endpoints::DiscoveredEndpoint {
                        host: host.to_string(),
                        ip: ip.to_string(),
                        source: "cached".to_string(),
                        resource_name: String::new(),
                    });
                }
            }
        }
    }

    // D2 (#102): persist recovered legacy hosts too. Unknown attribution may retain stale
    // records during failures; a fully successful scan replaces the cache above.
    let _ =
        crate::commands::local_dns::update_endpoint_cache(cache_path, profile_id, &merged, None);
    merged
}

#[tauri::command]
pub async fn sync_hosts_file_cmd(profile_id: String) -> Result<SyncHostsResult, String> {
    let paths = ClusterDeckPaths::resolve()?;
    let runner = SystemRunner;
    sync_hosts_file_with_deps(
        &runner,
        &paths,
        Path::new(crate::services::hosts_file::HOSTS_FILE_PATH),
        &profile_id,
    )
    .await
}

pub async fn sync_hosts_file_with_deps(
    runner: &dyn CommandRunner,
    paths: &ClusterDeckPaths,
    hosts_path: &Path,
    profile_id: &str,
) -> Result<SyncHostsResult, String> {
    let profile = store::get_profile(paths, profile_id)?;
    let kubeconfig_path = paths.kubeconfig_file(profile_id);

    let discovery_res: Result<crate::services::k8s_endpoints::DiscoveryResult, String> =
        if kubeconfig_path.exists() {
            let default_ip = profile.hosts.first().map(|h| h.address.as_str());
            crate::services::k8s_endpoints::discover_cluster_endpoints_detailed(
                runner,
                &kubeconfig_path,
                default_ip,
                profile.gateway_address.as_deref(),
            )
            .await
        } else if profile.kubeconfig.is_none() {
            Ok(crate::services::k8s_endpoints::DiscoveryResult {
                endpoints: Vec::new(),
                succeeded_kinds: Vec::new(),
                failed_kinds: Vec::new(),
                errors_by_kind: Vec::new(),
            })
        } else {
            Err("kubeconfig not found for profile; please connect first".to_string())
        };

    if profile.host_mapping == crate::services::config::HostMapping::LocalDns {
        let dns = crate::commands::local_dns::SystemDns::resolve()?;
        return crate::commands::local_dns::sync_local_dns_profile_detailed(
            paths,
            &dns.env(),
            hosts_path,
            &profile,
            match &discovery_res {
                Ok(res) => Ok(res.clone()),
                Err(e) => Err(e.as_str()),
            },
        )
        .await;
    }

    // D1 (#102): discovery stays outside the lane to avoid blocking saves on network I/O.
    // Re-read under the lane before any mutation; a mode change requires a new Sync action.
    let _lane = crate::commands::local_dns::lock_lane().await;
    let profile = store::get_profile(paths, profile_id)?;
    if profile.host_mapping != crate::services::config::HostMapping::Hosts {
        return Err("Host mapping changed or is off; refresh the profile before syncing".into());
    }

    let endpoints = hosts_endpoints_for(
        &paths.endpoints_cache_file(),
        hosts_path,
        profile_id,
        &discovery_res,
    );
    let warning = match &discovery_res {
        Ok(res) if !res.failed_kinds.is_empty() => {
            let names: Vec<&str> = res
                .failed_kinds
                .iter()
                .map(|k| crate::services::k8s_endpoints::display_kind_name(k))
                .collect();
            Some(format!(
                "Endpoint discovery partially failed for: {}; keeping last known records.",
                names.join(", ")
            ))
        }
        Ok(_) => None,
        Err(_) => Some(
            "Could not discover endpoints (Connect first); keeping last known records.".to_string(),
        ),
    };
    crate::services::hosts_file::upsert_hosts_block_with_endpoints_at(
        runner, hosts_path, &profile, &endpoints,
    )
    .await?;

    let hosts_count = profile.hosts.len() + if profile.bastion.is_some() { 1 } else { 0 };
    let endpoints_count = endpoints.len();

    // Warning only: the block is already written, so our entries are in the file now.
    let written = std::fs::read_to_string(hosts_path).unwrap_or_default();
    let conflicts = crate::services::hosts_file::find_hostname_conflicts(&written, profile_id);

    Ok(SyncHostsResult {
        success: warning.is_none(),
        endpoints_count,
        hosts_count,
        endpoints,
        message: if let Some(w) = &warning {
            w.clone()
        } else {
            format!(
                "Successfully synced {hosts_count} host(s) and {endpoints_count} cluster endpoint(s) to /etc/hosts"
            )
        },
        warning,
        conflicts,
        dns_conflicts: vec![],
        skipped_hosts: vec![],
        dns_prompted: false,
    })
}

#[tauri::command]
pub async fn get_hosts_file_status(profile_id: String) -> Result<HostsFileStatus, String> {
    let paths = ClusterDeckPaths::resolve()?;
    let profile = store::get_profile(&paths, &profile_id)?;
    let existing =
        std::fs::read_to_string(crate::services::hosts_file::HOSTS_FILE_PATH).unwrap_or_default();

    let active_opt = crate::services::hosts_file::get_profile_hosts_block(&existing, &profile_id);
    let is_synced = active_opt.is_some();
    let active_entries = active_opt.unwrap_or_default();

    let pending_block =
        crate::services::hosts_file::render_hosts_block_with_endpoints(&profile, &[])?;
    let pending_entries: Vec<String> = pending_block
        .lines()
        .filter(|l| !l.starts_with('#') && !l.trim().is_empty())
        .map(|s| s.to_string())
        .collect();

    // D1: current reality (what /etc/hosts resolves now), not a prediction of a future sync.
    let conflicts = crate::services::hosts_file::find_hostname_conflicts(&existing, &profile_id);

    Ok(HostsFileStatus {
        managed_by_profile: profile.host_mapping == crate::services::config::HostMapping::Hosts,
        is_synced,
        active_entries,
        pending_entries,
        conflicts,
    })
}

#[tauri::command]
pub async fn remove_hosts_file_cmd(profile_id: String) -> Result<SyncHostsResult, String> {
    let paths = ClusterDeckPaths::resolve()?;
    let profile = store::get_profile(&paths, &profile_id)?;
    if profile.host_mapping == crate::services::config::HostMapping::LocalDns {
        let dns = crate::commands::local_dns::SystemDns::resolve()?;
        return crate::commands::local_dns::remove_local_dns_profile(
            &paths,
            &dns.env(),
            crate::commands::local_dns::hosts_path(),
            &profile_id,
        )
        .await;
    }
    let runner = SystemRunner;
    crate::services::hosts_file::remove_hosts_block(&runner, &profile_id).await?;

    Ok(SyncHostsResult {
        success: true,
        endpoints_count: 0,
        hosts_count: 0,
        endpoints: vec![],
        message: format!("Successfully removed entries for profile '{profile_id}' from /etc/hosts"),
        warning: None,
        conflicts: vec![],
        dns_conflicts: vec![],
        skipped_hosts: vec![],
        dns_prompted: false,
    })
}

#[tauri::command]
pub async fn open_ssh_session(profile_id: String, host_name: String) -> Result<(), String> {
    let paths = ClusterDeckPaths::resolve()?;
    let runner = SystemRunner;
    let profile = load_profile_fresh(&runner, &paths, &profile_id).await?;

    let target_host = profile
        .hosts
        .iter()
        .find(|h| h.name == host_name)
        .ok_or_else(|| format!("host not found: {host_name}"))?;

    ssh_config::write_profile_config(&paths, &profile)?;
    let home = std::env::var("HOME").map_err(|e| e.to_string())?;
    let home_ssh_config = PathBuf::from(home).join(".ssh").join("config");
    ssh_config::ensure_ssh_include(&home_ssh_config, &paths)?;
    // Side-effect-only probe, result intentionally discarded: accept-new records this (possibly
    // just-recreated) VM's host key, and a changed key gets its stale known_hosts entry pruned,
    // so the interactive Terminal session below doesn't open onto a host-key failure. Terminal
    // reports its own connection errors, so there is nothing useful to do with the outcome here.
    let _ = ssh::probe_key_auth(&runner, target_host, profile.bastion.as_ref()).await;

    let alias = ssh_config::ssh_alias(&profile.id, &host_name);
    crate::services::process::open_with_system(
        &runner,
        &[format!("ssh://{alias}")],
        "failed to open ssh session",
    )
    .await
}

#[tauri::command]
pub async fn backup_kubeconfig(
    move_file: Option<bool>,
) -> Result<crate::services::kubeconfig::BackupKubeconfigResult, String> {
    crate::services::kubeconfig::backup_user_kubeconfig(move_file.unwrap_or(true))
}

#[tauri::command]
pub async fn merge_kubeconfig_to_system(
    profile_id: String,
    backup_first: Option<bool>,
) -> Result<crate::services::kubeconfig::MergeKubeconfigResult, String> {
    let paths = ClusterDeckPaths::resolve()?;
    let profile = store::get_profile(&paths, &profile_id)?;
    let runner = SystemRunner;
    let res = crate::services::kubeconfig::merge_profile_kubeconfig_to_user_config(
        &runner,
        &paths,
        &profile,
        backup_first.unwrap_or(true),
        CurrentContext::Select,
    )
    .await?;

    if let Ok(Some(mut s)) = state::get_status(&paths, &profile_id) {
        s.kubeconfig = true;
        let _ = state::save_status(&paths, &profile_id, s);
    }

    Ok(res)
}

#[tauri::command]
pub async fn list_kubeconfig_backups(
) -> Result<Vec<crate::services::kubeconfig::KubeconfigBackupInfo>, String> {
    crate::services::kubeconfig::list_kubeconfig_backups()
}

#[tauri::command]
pub async fn restore_kubeconfig_backup(
    filename: String,
) -> Result<crate::services::kubeconfig::BackupKubeconfigResult, String> {
    crate::services::kubeconfig::restore_kubeconfig_backup(&filename)
}

#[tauri::command]
pub async fn delete_kubeconfig_backup(filename: String) -> Result<(), String> {
    crate::services::kubeconfig::delete_kubeconfig_backup(&filename)
}

#[tauri::command]
pub async fn get_user_kubeconfig_details(
    include_raw: Option<bool>,
) -> Result<crate::services::kubeconfig::UserKubeconfigDetails, String> {
    crate::services::kubeconfig::get_user_kubeconfig_details(include_raw.unwrap_or(false))
}

#[tauri::command]
pub async fn set_current_context(context_name: String) -> Result<(), String> {
    crate::services::kubeconfig::set_current_context(&context_name)
}

#[tauri::command]
pub async fn delete_user_kube_context(context_name: String) -> Result<(), String> {
    crate::services::kubeconfig::delete_user_kube_context(&context_name)
}

#[tauri::command]
pub async fn list_managed_profile_kubeconfigs(
) -> Result<Vec<crate::services::kubeconfig::ManagedProfileKubeconfig>, String> {
    let paths = ClusterDeckPaths::resolve()?;
    crate::services::kubeconfig::list_managed_profile_kubeconfigs(&paths)
}

#[tauri::command]
pub async fn open_path_in_finder(path: String) -> Result<(), String> {
    let runner = SystemRunner;
    crate::services::kubeconfig::open_path_in_finder(&runner, &path).await
}

#[tauri::command]
pub async fn open_url_in_browser(url: String) -> Result<(), String> {
    if !crate::services::validate::is_safe_open_url(&url) {
        return Err(
            "invalid URL: must start with http:// or https:// and must not contain spaces or control characters"
                .to_string(),
        );
    }
    let runner = SystemRunner;
    crate::services::process::open_with_system(&runner, &[url.trim().to_string()], "").await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::config::{BootstrapPolicy, Host};
    use crate::services::process::CommandOutput;
    use async_trait::async_trait;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct AlwaysSucceedsRunner {
        calls: AtomicUsize,
    }

    #[async_trait]
    impl CommandRunner for AlwaysSucceedsRunner {
        async fn run(&self, _bin: &str, _args: &[String]) -> Result<CommandOutput, String> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(CommandOutput {
                stdout: String::new(),
                stderr: String::new(),
                success: true,
            })
        }
    }

    struct NeverCalledRunner;

    #[async_trait]
    impl CommandRunner for NeverCalledRunner {
        async fn run(&self, bin: &str, _args: &[String]) -> Result<CommandOutput, String> {
            panic!("runner should not be invoked, but was called with bin={bin}");
        }
    }

    fn key_host(name: &str) -> Host {
        Host {
            name: name.to_string(),
            address: "192.0.2.10".to_string(),
            port: 22,
            user: "root".to_string(),
            identity_file: None,
            auth: AuthMode::Key,
        }
    }

    fn password_host(name: &str) -> Host {
        Host {
            auth: AuthMode::Password,
            ..key_host(name)
        }
    }

    fn profile_with_hosts(hosts: Vec<Host>) -> Profile {
        Profile {
            id: "test".to_string(),
            name: "Test".to_string(),
            hosts,
            bastion: None,
            bootstrap: BootstrapPolicy::default(),
            kubeconfig: None,
            host_mapping: crate::services::config::HostMapping::Off,
            approved_dns_zones: Vec::new(),
            gateway_address: None,
            trusted_cas: Vec::new(),
            local_runtime: None,
        }
    }

    #[test]
    fn failed_fetch_does_not_verify_an_existing_placeholder() {
        let mut profile = profile_with_hosts(vec![password_host("m1")]);
        profile.kubeconfig = Some(crate::services::config::KubeconfigSource {
            control_plane: "m1".to_string(),
            remote_path: "/etc/kubernetes/admin.conf".to_string(),
            local_path: String::new(),
            context: "test".to_string(),
        });
        // The path is deliberately an existing file. The failed fetch must still leave the
        // verification status false; its content may be a generated placeholder or stale keys.
        assert!(!has_kubeconfig_to_verify(
            &profile,
            false,
            Path::new(file!())
        ));
        assert!(has_kubeconfig_to_verify(&profile, true, Path::new(file!())));
    }

    #[tokio::test]
    async fn bootstrap_profile_hosts_skips_password_auth_hosts() {
        let runner = AlwaysSucceedsRunner {
            calls: AtomicUsize::new(0),
        };
        let profile = profile_with_hosts(vec![key_host("m1"), password_host("m2")]);

        let results = bootstrap_profile_hosts(&runner, &profile, "irrelevant").await;

        assert_eq!(results.len(), 2);
        let key_result = results.iter().find(|r| r.host == "m1").unwrap();
        assert!(key_result.key_deployed);

        let pwd_result = results.iter().find(|r| r.host == "m2").unwrap();
        assert!(!pwd_result.key_deployed);
        assert!(!pwd_result.verified);
        assert_eq!(
            pwd_result.detail,
            "skipped: host uses password authentication"
        );

        // deploy_public_key (ssh-copy-id) + one successful probe for the key host only; the
        // password host must never reach the runner at all.
        assert_eq!(runner.calls.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn bootstrap_profile_hosts_skips_every_host_when_all_are_password_auth() {
        let runner = NeverCalledRunner;
        let profile = profile_with_hosts(vec![password_host("m1"), password_host("m2")]);

        let results = bootstrap_profile_hosts(&runner, &profile, "irrelevant").await;

        assert_eq!(results.len(), 2);
        assert!(results.iter().all(|r| !r.key_deployed && !r.verified));
    }

    #[tokio::test]
    async fn compute_ssh_reachability_keeps_cached_value_when_profile_has_no_key_hosts() {
        let runner = NeverCalledRunner;
        let profile = profile_with_hosts(vec![password_host("m1")]);

        assert!(compute_ssh_reachability(&runner, &profile, true).await);
        assert!(!compute_ssh_reachability(&runner, &profile, false).await);
    }

    #[tokio::test]
    async fn compute_ssh_reachability_probes_key_hosts_and_ignores_password_hosts() {
        let runner = AlwaysSucceedsRunner {
            calls: AtomicUsize::new(0),
        };
        let profile = profile_with_hosts(vec![password_host("m1"), key_host("m2")]);

        // Starts from a false cached value to prove the `true` result comes from actually
        // probing the key host, not from the cached fallback.
        let reachable = compute_ssh_reachability(&runner, &profile, false).await;
        assert!(reachable);
        assert_eq!(runner.calls.load(Ordering::SeqCst), 1);
    }

    fn runtime() -> crate::services::config::LocalRuntimeSource {
        crate::services::config::LocalRuntimeSource {
            provider: crate::services::local_runtime_lifecycle::LocalRuntimeProvider::Colima,
            instance: "default".to_string(),
        }
    }

    #[test]
    fn merge_refreshed_endpoints_touches_only_endpoint_fields() {
        let mut snapshot = profile_with_hosts(vec![key_host("m1")]);
        snapshot.local_runtime = Some(runtime());
        let mut fresh = profile_with_hosts(vec![Host {
            user: "edited".to_string(),
            ..key_host("m1")
        }]);
        fresh.local_runtime = Some(runtime());
        fresh.name = "Edited name".to_string();
        fresh
            .trusted_cas
            .push(crate::services::ca_trust::TrustedCa {
                secret_ref: "ns/ca".to_string(),
                fingerprint_sha256: "a".to_string(),
                fingerprint_sha1: "b".to_string(),
                subject_cn: "cn".to_string(),
                not_after: "x".to_string(),
                trusted_at: "y".to_string(),
            });
        let mut refreshed = snapshot.clone();
        refreshed.hosts[0].address = "127.0.0.1".to_string();
        refreshed.hosts[0].port = 60022;
        refreshed.name = "Stale name".to_string();

        assert!(merge_refreshed_endpoints(&mut fresh, &snapshot, &refreshed));

        assert_eq!(fresh.hosts[0].address, "127.0.0.1");
        assert_eq!(fresh.hosts[0].port, 60022);
        assert_eq!(fresh.hosts[0].user, "edited");
        assert_eq!(fresh.name, "Edited name");
        assert_eq!(fresh.trusted_cas.len(), 1);
    }

    #[test]
    fn merge_refreshed_endpoints_keeps_concurrent_edit_of_unrefreshed_host() {
        let mut snapshot = profile_with_hosts(vec![key_host("a"), key_host("b")]);
        snapshot.local_runtime = Some(runtime());
        // Host b was edited concurrently (new address/port) while host a refreshed.
        let mut fresh = snapshot.clone();
        fresh.hosts[1].address = "198.51.100.7".to_string();
        fresh.hosts[1].port = 2200;
        let mut refreshed = snapshot.clone();
        refreshed.hosts[0].port = 60022;

        assert!(merge_refreshed_endpoints(&mut fresh, &snapshot, &refreshed));

        assert_eq!(fresh.hosts[0].port, 60022);
        assert_eq!(fresh.hosts[1].address, "198.51.100.7");
        assert_eq!(fresh.hosts[1].port, 2200);
    }

    #[test]
    fn merge_refreshed_endpoints_skips_when_local_runtime_removed_or_changed() {
        let mut snapshot = profile_with_hosts(vec![key_host("m1")]);
        snapshot.local_runtime = Some(runtime());
        let mut refreshed = snapshot.clone();
        refreshed.hosts[0].port = 60022;

        let mut removed = snapshot.clone();
        removed.local_runtime = None;
        assert!(!merge_refreshed_endpoints(
            &mut removed,
            &snapshot,
            &refreshed
        ));
        assert_eq!(removed.hosts[0].port, 22);

        let mut changed = snapshot.clone();
        changed.local_runtime.as_mut().unwrap().instance = "other".to_string();
        assert!(!merge_refreshed_endpoints(
            &mut changed,
            &snapshot,
            &refreshed
        ));
        assert_eq!(changed.hosts[0].port, 22);
    }

    /// Fake Colima whose `colima list` runs `mutate` first, simulating a concurrent writer that
    /// changes the store while the lock-free detection is in flight.
    struct MidFlightRunner<F: Fn() + Send + Sync> {
        mutate: F,
    }

    #[async_trait]
    impl<F: Fn() + Send + Sync> CommandRunner for MidFlightRunner<F> {
        async fn run(&self, bin: &str, args: &[String]) -> Result<CommandOutput, String> {
            let stdout = if bin == "colima" && args.first().map(String::as_str) == Some("list") {
                (self.mutate)();
                r#"{"name":"default","status":"Running","runtime":"docker"}"#
            } else if bin == "colima" {
                "Host colima\n  IdentityFile \"/k\"\n  User u\n  Hostname 127.0.0.1\n  Port 58096\n"
            } else {
                ""
            };
            Ok(CommandOutput {
                stdout: stdout.into(),
                stderr: String::new(),
                success: true,
            })
        }
    }

    fn temp_paths(tag: &str) -> ClusterDeckPaths {
        let dir = std::env::temp_dir().join(format!("cd-lpf-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        ClusterDeckPaths::at(dir)
    }

    fn runtime_profile() -> Profile {
        let mut p = profile_with_hosts(vec![key_host("colima-vm")]);
        p.id = "colima-default".to_string();
        p.local_runtime = Some(runtime());
        p
    }

    #[tokio::test]
    async fn load_profile_fresh_does_not_resurrect_profile_deleted_mid_flight() {
        let paths = temp_paths("del");
        store::upsert_profile(&paths, runtime_profile()).unwrap();
        let p2 = paths.clone();
        let runner = MidFlightRunner {
            mutate: move || store::delete_profile(&p2, "colima-default").unwrap(),
        };

        let res = load_profile_fresh(&runner, &paths, "colima-default").await;

        assert!(res.is_err());
        assert!(store::load_profiles(&paths).unwrap().is_empty());
    }

    #[tokio::test]
    async fn load_profile_fresh_writes_nothing_when_local_runtime_removed_mid_flight() {
        let paths = temp_paths("lr");
        store::upsert_profile(&paths, runtime_profile()).unwrap();
        let p2 = paths.clone();
        let runner = MidFlightRunner {
            mutate: move || {
                let mut p = runtime_profile();
                p.local_runtime = None;
                store::upsert_profile(&p2, p).unwrap();
            },
        };

        let res = load_profile_fresh(&runner, &paths, "colima-default")
            .await
            .unwrap();

        assert!(res.local_runtime.is_none());
        let stored = store::get_profile(&paths, "colima-default").unwrap();
        assert!(stored.local_runtime.is_none());
        assert_eq!(stored.hosts[0].port, 22);
    }

    fn bastion_profile() -> Profile {
        let mut p = profile_with_hosts(vec![key_host("cp1")]);
        p.bastion = Some(crate::services::config::Bastion {
            name: "bastion".to_string(),
            address: "203.0.113.5".to_string(),
            port: 22,
            user: "ops".to_string(),
            identity_file: None,
        });
        p.kubeconfig = Some(crate::services::config::KubeconfigSource {
            remote_path: String::new(),
            control_plane: "cp1".to_string(),
            local_path: String::new(),
            context: "test".to_string(),
        });
        p
    }

    fn tunnel_test_paths(tag: &str) -> ClusterDeckPaths {
        let dir = std::env::temp_dir().join(format!(
            "clusterdeck-verify-tunnel-{tag}-{}",
            std::process::id()
        ));
        std::fs::remove_dir_all(&dir).ok();
        ClusterDeckPaths::at(dir)
    }

    #[tokio::test]
    async fn ensure_verify_tunnel_reports_missing_target_without_spawning() {
        let paths = tunnel_test_paths("missing");
        let manager = crate::services::tunnel::TunnelManager::new(std::sync::Arc::new(
            crate::services::tunnel::SystemPortProbe,
        ));
        let err = ensure_verify_tunnel(
            &manager,
            &NeverCalledRunner,
            &paths,
            &bastion_profile(),
            &paths.kubeconfig_file("test"),
        )
        .await
        .unwrap_err();
        assert!(err.contains("reconnect"), "{err}");
    }

    #[tokio::test]
    async fn ensure_verify_tunnel_reports_tunnel_start_failure() {
        let paths = tunnel_test_paths("failure");
        state::save_tunnel_target(
            &paths,
            "test",
            &crate::services::tunnel::TunnelTarget {
                host: "127.0.0.1".to_string(),
                port: 6443,
            },
        )
        .unwrap();
        let manager = crate::services::tunnel::TunnelManager::new(std::sync::Arc::new(
            crate::services::tunnel::SystemPortProbe,
        ));
        write_test_kubeconfig(&paths, "      certificate-authority-data: ZmFrZS1jYQ==");
        // AlwaysSucceedsRunner keeps the default `spawn_background`, which is unsupported.
        let runner = AlwaysSucceedsRunner {
            calls: AtomicUsize::new(0),
        };
        let err = ensure_verify_tunnel(
            &manager,
            &runner,
            &paths,
            &bastion_profile(),
            &paths.kubeconfig_file("test"),
        )
        .await
        .unwrap_err();
        assert!(err.contains("API tunnel failed"), "{err}");
        std::fs::remove_dir_all(&paths.base).ok();
    }

    fn tunnel_manager() -> crate::services::tunnel::TunnelManager {
        crate::services::tunnel::TunnelManager::new(std::sync::Arc::new(
            crate::services::tunnel::SystemPortProbe,
        ))
    }

    #[tokio::test]
    async fn restore_tunnels_skips_profiles_without_a_persisted_target() {
        let paths = tunnel_test_paths("restore-no-target");
        let mut profile = bastion_profile();
        profile.id = "test".into();
        store::upsert_profile(&paths, profile).unwrap();
        write_test_kubeconfig(&paths, "      certificate-authority-data: ZmFrZS1jYQ==");

        let results = restore_tunnels(&tunnel_manager(), &NeverCalledRunner, &paths).await;

        assert!(results.is_empty());
        std::fs::remove_dir_all(&paths.base).ok();
    }

    #[tokio::test]
    async fn restore_tunnels_skips_profiles_without_a_kubeconfig_on_disk() {
        let paths = tunnel_test_paths("restore-no-kubeconfig");
        let mut profile = bastion_profile();
        profile.id = "test".into();
        store::upsert_profile(&paths, profile).unwrap();
        state::save_tunnel_target(
            &paths,
            "test",
            &crate::services::tunnel::TunnelTarget {
                host: "127.0.0.1".to_string(),
                port: 6443,
            },
        )
        .unwrap();

        let results = restore_tunnels(&tunnel_manager(), &NeverCalledRunner, &paths).await;

        assert!(results.is_empty());
        std::fs::remove_dir_all(&paths.base).ok();
    }

    #[tokio::test]
    async fn restore_tunnels_attempts_a_profile_that_had_a_tunnel_and_reports_failure() {
        let paths = tunnel_test_paths("restore-attempt");
        let mut profile = bastion_profile();
        profile.id = "test".into();
        store::upsert_profile(&paths, profile).unwrap();
        state::save_tunnel_target(
            &paths,
            "test",
            &crate::services::tunnel::TunnelTarget {
                host: "127.0.0.1".to_string(),
                port: 6443,
            },
        )
        .unwrap();
        write_test_kubeconfig(&paths, "      certificate-authority-data: ZmFrZS1jYQ==");
        // The default `spawn_background` is unsupported, so the attempt fails -- which proves
        // it was made, and that the failure is reported instead of propagated.
        let runner = AlwaysSucceedsRunner {
            calls: AtomicUsize::new(0),
        };

        let results = restore_tunnels(&tunnel_manager(), &runner, &paths).await;

        assert_eq!(results.len(), 1);
        assert_eq!(results[0].0, "test");
        assert!(results[0]
            .1
            .as_ref()
            .unwrap_err()
            .contains("API tunnel failed"));
        std::fs::remove_dir_all(&paths.base).ok();
    }

    struct AllOpenProbe;

    #[async_trait]
    impl crate::services::tunnel::PortProbe for AllOpenProbe {
        fn is_bindable(&self, _port: u16) -> bool {
            true
        }
        async fn is_open(&self, _port: u16) -> bool {
            true
        }
    }

    struct AliveProcess;

    #[async_trait]
    impl crate::services::process::BackgroundProcess for AliveProcess {
        fn try_exit_status(&mut self) -> Option<i32> {
            None
        }
        fn pid(&self) -> Option<u32> {
            Some(4242)
        }
        async fn kill(&mut self) {}
        async fn stderr_tail(&mut self) -> String {
            String::new()
        }
    }

    /// Every ssh spawn succeeds and stays alive; `lsof` is unavailable (readiness then rests on
    /// the probe alone); other commands succeed with empty output.
    struct SpawnOkRunner {
        spawns: AtomicUsize,
    }

    #[async_trait]
    impl CommandRunner for SpawnOkRunner {
        async fn run(&self, bin: &str, _args: &[String]) -> Result<CommandOutput, String> {
            if bin == "lsof" {
                return Err("'lsof' executable not found".to_string());
            }
            Ok(CommandOutput {
                stdout: String::new(),
                stderr: String::new(),
                success: true,
            })
        }
        async fn spawn_background(
            &self,
            _bin: &str,
            _args: &[String],
        ) -> Result<Box<dyn crate::services::process::BackgroundProcess>, String> {
            self.spawns.fetch_add(1, Ordering::SeqCst);
            Ok(Box::new(AliveProcess))
        }
    }

    fn open_manager() -> crate::services::tunnel::TunnelManager {
        crate::services::tunnel::TunnelManager::new(std::sync::Arc::new(AllOpenProbe))
    }

    /// A restorable bastion profile: stored, with a persisted tunnel target and a pinned-TLS
    /// kubeconfig whose server port is `server_port`.
    fn seed_restorable(paths: &ClusterDeckPaths, id: &str, server_port: u16, tls: &str) {
        let mut profile = bastion_profile();
        profile.id = id.into();
        store::upsert_profile(paths, profile).unwrap();
        state::save_tunnel_target(
            paths,
            id,
            &crate::services::tunnel::TunnelTarget {
                host: "127.0.0.1".to_string(),
                port: 6443,
            },
        )
        .unwrap();
        paths.ensure_dirs().unwrap();
        std::fs::write(
            paths.kubeconfig_file(id),
            format!(
                "clusters:\n  - name: {id}\n    cluster:\n      server: https://127.0.0.1:{server_port}\n{tls}\ncontexts:\n  - name: {id}\n    context:\n      cluster: {id}\n      user: {id}\nusers:\n  - name: {id}\n    user:\n      token: t\n"
            ),
        )
        .unwrap();
    }

    const PINNED_TLS: &str = "      certificate-authority-data: ZmFrZS1jYQ==";

    #[tokio::test]
    async fn restore_tunnels_reports_retarget_then_no_change() {
        let paths = tunnel_test_paths("restore-retarget");
        // Port 1 is outside the tunnel port range, so the first restore must retarget.
        seed_restorable(&paths, "cdtest-ret", 1, PINNED_TLS);
        let runner = SpawnOkRunner {
            spawns: AtomicUsize::new(0),
        };

        let first = restore_tunnels(&open_manager(), &runner, &paths).await;
        assert!(matches!(first[0].1, Ok(true)), "{:?}", first[0].1);

        // Same profile id picks the same preferred port, so nothing changes the second time.
        let second = restore_tunnels(&open_manager(), &runner, &paths).await;
        assert!(matches!(second[0].1, Ok(false)), "{:?}", second[0].1);
        std::fs::remove_dir_all(&paths.base).ok();
    }

    fn reconcile_kubeconfig(id: &str, server: &str) -> String {
        format!(
            "apiVersion: v1\nkind: Config\ncurrent-context: {id}\nclusters:\n  - name: {id}\n    cluster:\n      server: {server}\n{PINNED_TLS}\ncontexts:\n  - name: {id}\n    context:\n      cluster: {id}\n      user: {id}\nusers:\n  - name: {id}\n    user:\n      token: t\n"
        )
    }

    /// Stored kubeconfig at `stored_port`; returns (paths, fake user config path, bak dir).
    fn reconcile_fixture(
        tag: &str,
        stored_port: u16,
        user_yaml: Option<String>,
    ) -> (ClusterDeckPaths, std::path::PathBuf, std::path::PathBuf) {
        let paths = tunnel_test_paths(tag);
        paths.ensure_dirs().unwrap();
        std::fs::write(
            paths.kubeconfig_file("cdtest-rec"),
            reconcile_kubeconfig("cdtest-rec", &format!("https://127.0.0.1:{stored_port}")),
        )
        .unwrap();
        let user = paths.base.join("fake-home").join(".kube").join("config");
        if let Some(y) = user_yaml {
            std::fs::create_dir_all(user.parent().unwrap()).unwrap();
            std::fs::write(&user, y).unwrap();
        }
        let bak = paths.base.join("fake-home").join(".kube").join("bak");
        (paths, user, bak)
    }

    fn server_in(path: &Path, id: &str) -> String {
        let v: serde_yaml::Value =
            serde_yaml::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
        v["clusters"]
            .as_sequence()
            .unwrap()
            .iter()
            .find(|c| c["name"].as_str() == Some(id))
            .unwrap()["cluster"]["server"]
            .as_str()
            .unwrap()
            .to_string()
    }

    #[tokio::test]
    async fn reconcile_merges_when_user_config_has_a_stale_port() {
        let stale = reconcile_kubeconfig("cdtest-rec", "https://127.0.0.1:1111");
        let (paths, user, bak) = reconcile_fixture("rec-stale", 2222, Some(stale));
        let mut profile = bastion_profile();
        profile.id = "cdtest-rec".into();

        let err = reconcile_user_kubeconfig_at(&paths, &profile, &user, &bak).await;
        assert!(err.is_none(), "{err:?}");
        assert_eq!(server_in(&user, "cdtest-rec"), "https://127.0.0.1:2222");
        std::fs::remove_dir_all(&paths.base).ok();
    }

    #[tokio::test]
    async fn reconcile_keeps_the_users_selected_context() {
        let stale = reconcile_kubeconfig("cdtest-rec", "https://127.0.0.1:1111")
            .replace("current-context: cdtest-rec", "current-context: cdtest-b");
        let (paths, user, bak) = reconcile_fixture("rec-ctx", 2222, Some(stale));
        let mut profile = bastion_profile();
        profile.id = "cdtest-rec".into();

        assert!(reconcile_user_kubeconfig_at(&paths, &profile, &user, &bak)
            .await
            .is_none());
        assert_eq!(server_in(&user, "cdtest-rec"), "https://127.0.0.1:2222");
        let v: serde_yaml::Value =
            serde_yaml::from_str(&std::fs::read_to_string(&user).unwrap()).unwrap();
        assert_eq!(v["current-context"].as_str(), Some("cdtest-b"));
        std::fs::remove_dir_all(&paths.base).ok();
    }

    #[tokio::test]
    async fn reconcile_reports_a_corrupt_user_config_without_writing() {
        let (paths, user, bak) =
            reconcile_fixture("rec-corrupt", 2222, Some("clusters: [unclosed".into()));
        let mut profile = bastion_profile();
        profile.id = "cdtest-rec".into();

        let err = reconcile_user_kubeconfig_at(&paths, &profile, &user, &bak)
            .await
            .expect("corrupt user config must be reported");
        assert!(err.contains("cannot parse"), "{err}");
        assert_eq!(
            std::fs::read_to_string(&user).unwrap(),
            "clusters: [unclosed"
        );
        std::fs::remove_dir_all(&paths.base).ok();
    }

    #[tokio::test]
    async fn reconcile_leaves_a_matching_user_config_untouched() {
        let same = reconcile_kubeconfig("cdtest-rec", "https://127.0.0.1:2222");
        let (paths, user, bak) = reconcile_fixture("rec-same", 2222, Some(same.clone()));
        let mut profile = bastion_profile();
        profile.id = "cdtest-rec".into();

        assert!(reconcile_user_kubeconfig_at(&paths, &profile, &user, &bak)
            .await
            .is_none());
        assert_eq!(std::fs::read_to_string(&user).unwrap(), same);
        std::fs::remove_dir_all(&paths.base).ok();
    }

    #[tokio::test]
    async fn reconcile_ignores_a_profile_absent_from_the_user_config() {
        let other = reconcile_kubeconfig("cdtest-other", "https://127.0.0.1:1111");
        let (paths, user, bak) = reconcile_fixture("rec-absent", 2222, Some(other.clone()));
        let mut profile = bastion_profile();
        profile.id = "cdtest-rec".into();

        assert!(reconcile_user_kubeconfig_at(&paths, &profile, &user, &bak)
            .await
            .is_none());
        assert_eq!(std::fs::read_to_string(&user).unwrap(), other);

        // No user config file at all must not create one.
        let (paths2, user2, bak2) = reconcile_fixture("rec-nofile", 2222, None);
        assert!(
            reconcile_user_kubeconfig_at(&paths2, &profile, &user2, &bak2)
                .await
                .is_none()
        );
        assert!(!user2.exists());
        std::fs::remove_dir_all(&paths.base).ok();
        std::fs::remove_dir_all(&paths2.base).ok();
    }

    #[tokio::test]
    async fn restore_tunnels_continues_after_a_profile_fails() {
        let paths = tunnel_test_paths("restore-multi");
        // "cdtest-aaa" has no pinned TLS, so the guarded path refuses it; "cdtest-bbb" must still be tried.
        seed_restorable(&paths, "cdtest-aaa", 1, "");
        seed_restorable(&paths, "cdtest-bbb", 1, PINNED_TLS);
        let runner = SpawnOkRunner {
            spawns: AtomicUsize::new(0),
        };

        let results = restore_tunnels(&open_manager(), &runner, &paths).await;

        assert_eq!(results.len(), 2);
        let by_id: std::collections::HashMap<_, _> =
            results.iter().map(|(id, r)| (id.as_str(), r)).collect();
        assert!(by_id["cdtest-aaa"].is_err());
        assert!(matches!(by_id["cdtest-bbb"], Ok(true)));
        assert_eq!(runner.spawns.load(Ordering::SeqCst), 1);
        std::fs::remove_dir_all(&paths.base).ok();
    }

    #[tokio::test]
    async fn restore_tunnels_skips_a_profile_without_a_bastion() {
        let paths = tunnel_test_paths("restore-no-bastion");
        seed_restorable(&paths, "cdtest-ret", 1, PINNED_TLS);
        let mut profile = bastion_profile();
        profile.id = "cdtest-ret".into();
        profile.bastion = None;
        store::upsert_profile(&paths, profile).unwrap();

        let results = restore_tunnels(&open_manager(), &NeverCalledRunner, &paths).await;

        assert!(results.is_empty());
        std::fs::remove_dir_all(&paths.base).ok();
    }

    fn write_test_kubeconfig(paths: &ClusterDeckPaths, tls_lines: &str) {
        paths.ensure_dirs().unwrap();
        std::fs::write(
            paths.kubeconfig_file("test"),
            format!(
                "clusters:\n  - name: test\n    cluster:\n      server: https://127.0.0.1:16500\n{tls_lines}\ncontexts:\n  - name: test\n    context:\n      cluster: test\n      user: test\nusers:\n  - name: test\n    user:\n      token: t\n"
            ),
        )
        .unwrap();
    }

    #[tokio::test]
    async fn ensure_verify_tunnel_never_spawns_for_unpinned_or_insecure_kubeconfig() {
        for (name, tls) in [
            (
                "insecure",
                "      certificate-authority-data: ZmFrZS1jYQ==\n      insecure-skip-tls-verify: yes",
            ),
            ("noca", "      tls-server-name: 127.0.0.1"),
        ] {
            let paths = tunnel_test_paths(name);
            state::save_tunnel_target(
                &paths,
                "test",
                &crate::services::tunnel::TunnelTarget {
                    host: "127.0.0.1".to_string(),
                    port: 6443,
                },
            )
            .unwrap();
            write_test_kubeconfig(&paths, tls);
            let manager = crate::services::tunnel::TunnelManager::new(std::sync::Arc::new(
                crate::services::tunnel::SystemPortProbe,
            ));
            let err = ensure_verify_tunnel(
                &manager,
                &NeverCalledRunner,
                &paths,
                &bastion_profile(),
                &paths.kubeconfig_file("test"),
            )
            .await
            .unwrap_err();
            assert!(err.contains("API tunnel refused"), "{name}: {err}");
            std::fs::remove_dir_all(&paths.base).ok();
        }
    }

    struct MockDiscoveryRunner {
        service_json: String,
        ingress_json: String,
        hosts_path: Option<PathBuf>,
    }

    #[async_trait]
    impl CommandRunner for MockDiscoveryRunner {
        async fn run(&self, bin: &str, args: &[String]) -> Result<CommandOutput, String> {
            if bin == "osascript" {
                if let Some(script) = args.get(1) {
                    if let Some(start) = script.find("/bin/cp -P '") {
                        let rest = &script[start + 12..];
                        if let Some(end) = rest.find('\'') {
                            let src = &rest[..end];
                            if let Ok(content) = std::fs::read_to_string(src) {
                                if let Some(target) = &self.hosts_path {
                                    let _ = std::fs::write(target, content);
                                }
                            }
                        }
                    }
                }
                return Ok(CommandOutput {
                    stdout: String::new(),
                    stderr: String::new(),
                    success: true,
                });
            }
            let last = args.last().map(String::as_str).unwrap_or("");
            if last.ends_with("/api/v1/services") {
                Ok(CommandOutput {
                    stdout: self.service_json.clone(),
                    stderr: String::new(),
                    success: true,
                })
            } else if last.ends_with("/ingresses") {
                if self.ingress_json.is_empty() {
                    Ok(CommandOutput {
                        stdout: String::new(),
                        stderr: "error".into(),
                        success: false,
                    })
                } else {
                    Ok(CommandOutput {
                        stdout: self.ingress_json.clone(),
                        stderr: String::new(),
                        success: true,
                    })
                }
            } else {
                Ok(CommandOutput {
                    stdout: r#"{"items":[]}"#.into(),
                    stderr: String::new(),
                    success: true,
                })
            }
        }
    }

    #[tokio::test]
    async fn scan_failure_path_returns_warning_when_dns_or_cache_fails() {
        let paths = temp_paths("scan-fail");
        let mut p = profile_with_hosts(vec![key_host("cp1")]);
        p.id = "scan-test".into();
        store::upsert_profile(&paths, p).unwrap();
        paths.ensure_dirs().unwrap();
        std::fs::write(
            paths.kubeconfig_file("scan-test"),
            "clusters:\n- cluster:\n    server: https://127.0.0.1:6443\n",
        )
        .unwrap();

        let runner = MockDiscoveryRunner {
            service_json: r#"{"items":[]}"#.into(),
            ingress_json: r#"{"items":[]}"#.into(),
            hosts_path: None,
        };

        // 1. DNS environment resolution failure propagates warning
        let res1 = discover_cluster_endpoints_internal(
            &runner,
            &paths,
            Err("simulated dns error".to_string()),
            "scan-test",
        )
        .await
        .unwrap();
        assert!(res1.warning.is_some());
        assert!(res1.warning.unwrap().contains("simulated dns error"));

        // 2. Cache path write failure propagates warning
        let unwriteable_cache = std::path::PathBuf::from("/dev/null/impossible/endpoints.json");
        let res2 = discover_cluster_endpoints_internal(
            &runner,
            &paths,
            Ok(unwriteable_cache),
            "scan-test",
        )
        .await
        .unwrap();
        assert!(res2.warning.is_some());
        assert!(res2
            .warning
            .unwrap()
            .contains("failed to update endpoint cache"));

        // 3. Scan command failure for nonexistent profile
        let res3 = discover_cluster_endpoints_cmd("nonexistent-profile-xyz".into()).await;
        assert!(res3.is_err());

        std::fs::remove_dir_all(&paths.base).ok();
    }

    #[tokio::test]
    async fn hosts_sync_partial_discovery_preserves_last_known_records() {
        let paths = temp_paths("hosts-partial");
        let mut p = profile_with_hosts(vec![key_host("cp1")]);
        p.id = "hosts-test".into();
        p.host_mapping = crate::services::config::HostMapping::Hosts;
        store::upsert_profile(&paths, p.clone()).unwrap();
        paths.ensure_dirs().unwrap();
        std::fs::write(
            paths.kubeconfig_file("hosts-test"),
            "clusters:\n- cluster:\n    server: https://127.0.0.1:6443\n",
        )
        .unwrap();

        let hosts_file = paths.base.join("hosts_test_file");
        // Initial hosts block has an existing ingress endpoint: api.lab.example.internal
        let initial_block =
            "# >>> ClusterDeck BEGIN (profile: hosts-test) >>>\n192.0.2.10 cp1.hosts-test.clusterdeck.local\n# Discovered cluster endpoints (Ingress, APISIX, etc.)\n192.0.2.60 api.lab.example.internal\n# <<< ClusterDeck END (profile: hosts-test) <<<\n";
        std::fs::write(&hosts_file, initial_block).unwrap();

        // Runner where services query succeeds with a new svc endpoint, but ingress query fails
        let runner = MockDiscoveryRunner {
            service_json: r#"{
                "items": [{
                    "metadata": {"name": "my-svc", "namespace": "default", "annotations": {"external-dns.alpha.kubernetes.io/hostname": "svc.lab.example.internal"}},
                    "spec": {"type": "LoadBalancer"},
                    "status": {"loadBalancer": {"ingress": [{"ip": "192.0.2.50"}]}}
                }]
            }"#.into(),
            ingress_json: String::new(), // causes ingress failure
            hosts_path: Some(hosts_file.clone()),
        };

        let res = sync_hosts_file_with_deps(&runner, &paths, &hosts_file, "hosts-test")
            .await
            .unwrap();

        assert!(!res.success);
        assert!(res
            .warning
            .as_ref()
            .unwrap()
            .contains("keeping last known records"));
        let hosts_content = std::fs::read_to_string(&hosts_file).unwrap();

        // Must keep the old ingress endpoint from previous hosts block AND include the new service endpoint
        assert!(
            hosts_content.contains("192.0.2.60 api.lab.example.internal"),
            "hosts content missing preserved ingress endpoint: {hosts_content}"
        );
        assert!(
            hosts_content.contains("192.0.2.50 svc.lab.example.internal"),
            "hosts content missing new service endpoint: {hosts_content}"
        );
        assert_eq!(res.endpoints_count, 2);

        std::fs::remove_dir_all(&paths.base).ok();
    }
}
