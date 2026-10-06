#![allow(dead_code)]

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

use crate::services::config::{
    Bastion, BootstrapPolicy, Host, KubeconfigSource, LocalRuntimeSource, Profile,
};
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
    #[serde(default)]
    trusted_cas: Vec<crate::services::ca_trust::TrustedCa>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    local_runtime: Option<LocalRuntimeSource>,
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
                trusted_cas: body.trusted_cas,
                local_runtime: body.local_runtime,
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

static PROFILE_WRITE_MUTEX: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// Process-wide mutual exclusion guard for writing to `profiles.yaml`.
/// Registered as Tauri managed state (`lib.rs`'s `.manage(...)`) to serialize profile saves,
/// deletes, inventory profile creations, and CA trust updates across commands.
#[derive(Debug, Default, Clone)]
pub struct ProfileWriteGuard;

impl ProfileWriteGuard {
    pub async fn lock(&self) -> tokio::sync::MutexGuard<'static, ()> {
        PROFILE_WRITE_MUTEX.lock().await
    }

    pub async fn lock_process() -> tokio::sync::MutexGuard<'static, ()> {
        PROFILE_WRITE_MUTEX.lock().await
    }
}

pub fn save_profiles(paths: &ClusterDeckPaths, profiles: &[Profile]) -> Result<(), String> {
    let file_path = paths.profiles_file();
    let parent = file_path
        .parent()
        .ok_or_else(|| "invalid profiles file path: missing parent directory".to_string())?;
    std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;

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
                trusted_cas: p.trusted_cas.clone(),
                local_runtime: p.local_runtime.clone(),
            },
        );
    }
    let file = ProfilesFile { profiles: map };
    let yaml = serde_yaml::to_string(&file).map_err(|e| e.to_string())?;

    // Atomic write: write to a temporary file in the same directory, then rename to target path.
    // Placing the tmp file in the same directory guarantees rename is atomic within the filesystem.
    let rand_suffix = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let tmp_path = parent.join(format!(
        ".profiles.yaml.tmp.{}.{}",
        std::process::id(),
        rand_suffix
    ));

    if let Err(err) = std::fs::write(&tmp_path, &yaml) {
        let _ = std::fs::remove_file(&tmp_path);
        return Err(err.to_string());
    }

    if let Err(err) = std::fs::rename(&tmp_path, &file_path) {
        let _ = std::fs::remove_file(&tmp_path);
        return Err(err.to_string());
    }

    Ok(())
}

