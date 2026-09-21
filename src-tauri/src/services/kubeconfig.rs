#![allow(dead_code)]

use crate::services::config::Profile;
use crate::services::paths::ClusterDeckPaths;
use crate::services::process::CommandRunner;
use serde::Serialize;
#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;

#[derive(Debug, Clone, Serialize)]
pub struct KubeconfigSummary {
    pub cluster_name: String,
    pub context_name: String,
    pub local_path: String,
}

const CANDIDATE_KUBECONFIG_PATHS: [&str; 4] = [
    "/etc/rancher/k3s/k3s.yaml",
    "/etc/kubernetes/admin.conf",
    "~/.kube/config",
    "/var/lib/microk8s/credentials/client.config",
];

pub fn is_valid_kubeconfig_yaml(content: &str) -> bool {
    let trimmed = content.trim();
    if trimmed.is_empty() {
        return false;
    }
    if let Ok(val) = serde_yaml::from_str::<serde_yaml::Value>(trimmed) {
        val.get("clusters").and_then(|v| v.as_sequence()).is_some()
            && val.get("users").and_then(|v| v.as_sequence()).is_some()
            && val.get("contexts").and_then(|v| v.as_sequence()).is_some()
    } else {
        false
    }
}

/// Reads `path`, parses it as YAML, and returns its `current-context` value. Returns `None` on
/// any I/O/parse failure or when the field is absent or empty, so callers can fall back.
pub fn read_current_context(path: &std::path::Path) -> Option<String> {
    let raw = std::fs::read_to_string(path).ok()?;
    let val: serde_yaml::Value = serde_yaml::from_str(&raw).ok()?;
    let ctx = val.get("current-context")?.as_str()?;
    if ctx.is_empty() {
        return None;
    }
    Some(ctx.to_string())
}

fn rewrite_server_endpoint(server_url: &str, target_address: &str) -> String {
    if target_address == "127.0.0.1" || target_address == "localhost" {
        return server_url.to_string();
    }

    if server_url.contains("://127.0.0.1") {
        server_url.replace("://127.0.0.1", &format!("://{target_address}"))
    } else if server_url.contains("://localhost") {
        server_url.replace("://localhost", &format!("://{target_address}"))
    } else {
        server_url.to_string()
    }
}

pub fn normalize(raw_yaml: &str, profile_id: &str) -> Result<String, String> {
    normalize_with_host(raw_yaml, profile_id, None)
}

pub fn normalize_with_host(
    raw_yaml: &str,
    profile_id: &str,
    host: Option<&crate::services::config::Host>,
) -> Result<String, String> {
    let mut val: serde_yaml::Value =
        serde_yaml::from_str(raw_yaml).map_err(|e| format!("Invalid YAML format: {e}"))?;

    let clusters_len = val
        .get("clusters")
        .and_then(|v| v.as_sequence())
        .map(|s| s.len());
    let contexts_len = val
        .get("contexts")
        .and_then(|v| v.as_sequence())
        .map(|s| s.len());
    let users_len = val
        .get("users")
        .and_then(|v| v.as_sequence())
        .map(|s| s.len());

    if clusters_len != Some(1) || contexts_len != Some(1) || users_len != Some(1) {
        return Err("multi-cluster kubeconfig is not supported in MVP".to_string());
    }

    let profile_val = serde_yaml::Value::String(profile_id.to_string());

    if let Some(cluster_item) = val.get_mut("clusters").and_then(|v| v.get_mut(0)) {
        if let Some(map) = cluster_item.as_mapping_mut() {
            map.insert(
                serde_yaml::Value::String("name".to_string()),
                profile_val.clone(),
            );

            // Normalize endpoint and add tls-server-name if host is provided
            if let Some(h) = host {
                if let Some(inner) = map.get_mut("cluster").and_then(|v| v.as_mapping_mut()) {
                    if let Some(server_val) = inner.get_mut("server") {
                        if let Some(s) = server_val.as_str() {
                            let rewritten = rewrite_server_endpoint(s, &h.address);
                            *server_val = serde_yaml::Value::String(rewritten);
                        }
                    }

                    if h.address != "127.0.0.1" && h.address != "localhost" && !h.name.is_empty() {
                        inner.insert(
                            serde_yaml::Value::String("tls-server-name".to_string()),
                            serde_yaml::Value::String(h.name.clone()),
                        );
                    }
                }
            }
        }
    }

    if let Some(ctx_item) = val.get_mut("contexts").and_then(|v| v.get_mut(0)) {
        if let Some(map) = ctx_item.as_mapping_mut() {
            map.insert(
                serde_yaml::Value::String("name".to_string()),
                profile_val.clone(),
            );
            if let Some(inner) = map.get_mut("context").and_then(|v| v.as_mapping_mut()) {
                inner.insert(
                    serde_yaml::Value::String("cluster".to_string()),
                    profile_val.clone(),
                );
                inner.insert(
                    serde_yaml::Value::String("user".to_string()),
                    profile_val.clone(),
                );
            }
        }
    }

    if let Some(user_item) = val.get_mut("users").and_then(|v| v.get_mut(0)) {
        if let Some(map) = user_item.as_mapping_mut() {
            map.insert(
                serde_yaml::Value::String("name".to_string()),
                profile_val.clone(),
            );
        }
    }

    if let Some(map) = val.as_mapping_mut() {
        map.insert(
            serde_yaml::Value::String("current-context".to_string()),
            profile_val,
        );
    }

    serde_yaml::to_string(&val).map_err(|e| format!("Failed to serialize normalized YAML: {e}"))
}

async fn fetch_remote_kubeconfig_content(
    runner: &dyn CommandRunner,
    paths: &ClusterDeckPaths,
    profile: &Profile,
    host: &crate::services::config::Host,
    configured_path: &str,
) -> Result<String, String> {
    let alias = crate::services::ssh_config::ssh_alias(&profile.id, &host.name);
    let ssh_conf_path = paths.ssh_conf(&profile.id);

    // Read candidate paths over SSH so the local destination is never exposed to a
    // transfer-completion permission race.
    let mut candidates = vec![configured_path];
    for p in CANDIDATE_KUBECONFIG_PATHS {
        if !candidates.contains(&p) {
            candidates.push(p);
        }
    }

    let mut last_ssh_err = String::new();
    for candidate in candidates {
        let read_cmd = format!("sudo cat '{candidate}' 2>/dev/null || cat '{candidate}'");
        let ssh_args = vec![
            "-F".to_string(),
            ssh_conf_path.to_string_lossy().to_string(),
            "-o".to_string(),
            "BatchMode=yes".to_string(),
            "-o".to_string(),
            "ConnectTimeout=5".to_string(),
            alias.clone(),
            read_cmd,
        ];

        let ssh_output = crate::services::ssh::run_with_host_key_retry(
            runner,
            "ssh",
            &ssh_args,
            &[],
            host,
            profile.bastion.as_ref(),
        )
        .await;

        match ssh_output {
            Ok(out) => {
                if out.success && is_valid_kubeconfig_yaml(&out.stdout) {
                    return Ok(out.stdout);
                } else if !out.stderr.is_empty() {
                    last_ssh_err = out.stderr;
                }
            }
            Err(e) => {
                last_ssh_err = e;
            }
        }
    }

    Err(format!(
        "kubeconfig fetch failed: ssh probe error: {last_ssh_err}"
    ))
}

