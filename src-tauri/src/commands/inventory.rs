use crate::services::inventory::{self, CreateProfileOutcome, Inventory};
use crate::services::paths::ClusterDeckPaths;
use crate::services::process::SystemRunner;
use crate::services::store::ProfileWriteGuard;

#[tauri::command]
pub async fn discover_inventory(demo: bool) -> Inventory {
    let runner = SystemRunner;
    inventory::discover(&runner, demo).await
}

#[tauri::command]
pub async fn create_profile_from_environment(
    guard: tauri::State<'_, ProfileWriteGuard>,
    environment: String,
) -> Result<CreateProfileOutcome, String> {
    let _lock = guard.lock().await;
    let runner = SystemRunner;
    let paths = ClusterDeckPaths::resolve()?;
    inventory::create_profile_from_environment(&runner, &paths, &environment).await
}
