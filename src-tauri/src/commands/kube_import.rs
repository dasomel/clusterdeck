#[tauri::command]
pub fn list_local_kube_contexts_cmd(
) -> Result<Vec<crate::services::kube_import::LocalKubeContext>, String> {
    crate::services::kube_import::list_local_kube_contexts()
}
