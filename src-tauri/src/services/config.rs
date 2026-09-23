#![allow(dead_code)]

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Profile {
    pub id: String,
    pub name: String,
    pub hosts: Vec<Host>,
    pub bastion: Option<Bastion>,
    pub bootstrap: BootstrapPolicy,
    pub kubeconfig: Option<KubeconfigSource>,
    #[serde(default)]
    pub manage_hosts_file: bool,
    #[serde(default)]
    pub trusted_cas: Vec<crate::services::ca_trust::TrustedCa>,
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
