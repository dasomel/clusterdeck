//! Regression tests for #59: profiles.yaml must never lose an entry or a field that this
//! version does not understand. Placeholder values only (lab.example.internal, 192.0.2.x).

use super::*;
use crate::services::ca_trust::TrustedCa;
use crate::services::config::Host;
use serde_yaml::Value;

pub(super) const FIXTURE: &str = r#"
# comments are not preserved (documented limitation)
profiles:
  good:
    name: Good
    hosts:
      - name: m1
        address: 192.0.2.10
        port: 22
        user: root
        future_host_key: keep-host
    bastion: null
    bootstrap: {enabled: false, retries: 3, retry_delay_secs: 5}
    kubeconfig: null
    manage_hosts_file: false
    host_mapping: local_dns
    approved_dns_zones: [lab.example.internal]
    trusted_cas: []
    future_key: {nested: [1, 2]}
  other:
    name: Other
    hosts:
      - name: n1
        address: 192.0.2.20
        port: 22
        user: root
        future_host_key: keep-other-host
    manage_hosts_file: false
    host_mapping: off
    local_runtime: {provider: colima, instance: default}
    trusted_cas:
      - secret_ref: ns/a
        fingerprint_sha256: aa
        fingerprint_sha1: aa
        subject_cn: a.invalid
        not_after: later
        trusted_at: now
      - secret_ref: ns/b
        fingerprint_sha256: bb
        fingerprint_sha1: bb
        subject_cn: b.invalid
        not_after: later
        trusted_at: now
        future_ca_key: keep-ca
  bad-validator:
    name: Bad Validator
    hosts: []
    gateway_address: "0.0.0.0"
    host_mapping: off
    manage_hosts_file: false
    future_key: kept
"#;

/// Deserialize-type failures (not just validator failures): wrong types, unknown enum value.
pub(super) const FIXTURE_UNDESERIALIZABLE: &str = r#"
profiles:
  good:
    name: Good
    hosts: []
    host_mapping: off
    manage_hosts_file: false
    future_key: 1
  bad-type:
    name: [not, a, string]
    hosts: 5
  future-mode:
    name: Future
    hosts: []
    host_mapping: quantum_dns
    future_key: keep
"#;

pub(super) fn temp_paths(tag: &str) -> ClusterDeckPaths {
    let dir = std::env::temp_dir().join(format!(
        "clusterdeck-store-lossless-{tag}-{}",
        std::process::id()
    ));
    std::fs::remove_dir_all(&dir).ok();
    ClusterDeckPaths::at(dir)
}

pub(super) fn seeded(tag: &str, yaml: &str) -> ClusterDeckPaths {
    let paths = temp_paths(tag);
    std::fs::create_dir_all(paths.profiles_file().parent().unwrap()).unwrap();
    std::fs::write(paths.profiles_file(), yaml).unwrap();
    paths
}

pub(super) fn snapshot(paths: &ClusterDeckPaths) -> Value {
    serde_yaml::from_str(&std::fs::read_to_string(paths.profiles_file()).unwrap()).unwrap()
}

pub(super) fn entry(snap: &Value, key: &str) -> Value {
    snap["profiles"][key].clone()
}

pub(super) fn ca(secret_ref: &str, tag: &str) -> TrustedCa {
    TrustedCa {
        secret_ref: secret_ref.into(),
        fingerprint_sha256: tag.into(),
        fingerprint_sha1: tag.into(),
        subject_cn: format!("{tag}.invalid"),
        not_after: "later".into(),
        trusted_at: "now".into(),
    }
}

/// Everything except `touched` must be Value-identical to before the mutation, including
/// invalid entries and unknown keys.
fn assert_untouched_except(before: &Value, after: &Value, touched: &[&str]) {
    let keys: Vec<String> = before["profiles"]
        .as_mapping()
        .unwrap()
        .keys()
        .map(|k| k.as_str().unwrap().to_string())
        .collect();
    for k in keys {
        if touched.contains(&k.as_str()) {
            continue;
        }
        assert_eq!(
            entry(after, &k),
            entry(before, &k),
            "entry '{k}' must round-trip untouched"
        );
    }
}

// ---- operations modelled exactly as the pre-fix callers performed them ----

