use super::{number, parse_assignments, usable_ip};
use crate::services::inventory::model::Machine;

pub fn vbox_cfg_and_disks(text: &str) -> (Option<String>, Vec<String>) {
    let mut cfg = None;
    let mut disks = vec![];
    for line in text.lines() {
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let value = value.trim().trim_matches('"');
        if key == "CfgFile" {
            cfg = Some(value.to_owned());
        } else if key.starts_with('"')
            && key.matches('-').count() == 2
            && [".vdi", ".vmdk", ".vhd"]
                .iter()
                .any(|e| value.to_lowercase().ends_with(e))
        {
            disks.push(value.to_owned());
        }
    }
    (cfg, disks)
}

fn mbytes(line: &str) -> Option<f64> {
    line.split_whitespace()
        .next()?
        .parse::<f64>()
        .ok()
        .map(|mb| mb / 1024.0)
}

/// `VBoxManage showmediuminfo` → (capacity GiB, size on disk GiB)
pub fn parse_vbox_medium(text: &str) -> (Option<f64>, Option<f64>) {
    let (mut capacity, mut used) = (None, None);
    for line in text.lines() {
        if let Some(v) = line.strip_prefix("Capacity:") {
            capacity = mbytes(v.trim());
        }
        if let Some(v) = line.strip_prefix("Size on disk:") {
            used = mbytes(v.trim());
        }
    }
    (capacity, used)
}

pub fn parse_vbox_list(text: &str) -> Vec<(String, String)> {
    text.lines()
        .filter_map(|line| {
            let (name, uuid) = line.rsplit_once(" {")?;
            Some((
                name.trim_matches('"').into(),
                uuid.strip_suffix('}')?.into(),
            ))
        })
        .collect()
}

pub fn parse_vbox(text: &str, name: &str, uuid: &str) -> Machine {
    let values = parse_assignments(text);
    let mut m = Machine::new("virtualbox", uuid, name);
    m.state = values
        .get("VMState")
        .cloned()
        .unwrap_or_else(|| "unknown".into());
    m.cpu = number(&values, "cpus");
    m.memory_gib = number(&values, "memory").map(|mb| mb / 1024.0);
    m
}

pub fn parse_vbox_ips(text: &str) -> Vec<String> {
    let mut ips = vec![];
    for line in text.lines() {
        let Some(rest) = line.strip_prefix("Name: /VirtualBox/GuestInfo/Net/") else {
            continue;
        };
        let Some((name, value)) = rest.split_once(", value: ") else {
            continue;
        };
        if !name.ends_with("/V4/IP") {
            continue;
        }
        if let Some(ip) = value.split(',').next().and_then(usable_ip) {
            if !ips.contains(&ip) {
                ips.push(ip);
            }
        }
    }
    ips
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_virtualbox_disk_facts() {
        let (cfg, disks) = vbox_cfg_and_disks(
            "CfgFile=\"/vm/a.vbox\"\n\"SATA-0-0\"=\"/vm/a.vdi\"\n\"SATA-1-0\"=\"none\"\nname=\"x\"",
        );
        assert_eq!(
            (cfg.as_deref(), disks),
            (Some("/vm/a.vbox"), vec!["/vm/a.vdi".to_string()])
        );
        assert_eq!(
            parse_vbox_medium("Capacity:       65536 MBytes\nSize on disk:   2048 MBytes"),
            (Some(64.0), Some(2.0))
        );
    }
}
