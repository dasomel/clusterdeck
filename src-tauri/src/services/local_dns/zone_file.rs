#![allow(dead_code)]

use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::net::Ipv4Addr;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::services::validate::{
    is_allowed_private_zone, is_safe_host_domain, is_safe_ip_address, is_safe_profile_id,
    profile_dns_label,
};

pub const ZONES_FILE_VERSION: u32 = 1;
pub const DEFAULT_LOCAL_DNS_PORT: u16 = 15353;

static TMP_SEQ: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ZonesFile {
    pub version: u32,
    #[serde(default)]
    pub generation: u64,
    pub port: u16,
    pub zones: Vec<ZoneEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ZoneEntry {
    pub zone: String,
    pub profile_id: String,
    pub records: Vec<RecordEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecordEntry {
    pub name: String,
    pub ipv4: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StatusFile {
    pub generation: u64,
    pub loaded_at: String,
    pub error: Option<String>,
}

/// Validated, immutable snapshot of authoritative zones and A records served by the responder.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ZoneTable {
    pub generation: u64,
    pub port: u16,
    zones: BTreeSet<String>,
    records: BTreeMap<String, Vec<Ipv4Addr>>,
}

impl ZoneTable {
    pub fn from_zones_file(file: &ZonesFile) -> Result<Self, String> {
        if file.version != ZONES_FILE_VERSION {
            return Err(format!(
                "unsupported zones.json version: {} (expected {ZONES_FILE_VERSION})",
                file.version
            ));
        }

        let mut zones = BTreeSet::new();
        let mut records: BTreeMap<String, Vec<Ipv4Addr>> = BTreeMap::new();
        // R5: distinct profile_ids must not collapse onto the same DNS label.
        let mut label_owners: BTreeMap<String, String> = BTreeMap::new();

        for entry in &file.zones {
            if !is_allowed_private_zone(&entry.zone) {
                return Err(format!(
                    "invalid or disallowed private zone: {}",
                    entry.zone
                ));
            }
            if !zones.insert(entry.zone.clone()) {
                return Err(format!("duplicate zone: {}", entry.zone));
            }
            if !is_safe_profile_id(&entry.profile_id) {
                return Err(format!(
                    "invalid profile_id '{}' for zone '{}'",
                    entry.profile_id, entry.zone
                ));
            }
            let dns_label = profile_dns_label(&entry.profile_id).ok_or_else(|| {
                format!(
                    "profile_id '{}' does not produce a valid DNS label",
                    entry.profile_id
                )
            })?;
            if let Some(existing_owner) = label_owners.get(&dns_label) {
                if existing_owner != &entry.profile_id {
                    return Err(format!(
                        "profiles '{}' and '{}' map to the same DNS label '{}'",
                        existing_owner, entry.profile_id, dns_label
                    ));
                }
            } else {
                label_owners.insert(dns_label, entry.profile_id.clone());
            }

            for rec in &entry.records {
                if !is_safe_host_domain(&rec.name) {
                    return Err(format!(
                        "invalid record domain '{}' in zone '{}'",
                        rec.name, entry.zone
                    ));
                }
                let fqdn = rec.name.to_ascii_lowercase();
                if !is_name_in_zone(&fqdn, &entry.zone) {
                    return Err(format!(
                        "record '{}' is outside zone '{}'",
                        rec.name, entry.zone
                    ));
                }
                let ipv4 = validate_record_ipv4(&rec.name, &rec.ipv4)?;

                let addrs = records.entry(fqdn).or_default();
                if !addrs.contains(&ipv4) {
                    addrs.push(ipv4);
                }
            }
        }

        Ok(Self {
            generation: file.generation,
            port: file.port,
            zones,
            records,
        })
    }

    pub fn is_in_zone(&self, lower_qname: &str) -> bool {
        self.zones
            .iter()
            .any(|zone| is_name_in_zone(lower_qname, zone))
    }

    pub fn lookup_ipv4(&self, lower_qname: &str) -> Option<&[Ipv4Addr]> {
        self.records.get(lower_qname).map(Vec::as_slice)
    }

    pub fn zones(&self) -> &BTreeSet<String> {
        &self.zones
    }
}

/// The single rule for a record's address, shared with `zones_builder` so the builder never
/// emits a record that would make `load_and_validate` reject the whole file (R5): general IP
/// safety first, then IPv4-only, and never the unspecified address.
pub fn validate_record_ipv4(record_name: &str, ip: &str) -> Result<Ipv4Addr, String> {
    if !is_safe_ip_address(ip) {
        return Err(format!(
            "invalid IP address '{ip}' for record '{record_name}'"
        ));
    }
    let ipv4: Ipv4Addr = ip
        .parse()
        .map_err(|_| format!("IPv6 address '{ip}' is not supported for record '{record_name}'"))?;
    if ipv4.is_unspecified() {
        return Err(format!(
            "unspecified IPv4 address '{ip}' is not allowed for record '{record_name}'"
        ));
    }
    Ok(ipv4)
}

/// Returns true when `name` is equal to `zone` or is a subdomain under `zone` (both lowercase,
/// without trailing dots).
pub fn is_name_in_zone(name: &str, zone: &str) -> bool {
    name == zone
        || (name.len() > zone.len()
            && name.ends_with(zone)
            && name.as_bytes()[name.len() - zone.len() - 1] == b'.')
}

pub fn load_and_validate(path: impl AsRef<Path>) -> Result<ZoneTable, String> {
    let path = path.as_ref();
    let content = std::fs::read_to_string(path)
        .map_err(|e| format!("failed to read {}: {e}", path.display()))?;
    let parsed: ZonesFile = serde_json::from_str(&content)
        .map_err(|e| format!("failed to parse {}: {e}", path.display()))?;
    ZoneTable::from_zones_file(&parsed)
}

pub fn write_zones(path: impl AsRef<Path>, zones: &ZonesFile) -> Result<(), String> {
    atomic_write_json(path.as_ref(), zones, "zones.json")
}

pub fn write_status(path: impl AsRef<Path>, status: StatusFile) -> Result<(), String> {
    atomic_write_json(path.as_ref(), &status, "status.json")
}

pub fn load_status(path: impl AsRef<Path>) -> Result<StatusFile, String> {
    let path = path.as_ref();
    let content = std::fs::read_to_string(path)
        .map_err(|e| format!("failed to read {}: {e}", path.display()))?;
    serde_json::from_str(&content).map_err(|e| format!("failed to parse {}: {e}", path.display()))
}

fn atomic_write_json<T: Serialize>(path: &Path, value: &T, label: &str) -> Result<(), String> {
    let parent = match path.parent() {
        Some(p) if !p.as_os_str().is_empty() => p,
        _ => Path::new("."),
    };
    std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;

    let json = serde_json::to_string(value).map_err(|e| e.to_string())?;
    let seq = TMP_SEQ.fetch_add(1, Ordering::Relaxed);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let file_name = path.file_name().and_then(|n| n.to_str()).unwrap_or(label);
    let tmp_path = parent.join(format!(
        ".{file_name}.tmp.{}.{nanos}.{seq}",
        std::process::id()
    ));

    if let Err(err) = std::fs::write(&tmp_path, json.as_bytes()) {
        let _ = std::fs::remove_file(&tmp_path);
        return Err(err.to_string());
    }
    if let Err(err) = std::fs::rename(&tmp_path, path) {
        let _ = std::fs::remove_file(&tmp_path);
        return Err(err.to_string());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(tag: &str) -> std::path::PathBuf {
        let seq = TMP_SEQ.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "clusterdeck-zone-file-test-{tag}-{}-{seq}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn valid_zones_file() -> ZonesFile {
        ZonesFile {
            version: 1,
            generation: 7,
            port: DEFAULT_LOCAL_DNS_PORT,
            zones: vec![ZoneEntry {
                zone: "lab.example.internal".into(),
                profile_id: "cka-lab".into(),
                records: vec![
                    RecordEntry {
                        name: "api.lab.example.internal".into(),
                        ipv4: "192.0.2.10".into(),
                    },
                    RecordEntry {
                        name: "lab.example.internal".into(),
                        ipv4: "192.0.2.11".into(),
                    },
                ],
            }],
        }
    }

    #[test]
    fn load_and_validate_accepts_valid_file_and_roundtrips_exact_json_shape() {
        let dir = temp_dir("valid");
        let path = dir.join("zones.json");
        let raw = r#"{"version":1,"generation":4,"port":15353,"zones":[{"zone":"lab.example.internal","profile_id":"cka-lab","records":[{"name":"api.lab.example.internal","ipv4":"192.0.2.10"}]}]}"#;
        std::fs::write(&path, raw).unwrap();

        let table = load_and_validate(&path).unwrap();
        assert_eq!(table.generation, 4);
        assert_eq!(table.port, 15353);
        assert!(table.is_in_zone("lab.example.internal"));
        assert!(table.is_in_zone("api.lab.example.internal"));
        assert!(!table.is_in_zone("other.example.internal"));
        assert_eq!(
            table.lookup_ipv4("api.lab.example.internal"),
            Some(&[Ipv4Addr::new(192, 0, 2, 10)][..])
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn load_and_validate_rejects_bad_version() {
        let dir = temp_dir("bad-version");
        let path = dir.join("zones.json");
        let mut file = valid_zones_file();
        file.version = 2;
        write_zones(&path, &file).unwrap();

        let err = load_and_validate(&path).unwrap_err();
        assert!(err.contains("version"), "unexpected error: {err}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn load_and_validate_rejects_non_private_or_short_zone() {
        let dir = temp_dir("non-private-zone");
        let path = dir.join("zones.json");

        for bad_zone in [
            "apps.github.com",
            "example.internal",
            "internal",
            "Upper.example.internal",
        ] {
            let mut file = valid_zones_file();
            file.zones[0].zone = bad_zone.into();
            file.zones[0].records[0].name = format!("api.{bad_zone}");
            file.zones[0].records.truncate(1);
            write_zones(&path, &file).unwrap();

            let err = load_and_validate(&path).unwrap_err();
            assert!(
                err.contains("zone"),
                "expected zone rejection for {bad_zone}, got: {err}"
            );
        }

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn load_and_validate_rejects_record_outside_its_zone() {
        let dir = temp_dir("outside-zone");
        let path = dir.join("zones.json");

        for bad_name in [
            "api.other.example.internal",
            "notlab.example.internal",
            "example.internal",
        ] {
            let mut file = valid_zones_file();
            file.zones[0].records[0].name = bad_name.into();
            write_zones(&path, &file).unwrap();

            let err = load_and_validate(&path).unwrap_err();
            assert!(
                err.contains("outside zone"),
                "expected outside-zone rejection for {bad_name}, got: {err}"
            );
        }

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn load_and_validate_rejects_duplicate_zone() {
        let dir = temp_dir("dup-zone");
        let path = dir.join("zones.json");
        let mut file = valid_zones_file();
        file.zones.push(ZoneEntry {
            zone: "lab.example.internal".into(),
            profile_id: "other-lab".into(),
            records: vec![],
        });
        write_zones(&path, &file).unwrap();

        let err = load_and_validate(&path).unwrap_err();
        assert!(err.contains("duplicate zone"), "unexpected error: {err}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn load_and_validate_rejects_ipv6_and_invalid_ip() {
        let dir = temp_dir("ipv6");
        let path = dir.join("zones.json");

        let mut ipv6_file = valid_zones_file();
        ipv6_file.zones[0].records[0].ipv4 = "2001:db8::10".into();
        write_zones(&path, &ipv6_file).unwrap();
        let err = load_and_validate(&path).unwrap_err();
        assert!(err.contains("IPv6"), "unexpected error: {err}");

        let mut bad_ip_file = valid_zones_file();
        bad_ip_file.zones[0].records[0].ipv4 = "not-an-ip".into();
        write_zones(&path, &bad_ip_file).unwrap();
        let err = load_and_validate(&path).unwrap_err();
        assert!(err.contains("invalid IP"), "unexpected error: {err}");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn load_and_validate_rejects_invalid_profile_id_and_dns_label_collision() {
        let dir = temp_dir("profile-id");
        let path = dir.join("zones.json");

        let mut bad_profile = valid_zones_file();
        bad_profile.zones[0].profile_id = "../evil".into();
        write_zones(&path, &bad_profile).unwrap();
        assert!(load_and_validate(&path).is_err());

        let mut collision = valid_zones_file();
        collision.zones[0].profile_id = "cka_lab".into();
        collision.zones.push(ZoneEntry {
            zone: "second.example.internal".into(),
            profile_id: "cka-lab".into(),
            records: vec![],
        });
        write_zones(&path, &collision).unwrap();
        let err = load_and_validate(&path).unwrap_err();
        assert!(err.contains("same DNS label"), "unexpected error: {err}");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn write_status_is_atomic_and_leaves_no_tmp_files() {
        let dir = temp_dir("status-atomic");
        let status_path = dir.join("status.json");

        let status = StatusFile {
            generation: 3,
            loaded_at: "2026-10-16T00:00:00Z".into(),
            error: None,
        };
        write_status(&status_path, status.clone()).unwrap();

        let raw = std::fs::read_to_string(&status_path).unwrap();
        assert!(
            raw.contains("\"error\":null"),
            "expected error:null in {raw}"
        );
        let loaded = load_status(&status_path).unwrap();
        assert_eq!(loaded, status);

        let leftovers: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(Result::ok)
            .filter(|e| e.file_name().to_string_lossy().contains(".tmp."))
            .collect();
        assert!(leftovers.is_empty(), "leftover tmp files: {leftovers:?}");

        let _ = std::fs::remove_dir_all(&dir);
    }
}
