#![allow(dead_code)]

use crate::services::paths::ClusterDeckPaths;
use crate::services::tunnel::TunnelTarget;
use crate::services::verify::VerificationResult;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct StateFile {
    pub profiles: BTreeMap<String, VerificationResult>,
    /// ADR-0009: the original API server target of each bastion profile, so the tunnel can be
    /// re-established after an app restart without refetching the kubeconfig. `default` keeps
    /// state files written before this field loadable.
    #[serde(default)]
    pub tunnels: BTreeMap<String, TunnelTarget>,
}

pub fn load_state(paths: &ClusterDeckPaths) -> Result<StateFile, String> {
    let file = paths.state_file();
    if !file.exists() {
        return Ok(StateFile::default());
    }
    let content = std::fs::read_to_string(&file).map_err(|e| e.to_string())?;
    serde_json::from_str(&content).map_err(|e| e.to_string())
}

pub fn save_status(
    paths: &ClusterDeckPaths,
    profile_id: &str,
    result: VerificationResult,
) -> Result<(), String> {
    paths.ensure_dirs()?;
    let mut state = load_state(paths)?;
    state.profiles.insert(profile_id.to_string(), result);
    let json = serde_json::to_string_pretty(&state).map_err(|e| e.to_string())?;
    std::fs::write(paths.state_file(), json).map_err(|e| e.to_string())
}

pub fn get_status(
    paths: &ClusterDeckPaths,
    profile_id: &str,
) -> Result<Option<VerificationResult>, String> {
    let state = load_state(paths)?;
    Ok(state.profiles.get(profile_id).cloned())
}

pub fn save_tunnel_target(
    paths: &ClusterDeckPaths,
    profile_id: &str,
    target: &TunnelTarget,
) -> Result<(), String> {
    paths.ensure_dirs()?;
    let mut state = load_state(paths)?;
    if state.tunnels.get(profile_id) == Some(target) {
        return Ok(());
    }
    state.tunnels.insert(profile_id.to_string(), target.clone());
    let json = serde_json::to_string_pretty(&state).map_err(|e| e.to_string())?;
    std::fs::write(paths.state_file(), json).map_err(|e| e.to_string())
}

pub fn get_tunnel_target(
    paths: &ClusterDeckPaths,
    profile_id: &str,
) -> Result<Option<TunnelTarget>, String> {
    Ok(load_state(paths)?.tunnels.get(profile_id).cloned())
}

pub fn clear_tunnel_target(paths: &ClusterDeckPaths, profile_id: &str) -> Result<(), String> {
    let mut state = load_state(paths)?;
    if state.tunnels.remove(profile_id).is_some() {
        let json = serde_json::to_string_pretty(&state).map_err(|e| e.to_string())?;
        std::fs::write(paths.state_file(), json).map_err(|e| e.to_string())?;
    }
    Ok(())
}

pub fn delete_status(paths: &ClusterDeckPaths, profile_id: &str) -> Result<(), String> {
    let mut state = load_state(paths)?;
    let removed_status = state.profiles.remove(profile_id).is_some();
    let removed_tunnel = state.tunnels.remove(profile_id).is_some();
    if removed_status || removed_tunnel {
        let json = serde_json::to_string_pretty(&state).map_err(|e| e.to_string())?;
        std::fs::write(paths.state_file(), json).map_err(|e| e.to_string())?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::paths::ClusterDeckPaths;
    use crate::services::verify::VerificationResult;

    fn temp_paths(tag: &str) -> ClusterDeckPaths {
        let dir = std::env::temp_dir().join(format!(
            "clusterdeck-state-test-{tag}-{}",
            std::process::id()
        ));
        std::fs::remove_dir_all(&dir).ok();
        ClusterDeckPaths::at(dir)
    }

    #[test]
    fn get_status_returns_none_when_absent() {
        let paths = temp_paths("absent");
        assert!(get_status(&paths, "cka").unwrap().is_none());
    }

    #[test]
    fn save_then_get_status_roundtrips() {
        let paths = temp_paths("roundtrip");
        let result = VerificationResult {
            ssh: true,
            kubeconfig: true,
            kubernetes: true,
            node_count: Some(3),
            kubernetes_version: Some("v1.35.2".into()),
            api_endpoint: None,
            last_verified: Some("2026-08-24T00:00:00Z".into()),
        };
        save_status(&paths, "cka", result.clone()).unwrap();
        let loaded = get_status(&paths, "cka").unwrap().unwrap();
        assert_eq!(loaded.node_count, Some(3));
    }

    #[test]
    fn state_file_without_tunnels_still_loads() {
        let paths = temp_paths("legacy");
        paths.ensure_dirs().unwrap();
        std::fs::write(paths.state_file(), r#"{"profiles":{}}"#).unwrap();
        let state = load_state(&paths).unwrap();
        assert!(state.tunnels.is_empty());
        let target = TunnelTarget {
            host: "127.0.0.1".into(),
            port: 6443,
        };
        save_tunnel_target(&paths, "cka", &target).unwrap();
        assert_eq!(get_tunnel_target(&paths, "cka").unwrap(), Some(target));
        std::fs::remove_dir_all(&paths.base).ok();
    }
}
