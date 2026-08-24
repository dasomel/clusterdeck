#![allow(dead_code)]

pub fn is_safe_ssh_identifier(s: &str) -> bool {
    !s.is_empty() && !s.starts_with('-') && !s.contains('\n') && !s.contains('\r')
}

pub fn is_safe_profile_id(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 64
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

pub fn validate_profile(profile: &crate::services::config::Profile) -> Result<(), String> {
    if !is_safe_profile_id(&profile.id) {
        return Err(format!("invalid profile id: {}", profile.id));
    }
    for host in &profile.hosts {
        if !is_safe_ssh_identifier(&host.user) || !is_safe_ssh_identifier(&host.address) {
            return Err(format!("invalid host user/address for host {}", host.name));
        }
        if let Some(identity) = &host.identity_file {
            if !identity.is_empty() && (!is_safe_ssh_identifier(identity)) {
                return Err(format!("invalid identity_file for host {}", host.name));
            }
        }
    }
    if let Some(bastion) = &profile.bastion {
        if !is_safe_ssh_identifier(&bastion.user) || !is_safe_ssh_identifier(&bastion.address) {
            return Err("invalid bastion user/address".to_string());
        }
        if let Some(identity) = &bastion.identity_file {
            if !identity.is_empty() && !is_safe_ssh_identifier(identity) {
                return Err("invalid bastion identity_file".to_string());
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::config::{BootstrapPolicy, Host, Profile};

    #[test]
    fn is_safe_ssh_identifier_rejects_newlines_and_dashes() {
        assert!(!is_safe_ssh_identifier(""));
        assert!(!is_safe_ssh_identifier("-oProxyCommand=evil"));
        assert!(!is_safe_ssh_identifier("user\nHost evil"));
        assert!(!is_safe_ssh_identifier("user\rHost evil"));
        assert!(is_safe_ssh_identifier("root"));
        assert!(is_safe_ssh_identifier("192.168.1.1"));
    }

    #[test]
    fn is_safe_profile_id_rejects_path_traversal() {
        assert!(!is_safe_profile_id("../../etc"));
        assert!(!is_safe_profile_id(""));
        assert!(is_safe_profile_id("cka-lab"));
        assert!(is_safe_profile_id("cka_lab_1"));
    }

    #[test]
    fn validate_profile_rejects_newline_in_host_address_and_accepts_valid() {
        let valid_profile = Profile {
            id: "cka-lab".into(),
            name: "CKA Lab".into(),
            hosts: vec![Host {
                name: "m1".into(),
                address: "192.168.1.10".into(),
                port: 22,
                user: "root".into(),
                identity_file: None,
            }],
            bastion: None,
            bootstrap: BootstrapPolicy::default(),
            kubeconfig: None,
        };
        assert!(validate_profile(&valid_profile).is_ok());

        let mut invalid_profile = valid_profile.clone();
        invalid_profile.hosts[0].address = "192.168.1.10\nHost evil".into();
        assert!(validate_profile(&invalid_profile).is_err());
    }
}
