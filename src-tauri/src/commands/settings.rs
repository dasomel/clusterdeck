use crate::services::app_settings::{self, AppSettings, InstalledApps};
use crate::services::paths::ClusterDeckPaths;
use crate::services::process::SystemRunner;

#[tauri::command]
pub async fn get_app_settings() -> Result<AppSettings, String> {
    Ok(app_settings::load_settings(&ClusterDeckPaths::resolve()?))
}

#[tauri::command]
pub async fn set_app_settings(settings: AppSettings) -> Result<(), String> {
    app_settings::save_settings(&ClusterDeckPaths::resolve()?, &settings)
}

#[tauri::command]
pub async fn list_installed_apps() -> Result<InstalledApps, String> {
    Ok(app_settings::list_installed_apps(&SystemRunner).await)
}
