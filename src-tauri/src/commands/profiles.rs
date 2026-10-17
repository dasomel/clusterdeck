use crate::services::{config::Profile, paths::ClusterDeckPaths, store};

#[tauri::command]
pub fn list_profiles() -> Result<Vec<Profile>, String> {
    let paths = ClusterDeckPaths::resolve()?;
    store::load_profiles(&paths)
}

#[tauri::command]
pub fn get_profile_cmd(profile_id: String) -> Result<Profile, String> {
    let paths = ClusterDeckPaths::resolve()?;
    store::get_profile(&paths, &profile_id)
}

// Split out of `save_profile` so the trusted_cas-preserving logic can be unit tested against a
// temp ClusterDeckPaths, the same way services/store.rs's functions are -- `save_profile` itself
// hardcodes ClusterDeckPaths::resolve() (reads $HOME), which a test should not override via a
// global env var mutation.
fn save_profile_with_paths(paths: &ClusterDeckPaths, mut profile: Profile) -> Result<(), String> {
    // trusted_cas is backend-owned: it's only ever mutated by trust_ca_cmd/replace_ca_cmd
    // (services/ca_trust.rs), never by the profile-editor form. Preserve whatever is currently
    // stored rather than trusting the frontend's copy, which can be stale -- otherwise saving
    // any other field change silently discards trust records and permanently orphans a
    // trusted CA in the keychain (no fingerprint left to ever untrust it).
    if let Ok(existing) = store::get_profile(paths, &profile.id) {
        profile.trusted_cas = existing.trusted_cas;
    }
    store::upsert_profile(paths, profile)
}

use crate::commands::local_dns::DnsApplyView;

#[tauri::command]
pub async fn save_profile(
    guard: tauri::State<'_, crate::services::store::ProfileWriteGuard>,
    profile: Profile,
) -> Result<Option<DnsApplyView>, String> {
    let paths = ClusterDeckPaths::resolve()?;
    save_profile_internal(
        &paths,
        &guard,
        profile,
        crate::commands::local_dns::SystemDns::resolve,
        crate::commands::local_dns::hosts_path(),
    )
    .await
}

pub(crate) async fn save_profile_internal<R>(
    paths: &ClusterDeckPaths,
    guard: &crate::services::store::ProfileWriteGuard,
    profile: Profile,
    resolve_dns: R,
    hosts_path: &std::path::Path,
) -> Result<Option<DnsApplyView>, String>
where
    R: FnOnce() -> Result<crate::commands::local_dns::SystemDns, String>,
{
    let _write_lock = guard.lock().await;
    let _lane_lock = crate::commands::local_dns::lock_lane().await;

    let old = store::get_profile(paths, &profile.id).ok();
    save_profile_with_paths(paths, profile.clone())?;

    let latest = store::get_profile(paths, &profile.id).unwrap_or(profile);
    if !crate::commands::local_dns::needs_reapply_after_save(old.as_ref(), &latest)
        && !crate::commands::local_dns::stale_block_after_save(hosts_path, &latest)
    {
        return Ok(None);
    }

    let dns = resolve_dns()?;
    crate::commands::local_dns::reapply_after_save_locked(
        paths,
        &dns.env(),
        hosts_path,
        old.as_ref(),
        &latest,
    )
    .await
    .map_err(|e| format!("profile saved, but updating local DNS failed: {e}"))
}

#[tauri::command]
pub async fn delete_profile_cmd(
    guard: tauri::State<'_, crate::services::store::ProfileWriteGuard>,
    profile_id: String,
) -> Result<Option<DnsApplyView>, String> {
    let paths = ClusterDeckPaths::resolve()?;
    delete_profile_internal(&paths, &guard, profile_id).await
}

pub(crate) async fn delete_profile_internal(
    paths: &ClusterDeckPaths,
    guard: &crate::services::store::ProfileWriteGuard,
    profile_id: String,
) -> Result<Option<DnsApplyView>, String> {
    if !crate::services::validate::is_safe_profile_id(&profile_id) {
        return Err("invalid profile id".to_string());
    }
    let _write_lock = guard.lock().await;
    let _lane_lock = crate::commands::local_dns::lock_lane().await;
    let runner = crate::services::process::SystemRunner;

    let _ = crate::services::hosts_file::remove_hosts_block(&runner, &profile_id).await;

    let ssh_conf = paths.ssh_conf(&profile_id);
    if ssh_conf.exists() {
        let _ = std::fs::remove_file(ssh_conf);
    }
    let kc_file = paths.kubeconfig_file(&profile_id);
    if kc_file.exists() {
        let _ = std::fs::remove_file(kc_file);
    }
    crate::services::tunnel::global()
        .stop_tunnel(&profile_id)
        .await;
    let _ = crate::services::state::delete_status(paths, &profile_id);

    let was_local_dns = delete_stored_profile(paths, &profile_id)?;
    crate::commands::local_dns::remove_cached_endpoints(
        &paths.endpoints_cache_file(),
        &profile_id,
    )?;
    if !was_local_dns {
        return Ok(None);
    }
    let dns = crate::commands::local_dns::SystemDns::resolve()?;
    let (view, _) = crate::commands::local_dns::reapply_locked(
        paths,
        &dns.env(),
        Some(&profile_id),
        None,
        None,
        Some(&profile_id),
    )
    .await
    .map_err(|e| format!("profile deleted, but updating local DNS failed: {e}"))?;
    Ok(Some(view))
}