pub(super) fn op_editor_save(paths: &ClusterDeckPaths) -> Result<(), String> {
    let mut p = get_profile(paths, "good")?;
    p.name = "Good (edited)".into();
    upsert_profile(paths, p)
}

pub(super) fn op_zone_approve(paths: &ClusterDeckPaths) -> Result<(), String> {
    patch_profile(paths, "other", |m| {
        patch::add_zone(m, "svc.example.internal")
    })
}

pub(super) fn op_zone_revoke(paths: &ClusterDeckPaths) -> Result<(), String> {
    patch_profile(paths, "good", |m| {
        patch::remove_zone(m, "lab.example.internal")
    })
}

pub(super) fn op_ca_trust(paths: &ClusterDeckPaths) -> Result<(), String> {
    let c = ca("ns/new", "cc");
    patch_profile(paths, "other", |m| patch::set_trusted_ca(m, &c))
}

pub(super) fn op_ca_replace(paths: &ClusterDeckPaths) -> Result<(), String> {
    let c = ca("ns/a", "a2");
    patch_profile(paths, "other", |m| patch::set_trusted_ca(m, &c))
}

pub(super) fn op_ca_cleanup(paths: &ClusterDeckPaths) -> Result<(), String> {
    patch_profile(paths, "other", |m| patch::remove_trusted_ca(m, "ns/a"))
}

pub(super) fn op_ca_remove(paths: &ClusterDeckPaths) -> Result<(), String> {
    patch_profile(paths, "other", |m| patch::remove_trusted_ca(m, "ns/a"))
}

pub(super) fn op_runtime_refresh(paths: &ClusterDeckPaths) -> Result<(), String> {
    patch_profile(paths, "other", |m| {
        patch::set_host_endpoint(m, 0, "n1", "192.0.2.20", 2222, None)
    })
}

pub(super) fn op_create_from_environment(paths: &ClusterDeckPaths) -> Result<(), String> {
    let mut profiles = load_profiles(paths)?;
    let host = Host {
        name: "env1".into(),
        address: "192.0.2.50".into(),
        port: 22,
        user: "vagrant".into(),
        identity_file: None,
        auth: crate::services::config::AuthMode::Key,
    };
    let target = crate::services::inventory::apply::Target {
        id: "env-new".into(),
        name: "Env".into(),
        hosts: vec![host],
        local_runtime: None,
        kubeconfig_path: "/etc/rancher/k3s/k3s.yaml".into(),
        context: "env".into(),
    };
    let (id, _, _) = crate::services::inventory::apply::apply(&mut profiles, &target)?;
    let profile = profiles.into_iter().find(|p| p.id == id).unwrap();
    upsert_profile(paths, profile)
}

#[test]
fn regress_editor_save_keeps_invalid_unknown_and_other_entries() {
    let paths = seeded("r-editor", FIXTURE);
    let before = snapshot(&paths);
    op_editor_save(&paths).unwrap();
    let after = snapshot(&paths);
    assert_untouched_except(&before, &after, &["good"]);
    assert_eq!(entry(&after, "good")["name"], Value::from("Good (edited)"));
    assert_eq!(
        entry(&after, "good")["future_key"],
        entry(&before, "good")["future_key"],
        "unknown top-level keys of the edited entry survive"
    );
}

#[test]
fn regress_delete_keeps_invalid_and_unknown_entries() {
    let paths = seeded("r-delete", FIXTURE);
    let before = snapshot(&paths);
    delete_profile(&paths, "other").unwrap();
    let after = snapshot(&paths);
    assert!(after["profiles"].get("other").is_none());
    assert_untouched_except(&before, &after, &["other"]);
}

#[test]
fn regress_zone_approve_keeps_invalid_and_unknown_entries() {
    let paths = seeded("r-approve", FIXTURE);
    let before = snapshot(&paths);
    op_zone_approve(&paths).unwrap();
    let after = snapshot(&paths);
    assert_untouched_except(&before, &after, &["other"]);
    assert_eq!(
        entry(&after, "other")["hosts"],
        entry(&before, "other")["hosts"],
        "a zone approval touches only approved_dns_zones"
    );
}

