use std::path::PathBuf;

use crate::services::inventory::details::{self, parse_vmrun_ip, parse_vmx};
use crate::services::inventory::model::Machine;
use crate::services::inventory::run_command;
use crate::services::process::CommandRunner;

pub(super) async fn discover(runner: &dyn CommandRunner) -> Result<Vec<Machine>, String> {
    let executable = if cfg!(target_os = "macos") {
        "/Applications/VMware Fusion.app/Contents/Library/vmrun"
    } else {
        "vmrun"
    };
    let listing = run_command(runner, executable, &["-T", "fusion", "list"]).await?;
    let mut result = vec![];
    for path in listing
        .lines()
        .map(str::trim)
        .filter(|p| p.ends_with(".vmx"))
    {
        let text = tokio::fs::read_to_string(path)
            .await
            .map_err(|e| format!("VMX read failed: {e}"))?;
        let mut m = parse_vmx(&text, path);
        let text_clone = text.clone();
        let path_buf = PathBuf::from(path);
        let mut m_clone = m.clone();
        m = tokio::task::spawn_blocking(move || {
            details::apply_vmx(&mut m_clone, &text_clone, &path_buf);
            m_clone.runtime_id = Some(
                std::fs::canonicalize(&path_buf)
                    .map(|p| p.to_string_lossy().into_owned())
                    .unwrap_or_else(|_| path_buf.to_string_lossy().into_owned()),
            );
            m_clone
        })
        .await
        .map_err(|e| e.to_string())?;
        if let Ok(out) = run_command(
            runner,
            executable,
            &["-T", "fusion", "getGuestIPAddress", path],
        )
        .await
        {
            m.ips.extend(parse_vmrun_ip(&out));
        }
        result.push(m);
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vmrun_ip_and_vmx_units() {
        assert_eq!(parse_vmrun_ip("192.168.1.5\n"), Some("192.168.1.5".into()));
        assert_eq!(parse_vmrun_ip("Error: VMware Tools are not running"), None);
        assert_eq!(parse_vmrun_ip("unknown"), None);
        assert_eq!(
            parse_vmx("memsize = \"8192\"", "/Lab.vmx").memory_gib,
            Some(8.0)
        );
    }
}
