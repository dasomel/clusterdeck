use crate::services::inventory::details;
use crate::services::inventory::model::Machine;
use crate::services::inventory::run_command;
use crate::services::process::CommandRunner;

// D3: parse_colima reuses the shared Colima NDJSON/array parsing from local_runtime,
// mapping fields into inventory's Machine type.
pub fn parse_colima(text: &str) -> Result<Vec<Machine>, String> {
    let rows = crate::services::local_runtime::parse_colima_json_rows(text)?;
    rows.into_iter()
        .map(|row| {
            let name = row.name;
            let mut m = Machine::new("colima", &name, &name);
            m.environment = format!("Colima / {name}");
            m.state = row
                .status
                .unwrap_or_else(|| "unknown".to_string())
                .to_lowercase();
            m.cpu = row.cpus.map(|c| c as f64);
            m.memory_gib = row.memory.map(|bytes| bytes as f64 / 1073741824.0);
            m.disk_gib = row.disk.map(|bytes| bytes as f64 / 1073741824.0);
            if let Some(address) = row.address.filter(|s| !s.is_empty()) {
                m.ips.push(address);
            }
            Ok(m)
        })
        .collect()
}

pub(super) async fn discover(runner: &dyn CommandRunner) -> Result<Vec<Machine>, String> {
    let output = run_command(runner, "colima", &["list", "--json"]).await?;
    let mut machines = parse_colima(&output)?;
    for m in &mut machines {
        let name = m.name.clone();
        let mut m_clone = m.clone();
        *m = tokio::task::spawn_blocking(move || {
            details::colima_details(&mut m_clone, &name);
            m_clone
        })
        .await
        .map_err(|e| e.to_string())?;
    }
    Ok(machines)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn colima_bytes_and_invalid_output() {
        let machines =
            parse_colima(r#"{"name":"default","status":"Running","cpus":4,"memory":8589934592}"#)
                .unwrap();
        assert_eq!(machines[0].memory_gib, Some(8.0));
        assert_eq!(machines[0].state, "running");
        assert!(parse_colima("garbage").is_err());
    }
}