pub async fn fetch_and_store(
    runner: &dyn CommandRunner,
    paths: &ClusterDeckPaths,
    profile: &Profile,
) -> Result<KubeconfigSummary, String> {
    if !crate::services::validate::is_safe_profile_id(&profile.id) {
        return Err("invalid profile id".to_string());
    }

    let kube_source = profile
        .kubeconfig
        .as_ref()
        .ok_or_else(|| "Profile has no kubeconfig configuration".to_string())?;

    let host = profile
        .hosts
        .iter()
        .find(|h| h.name == kube_source.control_plane)
        .ok_or_else(|| {
            format!(
                "Control plane host '{}' not found in profile hosts",
                kube_source.control_plane
            )
        })?;

    let raw_yaml =
        fetch_remote_kubeconfig_content(runner, paths, profile, host, &kube_source.remote_path)
            .await?;

    let normalized_yaml = normalize_with_host(&raw_yaml, &profile.id, Some(host))?;

    paths
        .ensure_dirs()
        .map_err(|e| format!("failed to create kubeconfigs directory: {e}"))?;
    let dest_path = paths.kubeconfig_file(&profile.id);

    #[cfg(unix)]
    {
        use std::fs::OpenOptions;
        use std::io::Write;
        let mut file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&dest_path)
            .map_err(|e| format!("failed to create kubeconfig file: {e}"))?;
        file.write_all(normalized_yaml.as_bytes())
            .map_err(|e| format!("failed to write kubeconfig file: {e}"))?;
    }
    #[cfg(not(unix))]
    {
        std::fs::write(&dest_path, normalized_yaml)
            .map_err(|e| format!("failed to write kubeconfig file: {e}"))?;
    }

    Ok(KubeconfigSummary {
        cluster_name: profile.id.clone(),
        context_name: profile.id.clone(),
        local_path: dest_path.to_string_lossy().to_string(),
    })
}

#[derive(Debug, Clone, Serialize, serde::Deserialize)]
pub struct BackupKubeconfigResult {
    pub backed_up: bool,
    pub backup_path: Option<String>,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, serde::Deserialize)]
pub struct MergeKubeconfigResult {
    pub success: bool,
    pub target_path: String,
    pub context_name: String,
    pub clusters_count: usize,
    pub contexts_count: usize,
    pub backup: Option<BackupKubeconfigResult>,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, serde::Deserialize)]
pub struct KubeconfigBackupInfo {
    pub filename: String,
    pub path: String,
    pub size_bytes: u64,
    pub modified_at: String,
}

#[derive(Debug, Clone, Serialize, serde::Deserialize)]
pub struct KubeContextInfo {
    pub name: String,
    pub cluster: String,
    pub user: String,
    pub server: String,
    pub is_current: bool,
}

#[derive(Debug, Clone, Serialize, serde::Deserialize)]
pub struct UserKubeconfigDetails {
    pub path: String,
    pub exists: bool,
    pub size_bytes: u64,
    pub current_context: Option<String>,
    pub contexts: Vec<KubeContextInfo>,
    pub raw_yaml: Option<String>,
}

#[derive(Debug, Clone, Serialize, serde::Deserialize)]
pub struct ManagedProfileKubeconfig {
    pub profile_id: String,
    pub profile_name: String,
    pub path: String,
    pub exists: bool,
    pub size_bytes: u64,
    pub current_context: Option<String>,
    pub server: Option<String>,
}

pub fn resolve_user_kubeconfig_path() -> Result<std::path::PathBuf, String> {
    let home =
        std::env::var("HOME").map_err(|_| "HOME environment variable is not set".to_string())?;
    Ok(std::path::PathBuf::from(home).join(".kube").join("config"))
}

pub fn resolve_user_kube_bak_dir() -> Result<std::path::PathBuf, String> {
    let home =
        std::env::var("HOME").map_err(|_| "HOME environment variable is not set".to_string())?;
    Ok(std::path::PathBuf::from(home).join(".kube").join("bak"))
}

pub fn backup_user_kubeconfig(move_file: bool) -> Result<BackupKubeconfigResult, String> {
    let src_path = resolve_user_kubeconfig_path()?;
    let bak_dir = resolve_user_kube_bak_dir()?;
    backup_kubeconfig_file(&src_path, &bak_dir, move_file)
}

pub fn backup_kubeconfig_file(
    src_path: &std::path::Path,
    bak_dir: &std::path::Path,
    move_file: bool,
) -> Result<BackupKubeconfigResult, String> {
    if !src_path.exists() {
        return Ok(BackupKubeconfigResult {
            backed_up: false,
            backup_path: None,
            message: format!("No kubeconfig found at {}", src_path.display()),
        });
    }

    std::fs::create_dir_all(bak_dir)
        .map_err(|e| format!("Failed to create backup directory: {e}"))?;

    let timestamp = chrono::Local::now().format("%Y%m%d_%H%M%S").to_string();
    let bak_filename = format!("config.{}", timestamp);
    let mut dest_path = bak_dir.join(&bak_filename);

    if dest_path.exists() {
        let nanos = chrono::Local::now().timestamp_subsec_nanos();
        dest_path = bak_dir.join(format!("config.{timestamp}_{nanos}"));
    }

    if move_file {
        if std::fs::rename(src_path, &dest_path).is_err() {
            std::fs::copy(src_path, &dest_path)
                .map_err(|e| format!("Failed to copy file to backup: {e}"))?;
            std::fs::remove_file(src_path)
                .map_err(|e| format!("Failed to remove original file during move: {e}"))?;
        }
    } else {
        std::fs::copy(src_path, &dest_path)
            .map_err(|e| format!("Failed to copy file to backup: {e}"))?;
    }

    #[cfg(unix)]
    {
        use std::fs::Permissions;
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&dest_path, Permissions::from_mode(0o600));
    }

    let action = if move_file { "Moved" } else { "Copied" };
    Ok(BackupKubeconfigResult {
        backed_up: true,
        backup_path: Some(dest_path.to_string_lossy().to_string()),
        message: format!("{action} {} to {}", src_path.display(), dest_path.display()),
    })
}

pub fn write_kubeconfig_file_atomic(path: &std::path::Path, content: &str) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("Failed to create directory: {e}"))?;
    }

    let parent_dir = path.parent().unwrap_or_else(|| std::path::Path::new("."));
    let tmp_filename = format!(
        ".kubeconfig-tmp-{}-{}",
        std::process::id(),
        chrono::Local::now().timestamp_subsec_nanos()
    );
    let tmp_path = parent_dir.join(tmp_filename);

    #[cfg(unix)]
    {
        use std::fs::OpenOptions;
        use std::io::Write;
        let mut file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&tmp_path)
            .map_err(|e| format!("Failed to create temp kubeconfig: {e}"))?;
        file.write_all(content.as_bytes())
            .map_err(|e| format!("Failed to write temp kubeconfig: {e}"))?;
    }
    #[cfg(not(unix))]
    {
        std::fs::write(&tmp_path, content)
            .map_err(|e| format!("Failed to write temp kubeconfig: {e}"))?;
    }

    std::fs::rename(&tmp_path, path).map_err(|e| {
        let _ = std::fs::remove_file(&tmp_path);
        format!("Failed to atomically replace kubeconfig file: {e}")
    })?;

    Ok(())
}

