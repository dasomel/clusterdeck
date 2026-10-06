#![allow(dead_code)]

// D5: static facts (disk capacity/usage, creation time) come from files and `showmediuminfo`, so they work
// for stopped machines. Everything is best-effort: a missing file leaves the field `None`.
use crate::services::inventory::model::Machine;
use std::{fs, os::unix::fs::MetadataExt, path::Path, time::UNIX_EPOCH};

mod vbox;
mod vmx;

pub use vbox::{
    parse_vbox, parse_vbox_ips, parse_vbox_list, parse_vbox_medium, vbox_cfg_and_disks,
};
pub use vmx::{apply_vmx, parse_vmrun_ip, parse_vmx};

const GIB: f64 = 1073741824.0;

pub fn created_ms(path: &Path) -> Option<u64> {
    let meta = fs::metadata(path).ok()?;
    let time = meta.created().or_else(|_| meta.modified()).ok()?;
    Some(time.duration_since(UNIX_EPOCH).ok()?.as_millis() as u64)
}

/// Bytes actually allocated on the host (sparse files count only written blocks).
pub fn allocated_gib(path: &Path) -> Option<f64> {
    Some(fs::metadata(path).ok()?.blocks() as f64 * 512.0 / GIB)
}

pub fn parse_assignments(text: &str) -> std::collections::HashMap<String, String> {
    text.lines()
        .filter_map(|line| {
            line.split_once('=')
                .map(|(k, v)| (k.trim().into(), v.trim().trim_matches('"').into()))
        })
        .collect()
}

pub fn number(values: &std::collections::HashMap<String, String>, key: &str) -> Option<f64> {
    values
        .get(key)?
        .parse::<f64>()
        .ok()
        .filter(|n| n.is_finite() && *n >= 0.0)
}

pub(super) fn usable_ip(text: &str) -> Option<String> {
    let ip: std::net::IpAddr = text.trim().parse().ok()?;
    let link_local = matches!(ip, std::net::IpAddr::V4(v4) if v4.is_link_local());
    (!ip.is_loopback() && !ip.is_unspecified() && !link_local).then(|| ip.to_string())
}

pub fn colima_details(m: &mut Machine, name: &str) {
    if !crate::services::validate::is_safe_path_segment(name) {
        return;
    }
    let home = std::env::var("COLIMA_HOME")
        .ok()
        .or_else(|| std::env::var("HOME").ok().map(|h| format!("{h}/.colima")));
    let Some(home) = home else { return };
    let profile = if name == "default" {
        "colima".into()
    } else {
        format!("colima-{name}")
    };
    let lima = Path::new(&home).join("_lima");
    m.created_at = created_ms(&lima.join(&profile));
    // The configured `disk` is Colima's data disk, stored apart from the VM's root image.
    m.disk_used_gib = allocated_gib(&lima.join("_disks").join(&profile).join("datadisk"));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn colima_details_rejects_unsafe_instance_name() {
        let mut m = Machine::new("colima", "x", "x");
        colima_details(&mut m, "../evil");
        assert!(m.created_at.is_none() && m.disk_used_gib.is_none());
    }
}