pub fn upsert_profile(paths: &ClusterDeckPaths, profile: Profile) -> Result<(), String> {
    crate::services::validate::validate_profile(&profile)?;
    // remote_path is checked here rather than in validate_profile: validate_profile also
    // filters profiles loaded from disk (see load_profiles above), and an unsafe/legacy
    // remote_path must never make an otherwise-valid saved profile silently disappear and then
    // get deleted on the next save. Here, at the save boundary, a rejection is instead a
    // user-facing error the caller can act on.
    if let Some(kubeconfig) = &profile.kubeconfig {
        if !crate::services::validate::is_safe_remote_path(&kubeconfig.remote_path) {
            return Err(format!(
                "invalid kubeconfig remote_path: {}",
                kubeconfig.remote_path
            ));
        }
    }
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
    use crate::services::config::{AuthMode, BootstrapPolicy, Host, Profile};

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
                auth: AuthMode::Key,
            }],
            bastion: None,
            bootstrap: BootstrapPolicy::default(),
            kubeconfig: None,
            manage_hosts_file: true,
            trusted_cas: Vec::new(),
            local_runtime: None,
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
            trusted_cas: Vec::new(),
            local_runtime: None,
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
            trusted_cas: Vec::new(),
            local_runtime: None,
        };
        assert!(upsert_profile(&paths, profile).is_err());
    }

    #[test]
    fn upsert_then_load_roundtrips_trusted_cas() {
        let paths = temp_paths("trusted-cas-roundtrip");
        let profile = Profile {
            id: "cka".into(),
            name: "CKA Lab".into(),
            hosts: vec![],
            bastion: None,
            bootstrap: BootstrapPolicy::default(),
            kubeconfig: None,
            manage_hosts_file: false,
            trusted_cas: vec![crate::services::ca_trust::TrustedCa {
                secret_ref: "platform-system/apisix-gateway-tls-secret".into(),
                fingerprint_sha256:
                    "8b75bf97e19ce7efe9bb4d6c76b4f10e072a9d6ea89d8f438f423afea7156246".into(),
                fingerprint_sha1: "67fc8cc8df72476829ecd88d188331a6d29baabb".into(),
                subject_cn: "clusterdeck-test-ca.invalid".into(),
                not_after: "Sep 18 05:40:47 2036 GMT".into(),
                trusted_at: "2026-09-21T00:00:00+00:00".into(),
            }],
            local_runtime: None,
        };
        upsert_profile(&paths, profile.clone()).unwrap();
        let loaded = get_profile(&paths, "cka").unwrap();
        assert_eq!(loaded.trusted_cas.len(), 1);
        assert_eq!(loaded.trusted_cas[0], profile.trusted_cas[0]);
    }

    #[test]
    fn load_profiles_keeps_profile_with_unsafe_legacy_remote_path_and_upsert_preserves_it_on_save()
    {
        // Regression: a profile saved before is_safe_remote_path existed (or hand-edited) must
        // not vanish from load_profiles, and a later upsert of an UNRELATED profile must not
        // delete it from disk as a side effect of save_profiles rewriting the whole file.
        let paths = temp_paths("legacy-remote-path");
        if let Some(parent) = paths.profiles_file().parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        let yaml = r#"
profiles:
  legacy:
    name: "Legacy"
    hosts:
      - name: m1
        address: 192.0.2.10
        port: 22
        user: root
    kubeconfig:
      remote_path: "relative/no/leading/slash.conf"
      control_plane: m1
      local_path: ""
      context: legacy
    manage_hosts_file: false
"#;
        std::fs::write(paths.profiles_file(), yaml).unwrap();

        let loaded = load_profiles(&paths).unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].id, "legacy");
        assert_eq!(
            loaded[0].kubeconfig.as_ref().unwrap().remote_path,
            "relative/no/leading/slash.conf"
        );

        let other = Profile {
            id: "other".into(),
            name: "Other".into(),
            hosts: vec![],
            bastion: None,
            bootstrap: BootstrapPolicy::default(),
            kubeconfig: None,
            manage_hosts_file: false,
            trusted_cas: Vec::new(),
            local_runtime: None,
        };
        upsert_profile(&paths, other).unwrap();

        let after_save = load_profiles(&paths).unwrap();
        assert_eq!(
            after_save.len(),
            2,
            "legacy profile must survive an unrelated upsert/save"
        );
        assert!(after_save.iter().any(|p| p.id == "legacy"));
        assert!(after_save.iter().any(|p| p.id == "other"));
    }

    #[test]
    fn upsert_profile_accepts_empty_remote_path_and_rejects_unsafe_one() {
        let paths = temp_paths("remote-path-validation");
        let mut profile = Profile {
            id: "cka".into(),
            name: "CKA Lab".into(),
            hosts: vec![],
            bastion: None,
            bootstrap: BootstrapPolicy::default(),
            kubeconfig: Some(KubeconfigSource {
                remote_path: "".into(),
                control_plane: "m1".into(),
                local_path: "".into(),
                context: "cka".into(),
            }),
            manage_hosts_file: false,
            trusted_cas: Vec::new(),
            local_runtime: None,
        };
        assert!(
            upsert_profile(&paths, profile.clone()).is_ok(),
            "empty remote_path means unconfigured and must be accepted"
        );

        profile.kubeconfig.as_mut().unwrap().remote_path = "/tmp/'; rm -rf ~ #".into();
        let err = upsert_profile(&paths, profile).unwrap_err();
        assert!(err.contains("remote_path"));
    }

    #[test]
    fn load_profiles_defaults_trusted_cas_when_field_absent_from_yaml() {
        let paths = temp_paths("trusted-cas-default");
        if let Some(parent) = paths.profiles_file().parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        // Simulates a profiles.yaml written before this field existed.
        let yaml = r#"
profiles:
  legacy:
    name: "Legacy"
    hosts: []
    manage_hosts_file: false
"#;
        std::fs::write(paths.profiles_file(), yaml).unwrap();
        let loaded = load_profiles(&paths).unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].trusted_cas.len(), 0);
    }

    #[test]
    fn load_profiles_defaults_local_runtime_to_none_when_field_absent_from_yaml() {
        let paths = temp_paths("local-runtime-default");
        if let Some(parent) = paths.profiles_file().parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        // Simulates profiles.yaml persisted before local_runtime existed (Issue: stale
        // Colima/Lima SSH port after VM restart).
        let yaml = r#"
profiles:
  legacy:
    name: "Legacy"
    hosts: []
    manage_hosts_file: false
"#;
        std::fs::write(paths.profiles_file(), yaml).unwrap();
        let loaded = load_profiles(&paths).unwrap();
        assert_eq!(loaded.len(), 1);
        assert!(loaded[0].local_runtime.is_none());
    }

    #[test]
    fn upsert_then_load_roundtrips_local_runtime() {
        let paths = temp_paths("local-runtime-roundtrip");
        let profile = Profile {
            id: "colima-default".into(),
            name: "Colima Local".into(),
            hosts: vec![],
            bastion: None,
            bootstrap: BootstrapPolicy::default(),
            kubeconfig: None,
            manage_hosts_file: false,
            trusted_cas: Vec::new(),
            local_runtime: Some(LocalRuntimeSource {
                provider: crate::services::local_runtime_lifecycle::LocalRuntimeProvider::Colima,
                instance: "default".into(),
            }),
        };
        upsert_profile(&paths, profile).unwrap();
        let loaded = get_profile(&paths, "colima-default").unwrap();
        let local_runtime = loaded.local_runtime.expect("local_runtime must round-trip");
        assert_eq!(
            local_runtime.provider,
            crate::services::local_runtime_lifecycle::LocalRuntimeProvider::Colima
        );
        assert_eq!(local_runtime.instance, "default");
    }

    fn sample_profile(id: &str) -> Profile {
        Profile {
            id: id.into(),
            name: "Test Profile".into(),
            hosts: vec![],
            bastion: None,
            bootstrap: BootstrapPolicy::default(),
            kubeconfig: None,
            manage_hosts_file: false,
            trusted_cas: Vec::new(),
            local_runtime: None,
        }
    }

    #[test]
    fn save_profiles_atomic_write_preserves_content_and_leaves_no_tmp_files() {
        let paths = temp_paths("atomic-write-test");
        let parent = paths.profiles_file().parent().unwrap().to_path_buf();
        std::fs::create_dir_all(&parent).unwrap();

        let profile = sample_profile("atomic-p1");
        save_profiles(&paths, std::slice::from_ref(&profile)).unwrap();

        // 1. Target file exists and has valid content
        let loaded = load_profiles(&paths).unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].id, "atomic-p1");

        // 2. No temporary file lingering in parent directory
        let tmp_files: Vec<_> = std::fs::read_dir(&parent)
            .unwrap()
            .filter_map(Result::ok)
            .filter(|e| {
                e.file_name()
                    .to_string_lossy()
                    .starts_with(".profiles.yaml.tmp.")
            })
            .collect();
        assert!(
            tmp_files.is_empty(),
            "temporary write files must be cleaned up"
        );

        // 3. Atomically overwrite with second profile
        let profile2 = sample_profile("atomic-p2");
        save_profiles(&paths, &[profile, profile2]).unwrap();
        let loaded2 = load_profiles(&paths).unwrap();
        assert_eq!(loaded2.len(), 2);
    }

    #[tokio::test]
    async fn profile_write_guard_serializes_concurrent_writers() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::sync::Arc;

        let active_writers = Arc::new(AtomicUsize::new(0));
        let max_concurrent = Arc::new(AtomicUsize::new(0));

        let mut handles = Vec::new();
        for _ in 0..10 {
            let active = Arc::clone(&active_writers);
            let max_c = Arc::clone(&max_concurrent);
            handles.push(tokio::spawn(async move {
                let guard = ProfileWriteGuard;
                let _lock = guard.lock().await;

                let current = active.fetch_add(1, Ordering::SeqCst) + 1;
                max_c.fetch_max(current, Ordering::SeqCst);
                tokio::time::sleep(std::time::Duration::from_millis(5)).await;
                active.fetch_sub(1, Ordering::SeqCst);
            }));
        }

        for h in handles {
            h.await.unwrap();
        }

        assert_eq!(
            max_concurrent.load(Ordering::SeqCst),
            1,
            "ProfileWriteGuard must allow only 1 concurrent writer"
        );
    }
}
