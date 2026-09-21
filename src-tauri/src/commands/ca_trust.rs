use chrono::Utc;

use crate::services::ca_trust::{self, CaTrustStatus, TrustedCa};
use crate::services::paths::ClusterDeckPaths;
use crate::services::process::SystemRunner;
use crate::services::store;

#[derive(Debug, Clone, serde::Serialize)]
pub struct DiscoveredCaView {
    pub secret_ref: String,
    pub source_hosts: Vec<String>,
    pub subject_cn: String,
    pub not_after: String,
    pub fingerprint_sha256: String,
    pub status: String, // "new" | "trusted" | "rotated"
    pub warnings: Vec<String>,
}

fn status_str(status: &CaTrustStatus) -> String {
    match status {
        CaTrustStatus::New => "new".to_string(),
        CaTrustStatus::Trusted => "trusted".to_string(),
        CaTrustStatus::Rotated => "rotated".to_string(),
    }
}

fn split_secret_ref(secret_ref: &str) -> Result<(String, String), String> {
    match secret_ref.split_once('/') {
        Some((ns, name)) if !ns.is_empty() && !name.is_empty() => {
            Ok((ns.to_string(), name.to_string()))
        }
        _ => Err(format!("invalid secret ref: {secret_ref}")),
    }
}

#[tauri::command]
pub async fn discover_cluster_cas_cmd(
    profile_id: String,
    endpoints: Vec<crate::services::k8s_endpoints::DiscoveredEndpoint>,
) -> Result<Vec<DiscoveredCaView>, String> {
    let paths = ClusterDeckPaths::resolve()?;
    let profile = store::get_profile(&paths, &profile_id)?;
    let runner = SystemRunner;
    let kubeconfig_path = paths.kubeconfig_file(&profile_id);
    if !kubeconfig_path.exists() {
        return Err("kubeconfig not found for profile; please connect first".to_string());
    }

    let discovered = ca_trust::discover_cluster_cas(&runner, &kubeconfig_path, &endpoints).await?;

    Ok(discovered
        .into_iter()
        .map(|d| {
            let status = ca_trust::compute_trust_status(&d, &profile.trusted_cas);
            DiscoveredCaView {
                secret_ref: d.secret_ref,
                source_hosts: d.source_hosts,
                subject_cn: d.meta.subject_cn,
                not_after: d.meta.not_after,
                fingerprint_sha256: d.meta.fingerprint_sha256,
                status: status_str(&status),
                warnings: d.meta.warnings,
            }
        })
        .collect())
}

#[tauri::command]
pub async fn trust_ca_cmd(profile_id: String, secret_ref: String) -> Result<TrustedCa, String> {
    let paths = ClusterDeckPaths::resolve()?;
    let mut profile = store::get_profile(&paths, &profile_id)?;
    let runner = SystemRunner;
    let kubeconfig_path = paths.kubeconfig_file(&profile_id);
    if !kubeconfig_path.exists() {
        return Err("kubeconfig not found for profile; please connect first".to_string());
    }

    let (namespace, name) = split_secret_ref(&secret_ref)?;
    let meta = ca_trust::fetch_ca(&runner, &kubeconfig_path, &namespace, &name).await?;
    ca_trust::trust_ca(&runner, &meta.pem).await?;

    let record = TrustedCa {
        secret_ref: secret_ref.clone(),
        fingerprint_sha256: meta.fingerprint_sha256,
        fingerprint_sha1: meta.fingerprint_sha1,
        subject_cn: meta.subject_cn,
        not_after: meta.not_after,
        trusted_at: Utc::now().to_rfc3339(),
    };
    // Idempotent: replaces any prior record for this secret_ref rather than duplicating it,
    // so re-confirming an already-Trusted CA (or completing a Rotated -> trust cycle) is safe.
    profile.trusted_cas.retain(|c| c.secret_ref != secret_ref);
    profile.trusted_cas.push(record.clone());
    store::upsert_profile(&paths, profile)?;

    Ok(record)
}

#[tauri::command]
pub async fn replace_ca_cmd(profile_id: String, secret_ref: String) -> Result<TrustedCa, String> {
    let paths = ClusterDeckPaths::resolve()?;
    let profile = store::get_profile(&paths, &profile_id)?;
    let runner = SystemRunner;

    // Validate preconditions before the destructive untrust step below, so the common
    // not-connected / malformed-secret-ref case never removes the old trust entry for nothing.
    split_secret_ref(&secret_ref)?;
    let kubeconfig_path = paths.kubeconfig_file(&profile_id);
    if !kubeconfig_path.exists() {
        return Err("kubeconfig not found for profile; please connect first".to_string());
    }

    if let Some(old) = profile
        .trusted_cas
        .iter()
        .find(|c| c.secret_ref == secret_ref)
    {
        // Best-effort: if the old cert is already gone from the keychain (e.g. the user
        // removed it by hand), that must not block trusting the new one.
        let _ = ca_trust::untrust_ca(&runner, &old.fingerprint_sha1).await;
    }

    match trust_ca_cmd(profile_id.clone(), secret_ref.clone()).await {
        Ok(record) => Ok(record),
        Err(e) => {
            // The old cert may already be out of the keychain (untrust above) while the new
            // one failed to go in -- keeping the stale record would report a false "trusted"
            // status on the next discover. Drop it so the CA correctly shows as untrusted
            // again rather than lying about its state.
            if let Ok(mut profile) = store::get_profile(&paths, &profile_id) {
                profile.trusted_cas.retain(|c| c.secret_ref != secret_ref);
                let _ = store::upsert_profile(&paths, profile);
            }
            Err(e)
        }
    }
}

#[tauri::command]
pub async fn remove_ca_cmd(profile_id: String, secret_ref: String) -> Result<(), String> {
    let paths = ClusterDeckPaths::resolve()?;
    let mut profile = store::get_profile(&paths, &profile_id)?;
    let runner = SystemRunner;

    if let Some(record) = profile
        .trusted_cas
        .iter()
        .find(|c| c.secret_ref == secret_ref)
    {
        // Best-effort, same precedent as replace_ca_cmd: the keychain entry may already be
        // gone (removed by hand, or by a prior operation), which must not block clearing our
        // own bookkeeping -- the user's intent is "stop tracking this as trusted".
        let _ = ca_trust::untrust_ca(&runner, &record.fingerprint_sha1).await;
    }
    profile.trusted_cas.retain(|c| c.secret_ref != secret_ref);
    store::upsert_profile(&paths, profile)
}
