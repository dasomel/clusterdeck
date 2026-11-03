use std::path::Path;

use super::*;
use crate::services::config::{
    AuthMode, Bastion, BootstrapPolicy, Host, HostMapping, LocalRuntimeSource,
};
use crate::services::inventory::apply::apply;
use crate::services::local_runtime_lifecycle::LocalRuntimeProvider;

struct Fixture {
    root: PathBuf,
    target: Target,
    profile: Profile,
}

impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!(
            "cd-131-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let project = root.join("a/lab");
        let hosts = ["master-1", "worker-1"]
            .iter()
            .enumerate()
            .map(|(idx, name)| {
                let key = project.join(format!(".vagrant/machines/{name}/virtualbox/private_key"));
                std::fs::create_dir_all(key.parent().unwrap()).unwrap();
                std::fs::write(&key, "fixture").unwrap();
                Host {
                    name: (*name).into(),
                    address: format!("192.0.2.{}", idx + 10),
                    port: 22,
                    user: "vagrant".into(),
                    identity_file: Some(key.to_string_lossy().into_owned()),
                    auth: AuthMode::Key,
                }
            })
            .collect();
        let target = Target {
            id: crate::services::inventory::identity::vagrant_profile_id(&project, "lab"),
            vagrant_project_dir: Some(project),
            name: "lab (Vagrant)".into(),
            hosts,
            local_runtime: None,
            kubeconfig_path: "/etc/kubernetes/admin.conf".into(),
            context: "lab".into(),
        };
        let mut profiles = vec![];
        apply(&mut profiles, &target, &canonicalize).unwrap();
        let mut profile = profiles.remove(0);
        profile.id = "vagrant-lab".into();
        profile.name = "Custom lab".into();
        profile.hosts[0].address = "127.0.0.1".into();
        profile.hosts[0].port = 2250;
        profile.hosts[0].user = "custom".into();
        profile.hosts[0].auth = AuthMode::Password;
        profile.bastion = Some(Bastion {
            name: "relay".into(),
            address: "192.0.2.50".into(),
            port: 2200,
            user: "ops".into(),
            identity_file: Some("/fixture/relay".into()),
        });
        profile.bootstrap = BootstrapPolicy {
            enabled: true,
            retries: 7,
            retry_delay_secs: 11,
        };
        profile.host_mapping = HostMapping::LocalDns;
        profile.approved_dns_zones = vec!["lab.test".into()];
        profile.gateway_address = Some("192.0.2.60".into());
        profile.trusted_cas = vec![crate::services::ca_trust::TrustedCa {
            secret_ref: "ns/ca".into(),
            fingerprint_sha256: "a".into(),
            fingerprint_sha1: "b".into(),
            subject_cn: "fixture".into(),
            not_after: "x".into(),
            trusted_at: "y".into(),
        }];
        Self {
            root,
            target,
            profile,
        }
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn canonicalize(path: &Path) -> Option<PathBuf> {
    path.canonicalize().ok()
}

fn assert_conflict(f: &Fixture) {
    let mut profiles = vec![f.profile.clone()];
    let before = serde_json::to_value(&profiles).unwrap();
    let error = apply(&mut profiles, &f.target, &canonicalize).unwrap_err();
    assert!(
        error.contains("is already used by existing profile"),
        "{error}"
    );
    assert_eq!(serde_json::to_value(&profiles).unwrap(), before);
}

#[test]
fn legacy_profile_refreshes_in_place_and_is_idempotent() {
    let f = Fixture::new();
    let mut expected = f.profile.clone();
    expected.hosts[0].address = f.target.hosts[0].address.clone();
    expected.hosts[0].port = 22;
    let mut profiles = vec![f.profile.clone()];
    // Host order is not identity; refresh by name, preserving the saved order.
    let mut target = f.target.clone();
    target.hosts.reverse();
    target.kubeconfig_path = "/custom/new-target".into();
    target.context = "new-context".into();
    let outcome = apply(&mut profiles, &target, &canonicalize).unwrap();
    assert_eq!(
        outcome,
        ("vagrant-lab".into(), false, vec!["master-1".into()])
    );
    assert_eq!(profiles.len(), 1);
    assert_eq!(
        serde_json::to_value(&profiles[0]).unwrap(),
        serde_json::to_value(&expected).unwrap()
    );
    assert_eq!(
        apply(&mut profiles, &target, &canonicalize).unwrap(),
        ("vagrant-lab".into(), false, vec![])
    );
    assert_eq!(
        serde_json::to_value(&profiles[0]).unwrap(),
        serde_json::to_value(&expected).unwrap()
    );
}

#[test]
fn legacy_identity_in_other_project_with_same_basename_is_not_adopted() {
    let mut f = Fixture::new();
    let key = f
        .root
        .join("b/lab/.vagrant/machines/master-1/virtualbox/private_key");
    std::fs::create_dir_all(key.parent().unwrap()).unwrap();
    std::fs::write(&key, "fixture").unwrap();
    f.profile.hosts[0].identity_file = Some(key.to_string_lossy().into_owned());
    assert_conflict(&f);
}

#[test]
fn legacy_host_name_set_mismatch_is_not_adopted() {
    let mut f = Fixture::new();
    f.profile.hosts[0].name = "other-master".into();
    assert_conflict(&f);
}

#[test]
fn non_legacy_id_is_never_adopted() {
    let mut f = Fixture::new();
    f.profile.id = "custom-lab".into();
    assert_conflict(&f);
}

#[test]
fn legacy_missing_key_or_project_evidence_is_not_adopted() {
    let mut f = Fixture::new();
    f.profile.hosts[0].identity_file = None;
    assert_conflict(&f);
    f.profile.hosts[0].identity_file = Some(f.root.join("missing").to_string_lossy().into_owned());
    assert_conflict(&f);
    f.target.vagrant_project_dir = None;
    assert_conflict(&f);
}

#[test]
fn legacy_duplicate_or_empty_host_names_are_not_adopted() {
    let mut f = Fixture::new();
    let original = f.profile.hosts[0].clone();
    f.profile.hosts[0] = f.profile.hosts[1].clone();
    assert_conflict(&f);
    f.profile.hosts[0] = original;
    f.target.hosts.push(f.target.hosts[0].clone());
    assert_conflict(&f);
    f.target.hosts.clear();
    assert_eq!(
        candidate(&[f.profile.clone()], &f.target, &canonicalize),
        None
    );
}

#[test]
fn ambiguous_legacy_candidates_are_not_adopted() {
    let f = Fixture::new();
    let mut profiles = vec![f.profile.clone(), f.profile.clone()];
    assert!(apply(&mut profiles, &f.target, &canonicalize)
        .unwrap_err()
        .contains("is already used"));
}

#[test]
fn id_match_takes_precedence_without_legacy_evidence() {
    let mut f = Fixture::new();
    f.profile.id = f.target.id.clone();
    f.profile.hosts[0].identity_file = None;
    let mut profiles = vec![f.profile.clone()];
    let outcome = apply(&mut profiles, &f.target, &|_| {
        panic!("id match needs no filesystem evidence")
    })
    .unwrap();
    assert_eq!(outcome.0, f.target.id);
    assert!(!outcome.1);
    assert_eq!(profiles[0].hosts[0].identity_file, None);
    assert_eq!(profiles[0].hosts[0].address, f.target.hosts[0].address);
}

#[test]
fn legacy_id_matches_profile_editor_utf16_replacement() {
    assert_eq!(
        legacy_id(Path::new("/fixture/Lab_한😀")),
        Some("vagrant--ab_---".into())
    );
    assert_eq!(
        legacy_id(Path::new("/fixture/lab--a.b")),
        Some("vagrant-lab--a-b".into())
    );
}

#[cfg(unix)]
#[test]
fn legacy_key_symlink_outside_project_is_not_adopted() {
    let f = Fixture::new();
    let key = Path::new(f.profile.hosts[0].identity_file.as_ref().unwrap());
    let outside = f.root.join("outside-key");
    std::fs::write(&outside, "fixture").unwrap();
    std::fs::remove_file(key).unwrap();
    std::os::unix::fs::symlink(outside, key).unwrap();
    assert_conflict(&f);
}

#[test]
fn legacy_adoption_preserves_local_runtime() {
    let mut f = Fixture::new();
    f.target.local_runtime = Some(LocalRuntimeSource {
        provider: LocalRuntimeProvider::Colima,
        instance: "new-instance".into(),
    });
    for saved in [
        None,
        Some(LocalRuntimeSource {
            provider: LocalRuntimeProvider::Lima,
            instance: "saved-instance".into(),
        }),
    ] {
        f.profile.local_runtime = saved;
        let before = serde_json::to_value(&f.profile.local_runtime).unwrap();
        let mut profiles = vec![f.profile.clone()];
        apply(&mut profiles, &f.target, &canonicalize).unwrap();
        assert_eq!(
            serde_json::to_value(&profiles[0].local_runtime).unwrap(),
            before
        );
    }
}

#[test]
fn legacy_adoption_still_refuses_conflicts_with_other_profiles() {
    let f = Fixture::new();
    let mut other = f.profile.clone();
    other.id = "other".into();
    let mut profiles = vec![f.profile.clone(), other];
    let error = apply(&mut profiles, &f.target, &canonicalize).unwrap_err();
    assert!(error.contains("existing profile 'other'"), "{error}");
}
