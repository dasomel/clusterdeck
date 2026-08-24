mod commands;
mod services;

pub fn run() {
    tauri::Builder::default()
        .invoke_handler(tauri::generate_handler![
            commands::app::get_app_info,
            commands::profiles::list_profiles,
            commands::profiles::get_profile_cmd,
            commands::profiles::save_profile,
            commands::profiles::delete_profile_cmd,
            commands::discovery::discover_hosts,
            commands::connection::test_connection,
        ])
        .run(tauri::generate_context!())
        .expect("error while running ClusterDeck");
}
