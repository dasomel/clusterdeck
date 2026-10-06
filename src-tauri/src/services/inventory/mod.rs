#![allow(dead_code)]

pub mod apply;
pub mod details;
pub mod identity;
pub mod k8s;
pub mod model;
pub mod profile_sync;
pub mod providers;

use std::time::UNIX_EPOCH;

use serde::{Deserialize, Serialize};

use crate::services::process::{CommandLimits, CommandRunner};
pub use model::{Inventory, Machine, ProviderStatus};
pub use profile_sync::create_profile_from_environment;

#[derive(Debug, Serialize, Deserialize)]
pub struct CreateProfileOutcome {
    pub profile_id: String,
    pub created: bool,
    pub updated_hosts: Vec<String>,
}

pub(crate) async fn run_command(
    runner: &dyn CommandRunner,
    bin: &str,
    args: &[&str],
) -> Result<String, String> {
    let owned_args: Vec<String> = args.iter().map(|s| s.to_string()).collect();
    let out = runner
        .run_bounded(bin, &owned_args, CommandLimits::default())
        .await?;
    if out.success {
        Ok(out.stdout)
    } else {
        Err(if out.stderr.is_empty() {
            format!("{bin} exited with failure")
        } else {
            out.stderr
        })
    }
}

pub fn format_timestamp_ms(time: std::time::SystemTime) -> String {
    time.duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis().to_string())
        .unwrap_or_else(|_| "0".into())
}

pub fn host_platform() -> model::HostPlatform {
    model::HostPlatform {
        platform: std::env::consts::OS.into(),
        architecture: std::env::consts::ARCH.into(),
        cpu: std::thread::available_parallelism().ok().map(usize::from),
        memory_gib: None,
    }
}

/// macOS physical memory in GiB via `sysctl -n hw.memsize` (absolute path: the app's PATH is
/// minimal when GUI-launched). Other platforms and any failure give `None`, which disables the
/// over-allocation warning rather than guessing.
pub(crate) async fn detect_host_memory_gib(runner: &dyn CommandRunner) -> Option<f64> {
    if !cfg!(target_os = "macos") {
        return None;
    }
    let text = run_command(runner, "/usr/sbin/sysctl", &["-n", "hw.memsize"])
        .await
        .ok()?;
    text.trim()
        .parse::<f64>()
        .ok()
        .filter(|n| n.is_finite() && *n > 0.0)
        .map(|bytes| bytes / 1073741824.0)
}

pub const PROVIDERS: &[(&str, &str)] = &[
    ("colima", "Colima"),
    ("virtualbox", "VirtualBox"),
    ("vmware", "VMware Fusion (running VMs)"),
    ("vagrant", "Vagrant"),
];

pub fn demo_inventory() -> Inventory {
    let mut inv = Inventory::new(
        "demo",
        format_timestamp_ms(std::time::SystemTime::now()),
        host_platform(),
    );
    for p in PROVIDERS {
        inv.providers
            .push(ProviderStatus::new(p.0, p.1, "demo", None));
    }
    for n in 1..=3 {
        let mut m = Machine::new("vmware", &format!("narwhal-m{n}"), &format!("master-{n}"));
        m.orchestrator = Some("vagrant".into());
        m.environment = "/path/to/narwhal".into();
        m.state = "running".into();
        m.cpu = Some(2.0);
        m.memory_gib = Some(4.0);
        m.disk_gib = Some(40.0);
        m.disk_used_gib = Some(6.0);
        m.ips = vec![format!("192.168.56.{}", 10 + n)];
        m.created_at = Some(1704067200000);
        m.kubernetes = "configured (sample)".into();
        inv.machines.push(m);
    }
    inv.summarize();
    inv
}

