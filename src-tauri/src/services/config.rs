// These models are the planned boundary between the desktop UI and the Tauri backend.
// Keep them in the bootstrap skeleton before the profile commands are wired up.
#![allow(dead_code)]

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Profile {
    pub id: String,
    pub name: String,
    pub hosts: Vec<Host>,
    pub bastion: Option<Bastion>,
    pub kubeconfig: Option<KubeconfigSource>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Host {
    pub name: String,
    pub address: String,
    #[serde(default = "default_port")]
    pub port: u16,
    pub user: String,
    pub identity_file: Option<String>,
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
pub struct KubeconfigSource {
    pub remote_path: String,
    pub control_plane: Option<String>,
    pub local_path: String,
    pub context: String,
}

fn default_port() -> u16 {
    22
}
