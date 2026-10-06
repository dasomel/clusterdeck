#![allow(dead_code)]

mod colima;
mod vagrant;
mod vbox;
mod vmware;

use crate::services::inventory::model::Machine;
use crate::services::process::CommandRunner;

pub use vagrant::vagrant_ssh_config;

pub async fn discover(runner: &dyn CommandRunner, id: &str) -> Result<Vec<Machine>, String> {
    match id {
        "colima" => colima::discover(runner).await,
        "virtualbox" => vbox::discover(runner).await,
        "vmware" => vmware::discover(runner).await,
        "vagrant" => vagrant::discover(runner).await,
        _ => Err("unsupported provider".into()),
    }
}
