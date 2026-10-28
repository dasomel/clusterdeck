//! Lossless model of `profiles.yaml` (#59).
//!
//! The file is parsed once into a raw YAML `Value`. Each entry is classified as `Valid`
//! (deserializes and passes `validate_profile`) or `Invalid` (reason attached), but its raw
//! `Value` is never discarded: every write re-reads the file, changes only the entries the
//! mutation names, and writes everything else back as the same raw `Value`. Entries this
//! version cannot understand and keys it does not know therefore survive any save.
//!
//! Limitations (documented in docs/LOCAL-DNS.md): comments and formatting are not preserved
//! (that needs a byte-preserving editor, tracked in #97); and a writer outside this process
//! that lands between the final compare and the rename is still lost (macOS offers no
//! cross-process lock for this file).

use std::path::Path;

use serde::{Deserialize, Serialize};
use serde_yaml::{Mapping, Value};

use crate::services::config::{
    Bastion, BootstrapPolicy, Host, HostMapping, KubeconfigSource, LocalRuntimeSource, Profile,
};
use crate::services::paths::ClusterDeckPaths;
use crate::services::resolver_files::sha256_hex;

/// Version prefix of an entry fingerprint. A fingerprint with another prefix is stale by
/// definition, so a UI built against a different scheme reloads instead of overwriting.
pub const FINGERPRINT_PREFIX: &str = "v1:";

pub const CHANGED_ELSEWHERE: &str = "this profile changed elsewhere — reload and try again";
pub const CHANGED_WHILE_SAVING: &str = "profiles.yaml changed while saving — retry";
pub const LIST_SHAPE_MSG: &str = "profiles.yaml uses a list layout that this version reads but never rewrites; convert `profiles:` to a map keyed by profile id (or remove the entries) before editing in ClusterDeck";
const BACKUP_NAME: &str = "profiles.yaml.pre-local-dns.bak";

/// Top-level keys of a profile body known to this version. MUST equal the serde field names
/// of `ProfileBody` (guarded by a test): an editor save removes exactly these from the old raw
/// map and inserts the new ones, so everything else (unknown, newer-version keys) survives.
pub const KNOWN_KEYS: [&str; 11] = [
    "name",
    "hosts",
    "bastion",
    "bootstrap",
    "kubeconfig",
    "manage_hosts_file",
    "host_mapping",
    "approved_dns_zones",
    "trusted_cas",
    "gateway_address",
    "local_runtime",
];

/// On-disk shape of one profile (the map key is the id).
#[derive(Serialize, Deserialize)]
pub(crate) struct ProfileBody {
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

impl ProfileBody {
    fn into_profile(self, id: &str) -> Profile {
        Profile {
            id: id.to_string(),
            name: self.name,
            hosts: self.hosts,
            bastion: self.bastion,
            bootstrap: self.bootstrap,
            kubeconfig: self.kubeconfig,
            host_mapping: HostMapping::migrate(self.host_mapping, self.manage_hosts_file),
            approved_dns_zones: self.approved_dns_zones,
            trusted_cas: self.trusted_cas,
            gateway_address: self.gateway_address,
            local_runtime: self.local_runtime,
        }
    }

    fn from_profile(p: &Profile) -> Self {
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
        }
    }
}

#[derive(Debug, Clone)]
pub enum EntryState {
    Valid(Box<Profile>),
    Invalid { reason: String },
}

#[derive(Debug, Clone)]
pub struct DocEntry {
    /// The YAML map key (list index in a list-shaped file). Identity of the entry.
    pub key: String,
    pub raw: Value,
    pub state: EntryState,
}

/// What the UI shows for an entry that cannot be loaded. `key` is an opaque handle: it is
/// displayed and echoed back, never used as a path, argv or hosts value.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct InvalidProfileView {
    pub key: String,
    pub reason: String,
    pub fingerprint: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Shape {
    Map,
    /// `profiles:` is a sequence: loaded read-only, every write refuses.
    List,
}

/// One profile's approved zones as the ownership check sees them. Invalid entries contribute
/// their raw zones (best effort) so a valid profile can never claim a zone an unreadable entry
/// may still own.
#[derive(Debug, Clone)]
pub struct ZoneClaim {
    pub id: String,
    pub zones: Vec<String>,
    pub valid: bool,
}

#[derive(Debug, Clone)]
pub struct ProfileDocument {
    root: Value,
    pub shape: Shape,
}

