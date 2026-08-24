use serde::Serialize;

#[derive(Debug, Serialize)]
pub struct AppInfo {
    pub name: &'static str,
    pub version: &'static str,
    pub platform: &'static str,
}

#[tauri::command]
pub fn get_app_info() -> AppInfo {
    AppInfo {
        name: "ClusterDeck",
        version: env!("CARGO_PKG_VERSION"),
        platform: "macOS",
    }
}
