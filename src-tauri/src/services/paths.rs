#![allow(dead_code)]

use std::path::PathBuf;

pub struct ClusterDeckPaths {
    pub base: PathBuf,
}

impl ClusterDeckPaths {
    pub fn resolve() -> Result<Self, String> {
        let home = std::env::var("HOME").map_err(|_| "HOME is not set".to_string())?;
        Ok(Self {
            base: PathBuf::from(home).join(".clusterdeck"),
        })
    }

    pub fn at(base: PathBuf) -> Self {
        Self { base }
    }

    pub fn profiles_file(&self) -> PathBuf {
        self.base.join("profiles.yaml")
    }

    pub fn ssh_dir(&self) -> PathBuf {
        self.base.join("ssh")
    }

    pub fn ssh_conf(&self, profile_id: &str) -> PathBuf {
        self.ssh_dir().join(format!("{profile_id}.conf"))
    }

    pub fn kubeconfigs_dir(&self) -> PathBuf {
        self.base.join("kubeconfigs")
    }

    pub fn kubeconfig_file(&self, profile_id: &str) -> PathBuf {
        self.kubeconfigs_dir().join(format!("{profile_id}.yaml"))
    }

    pub fn state_file(&self) -> PathBuf {
        self.base.join("state.json")
    }

    pub fn ensure_dirs(&self) -> Result<(), String> {
        std::fs::create_dir_all(self.ssh_dir()).map_err(|e| e.to_string())?;
        std::fs::create_dir_all(self.kubeconfigs_dir()).map_err(|e| e.to_string())?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_are_scoped_under_base() {
        let paths = ClusterDeckPaths::at("/tmp/clusterdeck-test-fixture".into());
        assert_eq!(
            paths.profiles_file(),
            std::path::PathBuf::from("/tmp/clusterdeck-test-fixture/profiles.yaml")
        );
        assert_eq!(
            paths.ssh_conf("cka"),
            std::path::PathBuf::from("/tmp/clusterdeck-test-fixture/ssh/cka.conf")
        );
        assert_eq!(
            paths.kubeconfig_file("cka"),
            std::path::PathBuf::from("/tmp/clusterdeck-test-fixture/kubeconfigs/cka.yaml")
        );
    }

    #[test]
    fn ensure_dirs_creates_expected_tree() {
        let dir = std::env::temp_dir().join(format!("clusterdeck-test-{}", std::process::id()));
        let paths = ClusterDeckPaths::at(dir.clone());
        paths.ensure_dirs().unwrap();
        assert!(paths.ssh_dir().is_dir());
        assert!(paths.kubeconfigs_dir().is_dir());
        std::fs::remove_dir_all(&dir).ok();
    }
}
