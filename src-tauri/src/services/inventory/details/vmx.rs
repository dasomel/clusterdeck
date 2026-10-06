use std::{
    fs,
    io::Read,
    path::{Path, PathBuf},
};

use super::{allocated_gib, created_ms, number, parse_assignments, usable_ip, GIB};
use crate::services::inventory::model::Machine;

pub fn parse_vmrun_ip(text: &str) -> Option<String> {
    usable_ip(text.lines().next().unwrap_or(""))
}

pub fn parse_vmx(text: &str, path: &str) -> Machine {
    let values = parse_assignments(text);
    let fallback = Path::new(path)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("VM");
    let mut m = Machine::new(
        "vmware",
        path,
        values
            .get("displayName")
            .map(String::as_str)
            .unwrap_or(fallback),
    );
    m.state = "running".into();
    m.cpu = number(&values, "numvcpus");
    m.memory_gib = number(&values, "memsize").map(|mb| mb / 1024.0);
    m
}

pub fn vmx_disk_files(text: &str) -> Vec<String> {
    text.lines()
        .filter_map(|line| {
            let (key, value) = line.split_once('=')?;
            let key = key.trim();
            let value = value.trim().trim_matches('"');
            let bus = ["scsi", "sata", "nvme", "ide"]
                .iter()
                .any(|b| key.starts_with(b));
            (bus && key.ends_with(".fileName") && value.ends_with(".vmdk"))
                .then(|| value.to_owned())
        })
        .collect()
}

/// Sum of `RW <sectors>` extents in a VMDK descriptor (text or embedded in a sparse header).
pub fn vmdk_capacity_gib(descriptor: &str) -> f64 {
    descriptor
        .lines()
        .filter_map(|l| {
            let mut parts = l.split_whitespace();
            matches!(parts.next(), Some("RW" | "RDONLY"))
                .then(|| parts.next()?.parse::<f64>().ok())
                .flatten()
        })
        .sum::<f64>()
        * 512.0
        / GIB
}

fn read_prefix(path: &Path) -> String {
    let mut buffer = vec![];
    if let Ok(file) = fs::File::open(path) {
        let _ = file.take(1 << 20).read_to_end(&mut buffer);
    }
    String::from_utf8_lossy(&buffer).into_owned()
}

pub fn is_safe_vmdk_path(path_str: &str) -> bool {
    if path_str.is_empty() || !path_str.ends_with(".vmdk") {
        return false;
    }
    if path_str
        .chars()
        .any(|c| c.is_control() || c == '\0' || c == '\r' || c == '\n')
    {
        return false;
    }
    let p = Path::new(path_str);
    !p.components()
        .any(|c| matches!(c, std::path::Component::ParentDir))
}

/// Disk capacity/usage and creation time from a .vmx and the VMDKs beside it.
pub fn apply_vmx(m: &mut Machine, vmx_text: &str, vmx_path: &Path) {
    let dir = vmx_path.parent().unwrap_or(Path::new("."));
    let (mut capacity, mut used) = (0.0, 0.0);
    for file in vmx_disk_files(vmx_text) {
        if !is_safe_vmdk_path(&file) {
            continue;
        }
        let disk = if Path::new(&file).is_absolute() {
            PathBuf::from(&file)
        } else {
            dir.join(&file)
        };
        capacity += vmdk_capacity_gib(&read_prefix(&disk));
        // Split disks live in sibling `<name>-s001.vmdk` extents; count every VMDK sharing the stem.
        let stem = Path::new(&file)
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or_default()
            .to_owned();
        let disk_dir = disk.parent().unwrap_or(dir);
        if let Ok(entries) = fs::read_dir(disk_dir) {
            used += entries
                .filter_map(Result::ok)
                .filter(|e| {
                    let n = e.file_name().to_string_lossy().into_owned();
                    n.starts_with(&stem) && n.ends_with(".vmdk")
                })
                .filter_map(|e| allocated_gib(&e.path()))
                .sum::<f64>();
        }
    }
    if capacity > 0.0 {
        m.disk_gib = Some(capacity);
        m.disk_used_gib = Some(used);
    }
    m.created_at = created_ms(vmx_path);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_vmware_disk_facts() {
        assert_eq!(
            vmx_disk_files("scsi0:0.fileName = \"d.vmdk\"\nide1:0.fileName = \"x.iso\""),
            vec!["d.vmdk"]
        );
        assert_eq!(
            vmdk_capacity_gib(
                "RW 41943040 SPARSE \"d-s001.vmdk\"\nRW 41943040 SPARSE \"d-s002.vmdk\""
            ),
            40.0
        );
    }

    #[test]
    fn vmdk_path_validation_accepts_relative_subdirs_and_absolute_paths_and_rejects_traversal() {
        // Safe relative subdir
        assert!(is_safe_vmdk_path("disks/root.vmdk"));
        // Safe relative nested subdir
        assert!(is_safe_vmdk_path("storage/sub/drive.vmdk"));
        // Safe absolute path
        assert!(is_safe_vmdk_path("/var/lib/vmware/root.vmdk"));
        // Safe simple file
        assert!(is_safe_vmdk_path("disk.vmdk"));

        // Reject traversal
        assert!(!is_safe_vmdk_path("../root.vmdk"));
        assert!(!is_safe_vmdk_path("disks/../../root.vmdk"));
        assert!(!is_safe_vmdk_path("/abs/../etc/root.vmdk"));
        // Reject non-vmdk
        assert!(!is_safe_vmdk_path("disks/root.iso"));
        assert!(!is_safe_vmdk_path("disks/root.vdi"));
        // Reject control chars
        assert!(!is_safe_vmdk_path("disks/root\0.vmdk"));
        assert!(!is_safe_vmdk_path("disks/root\n.vmdk"));
        // Reject empty
        assert!(!is_safe_vmdk_path(""));
    }
}
