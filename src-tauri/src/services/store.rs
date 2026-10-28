#![allow(dead_code)]

use crate::services::config::Profile;
use crate::services::paths::ClusterDeckPaths;
use crate::services::profile_document::{patch, EntryState, ProfileDocument, CHANGED_ELSEWHERE};

pub use crate::services::profile_document::fingerprint;

pub fn load_profiles(paths: &ClusterDeckPaths) -> Result<Vec<Profile>, String> {
    let doc = ProfileDocument::load(paths)?;
    let file_path = paths.profiles_file();
    for e in doc.entries() {
        if let EntryState::Invalid { reason } = e.state {
            eprintln!(
                "skipping invalid profile '{}' loaded from {}: {reason}",
                e.key,
                file_path.display()
            );
        }
    }
    Ok(doc.profiles())
}

pub fn load_document(paths: &ClusterDeckPaths) -> Result<ProfileDocument, String> {
    ProfileDocument::load(paths)
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
    crate::services::profile_document::mutate(paths, |doc| {
        for p in profiles {
            doc.merge_profile(p, &[])?;
        }
        Ok(true)
    })
}

pub fn upsert_profile(paths: &ClusterDeckPaths, profile: Profile) -> Result<(), String> {
    upsert_profile_editor(paths, profile, None)
}

pub fn upsert_profile_editor(
    paths: &ClusterDeckPaths,
    profile: Profile,
    expected_fp: Option<&str>,
) -> Result<(), String> {
    crate::services::validate::validate_profile(&profile)?;
    if let Some(kubeconfig) = &profile.kubeconfig {
        if !crate::services::validate::is_safe_remote_path(&kubeconfig.remote_path) {
            return Err(format!(
                "invalid kubeconfig remote_path: {}",
                kubeconfig.remote_path
            ));
        }
    }
    crate::services::profile_document::mutate(paths, |doc| {
        if let Some(expected) = expected_fp {
            if let Some(raw) = doc.raw(&profile.id) {
                let actual = crate::services::profile_document::fingerprint(&raw);
                if actual != expected {
                    return Err(CHANGED_ELSEWHERE.to_string());
                }
            }
        }
        doc.merge_profile(&profile, &[])?;
        doc.verify_entry(&profile.id)?;
        Ok(true)
    })
}

pub fn patch_profile(
    paths: &ClusterDeckPaths,
    id: &str,
    f: impl FnOnce(&mut serde_yaml::Mapping) -> Result<(), String>,
) -> Result<(), String> {
    crate::services::profile_document::mutate(paths, |doc| {
        doc.patch_entry(id, f)?;
        doc.verify_entry(id)?;
        Ok(true)
    })
}

pub fn delete_invalid(
    paths: &ClusterDeckPaths,
    key: &str,
    expected_fp: &str,
) -> Result<(), String> {
    crate::services::profile_document::mutate(paths, |doc| {
        let entry = doc
            .entry(key)
            .ok_or_else(|| format!("Invalid profile '{key}' not found"))?;
        if !matches!(entry.state, EntryState::Invalid { .. }) {
            return Err(format!("Profile '{key}' is valid; use delete_profile"));
        }
        let actual = crate::services::profile_document::fingerprint(&entry.raw);
        if actual != expected_fp {
            return Err(CHANGED_ELSEWHERE.to_string());
        }
        Ok(doc.remove(key))
    })
}