pub async fn discover(runner: &dyn CommandRunner, demo: bool) -> Inventory {
    if demo {
        return demo_inventory();
    }
    let mut inv = Inventory::new(
        "live",
        format_timestamp_ms(std::time::SystemTime::now()),
        host_platform(),
    );
    inv.host.memory_gib = detect_host_memory_gib(runner).await;

    let mut verified: Vec<&str> = vec![];
    let mut all_machines = vec![];
    for (id, label) in PROVIDERS {
        match providers::discover(runner, id).await {
            Ok(machines) => {
                inv.providers
                    .push(ProviderStatus::new(id, label, "available", None));
                all_machines.extend(machines);
                verified.push(id);
            }
            Err(e) => {
                let status = if e.contains("not found") || e.contains("No such file") {
                    "not-installed"
                } else {
                    "error"
                };
                inv.providers
                    .push(ProviderStatus::new(id, label, status, Some(&e)));
            }
        }
    }

    inv.machines = model::reconcile(all_machines, &verified);
    if !demo {
        k8s::detect(&mut inv.machines).await;
    }
    inv.summarize();
    inv
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn demo_mode_matches_infradeck_sample() {
        let inv = demo_inventory();
        assert_eq!(inv.mode, "demo");
        assert_eq!(inv.machines.len(), 3);
        assert_eq!(inv.summary.running, 3);
        assert_eq!(inv.summary.cpu, 6.0);
        assert_eq!(inv.summary.memory_gib, 12.0);
        assert_eq!(inv.summary.disk_gib, 120.0);
        assert_eq!(inv.summary.warnings.len(), 0);
        for m in &inv.machines {
            assert_eq!(m.orchestrator.as_deref(), Some("vagrant"));
            assert_eq!(m.kubernetes, "configured (sample)");
        }
    }

    struct SysctlRunner(&'static str);

    #[async_trait::async_trait]
    impl CommandRunner for SysctlRunner {
        async fn run(
            &self,
            bin: &str,
            _args: &[String],
        ) -> Result<crate::services::process::CommandOutput, String> {
            if bin == "/usr/sbin/sysctl" {
                return Ok(crate::services::process::CommandOutput {
                    stdout: self.0.into(),
                    stderr: String::new(),
                    success: true,
                });
            }
            Err(format!("'{bin}' executable not found"))
        }
    }

    #[tokio::test]
    async fn host_limits_detected_and_over_allocation_warns() {
        assert!(host_platform().cpu.is_some_and(|c| c > 0));
        let runner = SysctlRunner("4294967296\n");
        if cfg!(target_os = "macos") {
            let host_gib = detect_host_memory_gib(&runner).await;
            assert_eq!(host_gib, Some(4.0));
            let mut vm = Machine::new("virtualbox", "u", "big");
            vm.state = "running".into();
            vm.memory_gib = Some(8.0);
            assert_eq!(model::summarize(&[vm], host_gib).warnings.len(), 1);
            let live = discover(&runner, false).await;
            assert_eq!(live.host.memory_gib, Some(4.0));
        } else {
            assert_eq!(detect_host_memory_gib(&runner).await, None);
        }
        assert_eq!(detect_host_memory_gib(&SysctlRunner("junk")).await, None);
    }

    #[test]
    fn reconcile_and_summary_counts() {
        let mut m1 = Machine::new("virtualbox", "uuid-1", "m1");
        m1.state = "running".into();
        m1.cpu = Some(2.0);
        m1.memory_gib = Some(4.0);
        m1.disk_gib = Some(40.0);

        let mut m2 = Machine::new("virtualbox", "uuid-1", "m1");
        m2.ips = vec!["10.0.2.15".into(), "192.168.56.10".into()];

        let machines = model::reconcile(vec![m1, m2], &[]);
        assert_eq!(machines.len(), 1);
        assert_eq!(machines[0].ips.len(), 2);
    }

    #[tokio::test]
    async fn r9_contract_pinned_json_keys() {
        let runner = crate::services::process::SystemRunner;
        let inventory = discover(&runner, true).await;
        let v = serde_json::to_value(&inventory).unwrap();
        let obj = v.as_object().unwrap();

        let mut top_keys: Vec<_> = obj.keys().map(|s| s.as_str()).collect();
        top_keys.sort();
        assert_eq!(
            top_keys,
            vec![
                "host",
                "machines",
                "mode",
                "providers",
                "summary",
                "timestamp"
            ]
        );

        let machine = &inventory.machines[0];
        let mv = serde_json::to_value(machine).unwrap();
        let mobj = mv.as_object().unwrap();
        let mut mkeys: Vec<_> = mobj.keys().map(|s| s.as_str()).collect();
        mkeys.sort();
        assert_eq!(
            mkeys,
            vec![
                "cpu",
                "created_at",
                "disk_gib",
                "disk_used_gib",
                "environment",
                "id",
                "ips",
                "kubernetes",
                "memory_gib",
                "name",
                "orchestrator",
                "runtime",
                "runtime_id",
                "state"
            ]
        );

        let host = &inventory.host;
        let hv = serde_json::to_value(host).unwrap();
        let hobj = hv.as_object().unwrap();
        let mut hkeys: Vec<_> = hobj.keys().map(|s| s.as_str()).collect();
        hkeys.sort();
        assert_eq!(hkeys, vec!["architecture", "cpu", "memory_gib", "platform"]);

        let summary = &inventory.summary;
        let sv = serde_json::to_value(summary).unwrap();
        let sobj = sv.as_object().unwrap();
        let mut skeys: Vec<_> = sobj.keys().map(|s| s.as_str()).collect();
        skeys.sort();
        assert_eq!(
            skeys,
            vec![
                "cpu",
                "disk_gib",
                "memory_gib",
                "running",
                "unknown_resources",
                "warnings"
            ]
        );
    }
}
