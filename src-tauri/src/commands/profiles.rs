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

#[tauri::command]
pub fn save_profile(profile: Profile) -> Result<(), String> {
    let paths = ClusterDeckPaths::resolve()?;
    save_profile_with_paths(&paths, profile)
}

#[tauri::command]
pub async fn delete_profile_cmd(profile_id: String) -> Result<(), String> {
    if !crate::services::validate::is_safe_profile_id(&profile_id) {
        return Err("invalid profile id".to_string());
    }
    let paths = ClusterDeckPaths::resolve()?;
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
    let _ = crate::services::state::delete_status(&paths, &profile_id);

    store::delete_profile(&paths, &profile_id)
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
            manage_hosts_file: false,
            trusted_cas,
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
