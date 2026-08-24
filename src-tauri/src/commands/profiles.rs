use serde::Serialize;

#[derive(Debug, Serialize, Clone)]
pub struct ProfileSummary {
    pub id: String,
    pub name: String,
}

#[tauri::command]
pub fn list_profiles() -> Vec<ProfileSummary> {
    // TODO: Load profiles from ~/.clusterdeck/profiles.yaml.
    Vec::new()
}
