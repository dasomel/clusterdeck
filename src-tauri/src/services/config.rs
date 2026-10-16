#![allow(dead_code)]

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(from = "ProfileWire")]
pub struct Profile {
    pub id: String,
    pub name: String,
    pub hosts: Vec<Host>,
    pub bastion: Option<Bastion>,
    pub bootstrap: BootstrapPolicy,
    pub kubeconfig: Option<KubeconfigSource>,
    /// How this profile's discovered names are resolved locally (ADR-0011). Replaces the old
    /// `manage_hosts_file` bool; `store.rs` migrates and dual-writes it (plan R9).
    #[serde(default)]
    pub host_mapping: HostMapping,
    /// Local-DNS zones the user approved for this profile. A zone has at most one owner
    /// profile (enforced in `store`); validated with `validate::is_allowed_private_zone`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub approved_dns_zones: Vec<String>,
    #[serde(default)]
    pub trusted_cas: Vec<crate::services::ca_trust::TrustedCa>,
    /// Externally reachable address of the cluster's ingress/gateway (for example a cloud
    /// load balancer's public IP in front of NodePorts). When set, it is the target IP of every
    /// discovered endpoint instead of an LB IP or the control-plane address: Kubernetes cannot
    /// know an external cloud LB address (the gateway Service is NodePort, so there is no
    /// `status.loadBalancer`). `#[serde(default, skip_serializing_if = ...)]` so existing
    /// profile YAML round-trips unchanged. Must satisfy `validate::is_safe_ip_address`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gateway_address: Option<String>,
    /// Which local VM runtime this profile's host(s) came from, if created from a detected
    /// Colima/Lima instance. `#[serde(default, skip_serializing_if = "Option::is_none")]` so
    /// profile YAML persisted before this field existed still deserializes unchanged, and a
    /// profile with no local runtime origin doesn't grow a `local_runtime: null` line.
    /// `services::local_runtime::refresh_local_runtime_endpoint` uses this to re-resolve the
    /// current SSH address/port before connecting -- Colima/Lima forward a new SSH port on every
    /// VM restart, so a port recorded at profile-creation time goes stale and Test
    /// Connection/Connect fail with "Connection refused" against it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub local_runtime: Option<LocalRuntimeSource>,
}

/// Deserialization shape of `Profile`: accepts the legacy `manage_hosts_file` bool so YAML
/// and old IPC payloads share one migration path (`HostMapping::migrate`, plan R9).
#[derive(Deserialize)]
struct ProfileWire {
    id: String,
    name: String,
    hosts: Vec<Host>,
    bastion: Option<Bastion>,
    bootstrap: BootstrapPolicy,
    kubeconfig: Option<KubeconfigSource>,
    #[serde(default)]
    manage_hosts_file: bool,
    #[serde(default)]
    host_mapping: Option<HostMapping>,
    #[serde(default)]
    approved_dns_zones: Vec<String>,
    #[serde(default)]
    trusted_cas: Vec<crate::services::ca_trust::TrustedCa>,
    #[serde(default)]
    gateway_address: Option<String>,
    #[serde(default)]
    local_runtime: Option<LocalRuntimeSource>,
}

impl From<ProfileWire> for Profile {
    fn from(w: ProfileWire) -> Self {
        Profile {
            id: w.id,
            name: w.name,
            hosts: w.hosts,
            bastion: w.bastion,
            bootstrap: w.bootstrap,
            kubeconfig: w.kubeconfig,
            host_mapping: HostMapping::migrate(w.host_mapping, w.manage_hosts_file),
            approved_dns_zones: w.approved_dns_zones,
            trusted_cas: w.trusted_cas,
            gateway_address: w.gateway_address,
            local_runtime: w.local_runtime,
        }
    }
}

/// Name-resolution strategy for a profile. `Hosts` is the old `manage_hosts_file == true`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum HostMapping {
    #[default]
    Off,
    LocalDns,
    Hosts,
}

impl HostMapping {
    /// `host_mapping` wins when present; otherwise the legacy bool decides.
    pub fn migrate(mapping: Option<HostMapping>, legacy_manage_hosts_file: bool) -> HostMapping {
        mapping.unwrap_or(if legacy_manage_hosts_file {
            HostMapping::Hosts
        } else {
            HostMapping::Off
        })
    }
}

/// Identifies the local Colima/Lima instance a `Profile`'s host was created from. See
/// `Profile::local_runtime` for why this exists.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LocalRuntimeSource {
    pub provider: crate::services::local_runtime_lifecycle::LocalRuntimeProvider,
    pub instance: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Host {
    pub name: String,
    pub address: String,
    #[serde(default = "default_port")]
    pub port: u16,
    pub user: String,
    pub identity_file: Option<String>,
    /// SSH authentication mode for this host. `#[serde(default)]` (-> Key) so profile YAML
    /// persisted before this field existed still deserializes unchanged (Issue #26).
    #[serde(default)]
    pub auth: AuthMode,
}

/// Explicit SSH authentication mode for a `Host`. Defaults to `Key` so existing persisted
/// profiles that predate this field keep working without migration.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum AuthMode {
    #[default]
    Key,
    Password,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Bastion {
    pub name: String,
    pub address: String,
    #[serde(default = "default_port")]
    pub port: u16,
    pub user: String,
    pub identity_file: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BootstrapPolicy {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_retries")]
    pub retries: u32,
    #[serde(default = "default_retry_delay_secs")]
    pub retry_delay_secs: u64,
}

impl Default for BootstrapPolicy {
    fn default() -> Self {
        Self {
            enabled: false,
            retries: default_retries(),
            retry_delay_secs: default_retry_delay_secs(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KubeconfigSource {
    pub remote_path: String,
    pub control_plane: String, // Host.name of the source host
    pub local_path: String,
    pub context: String,
}

fn default_port() -> u16 {
    22
}

fn default_retries() -> u32 {
    3
}

fn default_retry_delay_secs() -> u64 {
    5
}

#[cfg(test)]
mod tests {
    use super::*;

    const BASE: &str =
        r#"{"id":"a","name":"A","hosts":[],"bastion":null,"bootstrap":{},"kubeconfig":null"#;

    #[test]
    fn ipc_payload_with_legacy_bool_migrates() {
        let on: Profile =
            serde_json::from_str(&format!(r#"{BASE},"manage_hosts_file":true}}"#)).unwrap();
        assert_eq!(on.host_mapping, HostMapping::Hosts);
        let off: Profile = serde_json::from_str(&format!("{BASE}}}")).unwrap();
        assert_eq!(off.host_mapping, HostMapping::Off);
    }

    #[test]
    fn host_mapping_wins_over_legacy_bool_for_json_and_yaml() {
        let j: Profile = serde_json::from_str(&format!(
            r#"{BASE},"manage_hosts_file":true,"host_mapping":"local_dns"}}"#
        ))
        .unwrap();
        assert_eq!(j.host_mapping, HostMapping::LocalDns);
        let y: Profile = serde_yaml::from_str(
            "id: a\nname: A\nhosts: []\nbastion: null\nbootstrap: {}\nkubeconfig: null\nmanage_hosts_file: true\nhost_mapping: off\n",
        )
        .unwrap();
        assert_eq!(y.host_mapping, HostMapping::Off);
        let legacy: Profile = serde_yaml::from_str(
            "id: a\nname: A\nhosts: []\nbastion: null\nbootstrap: {}\nkubeconfig: null\nmanage_hosts_file: true\n",
        )
        .unwrap();
        assert_eq!(legacy.host_mapping, HostMapping::Hosts);
    }
}
