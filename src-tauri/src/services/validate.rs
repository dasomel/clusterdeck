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

pub fn is_safe_ip_address(s: &str) -> bool {
    s.parse::<std::net::IpAddr>().is_ok()
}

/// Sink validator for `open_url_in_browser`: only http(s) URLs with no whitespace or control
/// characters may reach the `open` argv (macOS treats some URL schemes/args specially).
pub fn is_safe_open_url(url: &str) -> bool {
    let trimmed = url.trim();
    (trimmed.starts_with("http://") || trimmed.starts_with("https://"))
        && !trimmed.contains('\n')
        && !trimmed.contains('\r')
        && !trimmed.contains(' ')
}

/// Sink validator for the known_hosts path OpenSSH reports in its "Offending key"/"Add correct
/// host key" stderr lines, before that path is used as an `ssh-keygen -f` argument.
pub fn is_safe_known_hosts_path(path: &str) -> bool {
    if path.is_empty() || path.starts_with('-') {
        return false;
    }
    !path
        .chars()
        .any(|c| c.is_control() || c == '"' || c == '\'')
}

pub fn is_safe_host_domain(s: &str) -> bool {
    if s.is_empty() || s.len() > 253 {
        return false;
    }
    if s.starts_with('.') || s.ends_with('.') || s.contains("..") {
        return false;
    }
    for label in s.split('.') {
        if label.is_empty()
            || label.len() > 63
            || label.starts_with('-')
            || label.ends_with('-')
            || !label.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
        {
            return false;
        }
    }
    true
}

pub fn validate_profile(profile: &crate::services::config::Profile) -> Result<(), String> {
    if !is_safe_profile_id(&profile.id) {
        return Err(format!("invalid profile id: {}", profile.id));
    }
    for host in &profile.hosts {
        if !is_safe_ssh_identifier(&host.name) {
            return Err(format!("invalid host name: {}", host.name));
        }
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
        if !is_safe_ssh_identifier(&bastion.name) {
            return Err(format!("invalid bastion name: {}", bastion.name));
        }
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
    fn validate_profile_rejects_newline_in_host_name() {
        let mut profile = Profile {
            id: "cka-lab".into(),
            name: "CKA Lab".into(),
            hosts: vec![Host {
                name: "m1\nHost evil".into(),
                address: "192.168.1.10".into(),
                port: 22,
                user: "root".into(),
                identity_file: None,
            }],
            bastion: None,
            bootstrap: BootstrapPolicy::default(),
            kubeconfig: None,
            manage_hosts_file: false,
            trusted_cas: Vec::new(),
        };
        assert!(validate_profile(&profile).is_err());

        profile.hosts[0].name = "m1".into();
        profile.bastion = Some(crate::services::config::Bastion {
            name: "bastion\nHost evil".into(),
            address: "10.0.0.1".into(),
            port: 22,
            user: "root".into(),
            identity_file: None,
        });
        assert!(validate_profile(&profile).is_err());
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
            manage_hosts_file: false,
            trusted_cas: Vec::new(),
        };
        assert!(validate_profile(&valid_profile).is_ok());

        let mut invalid_profile = valid_profile.clone();
        invalid_profile.hosts[0].address = "192.168.1.10\nHost evil".into();
        assert!(validate_profile(&invalid_profile).is_err());
    }

    #[test]
    fn is_safe_open_url_accepts_http_https_and_rejects_unsafe_urls() {
        assert!(is_safe_open_url("https://example.com"));
        assert!(is_safe_open_url("http://example.com/path?x=1"));
        assert!(!is_safe_open_url("ftp://example.com"));
        assert!(!is_safe_open_url("javascript:alert(1)"));
        assert!(!is_safe_open_url("https://example.com/a b"));
        assert!(!is_safe_open_url("https://example.com\nHost: evil"));
        assert!(!is_safe_open_url("https://example.com\revil"));
    }

    #[test]
    fn is_safe_known_hosts_path_rejects_dash_prefix_and_control_chars() {
        assert!(is_safe_known_hosts_path("/Users/m/.ssh/known_hosts"));
        assert!(!is_safe_known_hosts_path(""));
        assert!(!is_safe_known_hosts_path("-oProxyCommand=evil"));
        assert!(!is_safe_known_hosts_path("/tmp/evil\"; rm -rf /"));
        assert!(!is_safe_known_hosts_path("/tmp/evil\nHost x"));
    }

    #[test]
    fn is_safe_ip_address_validates_ipv4_and_ipv6() {
        assert!(is_safe_ip_address("192.168.77.10"));
        assert!(is_safe_ip_address("10.0.0.1"));
        assert!(is_safe_ip_address("::1"));
        assert!(is_safe_ip_address("2001:db8::1"));
        assert!(!is_safe_ip_address(""));
        assert!(!is_safe_ip_address("not-an-ip"));
        assert!(!is_safe_ip_address("192.168.1.1\nevil"));
        assert!(!is_safe_ip_address("192.168.1.1 80"));
    }

    #[test]
    fn is_safe_host_domain_validates_rfc1123() {
        assert!(is_safe_host_domain("trino.local.beluga.internal"));
        assert!(is_safe_host_domain("api.example.com"));
        assert!(is_safe_host_domain("my-cluster-1.internal"));
        assert!(is_safe_host_domain("localhost"));
        assert!(!is_safe_host_domain(""));
        assert!(!is_safe_host_domain("*.example.com"));
        assert!(!is_safe_host_domain(".example.com"));
        assert!(!is_safe_host_domain("example.com."));
        assert!(!is_safe_host_domain("example..com"));
        assert!(!is_safe_host_domain("-bad.domain"));
        assert!(!is_safe_host_domain("bad-.domain"));
        assert!(!is_safe_host_domain("bad\nhost.internal"));
        assert!(!is_safe_host_domain("bad host.internal"));
        assert!(!is_safe_host_domain("bad/host.internal"));
    }
}
