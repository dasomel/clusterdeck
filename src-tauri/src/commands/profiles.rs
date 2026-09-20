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

#[tauri::command]
pub fn save_profile(profile: Profile) -> Result<(), String> {
    let paths = ClusterDeckPaths::resolve()?;
    store::upsert_profile(&paths, profile)
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