/// Atomically moves `zone` from its current owner to `to_profile` (one `profiles.yaml` write).
/// Callers hold the profile write mutex, like every other mutator here.
pub fn switch_dns_zone_owner(
    paths: &ClusterDeckPaths,
    zone: &str,
    to_profile: &str,
) -> Result<(), String> {
    crate::services::profile_document::mutate(paths, |doc| {
        let target = doc
            .entry(to_profile)
            .ok_or_else(|| format!("Profile not found: {to_profile}"))?;
        if !matches!(target.state, EntryState::Valid(_)) {
            return Err(format!("Profile not found: {to_profile}"));
        }
        let current_holders: Vec<String> = doc
            .zone_claims()
            .into_iter()
            .filter(|c| c.zones.iter().any(|z| z == zone))
            .map(|c| c.id)
            .collect();
        for holder in &current_holders {
            if holder != to_profile {
                doc.patch_entry(holder, |m| patch::remove_zone(m, zone))?;
            }
        }
        crate::services::profile_document::check_zone_ownership(
            &doc.zone_claims(),
            zone,
            &[to_profile],
        )?;
        doc.patch_entry(to_profile, |m| patch::add_zone(m, zone))?;
        for holder in &current_holders {
            doc.verify_entry(holder)?;
        }
        doc.verify_entry(to_profile)?;
        Ok(true)
    })
}

