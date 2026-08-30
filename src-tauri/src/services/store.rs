#![allow(dead_code)]

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

use crate::services::config::{Bastion, BootstrapPolicy, Host, KubeconfigSource, Profile};
use crate::services::paths::ClusterDeckPaths;

#[derive(Serialize, Deserialize, Default)]
struct ProfilesFile {
    #[serde(default)]
    profiles: BTreeMap<String, ProfileBody>,
}

#[derive(Serialize, Deserialize)]
struct ProfileBody {
    name: String,
    #[serde(default)]
    hosts: Vec<Host>,
    #[serde(default)]
    bastion: Option<Bastion>,
    #[serde(default)]
    bootstrap: BootstrapPolicy,
    #[serde(default)]
    kubeconfig: Option<KubeconfigSource>,
    #[serde(default)]
    manage_hosts_file: bool,
}

pub fn load_profiles(paths: &ClusterDeckPaths) -> Result<Vec<Profile>, String> {
    let file_path = paths.profiles_file();
    if !file_path.exists() {
        return Ok(Vec::new());
    }
    let content = std::fs::read_to_string(&file_path).map_err(|e| e.to_string())?;
    let parsed: ProfilesFile = serde_yaml::from_str(&content).map_err(|e| e.to_string())?;
    let profiles = parsed
        .profiles
        .into_iter()
        .filter_map(|(id, body)| {
            let profile = Profile {
                id,
                name: body.name,
                hosts: body.hosts,
                bastion: body.bastion,
                bootstrap: body.bootstrap,
                kubeconfig: body.kubeconfig,
                manage_hosts_file: body.manage_hosts_file,
            };
            match crate::services::validate::validate_profile(&profile) {
                Ok(()) => Some(profile),
                Err(e) => {
                    eprintln!(
                        "skipping invalid profile '{}' loaded from {}: {e}",
                        profile.id,
                        file_path.display()
                    );
                    None
                }
            }
        })
        .collect();
    Ok(profiles)
}

pub fn save_profiles(paths: &ClusterDeckPaths, profiles: &[Profile]) -> Result<(), String> {
    let file_path = paths.profiles_file();
    if let Some(parent) = file_path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let mut map = BTreeMap::new();
    for p in profiles {
        map.insert(
            p.id.clone(),
            ProfileBody {
                name: p.name.clone(),
                hosts: p.hosts.clone(),
                bastion: p.bastion.clone(),
                bootstrap: p.bootstrap.clone(),
                kubeconfig: p.kubeconfig.clone(),
                manage_hosts_file: p.manage_hosts_file,
            },
        );
    }
    let file = ProfilesFile { profiles: map };
    let yaml = serde_yaml::to_string(&file).map_err(|e| e.to_string())?;
    std::fs::write(&file_path, yaml).map_err(|e| e.to_string())?;
    Ok(())
}

pub fn upsert_profile(paths: &ClusterDeckPaths, profile: Profile) -> Result<(), String> {
    crate::services::validate::validate_profile(&profile)?;
    let mut profiles = load_profiles(paths)?;
    if let Some(pos) = profiles.iter().position(|p| p.id == profile.id) {
        profiles[pos] = profile;
    } else {
        profiles.push(profile);
    }
    save_profiles(paths, &profiles)
}

pub fn delete_profile(paths: &ClusterDeckPaths, profile_id: &str) -> Result<(), String> {
    let mut profiles = load_profiles(paths)?;
    profiles.retain(|p| p.id != profile_id);
    save_profiles(paths, &profiles)
}

pub fn get_profile(paths: &ClusterDeckPaths, profile_id: &str) -> Result<Profile, String> {
    let profiles = load_profiles(paths)?;
    profiles
        .into_iter()
        .find(|p| p.id == profile_id)
        .ok_or_else(|| format!("Profile not found: {profile_id}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::config::{BootstrapPolicy, Host, Profile};

    fn temp_paths(tag: &str) -> ClusterDeckPaths {
        let dir = std::env::temp_dir().join(format!(
            "clusterdeck-store-test-{tag}-{}",
            std::process::id()
        ));
        std::fs::remove_dir_all(&dir).ok();
        ClusterDeckPaths::at(dir)
    }

    #[test]
    fn load_profiles_returns_empty_when_file_missing() {
        let paths = temp_paths("missing");
        assert_eq!(load_profiles(&paths).unwrap().len(), 0);
    }

    #[test]
    fn upsert_then_load_roundtrips() {
        let paths = temp_paths("roundtrip");
        let profile = Profile {
            id: "cka".into(),
            name: "CKA Lab".into(),
            hosts: vec![Host {
                name: "cka-m1".into(),
                address: "192.0.2.10".into(),
                port: 22,
                user: "root".into(),
                identity_file: None,
            }],
            bastion: None,
            bootstrap: BootstrapPolicy::default(),
            kubeconfig: None,
            manage_hosts_file: true,
        };
        upsert_profile(&paths, profile.clone()).unwrap();
        let loaded = get_profile(&paths, "cka").unwrap();
        assert_eq!(loaded.name, "CKA Lab");
        assert_eq!(loaded.hosts.len(), 1);
        assert!(loaded.manage_hosts_file);
    }

    #[test]
    fn delete_profile_removes_entry() {
        let paths = temp_paths("delete");
        let profile = Profile {
            id: "x".into(),
            name: "X".into(),
            hosts: vec![],
            bastion: None,
            bootstrap: BootstrapPolicy::default(),
            kubeconfig: None,
            manage_hosts_file: false,
        };
        upsert_profile(&paths, profile).unwrap();
        delete_profile(&paths, "x").unwrap();
        assert!(get_profile(&paths, "x").is_err());
    }

    #[test]
    fn load_profiles_skips_invalid_profiles_and_returns_valid_ones() {
        let paths = temp_paths("skip-invalid");
        if let Some(parent) = paths.profiles_file().parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        let yaml = r#"
profiles:
  cka:
    name: "CKA Lab"
    hosts:
      - name: m1
        address: 192.0.2.10
        port: 22
        user: root
    manage_hosts_file: false
  "../../evil":
    name: "Evil"
    hosts: []
    manage_hosts_file: false
"#;
        std::fs::write(paths.profiles_file(), yaml).unwrap();

        let loaded = load_profiles(&paths).unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].id, "cka");
    }

    #[test]
    fn upsert_profile_rejects_invalid_profile_id() {
        let paths = temp_paths("invalid-id");
        let profile = Profile {
            id: "../../evil".into(),
            name: "Evil".into(),
            hosts: vec![],
            bastion: None,
            bootstrap: BootstrapPolicy::default(),
            kubeconfig: None,
            manage_hosts_file: false,
        };
        assert!(upsert_profile(&paths, profile).is_err());
    }
}
