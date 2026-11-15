use crate::services::clipboard;
use crate::services::process::SystemRunner;

// Main window only: the pasteboard is where revealed credentials land, so a secondary window
// must not be able to clear or probe it.
fn require_main_caller(label: &str) -> Result<(), String> {
    if label == "main" {
        Ok(())
    } else {
        Err("forbidden_caller".to_string())
    }
}

#[tauri::command]
pub async fn clipboard_change_count(window: tauri::WebviewWindow) -> Result<i64, String> {
    require_main_caller(window.label())?;
    clipboard::change_count(&SystemRunner).await
}

#[tauri::command]
pub async fn clear_clipboard_if_change_count(
    window: tauri::WebviewWindow,
    expected: i64,
) -> Result<bool, String> {
    require_main_caller(window.label())?;
    clipboard::clear_if_change_count(&SystemRunner, expected).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clipboard_commands_refuse_non_main_callers() {
        assert!(require_main_caller("main").is_ok());
        assert_eq!(
            require_main_caller("settings").unwrap_err(),
            "forbidden_caller"
        );
    }
}
