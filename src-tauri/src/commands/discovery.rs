use crate::services::discovery::{self, DiscoveredHost};

#[tauri::command]
pub async fn discover_hosts(
    input: String,
    port: Option<u16>,
) -> Result<Vec<DiscoveredHost>, String> {
    let targets = discovery::expand_targets(&input)?;
    Ok(discovery::probe_targets(targets, port.unwrap_or(22), 1500).await)
}
