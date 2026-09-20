use crate::services::local_runtime::{self, DiscoveredLocalHost};
use crate::services::process::SystemRunner;

#[tauri::command]
pub async fn detect_local_hosts() -> Result<Vec<DiscoveredLocalHost>, String> {
    let runner = SystemRunner;
    local_runtime::detect_local_hosts(&runner).await
}
