use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

use super::LoginErrorCode;
use crate::services::paths::ClusterDeckPaths;

// D1: pin the checked bytes across concurrent Connect replacements. Cost: one private
// temporary kubeconfig per action; remove this when the command is retired (ADR-0014).
pub(super) struct KubeconfigSnapshot(PathBuf);

impl KubeconfigSnapshot {
    pub(super) fn create(paths: &ClusterDeckPaths, raw: &[u8]) -> Result<Self, LoginErrorCode> {
        Self::create_inner(paths, raw).map_err(|_| LoginErrorCode::KubectlFailed)
    }

    fn create_inner(paths: &ClusterDeckPaths, raw: &[u8]) -> std::io::Result<Self> {
        let dir = paths.base.join("tmp");
        std::fs::create_dir_all(&dir)?;
        let mut random = [0u8; 16];
        File::open("/dev/urandom")?.read_exact(&mut random)?;
        let name = random
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>();
        let path = dir.join(format!("service-login-{name}.yaml"));
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        options.mode(0o600);
        let mut file = options.open(&path)?;
        let snapshot = Self(path);
        file.write_all(raw)?;
        Ok(snapshot)
    }

    pub(super) fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for KubeconfigSnapshot {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}
