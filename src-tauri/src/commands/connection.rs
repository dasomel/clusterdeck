use std::path::PathBuf;
use std::time::Duration;

use serde::Serialize;

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
    pub errors: Vec<String>,
}

#[tauri::command]
pub async fn probe_profile_hosts(profile_id: String) -> Result<Vec<HostStageResult>, String> {
    let paths = ClusterDeckPaths::resolve()?;
    let profile = store::get_profile(&paths, &profile_id)?;
    let runner = SystemRunner;

    let host_futures = profile.hosts.iter().map(|host| async {
        let probe = ssh::probe_with_retry(
            &runner,
            host,
            profile.bastion.as_ref(),
            1,
            Duration::from_secs(1),
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
pub async fn fetch_kubeconfig(profile_id: String) -> Result<KubeconfigSummary, String> {
    let paths = ClusterDeckPaths::resolve()?;
    let profile = store::get_profile(&paths, &profile_id)?;
    let runner = SystemRunner;

    crate::services::kubeconfig::fetch_and_store(&runner, &paths, &profile).await
}

#[tauri::command]
pub async fn verify_profile(profile_id: String) -> Result<VerificationResult, String> {
    let paths = ClusterDeckPaths::resolve()?;
    let profile = store::get_profile(&paths, &profile_id)?;
    let runner = SystemRunner;

    let kubeconfig_path = paths.kubeconfig_file(&profile_id);
    let mut result = if kubeconfig_path.exists() {
        verify::verify_cluster(&runner, &kubeconfig_path, &profile_id).await
    } else {
        VerificationResult {
            ssh: false,
            kubeconfig: false,
            kubernetes: false,
            node_count: None,
            api_endpoint: None,
            last_verified: None,
        }
    };

    result.kubeconfig = kubeconfig_path.exists();

    let mut any_reachable = false;
    for host in &profile.hosts {
        let probe = ssh::probe_with_retry(
            &runner,
            host,
            profile.bastion.as_ref(),
            1,
            Duration::from_secs(1),
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
) -> Result<ConnectionResult, String> {
    let paths = ClusterDeckPaths::resolve()?;
    let profile = store::get_profile(&paths, &profile_id)?;
    let runner = SystemRunner;

    let host_futures = profile.hosts.iter().map(|host| async {
        let mut probe = ssh::probe_with_retry(
            &runner,
            host,
            profile.bastion.as_ref(),
            profile.bootstrap.retries,
            Duration::from_secs(profile.bootstrap.retry_delay_secs),
        )
        .await;

        if !probe.reachable && profile.bootstrap.enabled {
            if let Some(pwd) = bootstrap_password.as_ref() {
                let boot_res = ssh::bootstrap_host(
                    &runner,
                    host,
                    profile.bastion.as_ref(),
                    pwd,
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
        match crate::services::kubeconfig::fetch_and_store(&runner, &paths, &profile).await {
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

    let mut verification = if kubeconfig_exists {
        verify::verify_cluster(&runner, &kubeconfig_path, &profile_id).await
    } else {
        VerificationResult {
            ssh: false,
            kubeconfig: false,
            kubernetes: false,
            node_count: None,
            api_endpoint: None,
            last_verified: None,
        }
    };

    verification.ssh = any_host_reachable;
    verification.kubeconfig = kubeconfig_exists;

    let _ = state::save_status(&paths, &profile_id, verification.clone());

    Ok(ConnectionResult {
        hosts: host_stage_results,
        aliases_written,
        kubeconfig: kubeconfig_summary,
        verification,
        errors,
    })
}
