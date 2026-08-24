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
pub fn delete_profile_cmd(profile_id: String) -> Result<(), String> {
    let paths = ClusterDeckPaths::resolve()?;
    store::delete_profile(&paths, &profile_id)
}
