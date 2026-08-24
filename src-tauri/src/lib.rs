mod commands;
mod services;

pub fn run() {
    tauri::Builder::default()
        .invoke_handler(tauri::generate_handler![
            commands::app::get_app_info,
            commands::profiles::list_profiles,
            commands::connection::test_connection,
        ])
        .run(tauri::generate_context!())
        .expect("error while running ClusterDeck");
}
