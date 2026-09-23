mod commands;
mod services;

pub fn run() {
    tauri::Builder::default()
        .manage(services::local_runtime_lifecycle::LifecycleGuard::default())
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
            commands::connection::backup_kubeconfig,
            commands::connection::merge_kubeconfig_to_system,
            commands::connection::list_kubeconfig_backups,
            commands::connection::restore_kubeconfig_backup,
            commands::connection::delete_kubeconfig_backup,
            commands::connection::get_user_kubeconfig_details,
            commands::connection::set_current_context,
            commands::connection::delete_user_kube_context,
            commands::connection::list_managed_profile_kubeconfigs,
            commands::connection::open_path_in_finder,
            commands::connection::discover_cluster_endpoints_cmd,
            commands::connection::sync_hosts_file_cmd,
            commands::connection::get_hosts_file_status,
            commands::connection::remove_hosts_file_cmd,
            commands::connection::open_url_in_browser,
            commands::ca_trust::discover_cluster_cas_cmd,
            commands::ca_trust::trust_ca_cmd,
            commands::ca_trust::replace_ca_cmd,
            commands::ca_trust::remove_ca_cmd,
            commands::kube_import::list_local_kube_contexts_cmd,
            commands::local_runtime::detect_local_hosts,
            commands::local_runtime::start_local_runtime,
            commands::local_runtime::stop_local_runtime,
            commands::local_runtime::restart_local_runtime,
            commands::local_runtime::open_local_runtime_shell,
            commands::local_runtime::open_local_runtime_context,
        ])
        .run(tauri::generate_context!())
        .expect("error while running ClusterDeck");
}