pub fn is_safe_filename(name: &str) -> bool {
    if name.is_empty() || name.len() > 255 {
        return false;
    }
    if name.contains('/') || name.contains('\\') || name.contains("..") {
        return false;
    }
    name.chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '_' || c == '-')
}

pub fn list_kubeconfig_backups() -> Result<Vec<KubeconfigBackupInfo>, String> {
    let bak_dir = resolve_user_kube_bak_dir()?;
    if !bak_dir.exists() {
        return Ok(Vec::new());
    }

    let entries =
        std::fs::read_dir(&bak_dir).map_err(|e| format!("Failed to read backup directory: {e}"))?;

    let mut backups = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_file() {
            let filename = entry.file_name().to_string_lossy().to_string();
            if filename.starts_with("config") {
                let metadata = entry.metadata().ok();
                let size_bytes = metadata.as_ref().map(|m| m.len()).unwrap_or(0);
                let modified_at = metadata
                    .and_then(|m| m.modified().ok())
                    .map(|st| {
                        let dt: chrono::DateTime<chrono::Local> = st.into();
                        dt.format("%Y-%m-%d %H:%M:%S").to_string()
                    })
                    .unwrap_or_else(|| "Unknown".to_string());

                backups.push(KubeconfigBackupInfo {
                    filename,
                    path: path.to_string_lossy().to_string(),
                    size_bytes,
                    modified_at,
                });
            }
        }
    }

    backups.sort_by(|a, b| b.filename.cmp(&a.filename));
    Ok(backups)
}

pub fn restore_kubeconfig_backup(filename: &str) -> Result<BackupKubeconfigResult, String> {
    if !is_safe_filename(filename) {
        return Err("Invalid backup filename".to_string());
    }

    let bak_dir = resolve_user_kube_bak_dir()?;
    let backup_file = bak_dir.join(filename);
    if !backup_file.exists() {
        return Err(format!("Backup file '{filename}' does not exist"));
    }

    let content = std::fs::read_to_string(&backup_file)
        .map_err(|e| format!("Failed to read backup file: {e}"))?;
    if !is_valid_kubeconfig_yaml(&content) {
        return Err("Backup file is not valid kubeconfig YAML".to_string());
    }

    let user_config_path = resolve_user_kubeconfig_path()?;
    if user_config_path.exists() {
        let _ = backup_kubeconfig_file(&user_config_path, &bak_dir, false);
    }

    write_kubeconfig_file_atomic(&user_config_path, &content)?;

    Ok(BackupKubeconfigResult {
        backed_up: true,
        backup_path: Some(backup_file.to_string_lossy().to_string()),
        message: format!("Restored ~/.kube/config from {filename}"),
    })
}

pub fn delete_kubeconfig_backup(filename: &str) -> Result<(), String> {
    if !is_safe_filename(filename) {
        return Err("Invalid backup filename".to_string());
    }

    let bak_dir = resolve_user_kube_bak_dir()?;
    let backup_file = bak_dir.join(filename);
    if !backup_file.exists() {
        return Err(format!("Backup file '{filename}' does not exist"));
    }

    std::fs::remove_file(&backup_file).map_err(|e| format!("Failed to delete backup file: {e}"))?;
    Ok(())
}

pub fn get_user_kubeconfig_details(include_raw: bool) -> Result<UserKubeconfigDetails, String> {
    let user_config_path = resolve_user_kubeconfig_path()?;
    if !user_config_path.exists() {
        return Ok(UserKubeconfigDetails {
            path: user_config_path.to_string_lossy().to_string(),
            exists: false,
            size_bytes: 0,
            current_context: None,
            contexts: Vec::new(),
            raw_yaml: None,
        });
    }

    let content = std::fs::read_to_string(&user_config_path)
        .map_err(|e| format!("Failed to read ~/.kube/config: {e}"))?;
    let metadata = std::fs::metadata(&user_config_path).ok();
    let size_bytes = metadata.map(|m| m.len()).unwrap_or(0);

    let val: serde_yaml::Value =
        serde_yaml::from_str(&content).map_err(|e| format!("Failed to parse YAML: {e}"))?;

    let current_context = val
        .get("current-context")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    let mut contexts = Vec::new();
    if let Some(ctx_seq) = val.get("contexts").and_then(|v| v.as_sequence()) {
        let clusters_seq = val.get("clusters").and_then(|v| v.as_sequence());
        for item in ctx_seq {
            let name = item
                .get("name")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            if name.is_empty() {
                continue;
            }
            let inner = item.get("context");
            let cluster = inner
                .and_then(|i| i.get("cluster"))
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let user = inner
                .and_then(|i| i.get("user"))
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();

            let server = clusters_seq
                .and_then(|seq| {
                    seq.iter()
                        .find(|c| c.get("name").and_then(|v| v.as_str()) == Some(cluster.as_str()))
                })
                .and_then(|c| c.get("cluster"))
                .and_then(|c| c.get("server"))
                .and_then(|s| s.as_str())
                .unwrap_or("—")
                .to_string();

            let is_current = current_context.as_deref() == Some(name.as_str());

            contexts.push(KubeContextInfo {
                name,
                cluster,
                user,
                server,
                is_current,
            });
        }
    }

    Ok(UserKubeconfigDetails {
        path: user_config_path.to_string_lossy().to_string(),
        exists: true,
        size_bytes,
        current_context,
        contexts,
        raw_yaml: if include_raw { Some(content) } else { None },
    })
}

pub fn set_current_context(context_name: &str) -> Result<(), String> {
    let user_config_path = resolve_user_kubeconfig_path()?;
    if !user_config_path.exists() {
        return Err("~/.kube/config does not exist".to_string());
    }

    let content = std::fs::read_to_string(&user_config_path)
        .map_err(|e| format!("Failed to read ~/.kube/config: {e}"))?;
    let mut val: serde_yaml::Value =
        serde_yaml::from_str(&content).map_err(|e| format!("Failed to parse YAML: {e}"))?;

    let exists = val
        .get("contexts")
        .and_then(|v| v.as_sequence())
        .map(|seq| {
            seq.iter()
                .any(|c| c.get("name").and_then(|n| n.as_str()) == Some(context_name))
        })
        .unwrap_or(false);

    if !exists {
        return Err(format!(
            "Context '{context_name}' not found in ~/.kube/config"
        ));
    }

    if let Some(map) = val.as_mapping_mut() {
        map.insert(
            serde_yaml::Value::String("current-context".to_string()),
            serde_yaml::Value::String(context_name.to_string()),
        );
    }

    let serialized =
        serde_yaml::to_string(&val).map_err(|e| format!("Failed to serialize YAML: {e}"))?;
    write_kubeconfig_file_atomic(&user_config_path, &serialized)?;
    Ok(())
}

