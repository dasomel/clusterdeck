#![allow(dead_code)]

use crate::services::paths::ClusterDeckPaths;
use crate::services::verify::VerificationResult;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct StateFile {
    pub profiles: BTreeMap<String, VerificationResult>,
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
}
