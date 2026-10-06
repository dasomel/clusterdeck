use std::path::Path;

use crate::services::inventory::details::{self, parse_vbox, parse_vbox_ips, parse_vbox_list};
use crate::services::inventory::model::Machine;
use crate::services::inventory::run_command;
use crate::services::process::CommandRunner;

pub(super) async fn discover(runner: &dyn CommandRunner) -> Result<Vec<Machine>, String> {
    let list_out = run_command(runner, "VBoxManage", &["list", "vms"]).await?;
    let rows = parse_vbox_list(&list_out);
    let mut result = vec![];
    for (name, uuid) in rows {
        let info = run_command(
            runner,
            "VBoxManage",
            &["showvminfo", &uuid, "--machinereadable"],
        )
        .await?;
        let mut m = parse_vbox(&info, &name, &uuid);
        let (cfg, disks) = details::vbox_cfg_and_disks(&info);
        let cfg_opt = cfg.clone();
        let disks_clone = disks.clone();
        let (created, disk_usage) = tokio::task::spawn_blocking(move || {
            let created = cfg_opt
                .as_deref()
                .and_then(|c| details::created_ms(Path::new(c)));
            let used: f64 = disks_clone
                .iter()
                .map(|d| details::allocated_gib(Path::new(d)).unwrap_or(0.0))
                .sum();
            (created, used)
        })
        .await
        .map_err(|e| e.to_string())?;
        m.created_at = created;
        let (mut capacity, mut used) = (0.0, disk_usage);
        for disk in &disks {
            let (cap, on_disk) = run_command(runner, "VBoxManage", &["showmediuminfo", disk])
                .await
                .map(|t| details::parse_vbox_medium(&t))
                .unwrap_or_default();
            capacity += cap.unwrap_or(0.0);
            if let Some(od) = on_disk {
                used = used.max(od);
            }
        }
        if capacity > 0.0 {
            m.disk_gib = Some(capacity);
            m.disk_used_gib = Some(used);
        }
        if m.state == "running" {
            if let Ok(props) =
                run_command(runner, "VBoxManage", &["guestproperty", "enumerate", &uuid]).await
            {
                m.ips = parse_vbox_ips(&props);
            }
        }
        result.push(m);
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn virtualbox_names_with_spaces() {
        assert_eq!(
            parse_vbox_list("\"My Lab\" {abc}"),
            vec![("My Lab".into(), "abc".into())]
        );
        assert_eq!(
            parse_vbox("memory=8192\nVMState=\"running\"", "VM", "abc").memory_gib,
            Some(8.0)
        );
    }

    #[test]
    fn guest_ips_filter_unusable_addresses() {
        let text =
            "Name: /VirtualBox/GuestInfo/Net/0/V4/IP, value: 10.0.2.15, timestamp: 1, flags:\n\
            Name: /VirtualBox/GuestInfo/Net/1/V4/IP, value: 192.168.56.11, timestamp: 1, flags:\n\
            Name: /VirtualBox/GuestInfo/Net/2/V4/IP, value: 169.254.1.2, timestamp: 1, flags:\n\
            Name: /VirtualBox/GuestInfo/Net/1/V6/IP, value: fe80::1, timestamp: 1, flags:";
        assert_eq!(parse_vbox_ips(text), vec!["10.0.2.15", "192.168.56.11"]);
    }
}