pub fn delete_user_kube_context(context_name: &str) -> Result<(), String> {
    let user_config_path = resolve_user_kubeconfig_path()?;
    if !user_config_path.exists() {
        return Err("~/.kube/config does not exist".to_string());
    }

    let content = std::fs::read_to_string(&user_config_path)
        .map_err(|e| format!("Failed to read ~/.kube/config: {e}"))?;
    let mut val: serde_yaml::Value =
        serde_yaml::from_str(&content).map_err(|e| format!("Failed to parse YAML: {e}"))?;

    let bak_dir = resolve_user_kube_bak_dir()?;
    let _ = backup_kubeconfig_file(&user_config_path, &bak_dir, false);

    let mut context_found = false;
    if let Some(ctx_seq) = val.get_mut("contexts").and_then(|v| v.as_sequence_mut()) {
        if let Some(pos) = ctx_seq
            .iter()
            .position(|c| c.get("name").and_then(|n| n.as_str()) == Some(context_name))
        {
            ctx_seq.remove(pos);
            context_found = true;
        }
    }

    if !context_found {
        return Err(format!("Context '{context_name}' not found"));
    }

    let is_current = val
        .get("current-context")
        .and_then(|v| v.as_str())
        .map(|s| s == context_name)
        .unwrap_or(false);

    if is_current {
        let next_ctx = val
            .get("contexts")
            .and_then(|v| v.as_sequence())
            .and_then(|s| s.first())
            .and_then(|c| c.get("name"))
            .and_then(|n| n.as_str())
            .unwrap_or("")
            .to_string();

        if let Some(map) = val.as_mapping_mut() {
            if next_ctx.is_empty() {
                map.remove(serde_yaml::Value::String("current-context".to_string()));
            } else {
                map.insert(
                    serde_yaml::Value::String("current-context".to_string()),
                    serde_yaml::Value::String(next_ctx),
                );
            }
        }
    }

    let serialized =
        serde_yaml::to_string(&val).map_err(|e| format!("Failed to serialize YAML: {e}"))?;
    write_kubeconfig_file_atomic(&user_config_path, &serialized)?;
    Ok(())
}

pub fn list_managed_profile_kubeconfigs(
    paths: &ClusterDeckPaths,
) -> Result<Vec<ManagedProfileKubeconfig>, String> {
    let profiles = crate::services::store::load_profiles(paths)?;
    let mut list = Vec::new();

    for p in profiles {
        let file_path = paths.kubeconfig_file(&p.id);
        let exists = file_path.exists();
        let mut size_bytes = 0;
        let mut current_context = None;
        let mut server = None;

        if exists {
            if let Ok(metadata) = std::fs::metadata(&file_path) {
                size_bytes = metadata.len();
            }
            if let Ok(content) = std::fs::read_to_string(&file_path) {
                if let Ok(val) = serde_yaml::from_str::<serde_yaml::Value>(&content) {
                    current_context = val
                        .get("current-context")
                        .and_then(|v| v.as_str())
                        .map(str::to_string);
                    server = val
                        .get("clusters")
                        .and_then(|v| v.as_sequence())
                        .and_then(|s| s.first())
                        .and_then(|c| c.get("cluster"))
                        .and_then(|c| c.get("server"))
                        .and_then(|s| s.as_str())
                        .map(str::to_string);
                }
            }
        }

        list.push(ManagedProfileKubeconfig {
            profile_id: p.id,
            profile_name: p.name,
            path: file_path.to_string_lossy().to_string(),
            exists,
            size_bytes,
            current_context,
            server,
        });
    }

    Ok(list)
}

pub async fn open_path_in_finder(runner: &dyn CommandRunner, path_str: &str) -> Result<(), String> {
    let p = std::path::Path::new(path_str);
    if !p.exists() {
        return Err(format!("Path does not exist: {path_str}"));
    }
    let target = if p.is_file() {
        vec!["-R".to_string(), path_str.to_string()]
    } else {
        vec![path_str.to_string()]
    };

    crate::services::process::open_with_system(runner, &target, "").await
}

pub fn merge_yaml_kubeconfigs(
    base_yaml: &str,
    overlay_yaml: &str,
    current_context: Option<&str>,
) -> Result<String, String> {
    let mut base_val: serde_yaml::Value =
        serde_yaml::from_str(base_yaml).map_err(|e| format!("Invalid base YAML: {e}"))?;
    let overlay_val: serde_yaml::Value =
        serde_yaml::from_str(overlay_yaml).map_err(|e| format!("Invalid overlay YAML: {e}"))?;

    for list_key in &["clusters", "contexts", "users"] {
        let key_val = serde_yaml::Value::String((*list_key).to_string());
        if let Some(overlay_list) = overlay_val.get(&key_val).and_then(|v| v.as_sequence()) {
            if base_val
                .get(&key_val)
                .and_then(|v| v.as_sequence())
                .is_none()
            {
                if let Some(map) = base_val.as_mapping_mut() {
                    map.insert(key_val.clone(), serde_yaml::Value::Sequence(Vec::new()));
                }
            }

            if let Some(base_list) = base_val.get_mut(&key_val).and_then(|v| v.as_sequence_mut()) {
                for item in overlay_list {
                    let item_name = item.get("name").and_then(|n| n.as_str());
                    if let Some(name) = item_name {
                        if let Some(idx) = base_list
                            .iter()
                            .position(|b| b.get("name").and_then(|n| n.as_str()) == Some(name))
                        {
                            base_list[idx] = item.clone();
                        } else {
                            base_list.push(item.clone());
                        }
                    } else {
                        base_list.push(item.clone());
                    }
                }
            }
        }
    }

    if let Some(ctx) = current_context {
        if let Some(map) = base_val.as_mapping_mut() {
            map.insert(
                serde_yaml::Value::String("current-context".to_string()),
                serde_yaml::Value::String(ctx.to_string()),
            );
        }
    }

    serde_yaml::to_string(&base_val).map_err(|e| format!("Failed to serialize merged YAML: {e}"))
}

/// Shape of the placeholder kubeconfig generate_default_kubeconfig emits when a profile has no
/// fetched kubeconfig yet. Field order matches the document's key order; `tls_server_name` is
/// only present when the control-plane host has a non-loopback address (mirrors normalize_with_host).
#[derive(Serialize)]
struct DefaultKubeconfigDoc {
    #[serde(rename = "apiVersion")]
    api_version: String,
    kind: String,
    clusters: Vec<DefaultClusterItem>,
    contexts: Vec<DefaultContextItem>,
    #[serde(rename = "current-context")]
    current_context: String,
    users: Vec<DefaultUserItem>,
}

#[derive(Serialize)]
struct DefaultClusterItem {
    name: String,
    cluster: DefaultClusterSpec,
}

#[derive(Serialize)]
struct DefaultClusterSpec {
    server: String,
    #[serde(rename = "insecure-skip-tls-verify")]
    insecure_skip_tls_verify: bool,
    #[serde(rename = "tls-server-name", skip_serializing_if = "Option::is_none")]
    tls_server_name: Option<String>,
}

#[derive(Serialize)]
struct DefaultContextItem {
    name: String,
    context: DefaultContextSpec,
}

#[derive(Serialize)]
struct DefaultContextSpec {
    cluster: String,
    user: String,
}

