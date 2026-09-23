use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::Serialize;

use crate::services::config::{AuthMode, Profile};
use crate::services::k8s_endpoints::DiscoveredEndpoint;
use crate::services::kubeconfig::KubeconfigSummary;
use crate::services::paths::ClusterDeckPaths;
use crate::services::process::SystemRunner;
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
}

#[derive(Debug, Clone, Serialize)]
pub struct HostsFileStatus {
    pub managed_by_profile: bool,
    pub is_synced: bool,
    pub active_entries: Vec<String>,
    pub pending_entries: Vec<String>,
}

#[tauri::command]
pub async fn probe_profile_hosts(
    profile_id: String,
    password: Option<String>,
) -> Result<Vec<HostStageResult>, String> {
    let paths = ClusterDeckPaths::resolve()?;
    let profile = store::get_profile(&paths, &profile_id)?;
    let runner = SystemRunner;
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

#[tauri::command]
pub async fn bootstrap_profile(
    profile_id: String,
    password: String,
) -> Result<Vec<BootstrapResult>, String> {
    let paths = ClusterDeckPaths::resolve()?;
    let profile = store::get_profile(&paths, &profile_id)?;
    let runner = SystemRunner;

    let mut results = Vec::new();
    for host in &profile.hosts {
        let boot = ssh::bootstrap_host(
            &runner,
            host,
            profile.bastion.as_ref(),
            &password,
            profile.bootstrap.retries,
            Duration::from_secs(profile.bootstrap.retry_delay_secs),
        )
        .await;
        results.push(boot);
    }
    Ok(results)
}

#[tauri::command]
pub async fn generate_aliases(profile_id: String) -> Result<(), String> {
    let paths = ClusterDeckPaths::resolve()?;
    let profile = store::get_profile(&paths, &profile_id)?;

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
    let profile = store::get_profile(&paths, &profile_id)?;
    let runner = SystemRunner;

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

#[tauri::command]
pub async fn verify_profile(profile_id: String) -> Result<VerificationResult, String> {
    let paths = ClusterDeckPaths::resolve()?;
    let profile = store::get_profile(&paths, &profile_id)?;
    let runner = SystemRunner;

    let kubeconfig_path = paths.kubeconfig_file(&profile_id);
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

    // No password parameter on this command: it's a read-only/polling status check, so
    // password-auth hosts can't be probed here and will simply report unreachable until a
    // Connect or Test Connection call (which do take a password) refreshes the cached status.
    let mut any_reachable = false;
    for host in &profile.hosts {
        let probe = ssh::probe_with_retry(
            &runner,
            host,
            profile.bastion.as_ref(),
            1,
            Duration::from_secs(1),
            None,
        )
        .await;
        if probe.reachable {
            any_reachable = true;
            break;
        }
    }
    result.ssh = any_reachable;

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
    let profile = store::get_profile(&paths, &profile_id)?;
    let runner = SystemRunner;
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
            Ok(summary) => {
                kubeconfig_summary = Some(summary);
            }
            Err(e) => {
                errors.push(format!("kubeconfig fetch failed: {e}"));
            }
        }
    }

    let kubeconfig_path = paths.kubeconfig_file(&profile_id);
    let kubeconfig_exists = kubeconfig_summary.is_some() || kubeconfig_path.exists();
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
    if verification.kubernetes && kubeconfig_exists {
        let default_ip = profile.hosts.first().map(|h| h.address.as_str());
        match crate::services::k8s_endpoints::discover_cluster_endpoints(
            &runner,
            &kubeconfig_path,
            default_ip,
        )
        .await
        {
            Ok(discovered) => {
                endpoints = discovered;
            }
            Err(e) => {
                errors.push(format!("endpoint discovery warning: {e}"));
            }
        }
    }

    if profile.manage_hosts_file {
        if let Err(e) = crate::services::hosts_file::upsert_hosts_block_with_endpoints(
            &runner, &profile, &endpoints,
        )
        .await
        {
            errors.push(format!("hosts file update failed: {e}"));
        }
    }

    // If this profile was previously merged into ~/.kube/config, auto-sync fresh credentials
    if verification.kubernetes
        && kubeconfig_exists
        && crate::services::kubeconfig::is_profile_present_in_user_config(&profile_id)
    {
        if let Err(e) = crate::services::kubeconfig::merge_profile_kubeconfig_to_user_config(
            &runner, &paths, &profile, false,
        )
        .await
        {
            errors.push(format!("automatic ~/.kube/config update failed: {e}"));
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

#[tauri::command]
pub async fn discover_cluster_endpoints_cmd(
    profile_id: String,
) -> Result<Vec<DiscoveredEndpoint>, String> {
    let paths = ClusterDeckPaths::resolve()?;
    let profile = store::get_profile(&paths, &profile_id)?;
    let runner = SystemRunner;
    let kubeconfig_path = paths.kubeconfig_file(&profile_id);

    if !kubeconfig_path.exists() {
        return Err("kubeconfig not found for profile; please connect first".to_string());
    }

    let default_ip = profile.hosts.first().map(|h| h.address.as_str());
    crate::services::k8s_endpoints::discover_cluster_endpoints(
        &runner,
        &kubeconfig_path,
        default_ip,
    )
    .await
}

#[tauri::command]
pub async fn sync_hosts_file_cmd(profile_id: String) -> Result<SyncHostsResult, String> {
    let paths = ClusterDeckPaths::resolve()?;
    let profile = store::get_profile(&paths, &profile_id)?;
    let runner = SystemRunner;
    let kubeconfig_path = paths.kubeconfig_file(&profile_id);

    let mut endpoints = Vec::new();
    if kubeconfig_path.exists() {
        let default_ip = profile.hosts.first().map(|h| h.address.as_str());
        if let Ok(eps) = crate::services::k8s_endpoints::discover_cluster_endpoints(
            &runner,
            &kubeconfig_path,
            default_ip,
        )
        .await
        {
            endpoints = eps;
        }
    }

    crate::services::hosts_file::upsert_hosts_block_with_endpoints(&runner, &profile, &endpoints)
        .await?;

    let hosts_count = profile.hosts.len() + if profile.bastion.is_some() { 1 } else { 0 };
    let endpoints_count = endpoints.len();

    Ok(SyncHostsResult {
        success: true,
        endpoints_count,
        hosts_count,
        endpoints,
        message: format!(
            "Successfully synced {hosts_count} host(s) and {endpoints_count} cluster endpoint(s) to /etc/hosts"
        ),
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

    Ok(HostsFileStatus {
        managed_by_profile: profile.manage_hosts_file,
        is_synced,
        active_entries,
        pending_entries,
    })
}

#[tauri::command]
pub async fn remove_hosts_file_cmd(profile_id: String) -> Result<SyncHostsResult, String> {
    let paths = ClusterDeckPaths::resolve()?;
    let _profile = store::get_profile(&paths, &profile_id)?;
    let runner = SystemRunner;
    crate::services::hosts_file::remove_hosts_block(&runner, &profile_id).await?;

    Ok(SyncHostsResult {
        success: true,
        endpoints_count: 0,
        hosts_count: 0,
        endpoints: vec![],
        message: format!("Successfully removed entries for profile '{profile_id}' from /etc/hosts"),
    })
}

#[tauri::command]
pub async fn open_ssh_session(profile_id: String, host_name: String) -> Result<(), String> {
    let paths = ClusterDeckPaths::resolve()?;
    let profile = store::get_profile(&paths, &profile_id)?;

    let target_host = profile
        .hosts
        .iter()
        .find(|h| h.name == host_name)
        .ok_or_else(|| format!("host not found: {host_name}"))?;

    ssh_config::write_profile_config(&paths, &profile)?;
    let home = std::env::var("HOME").map_err(|e| e.to_string())?;
    let home_ssh_config = PathBuf::from(home).join(".ssh").join("config");
    ssh_config::ensure_ssh_include(&home_ssh_config, &paths)?;
    let runner = SystemRunner;

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