#[test]
fn regress_zone_revoke_keeps_invalid_and_unknown_entries() {
    let paths = seeded("r-revoke", FIXTURE);
    let before = snapshot(&paths);
    op_zone_revoke(&paths).unwrap();
    let after = snapshot(&paths);
    assert_untouched_except(&before, &after, &["good"]);
    assert_eq!(
        entry(&after, "good")["future_key"],
        entry(&before, "good")["future_key"]
    );
    assert_eq!(
        entry(&after, "good")["hosts"],
        entry(&before, "good")["hosts"]
    );
}

#[test]
fn regress_zone_switch_keeps_invalid_and_unknown_entries() {
    let paths = seeded("r-switch", FIXTURE);
    let before = snapshot(&paths);
    switch_dns_zone_owner(&paths, "lab.example.internal", "other").unwrap();
    let after = snapshot(&paths);
    assert_untouched_except(&before, &after, &["good", "other"]);
    assert_eq!(
        entry(&after, "good")["future_key"],
        entry(&before, "good")["future_key"]
    );
    assert_eq!(
        entry(&after, "other")["hosts"],
        entry(&before, "other")["hosts"]
    );
}

fn assert_ca_op_is_field_level(tag: &str, op: fn(&ClusterDeckPaths) -> Result<(), String>) {
    let paths = seeded(tag, FIXTURE);
    let before = snapshot(&paths);
    op(&paths).unwrap();
    let after = snapshot(&paths);
    assert_untouched_except(&before, &after, &["other"]);
    let (b, a) = (entry(&before, "other"), entry(&after, "other"));
    assert_eq!(a["hosts"], b["hosts"], "hosts untouched by a CA change");
    assert_eq!(a["local_runtime"], b["local_runtime"]);
    let cas = |v: &Value| v["trusted_cas"].as_sequence().unwrap().clone();
    let kept = cas(&b)
        .into_iter()
        .find(|c| c["secret_ref"] == "ns/b")
        .unwrap();
    assert!(
        cas(&a).contains(&kept),
        "unrelated CA record (with its unknown field) survives"
    );
}

#[test]
fn regress_ca_trust_is_field_level() {
    assert_ca_op_is_field_level("r-ca-trust", op_ca_trust);
}

#[test]
fn regress_ca_replace_is_field_level() {
    assert_ca_op_is_field_level("r-ca-replace", op_ca_replace);
}

#[test]
fn regress_ca_cleanup_is_field_level() {
    assert_ca_op_is_field_level("r-ca-cleanup", op_ca_cleanup);
}

#[test]
fn regress_ca_remove_is_field_level() {
    assert_ca_op_is_field_level("r-ca-remove", op_ca_remove);
}

#[test]
fn regress_runtime_refresh_is_field_level() {
    let paths = seeded("r-runtime", FIXTURE);
    let before = snapshot(&paths);
    op_runtime_refresh(&paths).unwrap();
    let after = snapshot(&paths);
    assert_untouched_except(&before, &after, &["other"]);
    let host = &entry(&after, "other")["hosts"][0];
    assert_eq!(host["port"], Value::from(2222));
    assert_eq!(host["future_host_key"], Value::from("keep-other-host"));
    assert_eq!(
        entry(&after, "other")["trusted_cas"],
        entry(&before, "other")["trusted_cas"]
    );
}

#[test]
fn regress_create_from_environment_keeps_invalid_and_unknown_entries() {
    let paths = seeded("r-create", FIXTURE);
    let before = snapshot(&paths);
    op_create_from_environment(&paths).unwrap();
    let after = snapshot(&paths);
    assert!(after["profiles"].get("env-new").is_some());
    assert_untouched_except(&before, &after, &[]);
}

#[test]
fn regress_migration_backs_up_a_mixed_file() {
    // One entry already carries host_mapping, one is still legacy: the legacy one needs the
    // backup before any rewrite (the old "any entry has the key -> skip" rule lost it).
    let legacy = "profiles:\n  a:\n    name: A\n    hosts: []\n    manage_hosts_file: true\n  b:\n    name: B\n    hosts: []\n    host_mapping: off\n";
    let paths = seeded("r-mixed-backup", legacy);
    upsert_profile(&paths, get_profile(&paths, "b").unwrap()).unwrap();
    let backup = paths
        .profiles_file()
        .with_file_name("profiles.yaml.pre-local-dns.bak");
    assert_eq!(std::fs::read_to_string(backup).unwrap(), legacy);
}