pub fn delete_profile(paths: &ClusterDeckPaths, profile_id: &str) -> Result<(), String> {
    crate::services::profile_document::mutate(paths, |doc| Ok(doc.remove(profile_id)))
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
    use crate::services::config::{
        AuthMode, BootstrapPolicy, Host, HostMapping, KubeconfigSource, LocalRuntimeSource, Profile,
    };

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
            host_mapping: crate::services::config::HostMapping::Hosts,
            approved_dns_zones: Vec::new(),
            gateway_address: None,
            trusted_cas: Vec::new(),
            local_runtime: None,
        };
        upsert_profile(&paths, profile.clone()).unwrap();
        let loaded = get_profile(&paths, "cka").unwrap();
        assert_eq!(loaded.name, "CKA Lab");
        assert_eq!(loaded.hosts.len(), 1);
        assert_eq!(loaded.host_mapping, HostMapping::Hosts);
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
            host_mapping: crate::services::config::HostMapping::Off,
            approved_dns_zones: Vec::new(),
            gateway_address: None,
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
    fn gateway_address_roundtrips_and_is_omitted_when_none() {
        let paths = temp_paths("gateway-address");
        let mut profile = Profile {
            id: "gw".into(),
            name: "GW".into(),
            hosts: vec![],
            bastion: None,
            bootstrap: BootstrapPolicy::default(),
            kubeconfig: None,
            host_mapping: crate::services::config::HostMapping::Off,
            approved_dns_zones: Vec::new(),
            gateway_address: None,
            trusted_cas: Vec::new(),
            local_runtime: None,
        };
        upsert_profile(&paths, profile.clone()).unwrap();
        let yaml = std::fs::read_to_string(paths.profiles_file()).unwrap();
        assert!(!yaml.contains("gateway_address"), "{yaml}");

        profile.gateway_address = Some("203.0.113.10".into());
        upsert_profile(&paths, profile).unwrap();
        let loaded = get_profile(&paths, "gw").unwrap();
        assert_eq!(loaded.gateway_address.as_deref(), Some("203.0.113.10"));
    }

    #[test]
    fn legacy_yaml_without_gateway_address_deserializes_to_none() {
        let paths = temp_paths("gateway-legacy");
        std::fs::create_dir_all(paths.profiles_file().parent().unwrap()).unwrap();
        std::fs::write(
            paths.profiles_file(),
            "profiles:\n  old:\n    name: Old\n    hosts: []\n    manage_hosts_file: false\n",
        )
        .unwrap();
        let loaded = load_profiles(&paths).unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].gateway_address, None);
    }

    #[test]
    fn upsert_profile_rejects_invalid_gateway_address() {
        let paths = temp_paths("gateway-invalid");
        let profile = Profile {
            id: "gw".into(),
            name: "GW".into(),
            hosts: vec![],
            bastion: None,
            bootstrap: BootstrapPolicy::default(),
            kubeconfig: None,
            host_mapping: crate::services::config::HostMapping::Off,
            approved_dns_zones: Vec::new(),
            gateway_address: Some("203.0.113.10\n127.0.0.1 evil".into()),
            trusted_cas: Vec::new(),
            local_runtime: None,
        };
        assert!(upsert_profile(&paths, profile.clone()).is_err());
        let unspecified = Profile {
            gateway_address: Some("0.0.0.0".into()),
            ..profile
        };
        assert!(upsert_profile(&paths, unspecified).is_err());
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
            host_mapping: crate::services::config::HostMapping::Off,
            approved_dns_zones: Vec::new(),
            gateway_address: None,
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
            host_mapping: crate::services::config::HostMapping::Off,
            approved_dns_zones: Vec::new(),
            gateway_address: None,
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
            host_mapping: crate::services::config::HostMapping::Off,
            approved_dns_zones: Vec::new(),
            gateway_address: None,
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
            host_mapping: crate::services::config::HostMapping::Off,
            approved_dns_zones: Vec::new(),
            gateway_address: None,
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
            host_mapping: crate::services::config::HostMapping::Off,
            approved_dns_zones: Vec::new(),
            gateway_address: None,
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
            host_mapping: crate::services::config::HostMapping::Off,
            approved_dns_zones: Vec::new(),
            gateway_address: None,
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

    fn write_legacy(paths: &ClusterDeckPaths, yaml: &str) {
        std::fs::create_dir_all(paths.profiles_file().parent().unwrap()).unwrap();
        std::fs::write(paths.profiles_file(), yaml).unwrap();
    }

    #[test]
    fn legacy_manage_hosts_file_migrates_to_host_mapping() {
        let paths = temp_paths("hm-legacy");
        write_legacy(
            &paths,
            "profiles:\n  on:\n    name: On\n    hosts: []\n    manage_hosts_file: true\n  off:\n    name: Off\n    hosts: []\n    manage_hosts_file: false\n  absent:\n    name: Absent\n    hosts: []\n",
        );
        let loaded = load_profiles(&paths).unwrap();
        let mode = |id: &str| loaded.iter().find(|p| p.id == id).unwrap().host_mapping;
        assert_eq!(mode("on"), HostMapping::Hosts);
        assert_eq!(mode("off"), HostMapping::Off);
        assert_eq!(mode("absent"), HostMapping::Off);
        // Round trip: re-saving keeps the meaning.
        save_profiles(&paths, &loaded).unwrap();
        let again = load_profiles(&paths).unwrap();
        assert_eq!(
            again.iter().find(|p| p.id == "on").unwrap().host_mapping,
            HostMapping::Hosts
        );
    }

    #[test]
    fn host_mapping_wins_over_legacy_bool_and_both_are_written() {
        let paths = temp_paths("hm-wins");
        write_legacy(
            &paths,
            "profiles:\n  a:\n    name: A\n    hosts: []\n    manage_hosts_file: true\n    host_mapping: local_dns\n",
        );
        let loaded = load_profiles(&paths).unwrap();
        assert_eq!(loaded[0].host_mapping, HostMapping::LocalDns);

        save_profiles(&paths, &loaded).unwrap();
        let yaml = std::fs::read_to_string(paths.profiles_file()).unwrap();
        assert!(yaml.contains("host_mapping: local_dns"), "{yaml}");
        assert!(yaml.contains("manage_hosts_file: false"), "{yaml}");

        let mut p = loaded[0].clone();
        p.host_mapping = HostMapping::Hosts;
        save_profiles(&paths, &[p]).unwrap();
        let yaml = std::fs::read_to_string(paths.profiles_file()).unwrap();
        assert!(yaml.contains("host_mapping: hosts"), "{yaml}");
        assert!(yaml.contains("manage_hosts_file: true"), "{yaml}");
    }

    #[test]
    fn approved_dns_zones_roundtrip_and_are_omitted_when_empty() {
        let paths = temp_paths("zones-roundtrip");
        let mut p = sample_profile("z1");
        upsert_profile(&paths, p.clone()).unwrap();
        let yaml = std::fs::read_to_string(paths.profiles_file()).unwrap();
        assert!(!yaml.contains("approved_dns_zones"), "{yaml}");

        p.approved_dns_zones = vec!["lab.example.internal".into()];
        upsert_profile(&paths, p).unwrap();
        let loaded = get_profile(&paths, "z1").unwrap();
        assert_eq!(loaded.approved_dns_zones, vec!["lab.example.internal"]);
    }

    #[test]
    fn upsert_rejects_invalid_zones() {
        let paths = temp_paths("zones-invalid");
        for bad in [
            "example.com",
            "Lab.Example.Internal",
            "internal",
            "a.internal",
            "x y.example.internal",
        ] {
            let mut p = sample_profile("z1");
            p.approved_dns_zones = vec![bad.into()];
            assert!(upsert_profile(&paths, p).is_err(), "{bad} must be rejected");
        }
    }

    #[test]
    fn zone_has_a_single_owner_and_switch_moves_it() {
        let paths = temp_paths("zones-owner");
        let mut a = sample_profile("a");
        a.approved_dns_zones = vec!["lab.example.internal".into()];
        upsert_profile(&paths, a).unwrap();

        let mut b = sample_profile("b");
        b.approved_dns_zones = vec!["lab.example.internal".into()];
        let err = upsert_profile(&paths, b.clone()).unwrap_err();
        assert!(err.contains("'a'"), "error must name the owner: {err}");

        // Re-saving the owner itself is fine.
        let mut a2 = get_profile(&paths, "a").unwrap();
        a2.name = "Renamed".into();
        upsert_profile(&paths, a2).unwrap();

        b.approved_dns_zones.clear();
        upsert_profile(&paths, b).unwrap();
        switch_dns_zone_owner(&paths, "lab.example.internal", "b").unwrap();
        assert!(get_profile(&paths, "a")
            .unwrap()
            .approved_dns_zones
            .is_empty());
        assert_eq!(
            get_profile(&paths, "b").unwrap().approved_dns_zones,
            vec!["lab.example.internal"]
        );

        assert!(switch_dns_zone_owner(&paths, "lab.example.internal", "missing").is_err());
        assert!(switch_dns_zone_owner(&paths, "example.com", "b").is_err());
    }

    #[test]
    fn pre_local_dns_backup_is_created_once() {
        let paths = temp_paths("hm-backup");
        let legacy = "profiles:\n  a:\n    name: A\n    hosts: []\n    manage_hosts_file: true\n";
        write_legacy(&paths, legacy);
        let backup = paths
            .profiles_file()
            .with_file_name("profiles.yaml.pre-local-dns.bak");

        upsert_profile(&paths, sample_profile("b")).unwrap();
        assert_eq!(std::fs::read_to_string(&backup).unwrap(), legacy);

        // Later saves (file now has host_mapping, backup exists) never overwrite it.
        upsert_profile(&paths, sample_profile("c")).unwrap();
        assert_eq!(std::fs::read_to_string(&backup).unwrap(), legacy);
    }

    #[test]
    fn no_backup_when_no_profiles_file_existed() {
        let paths = temp_paths("hm-nobackup");
        upsert_profile(&paths, sample_profile("a")).unwrap();
        let backup = paths
            .profiles_file()
            .with_file_name("profiles.yaml.pre-local-dns.bak");
        assert!(!backup.exists());
    }

    #[test]
    fn overlapping_zones_across_profiles_are_rejected() {
        let paths = temp_paths("zones-overlap");
        let mut a = sample_profile("a");
        a.approved_dns_zones = vec!["lab.example.internal".into()];
        upsert_profile(&paths, a).unwrap();

        for zone in ["dev.lab.example.internal", "example.test"] {
            let mut b = sample_profile("b");
            b.approved_dns_zones = vec![zone.into()];
            let r = upsert_profile(&paths, b);
            if zone == "dev.lab.example.internal" {
                assert!(r.unwrap_err().contains("'a'"));
            } else {
                assert!(r.is_err(), "two-label zone is invalid anyway");
            }
        }
        // A parent of an existing child is rejected too.
        let mut c = sample_profile("c");
        c.approved_dns_zones = vec!["x.example.internal".into()];
        upsert_profile(&paths, c).unwrap();
        let mut d = sample_profile("d");
        d.approved_dns_zones = vec!["a.x.example.internal".into()];
        assert!(upsert_profile(&paths, d).is_err());
        // The same profile may hold nested zones.
        let mut a2 = get_profile(&paths, "a").unwrap();
        a2.approved_dns_zones
            .push("dev.lab.example.internal".into());
        upsert_profile(&paths, a2).unwrap();
    }

    #[test]
    fn switch_rejects_overlap_with_third_profile_and_reserved_zone() {
        let paths = temp_paths("zones-switch-overlap");
        let mut a = sample_profile("a");
        a.approved_dns_zones = vec![
            "dev.lab.example.internal".into(),
            "lab.example.internal".into(),
        ];
        upsert_profile(&paths, a).unwrap();
        upsert_profile(&paths, sample_profile("b")).unwrap();
        // Moving only the child would leave a's parent overlapping b's child.
        let err = switch_dns_zone_owner(&paths, "dev.lab.example.internal", "b").unwrap_err();
        assert!(err.contains("'a'"), "{err}");
        assert!(switch_dns_zone_owner(&paths, "foo.clusterdeck.internal", "b").is_err());
    }

    #[test]
    fn upsert_rejects_zone_under_clusterdeck_local() {
        let paths = temp_paths("zones-reserved");
        for zone in [
            "clusterdeck.internal",
            "x.clusterdeck.internal",
            "a.b.clusterdeck.internal",
            // Legacy domain stays reserved (D2).
            "clusterdeck.local",
            "x.clusterdeck.local",
        ] {
            let mut p = sample_profile("a");
            p.approved_dns_zones = vec![zone.into()];
            assert!(upsert_profile(&paths, p).is_err(), "{zone}");
        }
    }

    #[test]
    fn backup_decision_parses_yaml_instead_of_substring() {
        let paths = temp_paths("hm-backup-parse");
        // "host_mapping" appears only inside a name/value, no key: still needs a backup.
        let legacy = "profiles:\n  host_mapping_lab:\n    name: host_mapping\n    hosts: []\n    manage_hosts_file: false\n";
        write_legacy(&paths, legacy);
        let backup = paths
            .profiles_file()
            .with_file_name("profiles.yaml.pre-local-dns.bak");
        upsert_profile(&paths, sample_profile("b")).unwrap();
        assert_eq!(std::fs::read_to_string(&backup).unwrap(), legacy);
    }

    #[test]
    fn no_backup_when_a_profile_already_has_host_mapping_key() {
        let paths = temp_paths("hm-backup-has-key");
        write_legacy(
            &paths,
            "profiles:\n  a:\n    name: A\n    hosts: []\n    host_mapping: off\n",
        );
        let backup = paths
            .profiles_file()
            .with_file_name("profiles.yaml.pre-local-dns.bak");
        upsert_profile(&paths, sample_profile("b")).unwrap();
        assert!(!backup.exists());
    }

    #[cfg(unix)]
    #[test]
    fn failing_backup_fails_the_save_and_leaves_profiles_untouched() {
        let paths = temp_paths("hm-backup-fails");
        let legacy = "profiles:\n  a:\n    name: A\n    hosts: []\n    manage_hosts_file: true\n";
        write_legacy(&paths, legacy);
        // Dangling symlink: `exists()` is false but writing through it fails.
        let backup = paths
            .profiles_file()
            .with_file_name("profiles.yaml.pre-local-dns.bak");
        std::os::unix::fs::symlink("/nonexistent-clusterdeck-dir/x.bak", &backup).unwrap();
        let err = upsert_profile(&paths, sample_profile("b")).unwrap_err();
        assert!(err.contains("back up"), "{err}");
        assert_eq!(
            std::fs::read_to_string(paths.profiles_file()).unwrap(),
            legacy
        );
    }
}

#[cfg(test)]
mod lossless_tests;