#[derive(Serialize)]
struct DefaultUserItem {
    name: String,
    user: serde_yaml::Mapping,
}

pub fn generate_default_kubeconfig(profile: &Profile) -> Result<String, String> {
    if !crate::services::validate::is_safe_profile_id(&profile.id) {
        return Err("invalid profile id".to_string());
    }

    let (host_addr, host_name) = if let Some(ref kc) = profile.kubeconfig {
        if let Some(h) = profile.hosts.iter().find(|h| h.name == kc.control_plane) {
            (h.address.as_str(), h.name.as_str())
        } else if let Some(first) = profile.hosts.first() {
            (first.address.as_str(), first.name.as_str())
        } else {
            ("127.0.0.1", "localhost")
        }
    } else if let Some(first) = profile.hosts.first() {
        (first.address.as_str(), first.name.as_str())
    } else {
        ("127.0.0.1", "localhost")
    };

    let server_url = format!("https://{host_addr}:6443");
    let profile_id = profile.id.clone();

    let tls_server_name =
        if host_addr != "127.0.0.1" && host_addr != "localhost" && !host_name.is_empty() {
            Some(host_name.to_string())
        } else {
            None
        };

    let doc = DefaultKubeconfigDoc {
        api_version: "v1".to_string(),
        kind: "Config".to_string(),
        clusters: vec![DefaultClusterItem {
            name: profile_id.clone(),
            cluster: DefaultClusterSpec {
                server: server_url,
                insecure_skip_tls_verify: true,
                tls_server_name,
            },
        }],
        contexts: vec![DefaultContextItem {
            name: profile_id.clone(),
            context: DefaultContextSpec {
                cluster: profile_id.clone(),
                user: profile_id.clone(),
            },
        }],
        current_context: profile_id.clone(),
        users: vec![DefaultUserItem {
            name: profile_id,
            user: serde_yaml::Mapping::new(),
        }],
    };

    serde_yaml::to_string(&doc)
        .map_err(|e| format!("Failed to serialize generated kubeconfig: {e}"))
}

pub async fn ensure_profile_kubeconfig(
    runner: &dyn CommandRunner,
    paths: &ClusterDeckPaths,
    profile: &Profile,
) -> Result<std::path::PathBuf, String> {
    if !crate::services::validate::is_safe_profile_id(&profile.id) {
        return Err("invalid profile id".to_string());
    }

    let dest_path = paths.kubeconfig_file(&profile.id);
    if dest_path.exists() {
        return Ok(dest_path);
    }

    if profile.kubeconfig.is_some()
        && fetch_and_store(runner, paths, profile).await.is_ok()
        && dest_path.exists()
    {
        return Ok(dest_path);
    }

    let generated = generate_default_kubeconfig(profile)?;
    paths
        .ensure_dirs()
        .map_err(|e| format!("Failed to create kubeconfigs directory: {e}"))?;
    write_kubeconfig_file_atomic(&dest_path, &generated)?;

    Ok(dest_path)
}

pub async fn merge_profile_kubeconfig_to_user_config(
    runner: &dyn CommandRunner,
    paths: &ClusterDeckPaths,
    profile: &Profile,
    backup_first: bool,
) -> Result<MergeKubeconfigResult, String> {
    ensure_profile_kubeconfig(runner, paths, profile).await?;
    let user_config_path = resolve_user_kubeconfig_path()?;
    let bak_dir = resolve_user_kube_bak_dir()?;
    merge_profile_kubeconfig_to_file(
        runner,
        paths,
        &profile.id,
        &user_config_path,
        Some(&bak_dir),
        backup_first,
    )
    .await
}

pub fn is_profile_present_in_user_config(profile_id: &str) -> bool {
    let Ok(path) = resolve_user_kubeconfig_path() else {
        return false;
    };
    if !path.exists() {
        return false;
    }
    let Ok(content) = std::fs::read_to_string(&path) else {
        return false;
    };
    let Ok(val) = serde_yaml::from_str::<serde_yaml::Value>(&content) else {
        return false;
    };

    for key in &["clusters", "contexts", "users"] {
        if let Some(items) = val.get(*key).and_then(|v| v.as_sequence()) {
            if items
                .iter()
                .any(|item| item.get("name").and_then(|n| n.as_str()) == Some(profile_id))
            {
                return true;
            }
        }
    }
    false
}

