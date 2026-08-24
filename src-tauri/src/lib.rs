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
            commands::connection::probe_profile_hosts,
            commands::connection::bootstrap_profile,
            commands::connection::generate_aliases,
            commands::connection::fetch_kubeconfig,
            commands::connection::verify_profile,
            commands::connection::get_profile_status,
            commands::connection::connect_profile,
            commands::connection::open_ssh_session,
            commands::kube_import::list_local_kube_contexts_cmd,
        ])
        .run(tauri::generate_context!())
        .expect("error while running ClusterDeck");
}
