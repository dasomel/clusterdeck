#![allow(dead_code)]

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Machine {
    pub id: String,
    pub runtime_id: Option<String>,
    pub name: String,
    pub runtime: String,
    pub orchestrator: Option<String>,
    pub environment: String,
    pub state: String,
    pub cpu: Option<f64>,
    pub memory_gib: Option<f64>,
    pub disk_gib: Option<f64>,
    pub disk_used_gib: Option<f64>,
    pub created_at: Option<u64>,
    pub ips: Vec<String>,
    pub kubernetes: String,
}

impl Machine {
    pub fn new(runtime: &str, id: &str, name: &str) -> Self {
        Self {
            id: format!("{runtime}:{id}"),
            runtime_id: Some(id.into()),
            name: name.into(),
            runtime: runtime.into(),
            orchestrator: None,
            environment: runtime.into(),
            state: "unknown".into(),
            cpu: None,
            memory_gib: None,
            disk_gib: None,
            disk_used_gib: None,
            created_at: None,
            ips: vec![],
            kubernetes: "unknown".into(),
        }
    }
}

// D7: Vagrant's global-status is a cache. Unmatched "running" entries are corrected against providers
// whose discovery succeeded (`verified`); entries whose .vagrant data is gone are marked "stale".
pub fn reconcile(observations: Vec<Machine>, verified: &[&str]) -> Vec<Machine> {
    let mut result: Vec<Machine> = vec![];
    let mut unmatched: Vec<usize> = vec![];
    for observation in observations {
        let existing = result.iter_mut().find(|m| {
            m.runtime == observation.runtime
                && m.runtime_id.is_some()
                && m.runtime_id == observation.runtime_id
        });
        if let Some(current) = existing {
            if observation.orchestrator.is_some() {
                current.name = observation.name;
                current.orchestrator = observation.orchestrator;
                current.environment = observation.environment;
            }
            current.cpu = observation.cpu.or(current.cpu);
            current.memory_gib = observation.memory_gib.or(current.memory_gib);
            current.disk_gib = observation.disk_gib.or(current.disk_gib);
            current.disk_used_gib = observation.disk_used_gib.or(current.disk_used_gib);
            current.created_at = observation.created_at.or(current.created_at);
            for ip in observation.ips {
                if !current.ips.contains(&ip) {
                    current.ips.push(ip);
                }
            }
        } else {
            if observation.orchestrator.as_deref() == Some("vagrant") {
                unmatched.push(result.len());
            }
            result.push(observation);
        }
    }
    for i in unmatched {
        let m = &mut result[i];
        if m.state != "running" {
            continue;
        }
        if m.runtime_id.is_none() {
            m.state = "stale".into();
        } else if verified.contains(&m.runtime.as_str()) {
            m.state = "stopped".into();
        }
    }
    result
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Summary {
    pub running: usize,
    pub cpu: f64,
    pub memory_gib: f64,
    pub disk_gib: f64,
    pub unknown_resources: usize,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct HostPlatform {
    pub platform: String,
    pub architecture: String,
    pub cpu: Option<usize>,
    pub memory_gib: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ProviderStatus {
    pub id: String,
    pub label: String,
    pub status: String,
    pub message: Option<String>,
}

impl ProviderStatus {
    pub fn new(id: &str, label: &str, status: &str, message: Option<&str>) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
            status: status.into(),
            message: message.map(Into::into),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Inventory {
    pub mode: String,
    pub timestamp: String,
    pub host: HostPlatform,
    pub machines: Vec<Machine>,
    pub providers: Vec<ProviderStatus>,
    pub summary: Summary,
}

impl Inventory {
    pub fn new(mode: &str, timestamp: String, host: HostPlatform) -> Self {
        Self {
            mode: mode.into(),
            timestamp,
            host,
            machines: vec![],
            providers: vec![],
            summary: Summary {
                running: 0,
                cpu: 0.0,
                memory_gib: 0.0,
                disk_gib: 0.0,
                unknown_resources: 0,
                warnings: vec![],
            },
        }
    }

    pub fn summarize(&mut self) {
        self.summary = summarize(&self.machines, self.host.memory_gib);
    }
}

pub fn summarize(machines: &[Machine], host_memory: Option<f64>) -> Summary {
    let running: Vec<_> = machines.iter().filter(|m| m.state == "running").collect();
    let memory: f64 = running.iter().filter_map(|m| m.memory_gib).sum();
    Summary {
        running: running.len(),
        cpu: running.iter().filter_map(|m| m.cpu).sum(),
        memory_gib: memory,
        disk_gib: machines.iter().filter_map(|m| m.disk_gib).sum(),
        unknown_resources: running
            .iter()
            .filter(|m| m.cpu.is_none() || m.memory_gib.is_none())
            .count(),
        warnings: if host_memory.is_some_and(|host| memory > host) {
            vec!["Known running VM allocations exceed host memory.".into()]
        } else {
            vec![]
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merge_by_identity_not_name() {
        let mut vm = Machine::new("virtualbox", "uuid", "same");
        vm.memory_gib = Some(8.0);
        vm.cpu = Some(4.0);
        vm.state = "running".into();
        let mut orchestrated = vm.clone();
        orchestrated.memory_gib = None;
        orchestrated.orchestrator = Some("vagrant".into());
        orchestrated.environment = "Narwhal".into();
        let merged = reconcile(vec![vm.clone(), orchestrated], &[]);
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].memory_gib, Some(8.0));
        assert_eq!(merged[0].environment, "Narwhal");
        assert_eq!(summarize(&merged, Some(4.0)).warnings.len(), 1);
        let other = Machine::new("virtualbox", "different", "same");
        assert_eq!(reconcile(vec![vm, other], &[]).len(), 2);
    }

    #[test]
    fn stale_vagrant_entries_are_corrected() {
        let mk = |id: Option<&str>| {
            let mut m = Machine::new("vmware", "x", "vm");
            m.runtime_id = id.map(Into::into);
            m.orchestrator = Some("vagrant".into());
            m.state = "running".into();
            m
        };
        assert_eq!(reconcile(vec![mk(None)], &["vmware"])[0].state, "stale");
        assert_eq!(
            reconcile(vec![mk(Some("a"))], &["vmware"])[0].state,
            "stopped"
        );
        assert_eq!(reconcile(vec![mk(Some("a"))], &[])[0].state, "running");
    }
}