pub fn key_string(k: &Value) -> String {
    match k {
        Value::String(s) => s.clone(),
        other => {
            let mut out = String::new();
            write_canonical(&to_json(other), &mut out);
            out
        }
    }
}

fn find_key(map: &Mapping, key: &str) -> Option<Value> {
    map.keys().find(|k| key_string(k) == key).cloned()
}

fn to_json(v: &Value) -> serde_json::Value {
    use serde_json::Value as J;
    match v {
        Value::Null => J::Null,
        Value::Bool(b) => J::Bool(*b),
        Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                J::from(i)
            } else if let Some(u) = n.as_u64() {
                J::from(u)
            } else {
                n.as_f64()
                    .and_then(serde_json::Number::from_f64)
                    .map_or_else(|| J::String(n.to_string()), J::Number)
            }
        }
        Value::String(s) => J::String(s.clone()),
        Value::Sequence(s) => J::Array(s.iter().map(to_json).collect()),
        Value::Mapping(m) => {
            J::Object(m.iter().map(|(k, v)| (key_string(k), to_json(v))).collect())
        }
        Value::Tagged(t) => {
            let mut o = serde_json::Map::new();
            o.insert(t.tag.to_string(), to_json(&t.value));
            J::Object(o)
        }
    }
}

/// JSON with object keys sorted (independent of serde_json's `preserve_order` feature).
fn write_canonical(v: &serde_json::Value, out: &mut String) {
    use serde_json::Value as J;
    match v {
        J::Array(a) => {
            out.push('[');
            for (i, x) in a.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write_canonical(x, out);
            }
            out.push(']');
        }
        J::Object(o) => {
            let mut entries: Vec<_> = o.iter().collect();
            entries.sort_by(|a, b| a.0.cmp(b.0));
            out.push('{');
            for (i, (k, x)) in entries.into_iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                out.push_str(&serde_json::to_string(k).unwrap_or_default());
                out.push(':');
                write_canonical(x, out);
            }
            out.push('}');
        }
        scalar => out.push_str(&serde_json::to_string(scalar).unwrap_or_default()),
    }
}

/// `v1:` + SHA-256 of the canonical JSON of the raw entry.
pub fn fingerprint(raw: &Value) -> String {
    let mut canon = String::new();
    write_canonical(&to_json(raw), &mut canon);
    format!("{FINGERPRINT_PREFIX}{}", sha256_hex(&canon))
}

fn classify(key: &str, key_is_string: bool, raw: &Value) -> EntryState {
    let invalid = |reason: String| EntryState::Invalid { reason };
    if !key_is_string {
        return invalid("profile key is not a string".into());
    }
    let Some(map) = raw.as_mapping() else {
        return invalid("profile body is not a mapping".into());
    };
    match map.get("id") {
        None => {}
        Some(Value::String(id)) if id == key => {}
        Some(Value::String(id)) => {
            return invalid(format!(
                "id/key mismatch: body id '{id}' differs from map key '{key}'"
            ))
        }
        Some(_) => return invalid("body id is not a string".into()),
    }
    let body: ProfileBody = match serde_yaml::from_value(raw.clone()) {
        Ok(b) => b,
        Err(e) => return invalid(format!("cannot be read: {e}")),
    };
    let profile = body.into_profile(key);
    match crate::services::validate::validate_profile(&profile) {
        Ok(()) => EntryState::Valid(Box::new(profile)),
        Err(e) => invalid(e),
    }
}

fn classify_list_item(raw: &Value) -> EntryState {
    match serde_yaml::from_value::<Profile>(raw.clone()) {
        Ok(p) => match crate::services::validate::validate_profile(&p) {
            Ok(()) => EntryState::Valid(Box::new(p)),
            Err(e) => EntryState::Invalid { reason: e },
        },
        Err(e) => EntryState::Invalid {
            reason: format!("cannot be read: {e}"),
        },
    }
}