pub async fn merge_profile_kubeconfig_to_file(
    runner: &dyn CommandRunner,
    paths: &ClusterDeckPaths,
    profile_id: &str,
    user_config_path: &std::path::Path,
    bak_dir: Option<&std::path::Path>,
    backup_first: bool,
) -> Result<MergeKubeconfigResult, String> {
    if !crate::services::validate::is_safe_profile_id(profile_id) {
        return Err("invalid profile id".to_string());
    }

    let profile_kc_path = paths.kubeconfig_file(profile_id);
    if !profile_kc_path.exists() {
        return Err(format!(
            "Profile kubeconfig for '{profile_id}' not found. Run 'Connect / Sync' first."
        ));
    }

    let profile_raw = std::fs::read_to_string(&profile_kc_path)
        .map_err(|e| format!("Failed to read profile kubeconfig: {e}"))?;
    if !is_valid_kubeconfig_yaml(&profile_raw) {
        return Err("Profile kubeconfig is not valid YAML".to_string());
    }

    let mut backup_result = None;
    if backup_first && user_config_path.exists() {
        if let Some(bak_d) = bak_dir {
            match backup_kubeconfig_file(user_config_path, bak_d, false) {
                Ok(res) => backup_result = Some(res),
                Err(e) => return Err(format!("Safety backup before merge failed: {e}")),
            }
        }
    }

    if !user_config_path.exists() {
        write_kubeconfig_file_atomic(user_config_path, &profile_raw)?;
        return Ok(MergeKubeconfigResult {
            success: true,
            target_path: user_config_path.to_string_lossy().to_string(),
            context_name: profile_id.to_string(),
            clusters_count: 1,
            contexts_count: 1,
            backup: backup_result,
            message: format!(
                "Created {} with profile context '{profile_id}'",
                user_config_path.display()
            ),
        });
    }

    // Attempt kubectl config view --flatten first
    let env = vec![(
        "KUBECONFIG".to_string(),
        format!(
            "{}:{}",
            user_config_path.to_string_lossy(),
            profile_kc_path.to_string_lossy()
        ),
    )];
    let kubectl_res = runner
        .run_with_env(
            "kubectl",
            &[
                "config".to_string(),
                "view".to_string(),
                "--flatten".to_string(),
            ],
            &env,
        )
        .await;

    let mut merged_yaml = None;
    if let Ok(out) = kubectl_res {
        if out.success && is_valid_kubeconfig_yaml(&out.stdout) {
            if let Ok(mut val) = serde_yaml::from_str::<serde_yaml::Value>(&out.stdout) {
                if let Some(map) = val.as_mapping_mut() {
                    map.insert(
                        serde_yaml::Value::String("current-context".to_string()),
                        serde_yaml::Value::String(profile_id.to_string()),
                    );
                }
                if let Ok(serialized) = serde_yaml::to_string(&val) {
                    merged_yaml = Some(serialized);
                }
            }
            if merged_yaml.is_none() {
                merged_yaml = Some(out.stdout);
            }
        }
    }

    let final_yaml = match merged_yaml {
        Some(yaml) => yaml,
        None => {
            let existing_raw = std::fs::read_to_string(user_config_path)
                .map_err(|e| format!("Failed to read existing kubeconfig: {e}"))?;
            merge_yaml_kubeconfigs(&existing_raw, &profile_raw, Some(profile_id))?
        }
    };

    write_kubeconfig_file_atomic(user_config_path, &final_yaml)?;

    let mut clusters_count = 0;
    let mut contexts_count = 0;
    if let Ok(val) = serde_yaml::from_str::<serde_yaml::Value>(&final_yaml) {
        clusters_count = val
            .get("clusters")
            .and_then(|v| v.as_sequence())
            .map(|s| s.len())
            .unwrap_or(0);
        contexts_count = val
            .get("contexts")
            .and_then(|v| v.as_sequence())
            .map(|s| s.len())
            .unwrap_or(0);
    }

    Ok(MergeKubeconfigResult {
        success: true,
        target_path: user_config_path.to_string_lossy().to_string(),
        context_name: profile_id.to_string(),
        clusters_count,
        contexts_count,
        backup: backup_result,
        message: format!(
            "Merged context '{profile_id}' into {} (Total: {clusters_count} clusters, {contexts_count} contexts)",
            user_config_path.display()
        ),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::config::{BootstrapPolicy, Host, KubeconfigSource};
    use crate::services::process::CommandOutput;
    use async_trait::async_trait;
    use std::sync::atomic::{AtomicBool, Ordering};

    struct FakeSshRunner {
        sample_yaml: String,
        ssh_called: AtomicBool,
    }

    #[async_trait]
    impl CommandRunner for FakeSshRunner {
        async fn run(&self, bin: &str, _args: &[String]) -> Result<CommandOutput, String> {
            if bin == "ssh" {
                self.ssh_called.store(true, Ordering::SeqCst);
                return Ok(CommandOutput {
                    stdout: self.sample_yaml.clone(),
                    stderr: String::new(),
                    success: true,
                });
            }
            Err(format!("unexpected command {bin}"))
        }
    }

    const SAMPLE: &str = r#"
apiVersion: v1
kind: Config
clusters:
  - name: original-cluster
    cluster:
      server: https://192.0.2.10:6443
      certificate-authority-data: ZmFrZS1jYQ==
contexts:
  - name: original-context
    context:
      cluster: original-cluster
      user: original-user
current-context: original-context
users:
  - name: original-user
    user:
      client-certificate-data: ZmFrZS1jZXJ0
      client-key-data: ZmFrZS1rZXk=
"#;

    #[test]
    fn normalize_renames_cluster_context_and_user_to_profile_id() {
        let normalized = normalize(SAMPLE, "cka").unwrap();
        let value: serde_yaml::Value = serde_yaml::from_str(&normalized).unwrap();
        assert_eq!(value["current-context"].as_str().unwrap(), "cka");
        assert_eq!(value["clusters"][0]["name"].as_str().unwrap(), "cka");
        assert_eq!(value["contexts"][0]["name"].as_str().unwrap(), "cka");
        assert_eq!(
            value["contexts"][0]["context"]["cluster"].as_str().unwrap(),
            "cka"
        );
        assert_eq!(
            value["contexts"][0]["context"]["user"].as_str().unwrap(),
            "cka"
        );
        assert_eq!(value["users"][0]["name"].as_str().unwrap(), "cka");
        // certificate data must survive untouched
        assert_eq!(
            value["clusters"][0]["cluster"]["certificate-authority-data"]
                .as_str()
                .unwrap(),
            "ZmFrZS1jYQ=="
        );
    }

    #[test]
    fn normalize_rejects_multi_cluster_kubeconfig() {
        let multi = SAMPLE.replace(
            "clusters:\n  - name: original-cluster",
            "clusters:\n  - name: original-cluster\n    cluster:\n      server: https://x\n  - name: second",
        );
        assert!(normalize(&multi, "cka").is_err());
    }

    #[tokio::test]
    async fn fetch_and_store_fetches_normalizes_and_writes_kubeconfig() {
        let temp_dir =
            std::env::temp_dir().join(format!("clusterdeck-kc-test-{}", std::process::id()));
        let paths = ClusterDeckPaths::at(temp_dir.clone());
        let profile = Profile {
            id: "cka".to_string(),
            name: "CKA Lab".to_string(),
            hosts: vec![Host {
                name: "m1".to_string(),
                address: "192.0.2.10".to_string(),
                port: 22,
                user: "root".to_string(),
                identity_file: None,
            }],
            bastion: None,
            bootstrap: BootstrapPolicy::default(),
            kubeconfig: Some(KubeconfigSource {
                remote_path: "/etc/kubernetes/admin.conf".to_string(),
                control_plane: "m1".to_string(),
                local_path: "".to_string(),
                context: "cka".to_string(),
            }),
            manage_hosts_file: false,
            trusted_cas: Vec::new(),
        };

        let runner = FakeSshRunner {
            sample_yaml: SAMPLE.to_string(),
            ssh_called: AtomicBool::new(false),
        };

        let summary = fetch_and_store(&runner, &paths, &profile).await.unwrap();
        assert_eq!(summary.cluster_name, "cka");
        assert_eq!(summary.context_name, "cka");
        assert!(runner.ssh_called.load(Ordering::SeqCst));

        let stored_yaml = std::fs::read_to_string(paths.kubeconfig_file("cka")).unwrap();
        let value: serde_yaml::Value = serde_yaml::from_str(&stored_yaml).unwrap();
        assert_eq!(value["current-context"].as_str().unwrap(), "cka");

        std::fs::remove_dir_all(&temp_dir).ok();
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn fetch_and_store_creates_destination_with_0600_permissions() {
        use std::os::unix::fs::PermissionsExt;
        let temp_dir =
            std::env::temp_dir().join(format!("clusterdeck-kc-test-perms-{}", std::process::id()));
        let paths = ClusterDeckPaths::at(temp_dir.clone());
        let profile = Profile {
            id: "cka-perms".to_string(),
            name: "CKA Lab".to_string(),
            hosts: vec![Host {
                name: "m1".to_string(),
                address: "192.0.2.10".to_string(),
                port: 22,
                user: "root".to_string(),
                identity_file: None,
            }],
            bastion: None,
            bootstrap: BootstrapPolicy::default(),
            kubeconfig: Some(KubeconfigSource {
                remote_path: "/etc/kubernetes/admin.conf".to_string(),
                control_plane: "m1".to_string(),
                local_path: "".to_string(),
                context: "cka-perms".to_string(),
            }),
            manage_hosts_file: false,
            trusted_cas: Vec::new(),
        };

        let runner = FakeSshRunner {
            sample_yaml: SAMPLE.to_string(),
            ssh_called: AtomicBool::new(false),
        };

        fetch_and_store(&runner, &paths, &profile).await.unwrap();

        let dest_path = paths.kubeconfig_file("cka-perms");
        let mode = std::fs::metadata(&dest_path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);

        std::fs::remove_dir_all(&temp_dir).ok();
    }

    #[test]
    fn normalize_with_host_rewrites_loopback_and_adds_tls_server_name() {
        let loopback_sample = SAMPLE.replace("https://192.0.2.10:6443", "https://127.0.0.1:6443");
        let host = Host {
            name: "master-1".to_string(),
            address: "172.16.221.133".to_string(),
            port: 22,
            user: "vagrant".to_string(),
            identity_file: None,
        };

        let normalized = normalize_with_host(&loopback_sample, "dev-cluster", Some(&host)).unwrap();
        let value: serde_yaml::Value = serde_yaml::from_str(&normalized).unwrap();
        assert_eq!(
            value["clusters"][0]["cluster"]["server"].as_str().unwrap(),
            "https://172.16.221.133:6443"
        );
        assert_eq!(
            value["clusters"][0]["cluster"]["tls-server-name"]
                .as_str()
                .unwrap(),
            "master-1"
        );
    }

    #[tokio::test]
    async fn fetch_and_store_probes_later_candidate_when_configured_path_fails() {
        let temp_dir =
            std::env::temp_dir().join(format!("clusterdeck-kc-fallback-{}", std::process::id()));
        let paths = ClusterDeckPaths::at(temp_dir.clone());
        let profile = Profile {
            id: "candidate-test".to_string(),
            name: "Candidate Test".to_string(),
            hosts: vec![Host {
                name: "m1".to_string(),
                address: "192.0.2.10".to_string(),
                port: 22,
                user: "vagrant".to_string(),
                identity_file: None,
            }],
            bastion: None,
            bootstrap: BootstrapPolicy::default(),
            kubeconfig: Some(KubeconfigSource {
                remote_path: "/missing/config".to_string(),
                control_plane: "m1".to_string(),
                local_path: "".to_string(),
                context: "candidate-test".to_string(),
            }),
            manage_hosts_file: false,
            trusted_cas: Vec::new(),
        };

        struct CandidateRunner {
            successful_candidate: &'static str,
            sample_yaml: String,
            probed_paths: std::sync::Mutex<Vec<String>>,
        }

        #[async_trait]
        impl CommandRunner for CandidateRunner {
            async fn run(&self, bin: &str, args: &[String]) -> Result<CommandOutput, String> {
                if bin != "ssh" {
                    return Err(format!("unexpected command {bin}"));
                }
                let command = args.last().expect("ssh command argument");
                self.probed_paths.lock().unwrap().push(command.clone());
                let success = command.contains(self.successful_candidate);
                Ok(CommandOutput {
                    stdout: if success {
                        self.sample_yaml.clone()
                    } else {
                        String::new()
                    },
                    stderr: if success {
                        String::new()
                    } else {
                        "cat: /missing/config: No such file or directory".to_string()
                    },
                    success,
                })
            }
        }

        let runner = CandidateRunner {
            successful_candidate: "/etc/rancher/k3s/k3s.yaml",
            sample_yaml: SAMPLE.to_string(),
            probed_paths: std::sync::Mutex::new(Vec::new()),
        };

        let summary = fetch_and_store(&runner, &paths, &profile).await.unwrap();
        assert_eq!(summary.cluster_name, "candidate-test");
        let probed_paths = runner.probed_paths.lock().unwrap();
        assert_eq!(probed_paths.len(), 2);
        assert!(probed_paths[0].contains("/missing/config"));
        assert!(probed_paths[1].contains("/etc/rancher/k3s/k3s.yaml"));

        let stored_yaml = std::fs::read_to_string(paths.kubeconfig_file("candidate-test")).unwrap();
        let value: serde_yaml::Value = serde_yaml::from_str(&stored_yaml).unwrap();
        assert_eq!(value["current-context"].as_str().unwrap(), "candidate-test");

        std::fs::remove_dir_all(&temp_dir).ok();
    }

    #[test]
    fn backup_kubeconfig_file_moves_or_copies_correctly() {
        let temp_dir =
            std::env::temp_dir().join(format!("clusterdeck-bak-test-{}", std::process::id()));
        let kube_dir = temp_dir.join(".kube");
        let bak_dir = kube_dir.join("bak");
        std::fs::create_dir_all(&kube_dir).unwrap();
        let config_file = kube_dir.join("config");
        std::fs::write(&config_file, "dummy kubeconfig content").unwrap();

        // 1. Copy backup (move_file = false)
        let copy_res = backup_kubeconfig_file(&config_file, &bak_dir, false).unwrap();
        assert!(copy_res.backed_up);
        assert!(config_file.exists());
        let backup_path_str = copy_res.backup_path.unwrap();
        assert!(std::path::Path::new(&backup_path_str).exists());

        // 2. Move backup (move_file = true)
        let move_res = backup_kubeconfig_file(&config_file, &bak_dir, true).unwrap();
        assert!(move_res.backed_up);
        assert!(!config_file.exists());
        let move_path_str = move_res.backup_path.unwrap();
        assert!(std::path::Path::new(&move_path_str).exists());

        // 3. Backup on non-existent file
        let none_res = backup_kubeconfig_file(&config_file, &bak_dir, true).unwrap();
        assert!(!none_res.backed_up);

        std::fs::remove_dir_all(&temp_dir).ok();
    }

    #[test]
    fn merge_yaml_kubeconfigs_merges_clusters_and_contexts() {
        let base = r#"
apiVersion: v1
clusters:
- cluster:
    server: https://192.168.1.1:6443
  name: cluster-1
contexts:
- context:
    cluster: cluster-1
    user: user-1
  name: ctx-1
current-context: ctx-1
users:
- name: user-1
  user:
    token: token1
"#;

        let overlay = r#"
apiVersion: v1
clusters:
- cluster:
    server: https://10.0.0.1:6443
  name: cluster-2
contexts:
- context:
    cluster: cluster-2
    user: user-2
  name: ctx-2
current-context: ctx-2
users:
- name: user-2
  user:
    token: token2
"#;

        let merged = merge_yaml_kubeconfigs(base, overlay, Some("ctx-2")).unwrap();
        let val: serde_yaml::Value = serde_yaml::from_str(&merged).unwrap();

        assert_eq!(val["clusters"].as_sequence().unwrap().len(), 2);
        assert_eq!(val["contexts"].as_sequence().unwrap().len(), 2);
        assert_eq!(val["users"].as_sequence().unwrap().len(), 2);
        assert_eq!(val["current-context"].as_str().unwrap(), "ctx-2");
    }

    struct DummyKubeRunner;
    #[async_trait]
    impl CommandRunner for DummyKubeRunner {
        async fn run(&self, _bin: &str, _args: &[String]) -> Result<CommandOutput, String> {
            Err("dummy runner does not run commands".to_string())
        }
        async fn run_with_env(
            &self,
            _bin: &str,
            _args: &[String],
            _env: &[(String, String)],
        ) -> Result<CommandOutput, String> {
            Err("dummy runner does not run commands".to_string())
        }
    }

    #[tokio::test]
    async fn merge_profile_kubeconfig_to_file_creates_and_merges() {
        let temp_dir =
            std::env::temp_dir().join(format!("clusterdeck-merge-test-{}", std::process::id()));
        let paths = ClusterDeckPaths::at(temp_dir.clone());
        paths.ensure_dirs().unwrap();

        let profile_id = "test-prof";
        let profile_kc_file = paths.kubeconfig_file(profile_id);
        std::fs::write(&profile_kc_file, SAMPLE).unwrap();

        let user_kube_dir = temp_dir.join(".kube");
        let user_kc_file = user_kube_dir.join("config");
        let bak_dir = user_kube_dir.join("bak");

        let runner = DummyKubeRunner;

        // 1. Target doesn't exist -> creates it
        let res1 = merge_profile_kubeconfig_to_file(
            &runner,
            &paths,
            profile_id,
            &user_kc_file,
            Some(&bak_dir),
            true,
        )
        .await
        .unwrap();

        assert!(res1.success);
        assert!(user_kc_file.exists());
        assert_eq!(res1.clusters_count, 1);

        // 2. Target exists -> merges into it
        let res2 = merge_profile_kubeconfig_to_file(
            &runner,
            &paths,
            profile_id,
            &user_kc_file,
            Some(&bak_dir),
            true,
        )
        .await
        .unwrap();

        assert!(res2.success);
        assert!(res2.backup.is_some());
        assert_eq!(res2.clusters_count, 1); // same profile replaced

        std::fs::remove_dir_all(&temp_dir).ok();
    }

    #[tokio::test]
    async fn ensure_profile_kubeconfig_generates_when_absent() {
        let temp_dir =
            std::env::temp_dir().join(format!("clusterdeck-ensure-test-{}", std::process::id()));
        let paths = ClusterDeckPaths::at(temp_dir.clone());
        let profile = Profile {
            id: "cka-lab".to_string(),
            name: "CKA Lab".to_string(),
            hosts: vec![Host {
                name: "cka-m1".to_string(),
                address: "192.0.2.10".to_string(),
                port: 22,
                user: "vagrant".to_string(),
                identity_file: None,
            }],
            bastion: None,
            bootstrap: BootstrapPolicy::default(),
            kubeconfig: Some(KubeconfigSource {
                remote_path: "/etc/kubernetes/admin.conf".to_string(),
                control_plane: "cka-m1".to_string(),
                local_path: "".to_string(),
                context: "cka-lab".to_string(),
            }),
            manage_hosts_file: false,
            trusted_cas: Vec::new(),
        };

        let runner = DummyKubeRunner;
        let dest = ensure_profile_kubeconfig(&runner, &paths, &profile)
            .await
            .unwrap();
        assert!(dest.exists());

        let raw = std::fs::read_to_string(&dest).unwrap();
        let val: serde_yaml::Value = serde_yaml::from_str(&raw).unwrap();
        assert_eq!(val["current-context"].as_str().unwrap(), "cka-lab");
        let server = val["clusters"][0]["cluster"]["server"].as_str().unwrap();
        assert_eq!(server, "https://192.0.2.10:6443");

        std::fs::remove_dir_all(&temp_dir).ok();
    }

    #[test]
    fn is_safe_filename_validates_correctly() {
        assert!(is_safe_filename("config.20260919_150000"));
        assert!(is_safe_filename("config-backup.yaml"));
        assert!(!is_safe_filename(""));
        assert!(!is_safe_filename("../config"));
        assert!(!is_safe_filename("config/foo"));
        assert!(!is_safe_filename("config\\foo"));
        assert!(!is_safe_filename("config with spaces"));
    }

    #[test]
    fn context_management_helpers_work_on_yaml() {
        let sample_config = r#"
apiVersion: v1
clusters:
- cluster:
    server: https://1.1.1.1:6443
  name: c1
- cluster:
    server: https://2.2.2.2:6443
  name: c2
contexts:
- context:
    cluster: c1
    user: u1
  name: ctx1
- context:
    cluster: c2
    user: u2
  name: ctx2
current-context: ctx1
users:
- name: u1
  user: {}
- name: u2
  user: {}
"#;
        let temp_dir =
            std::env::temp_dir().join(format!("clusterdeck-ctx-test-{}", std::process::id()));
        let config_path = temp_dir.join("config");
        std::fs::create_dir_all(&temp_dir).unwrap();
        std::fs::write(&config_path, sample_config).unwrap();

        // Test changing current-context via YAML manipulation
        let mut val: serde_yaml::Value = serde_yaml::from_str(sample_config).unwrap();
        val.as_mapping_mut().unwrap().insert(
            serde_yaml::Value::String("current-context".to_string()),
            serde_yaml::Value::String("ctx2".to_string()),
        );
        let updated = serde_yaml::to_string(&val).unwrap();
        assert!(updated.contains("current-context: ctx2"));

        std::fs::remove_dir_all(&temp_dir).ok();
    }

    #[test]
    fn generate_default_kubeconfig_pins_exact_output_for_non_loopback_host() {
        // Characterization test: pins generate_default_kubeconfig's exact current output for a
        // profile whose control-plane host has a non-loopback address (tls-server-name must be
        // present). Guards the Mapping-builder -> typed-struct rewrite against output drift.
        let profile = Profile {
            id: "cka".to_string(),
            name: "CKA Lab".to_string(),
            hosts: vec![Host {
                name: "m1".to_string(),
                address: "192.0.2.10".to_string(),
                port: 22,
                user: "root".to_string(),
                identity_file: None,
            }],
            bastion: None,
            bootstrap: BootstrapPolicy::default(),
            kubeconfig: Some(KubeconfigSource {
                remote_path: "/etc/kubernetes/admin.conf".to_string(),
                control_plane: "m1".to_string(),
                local_path: "".to_string(),
                context: "cka".to_string(),
            }),
            manage_hosts_file: false,
            trusted_cas: Vec::new(),
        };
        let generated = generate_default_kubeconfig(&profile).unwrap();
        let expected = r#"apiVersion: v1
kind: Config
clusters:
- name: cka
  cluster:
    server: https://192.0.2.10:6443
    insecure-skip-tls-verify: true
    tls-server-name: m1
contexts:
- name: cka
  context:
    cluster: cka
    user: cka
current-context: cka
users:
- name: cka
  user: {}
"#;
        assert_eq!(generated, expected);
    }

    #[test]
    fn generate_default_kubeconfig_pins_exact_output_for_no_hosts() {
        // Characterization test: pins generate_default_kubeconfig's exact current output for a
        // profile with no hosts (falls back to 127.0.0.1, no tls-server-name).
        let profile = Profile {
            id: "empty-profile".to_string(),
            name: "Empty".to_string(),
            hosts: vec![],
            bastion: None,
            bootstrap: BootstrapPolicy::default(),
            kubeconfig: None,
            manage_hosts_file: false,
            trusted_cas: Vec::new(),
        };
        let generated = generate_default_kubeconfig(&profile).unwrap();
        let expected = r#"apiVersion: v1
kind: Config
clusters:
- name: empty-profile
  cluster:
    server: https://127.0.0.1:6443
    insecure-skip-tls-verify: true
contexts:
- name: empty-profile
  context:
    cluster: empty-profile
    user: empty-profile
current-context: empty-profile
users:
- name: empty-profile
  user: {}
"#;
        assert_eq!(generated, expected);
    }
}