/// Deletes the stored profile and reports whether it used local DNS (its zones must then leave
/// the resolver; see `local_dns::reapply_after_delete`).
pub(crate) fn delete_stored_profile(
    paths: &ClusterDeckPaths,
    profile_id: &str,
) -> Result<bool, String> {
    let was_local_dns = store::get_profile(paths, profile_id)
        .map(|p| p.host_mapping == crate::services::config::HostMapping::LocalDns)
        .unwrap_or(false);
    store::delete_profile(paths, profile_id)?;
    Ok(was_local_dns)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::ca_trust::TrustedCa;
    use crate::services::config::BootstrapPolicy;

    fn temp_paths(tag: &str) -> ClusterDeckPaths {
        let dir = std::env::temp_dir().join(format!(
            "clusterdeck-profiles-cmd-test-{tag}-{}",
            std::process::id()
        ));
        std::fs::remove_dir_all(&dir).ok();
        ClusterDeckPaths::at(dir)
    }

    fn sample_profile(id: &str, trusted_cas: Vec<TrustedCa>) -> Profile {
        Profile {
            id: id.to_string(),
            name: "Test Profile".to_string(),
            hosts: vec![],
            bastion: None,
            bootstrap: BootstrapPolicy::default(),
            kubeconfig: None,
            host_mapping: crate::services::config::HostMapping::Off,
            approved_dns_zones: Vec::new(),
            gateway_address: None,
            trusted_cas,
            local_runtime: None,
        }
    }

    fn sample_trusted_ca() -> TrustedCa {
        TrustedCa {
            secret_ref: "platform-system/apisix-gateway-tls-secret".into(),
            fingerprint_sha256: "8b75bf97e19ce7efe9bb4d6c76b4f10e072a9d6ea89d8f438f423afea7156246"
                .into(),
            fingerprint_sha1: "67fc8cc8df72476829ecd88d188331a6d29baabb".into(),
            subject_cn: "clusterdeck-test-ca.invalid".into(),
            not_after: "Sep 18 05:40:47 2036 GMT".into(),
            trusted_at: "2026-09-21T00:00:00+00:00".into(),
        }
    }

    #[test]
    fn save_profile_preserves_existing_trusted_cas_when_incoming_value_is_stale() {
        let paths = temp_paths("preserve-on-edit");

        // Seed a stored profile that already has a trusted CA, as if trust_ca_cmd ran earlier
        // in a previous session/request.
        store::upsert_profile(&paths, sample_profile("cka", vec![sample_trusted_ca()])).unwrap();

        // Simulate the frontend's stale cache: ProfileEditor saves the profile back with
        // `trusted_cas: []` because its in-memory `profiles` array was never refreshed after
        // the earlier trust action, per Fix 1's bug description.
        let edited_with_stale_empty_cas = sample_profile("cka", vec![]);
        save_profile_with_paths(&paths, edited_with_stale_empty_cas).unwrap();

        let reloaded = store::get_profile(&paths, "cka").unwrap();
        assert_eq!(
            reloaded.trusted_cas.len(),
            1,
            "the stored trusted_cas must survive an edit that carries a stale empty value"
        );
        assert_eq!(reloaded.trusted_cas[0], sample_trusted_ca());
    }

    #[test]
    fn save_profile_accepts_incoming_trusted_cas_for_a_brand_new_profile() {
        let paths = temp_paths("new-profile");

        // No profile exists yet for this id -- store::get_profile returns Err, so the `if let
        // Ok` guard in save_profile_with_paths must not fire, and the given (empty) trusted_cas
        // is what gets saved.
        assert!(store::get_profile(&paths, "fresh").is_err());
        save_profile_with_paths(&paths, sample_profile("fresh", vec![])).unwrap();

        let reloaded = store::get_profile(&paths, "fresh").unwrap();
        assert_eq!(reloaded.trusted_cas.len(), 0);
    }
}
