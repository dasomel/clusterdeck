#![allow(dead_code)]

use sha2::{Digest, Sha256};
use std::path::Path;

pub fn slug(s: &str) -> String {
    let s: String = s
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect();
    s.split('-')
        .filter(|p| !p.is_empty())
        .collect::<Vec<_>>()
        .join("-")
}

/// Loopback is not a unique identity: forwarded ports are reused by recreated or other VMs.
pub fn is_loopback_address(addr: &str) -> bool {
    addr.eq_ignore_ascii_case("localhost")
        || addr
            .trim_matches(|c| c == '[' || c == ']')
            .parse::<std::net::IpAddr>()
            .is_ok_and(|ip| ip.is_loopback())
}

/// An address the Kubernetes API can be reached on: not loopback, not the Vagrant NAT NIC
/// (10.0.2.x), not link-local.
pub fn is_reachable_address(addr: &str) -> bool {
    if is_loopback_address(addr) || addr.starts_with("10.0.2.") {
        return false;
    }
    match addr.parse::<std::net::IpAddr>() {
        Ok(std::net::IpAddr::V4(ip)) => !ip.is_link_local() && !ip.is_unspecified(),
        Ok(std::net::IpAddr::V6(ip)) => {
            !ip.is_unspecified() && (ip.segments()[0] & 0xffc0) != 0xfe80
        }
        Err(_) => true, // hostname
    }
}

/// Vagrant VMs usually have a NAT NIC (10.0.2.x) first; the reachable address is the host-only/bridged one.
/// Never falls back to a NAT/loopback address: those reach SSH but not the Kubernetes API.
pub fn reachable_ip(ips: &[String]) -> Option<&String> {
    ips.iter().find(|ip| is_reachable_address(ip))
}

/// `home` is injected (not read from `$HOME` here) so tests never mutate process-global env.
pub fn vagrant_identity(dir: &Path, machine: &str, home: Option<&Path>) -> Option<String> {
    if !crate::services::validate::is_safe_path_segment(machine) {
        return None;
    }
    // Item 5: A missing per-machine dir must fall through to the insecure fallback rather than aborting early
    let per_machine = std::fs::read_dir(dir.join(".vagrant/machines").join(machine))
        .ok()
        .and_then(|entries| {
            entries
                .filter_map(Result::ok)
                .map(|e| e.path().join("private_key"))
                .find(|p| p.exists())
        });
    let shared_ed25519 = home
        .map(|h| h.join(".vagrant.d/insecure_private_keys/vagrant.key.ed25519"))
        .filter(|p| p.exists());
    let shared_rsa = home
        .map(|h| h.join(".vagrant.d/insecure_private_keys/vagrant.key.rsa"))
        .filter(|p| p.exists());
    let shared_insecure = home
        .map(|h| h.join(".vagrant.d/insecure_private_key"))
        .filter(|p| p.exists());
    per_machine
        .or(shared_ed25519)
        .or(shared_rsa)
        .or(shared_insecure)
        .map(|p| p.to_string_lossy().into_owned())
}

pub fn parse_colima_ssh_config(text: &str) -> Option<(String, u16, String, Option<String>)> {
    let block = crate::services::local_runtime::parse_ssh_config_blocks(text)
        .into_iter()
        .next()?;
    Some((
        block.hostname?,
        block.port?,
        block.user?,
        block.identity_file,
    ))
}

pub fn vagrant_profile_id(dir: &Path, base: &str) -> String {
    let canonical = dir.canonicalize().unwrap_or_else(|_| dir.to_path_buf());
    let mut hasher = Sha256::new();
    hasher.update(canonical.to_string_lossy().as_bytes());
    let hash6: String = hasher
        .finalize()
        .iter()
        .take(3)
        .map(|b| format!("{b:02x}"))
        .collect();
    format!("vagrant-{base}-{hash6}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn r1_vagrant_id_is_sha256_scoped_to_canonical_project_path() {
        let temp_base = std::env::temp_dir().join(format!("cd-r1-test-{}", std::process::id()));
        let dir_a = temp_base.join("a/lab");
        let dir_b = temp_base.join("b/lab");
        std::fs::create_dir_all(&dir_a).unwrap();
        std::fs::create_dir_all(&dir_b).unwrap();

        let id_a = vagrant_profile_id(&dir_a, "lab");
        let id_b = vagrant_profile_id(&dir_b, "lab");

        assert_ne!(id_a, id_b);
        assert!(id_a.starts_with("vagrant-lab-"));
        assert!(id_b.starts_with("vagrant-lab-"));
        assert_eq!(id_a.len(), "vagrant-lab-".len() + 6);
        let _ = std::fs::remove_dir_all(&temp_base);
    }

    #[test]
    fn vagrant_identity_falls_through_to_insecure_fallback_when_machine_dir_missing() {
        let temp_dir = std::env::temp_dir().join(format!(
            "cd-key-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let dummy_project = temp_dir.join("project");
        let fake_home = temp_dir.join("home");
        let fallback_key_dir = fake_home.join(".vagrant.d/insecure_private_keys");
        std::fs::create_dir_all(&dummy_project).unwrap();
        std::fs::create_dir_all(&fallback_key_dir).unwrap();

        let fallback_key = fallback_key_dir.join("vagrant.key.ed25519");
        std::fs::write(&fallback_key, "fake-ed25519-key").unwrap();

        // Machine directory does not exist at all in dummy_project
        let key = vagrant_identity(&dummy_project, "master-1", Some(&fake_home));
        assert_eq!(key, Some(fallback_key.to_string_lossy().into_owned()));

        let _ = std::fs::remove_dir_all(&temp_dir);
    }

    #[test]
    fn parses_colima_ssh_config_and_ip_choice() {
        let text = "Host colima\n  IdentityFile \"/k/user\"\n  User m\n  Hostname 127.0.0.1\n  Port 61809\n";
        assert_eq!(
            parse_colima_ssh_config(text),
            Some((
                "127.0.0.1".into(),
                61809,
                "m".into(),
                Some("/k/user".into())
            ))
        );
        // NAT 10.0.2.x is skipped in favour of the host-only address.
        assert_eq!(
            reachable_ip(&["10.0.2.15".into(), "192.168.56.11".into()]),
            Some(&"192.168.56.11".to_string())
        );
        assert_eq!(slug("My Lab_01"), "my-lab-01");
        assert_eq!(
            reachable_ip(&["10.0.2.15".into(), "127.0.0.1".into()]),
            None
        );
    }

    #[test]
    fn loopback_and_reachable_address_classification() {
        for a in ["127.0.0.1", "127.9.9.9", "::1", "[::1]", "localhost"] {
            assert!(is_loopback_address(a), "{a}");
            assert!(!is_reachable_address(a), "{a}");
        }
        for a in ["169.254.1.1", "10.0.2.15", "fe80::1"] {
            assert!(!is_reachable_address(a), "{a}");
        }
        assert!(is_reachable_address("192.168.56.11"));
        assert!(!is_loopback_address("192.168.56.11"));
    }
}
