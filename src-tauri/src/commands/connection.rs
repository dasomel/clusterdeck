use serde::Serialize;

#[derive(Debug, Serialize)]
pub struct ConnectionResult {
    pub ssh: bool,
    pub kubeconfig: bool,
    pub kubernetes: bool,
}

#[tauri::command]
pub async fn test_connection(_profile_id: String) -> Result<ConnectionResult, String> {
    // TODO: Wire discovery → SSH → kubeconfig → kubectl verification.
    Ok(ConnectionResult {
        ssh: false,
        kubeconfig: false,
        kubernetes: false,
    })
}
