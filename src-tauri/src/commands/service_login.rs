use crate::services::paths::ClusterDeckPaths;
use crate::services::process::SystemRunner;
use crate::services::service_login::{
    self, EndpointLoginOption, LoginErrorCode, RevealedEndpointLogin,
};

#[tauri::command]
pub async fn endpoint_login_options(
    window: tauri::WebviewWindow,
    profile_id: String,
) -> Result<Vec<EndpointLoginOption>, String> {
    require_main_caller(window.label())?;
    let paths =
        ClusterDeckPaths::resolve().map_err(|_| LoginErrorCode::KubectlFailed.to_string())?;
    service_login::endpoint_login_options(&paths, &profile_id).map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn check_endpoint_login(
    window: tauri::WebviewWindow,
    profile_id: String,
    endpoint_id: String,
) -> Result<EndpointLoginOption, String> {
    require_main_caller(window.label())?;
    let paths =
        ClusterDeckPaths::resolve().map_err(|_| LoginErrorCode::KubectlFailed.to_string())?;
    let runner = SystemRunner;
    service_login::check_endpoint_login_with_deps(&runner, &paths, &profile_id, &endpoint_id)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn reveal_endpoint_login(
    window: tauri::WebviewWindow,
    profile_id: String,
    endpoint_id: String,
) -> Result<RevealedEndpointLogin, String> {
    require_main_caller(window.label())?;
    let paths =
        ClusterDeckPaths::resolve().map_err(|_| LoginErrorCode::KubectlFailed.to_string())?;
    let runner = SystemRunner;
    service_login::reveal_endpoint_login_with_deps(&runner, &paths, &profile_id, &endpoint_id)
        .await
        .map_err(|e| e.to_string())
}

fn require_main_caller(label: &str) -> Result<(), String> {
    if label == "main" {
        Ok(())
    } else {
        Err(LoginErrorCode::ForbiddenCaller.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn service_login_commands_refuse_non_main_callers() {
        assert!(require_main_caller("main").is_ok());
        for label in ["", "settings", "main-other"] {
            assert_eq!(require_main_caller(label).unwrap_err(), "forbidden_caller");
        }
    }
}
