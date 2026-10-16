#![allow(dead_code)]

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

use crate::services::config::{
    Bastion, BootstrapPolicy, Host, HostMapping, KubeconfigSource, LocalRuntimeSource, Profile,
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
    /// Legacy bool, still written (true iff `host_mapping == Hosts`) so an older build that
    /// reads this file keeps Hosts profiles working (plan R9). Only the fallback on read.
    #[serde(default)]
    manage_hosts_file: bool,
    /// Wins over `manage_hosts_file` when present.
    #[serde(default)]
    host_mapping: Option<HostMapping>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    approved_dns_zones: Vec<String>,
    #[serde(default)]
    trusted_cas: Vec<crate::services::ca_trust::TrustedCa>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    gateway_address: Option<String>,
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
                host_mapping: HostMapping::migrate(body.host_mapping, body.manage_hosts_file),
                approved_dns_zones: body.approved_dns_zones,
                trusted_cas: body.trusted_cas,
                gateway_address: body.gateway_address,
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

/// True when any profile mapping in `yaml` already has a `host_mapping` key. Unparseable
/// content counts as "no" so it gets backed up rather than silently overwritten.
fn has_host_mapping_key(yaml: &str) -> bool {
    let Ok(value) = serde_yaml::from_str::<serde_yaml::Value>(yaml) else {
        return false;
    };
    value
        .get("profiles")
        .and_then(|p| p.as_mapping())
        .is_some_and(|m| m.values().any(|body| body.get("host_mapping").is_some()))
}

/// Rejects `zone` when it is reserved or overlaps (equal, parent or child) a zone approved
/// for any profile other than `owner_id`/`also_allowed`. Overlap across profiles would let a
/// parent-zone owner publish names under a child zone owned by someone else.
fn check_zone_ownership(
    profiles: &[Profile],
    zone: &str,
    allowed_owners: &[&str],
) -> Result<(), String> {
    use crate::services::local_dns::zone_file::is_name_in_zone;
    if !crate::services::validate::is_allowed_private_zone(zone) {
        return Err(format!("invalid or disallowed DNS zone: {zone}"));
    }
    if crate::services::validate::is_reserved_clusterdeck_zone(zone) {
        return Err(format!(
            "DNS zone {zone} is reserved for ClusterDeck node aliases (clusterdeck.local)"
        ));
    }
    for p in profiles
        .iter()
        .filter(|p| !allowed_owners.contains(&p.id.as_str()))
    {
        if let Some(other) = p
            .approved_dns_zones
            .iter()
            .find(|o| is_name_in_zone(zone, o) || is_name_in_zone(o, zone))
        {
            return Err(format!(
                "DNS zone {zone} overlaps zone {other} approved for profile '{}'; switch ownership explicitly",
                p.id
            ));
        }
    }
    Ok(())
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
                manage_hosts_file: p.host_mapping == HostMapping::Hosts,
                host_mapping: Some(p.host_mapping),
                approved_dns_zones: p.approved_dns_zones.clone(),
                trusted_cas: p.trusted_cas.clone(),
                gateway_address: p.gateway_address.clone(),
                local_runtime: p.local_runtime.clone(),
            },
        );
    }
    let file = ProfilesFile { profiles: map };
    let yaml = serde_yaml::to_string(&file).map_err(|e| e.to_string())?;

    // Plan R9: keep a one-time copy of a pre-local-dns file so a downgrade or a bad migration
    // is recoverable. A failed backup fails the save: never migrate without it.
    let backup_path = parent.join("profiles.yaml.pre-local-dns.bak");
    if !backup_path.exists() {
        if let Ok(existing) = std::fs::read_to_string(&file_path) {
            if !has_host_mapping_key(&existing) {
                std::fs::write(&backup_path, &existing).map_err(|e| {
                    format!(
                        "could not back up {} to {} before migrating: {e}",
                        file_path.display(),
                        backup_path.display()
                    )
                })?;
            }
        }
    }

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
    // Plan R11: a zone has a single owner. Moving it is an explicit `switch_dns_zone_owner`.
    for zone in &profile.approved_dns_zones {
        check_zone_ownership(&profiles, zone, &[profile.id.as_str()])?;
    }
    if let Some(pos) = profiles.iter().position(|p| p.id == profile.id) {
        profiles[pos] = profile;
    } else {
        profiles.push(profile);
    }
    save_profiles(paths, &profiles)
}

/// Atomically moves `zone` from its current owner to `to_profile` (one `profiles.yaml` write).
/// Callers hold the profile write mutex, like every other mutator here.
pub fn switch_dns_zone_owner(
    paths: &ClusterDeckPaths,
    zone: &str,
    to_profile: &str,
) -> Result<(), String> {
    let mut profiles = load_profiles(paths)?;
    // Check against the state after the move: the exact zone leaves its current owner, so
    // only a different parent/child zone held by another profile blocks it.
    let mut after = profiles.clone();
    for p in &mut after {
        p.approved_dns_zones.retain(|z| z != zone);
    }
    check_zone_ownership(&after, zone, &[to_profile])?;
    if !profiles.iter().any(|p| p.id == to_profile) {
        return Err(format!("Profile not found: {to_profile}"));
    }
    for p in &mut profiles {
        if p.id == to_profile {
            if !p.approved_dns_zones.iter().any(|z| z == zone) {
                p.approved_dns_zones.push(zone.to_string());
            }
        } else {
            p.approved_dns_zones.retain(|z| z != zone);
        }
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
        assert!(switch_dns_zone_owner(&paths, "foo.clusterdeck.local", "b").is_err());
    }

    #[test]
    fn upsert_rejects_zone_under_clusterdeck_local() {
        let paths = temp_paths("zones-reserved");
        for zone in [
            "clusterdeck.local",
            "x.clusterdeck.local",
            "a.b.clusterdeck.local",
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