fn raw_zones(raw: &Value) -> Vec<String> {
    raw.get("approved_dns_zones")
        .and_then(Value::as_sequence)
        .map(|s| {
            s.iter()
                .filter_map(|z| z.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

fn read_optional(path: &Path) -> Result<Option<String>, String> {
    match std::fs::read_to_string(path) {
        Ok(s) => Ok(Some(s)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(format!("cannot read {}: {e}", path.display())),
    }
}

impl ProfileDocument {
    /// `None` = no file yet (an empty, writable document).
    pub fn parse(content: Option<&str>) -> Result<Self, String> {
        let empty = || {
            let mut m = Mapping::new();
            m.insert("profiles".into(), Value::Mapping(Mapping::new()));
            Value::Mapping(m)
        };
        let Some(text) = content else {
            return Ok(Self {
                root: empty(),
                shape: Shape::Map,
            });
        };
        let root: Value = serde_yaml::from_str(text).map_err(|e| {
            format!("profiles.yaml is not valid YAML ({e}); fix it by hand — ClusterDeck will not overwrite it")
        })?;
        match &root {
            Value::Null => Ok(Self {
                root: empty(),
                shape: Shape::Map,
            }),
            Value::Mapping(m) => {
                let shape = match m.get("profiles") {
                    None | Some(Value::Null) | Some(Value::Mapping(_)) => Shape::Map,
                    Some(Value::Sequence(_)) => Shape::List,
                    Some(_) => {
                        return Err("profiles.yaml: `profiles` must be a map keyed by profile id; ClusterDeck will not overwrite it".into())
                    }
                };
                Ok(Self { root, shape })
            }
            _ => Err("profiles.yaml: expected a top-level mapping with a `profiles` key; ClusterDeck will not overwrite it".into()),
        }
    }

    pub fn load(paths: &ClusterDeckPaths) -> Result<Self, String> {
        Self::parse(read_optional(&paths.profiles_file())?.as_deref())
    }

    fn profiles_map(&self) -> Option<&Mapping> {
        self.root.get("profiles").and_then(Value::as_mapping)
    }

    /// The `profiles` map, created when the file had none.
    fn profiles_mut(&mut self) -> &mut Mapping {
        let root = self.root.as_mapping_mut().expect("root is a mapping");
        let slot = root
            .entry("profiles".into())
            .or_insert_with(|| Value::Mapping(Mapping::new()));
        if !slot.is_mapping() {
            *slot = Value::Mapping(Mapping::new());
        }
        slot.as_mapping_mut().expect("just ensured a mapping")
    }

    pub fn entries(&self) -> Vec<DocEntry> {
        if self.shape == Shape::List {
            let seq = self
                .root
                .get("profiles")
                .and_then(Value::as_sequence)
                .cloned()
                .unwrap_or_default();
            return seq
                .into_iter()
                .enumerate()
                .map(|(i, raw)| DocEntry {
                    key: format!("[{i}]"),
                    state: classify_list_item(&raw),
                    raw,
                })
                .collect();
        }
        self.profiles_map()
            .map(|m| {
                m.iter()
                    .map(|(k, raw)| {
                        let key = key_string(k);
                        DocEntry {
                            state: classify(&key, k.is_string(), raw),
                            key,
                            raw: raw.clone(),
                        }
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    pub fn entry(&self, key: &str) -> Option<DocEntry> {
        self.entries().into_iter().find(|e| e.key == key)
    }

    pub fn profiles(&self) -> Vec<Profile> {
        self.entries()
            .into_iter()
            .filter_map(|e| match e.state {
                EntryState::Valid(p) => Some(*p),
                EntryState::Invalid { .. } => None,
            })
            .collect()
    }

    pub fn invalid(&self) -> Vec<InvalidProfileView> {
        self.entries()
            .into_iter()
            .filter_map(|e| match e.state {
                EntryState::Invalid { reason } => Some(InvalidProfileView {
                    fingerprint: fingerprint(&e.raw),
                    key: e.key,
                    reason,
                }),
                EntryState::Valid(_) => None,
            })
            .collect()
    }

    /// True when resolver files must only ever be added: some entry's zone ownership is
    /// unknown (invalid entry) or cannot be determined at all (list layout).
    pub fn protects_resolver(&self) -> bool {
        self.shape == Shape::List
            || self
                .entries()
                .iter()
                .any(|e| matches!(e.state, EntryState::Invalid { .. }))
    }

    pub fn zone_claims(&self) -> Vec<ZoneClaim> {
        self.entries()
            .into_iter()
            .map(|e| match e.state {
                EntryState::Valid(p) => ZoneClaim {
                    id: p.id.clone(),
                    zones: p.approved_dns_zones.clone(),
                    valid: true,
                },
                EntryState::Invalid { .. } => ZoneClaim {
                    zones: raw_zones(&e.raw),
                    id: e.key,
                    valid: false,
                },
            })
            .collect()
    }

    /// Any valid entry that still relies on the legacy `manage_hosts_file` bool.
    fn needs_legacy_conversion(&self) -> bool {
        self.entries().iter().any(|e| {
            matches!(e.state, EntryState::Valid(_))
                && e.raw
                    .as_mapping()
                    .is_some_and(|m| !m.contains_key("host_mapping"))
        })
    }

    pub fn raw(&self, key: &str) -> Option<Value> {
        let map = self.profiles_map()?;
        let k = find_key(map, key)?;
        map.get(&k).cloned()
    }

    pub fn set_raw(&mut self, key: &str, raw: Value) {
        let map = self.profiles_mut();
        match find_key(map, key) {
            Some(k) => {
                map.insert(k, raw);
            }
            None => {
                map.insert(Value::String(key.to_string()), raw);
            }
        }
    }

    pub fn remove(&mut self, key: &str) -> bool {
        let map = self.profiles_mut();
        match find_key(map, key) {
            Some(k) => map.shift_remove(&k).is_some(),
            None => false,
        }
    }

    /// Applies `f` to the raw body of the VALID entry `id`. Nothing is validated here; call
    /// `verify_entry` once every patch of the transaction is in.
    pub fn patch_entry(
        &mut self,
        id: &str,
        f: impl FnOnce(&mut Mapping) -> Result<(), String>,
    ) -> Result<(), String> {
        let entry = self
            .entry(id)
            .ok_or_else(|| format!("Profile not found: {id}"))?;
        if let EntryState::Invalid { reason } = &entry.state {
            return Err(format!("Invalid profile '{id}': {reason}"));
        }
        let mut map = entry
            .raw
            .as_mapping()
            .cloned()
            .ok_or_else(|| format!("Profile '{id}' is not a mapping"))?;
        f(&mut map)?;
        self.set_raw(id, Value::Mapping(map));
        Ok(())
    }

    /// The entry must still deserialize and validate, keep its id, and not claim a zone
    /// another entry owns (including entries this version cannot read).
    pub fn verify_entry(&self, id: &str) -> Result<(), String> {
        let entry = self
            .entry(id)
            .ok_or_else(|| format!("Profile not found: {id}"))?;
        match entry.state {
            EntryState::Invalid { reason } => {
                Err(format!("refusing to save profile '{id}': {reason}"))
            }
            EntryState::Valid(p) => {
                let claims = self.zone_claims();
                for zone in &p.approved_dns_zones {
                    check_zone_ownership(&claims, zone, &[id])?;
                }
                Ok(())
            }
        }
    }

    /// Editor save: removes every known key from the old raw body (except `keep`), then
    /// inserts the freshly serialized ones. Unknown keys survive.
    pub fn merge_profile(&mut self, profile: &Profile, keep: &[&str]) -> Result<(), String> {
        let new_body = match serde_yaml::to_value(ProfileBody::from_profile(profile))
            .map_err(|e| e.to_string())?
        {
            Value::Mapping(m) => m,
            _ => return Err("profile did not serialize to a mapping".into()),
        };
        let old = self.raw(&profile.id);
        let mut out = old
            .as_ref()
            .and_then(Value::as_mapping)
            .cloned()
            .unwrap_or_default();
        let had_old = old.is_some();
        for k in KNOWN_KEYS {
            if !(had_old && keep.contains(&k)) {
                out.shift_remove(k);
            }
        }
        for (k, v) in new_body {
            if had_old && k.as_str().is_some_and(|s| keep.contains(&s)) {
                continue;
            }
            out.insert(k, v);
        }
        self.set_raw(&profile.id, Value::Mapping(out));
        Ok(())
    }
}

/// Rejects `zone` when it is reserved or overlaps (equal, parent or child) a zone claimed by
/// any entry other than `allowed_owners`. Overlap across profiles would let a parent-zone owner
/// publish names under a child zone owned by someone else.
pub fn check_zone_ownership(
    claims: &[ZoneClaim],
    zone: &str,
    allowed_owners: &[&str],
) -> Result<(), String> {
    use crate::services::local_dns::zone_file::is_name_in_zone;
    if !crate::services::validate::is_allowed_private_zone(zone) {
        return Err(format!("invalid or disallowed DNS zone: {zone}"));
    }
    if crate::services::validate::is_reserved_clusterdeck_zone(zone) {
        return Err(format!(
            "DNS zone {zone} is reserved for ClusterDeck node aliases ({})",
            crate::services::validate::NODE_ALIAS_DOMAIN
        ));
    }
    for c in claims
        .iter()
        .filter(|c| !allowed_owners.contains(&c.id.as_str()))
    {
        if let Some(other) = c
            .zones
            .iter()
            .find(|o| is_name_in_zone(zone, o) || is_name_in_zone(o, zone))
        {
            let who = if c.valid {
                "profile"
            } else {
                "invalid profile"
            };
            return Err(format!(
                "DNS zone {zone} overlaps zone {other} approved for {who} '{}'; switch ownership explicitly",
                c.id
            ));
        }
    }
    Ok(())
}

/// Plain-profile -> raw body, as the editor writes it (dual-write of the legacy bool included).
#[allow(dead_code)]
pub fn serialize_profile(profile: &Profile) -> Result<Value, String> {
    serde_yaml::to_value(ProfileBody::from_profile(profile)).map_err(|e| e.to_string())
}

/// Re-reads the file, runs `f`, and writes the result atomically. Callers hold the profile
/// write mutex; the compare just before the rename also catches an unlocked or external
/// writer. `f` returns `Ok(false)` for "nothing to write".
pub(crate) fn mutate(
    paths: &ClusterDeckPaths,
    f: impl FnOnce(&mut ProfileDocument) -> Result<bool, String>,
) -> Result<(), String> {
    let file_path = paths.profiles_file();
    let parent = file_path
        .parent()
        .ok_or_else(|| "invalid profiles file path: missing parent directory".to_string())?;
    let original = read_optional(&file_path)?;
    let mut doc = ProfileDocument::parse(original.as_deref())?;
    if doc.shape == Shape::List {
        return Err(LIST_SHAPE_MSG.to_string());
    }
    let needs_backup = doc.needs_legacy_conversion();
    if !f(&mut doc)? {
        return Ok(());
    }
    let yaml = serde_yaml::to_string(&doc.root).map_err(|e| e.to_string())?;
    std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;

    // Plan R9: keep a one-time copy of a pre-local-dns file so a downgrade or a bad migration
    // is recoverable. A failed backup fails the save: never migrate without it.
    if needs_backup {
        if let Some(existing) = &original {
            ensure_backup(parent, existing, &file_path)?;
        }
    }

    // Atomic write: temp file in the same directory, then rename (atomic within a filesystem).
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let tmp_path = parent.join(format!(
        ".profiles.yaml.tmp.{}.{}",
        std::process::id(),
        nanos
    ));
    if let Err(err) = std::fs::write(&tmp_path, &yaml) {
        let _ = std::fs::remove_file(&tmp_path);
        return Err(err.to_string());
    }

    // Abort if anyone changed the file since we read it. A writer landing between this
    // compare and the rename is still lost (no cross-process lock on macOS).
    let unchanged = match read_optional(&file_path) {
        Ok(now) => now.as_deref().map(sha256_hex) == original.as_deref().map(sha256_hex),
        Err(_) => false,
    };
    if !unchanged {
        let _ = std::fs::remove_file(&tmp_path);
        return Err(CHANGED_WHILE_SAVING.to_string());
    }
    if let Err(err) = std::fs::rename(&tmp_path, &file_path) {
        let _ = std::fs::remove_file(&tmp_path);
        return Err(err.to_string());
    }
    Ok(())
}

/// Backup via temp file + exclusive link, so a partial backup can never be mistaken for a
/// complete one and an existing backup is never replaced.
fn ensure_backup(parent: &Path, original: &str, file_path: &Path) -> Result<(), String> {
    let backup = parent.join(BACKUP_NAME);
    if backup.exists() {
        return Ok(());
    }
    let fail = |e: std::io::Error| {
        format!(
            "could not back up {} to {} before migrating: {e}",
            file_path.display(),
            backup.display()
        )
    };
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let tmp = parent.join(format!(".{BACKUP_NAME}.tmp.{}.{nanos}", std::process::id()));
    std::fs::write(&tmp, original).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        fail(e)
    })?;
    let linked = std::fs::hard_link(&tmp, &backup);
    let _ = std::fs::remove_file(&tmp);
    match linked {
        Ok(()) => Ok(()),
        // Lost a race to a complete backup: fine. A dangling link in the way is not.
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists && backup.exists() => Ok(()),
        Err(e) => Err(fail(e)),
    }
}

/// Field-level edits for `store::patch_profile` closures: each touches only its own keys of
/// the raw body, so unrelated hosts, CA records and unknown fields stay as they were.
pub mod patch {
    use super::*;
    use crate::services::ca_trust::TrustedCa;

    fn seq_mut<'a>(m: &'a mut Mapping, key: &str) -> Result<&'a mut Vec<Value>, String> {
        let slot = m
            .entry(key.into())
            .or_insert_with(|| Value::Sequence(Vec::new()));
        if slot.is_null() {
            *slot = Value::Sequence(Vec::new());
        }
        slot.as_sequence_mut()
            .ok_or_else(|| format!("`{key}` is not a list"))
    }

    fn str_field<'a>(v: &'a Value, key: &str) -> Option<&'a str> {
        v.get(key).and_then(Value::as_str)
    }

    /// Replaces any record for the same `secret_ref`, then appends `ca`.
    pub fn set_trusted_ca(m: &mut Mapping, ca: &TrustedCa) -> Result<(), String> {
        let value = serde_yaml::to_value(ca).map_err(|e| e.to_string())?;
        let cas = seq_mut(m, "trusted_cas")?;
        cas.retain(|c| str_field(c, "secret_ref") != Some(ca.secret_ref.as_str()));
        cas.push(value);
        Ok(())
    }

    pub fn remove_trusted_ca(m: &mut Mapping, secret_ref: &str) -> Result<(), String> {
        seq_mut(m, "trusted_cas")?.retain(|c| str_field(c, "secret_ref") != Some(secret_ref));
        Ok(())
    }

    pub fn add_zone(m: &mut Mapping, zone: &str) -> Result<(), String> {
        let zones = seq_mut(m, "approved_dns_zones")?;
        if !zones.iter().any(|z| z.as_str() == Some(zone)) {
            zones.push(Value::String(zone.to_string()));
        }
        Ok(())
    }

    pub fn remove_zone(m: &mut Mapping, zone: &str) -> Result<(), String> {
        let zones = seq_mut(m, "approved_dns_zones")?;
        zones.retain(|z| z.as_str() != Some(zone));
        if zones.is_empty() {
            // Matches the serializer, which omits an empty list.
            m.shift_remove("approved_dns_zones");
        }
        Ok(())
    }

    /// Updates address/port/identity_file of the host at `index`. `expect_name` guards against
    /// the file having changed under us (index no longer names the same host).
    #[allow(dead_code)]
    pub fn set_host_endpoint(
        m: &mut Mapping,
        index: usize,
        expect_name: &str,
        address: &str,
        port: u16,
        identity_file: Option<&str>,
    ) -> Result<(), String> {
        let hosts = seq_mut(m, "hosts")?;
        let host = hosts
            .get_mut(index)
            .and_then(Value::as_mapping_mut)
            .filter(|h| h.get("name").and_then(Value::as_str) == Some(expect_name))
            .ok_or_else(|| CHANGED_ELSEWHERE.to_string())?;
        host.insert("address".into(), Value::String(address.to_string()));
        host.insert("port".into(), Value::from(port));
        match identity_file {
            Some(i) => {
                host.insert("identity_file".into(), Value::String(i.to_string()));
            }
            None if host.contains_key("identity_file") => {
                host.insert("identity_file".into(), Value::Null);
            }
            None => {}
        }
        Ok(())
    }

    /// Environment sync: update address/port of the host named like `host`, or append it.
    #[allow(dead_code)]
    pub fn upsert_host_endpoint(m: &mut Mapping, host: &Host) -> Result<(), String> {
        let hosts = seq_mut(m, "hosts")?;
        let existing = hosts
            .iter_mut()
            .filter_map(Value::as_mapping_mut)
            .find(|h| h.get("name").and_then(Value::as_str) == Some(host.name.as_str()));
        match existing {
            Some(h) => {
                h.insert("address".into(), Value::String(host.address.clone()));
                h.insert("port".into(), Value::from(host.port));
            }
            None => hosts.push(serde_yaml::to_value(host).map_err(|e| e.to_string())?),
        }
        Ok(())
    }

    #[allow(dead_code)]
    pub fn set_local_runtime_if_absent(
        m: &mut Mapping,
        source: &LocalRuntimeSource,
    ) -> Result<(), String> {
        if m.get("local_runtime").is_none_or(Value::is_null) {
            m.insert(
                "local_runtime".into(),
                serde_yaml::to_value(source).map_err(|e| e.to_string())?,
            );
        }
        Ok(())
    }
}
