#![allow(dead_code)]

use crate::services::config::{
    BootstrapPolicy, Host, KubeconfigSource, LocalRuntimeSource, Profile,
};

#[derive(Debug, Clone)]
pub struct Target {
    pub id: String,
    pub name: String,
    pub hosts: Vec<Host>,
    pub local_runtime: Option<LocalRuntimeSource>,
    pub kubeconfig_path: String,
    pub context: String,
}

// D5/D6: Apply target to typed Profiles. Validates via services/validate.rs,
// refreshes existing matching profile (same id) without replacing user-configured fields.
pub fn apply(
    profiles: &mut Vec<Profile>,
    t: &Target,
) -> Result<(String, bool, Vec<String>), String> {
    if !crate::services::validate::is_safe_profile_id(&t.id) {
        return Err(format!("invalid profile id: {}", t.id));
    }
    for h in &t.hosts {
        if !crate::services::validate::is_safe_ssh_identifier(&h.name) {
            return Err(format!("invalid host name: {}", h.name));
        }
        if !crate::services::validate::is_safe_ssh_identifier(&h.address)
            || !crate::services::validate::is_safe_ssh_identifier(&h.user)
        {
            return Err(format!("invalid host user/address for host {}", h.name));
        }
        if let Some(i) = &h.identity_file {
            if !i.is_empty() && !crate::services::validate::is_safe_ssh_identifier(i) {
                return Err(format!("invalid identity file for host {}", h.name));
            }
        }
    }
    if let Some(lr) = &t.local_runtime {
        if !crate::services::validate::is_safe_local_runtime_instance_name(&lr.instance) {
            return Err(format!("invalid local_runtime instance: {}", lr.instance));
        }
    }

    // R1: If a DIFFERENT profile already uses the same address+port, REFUSE.
    for p in profiles.iter() {
        if p.id != t.id {
            for h in &p.hosts {
                for new_host in &t.hosts {
                    // Loopback (forwarded ports) is reused by other/recreated VMs: not an identity.
                    if h.address == new_host.address
                        && h.port == new_host.port
                        && !crate::services::inventory::identity::is_loopback_address(&h.address)
                    {
                        return Err(format!(
                            "host address '{}:{}' is already used by existing profile '{}'",
                            h.address, h.port, p.id
                        ));
                    }
                }
            }
        }
    }

    // R1: Match an existing profile by ID ONLY.
    let existing_idx = profiles.iter().position(|p| p.id == t.id);

    let Some(idx) = existing_idx else {
        let first_cp = t
            .hosts
            .iter()
            .find(|h| {
                ["master", "control", "server"]
                    .iter()
                    .any(|w| h.name.contains(w))
            })
            .unwrap_or(&t.hosts[0]);
        let profile = Profile {
            id: t.id.clone(),
            name: t.name.clone(),
            hosts: t.hosts.clone(),
            bastion: None,
            bootstrap: BootstrapPolicy::default(),
            kubeconfig: Some(KubeconfigSource {
                remote_path: t.kubeconfig_path.clone(),
                control_plane: first_cp.name.clone(),
                local_path: format!("~/.clusterdeck/kubeconfigs/{}.yaml", t.id),
                context: t.context.clone(),
            }),
            manage_hosts_file: false,
            trusted_cas: vec![],
            local_runtime: t.local_runtime.clone(),
        };
        profiles.push(profile);
        return Ok((t.id.clone(), true, vec![]));
    };

    let profile = &mut profiles[idx];
    let id = profile.id.clone();
    let mut updated = vec![];
    for spec in &t.hosts {
        if let Some(h) = profile.hosts.iter_mut().find(|h| h.name == spec.name) {
            let changed = h.address != spec.address || h.port != spec.port;
            if changed {
                h.address = spec.address.clone();
                h.port = spec.port;
                updated.push(spec.name.clone());
            }
        } else {
            profile.hosts.push(spec.clone());
            updated.push(spec.name.clone());
        }
    }
    if let Some(instance) = &t.local_runtime {
        if profile.local_runtime.is_none() {
            profile.local_runtime = Some(instance.clone());
        }
    }
    Ok((id, false, updated))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::config::AuthMode;

    fn target() -> Target {
        let host = |n: &str, ip: &str| Host {
            name: n.into(),
            address: ip.into(),
            port: 22,
            user: "vagrant".into(),
            identity_file: None,
            auth: AuthMode::Key,
        };
        Target {
            id: "vagrant-lab-111111".into(),
            name: "lab (Vagrant)".into(),
            hosts: vec![
                host("master-1", "192.168.1.10"),
                host("worker-1", "192.168.1.11"),
            ],
            local_runtime: None,
            kubeconfig_path: "/etc/kubernetes/admin.conf".into(),
            context: "lab".into(),
        }
    }

    #[test]
    fn r1_refuses_when_different_profile_already_uses_address_and_port() {
        let mut profiles = vec![Profile {
            id: "vagrant-lab-111111".into(),
            name: "Lab 1".into(),
            hosts: vec![Host {
                name: "master-1".into(),
                address: "192.168.1.10".into(),
                port: 22,
                user: "vagrant".into(),
                identity_file: None,
                auth: AuthMode::Key,
            }],
            bastion: None,
            bootstrap: BootstrapPolicy::default(),
            kubeconfig: None,
            manage_hosts_file: false,
            trusted_cas: vec![],
            local_runtime: None,
        }];

        let mut t = target();
        t.id = "vagrant-lab-222222".into();
        t.hosts[0].address = "192.168.1.10".into();
        t.hosts[0].port = 22;

        let err = apply(&mut profiles, &t).unwrap_err();
        assert!(err.contains("host address '192.168.1.10:22' is already used by existing profile 'vagrant-lab-111111'"));
    }

    #[test]
    fn loopback_address_port_is_not_a_collision_but_same_id_still_matches() {
        let mut existing = other_profile();
        existing.id = "vagrant-lab-111111".into();
        existing.hosts = vec![Host {
            address: "127.0.0.1".into(),
            port: 2222,
            ..target().hosts[0].clone()
        }];
        let mut profiles = vec![existing];
        let mut t = target();
        t.id = "vagrant-lab-222222".into();
        t.hosts[0].address = "127.0.0.1".into();
        t.hosts[0].port = 2222;
        let (_, created, _) = apply(&mut profiles, &t).unwrap();
        assert!(created);
        assert_eq!(profiles.len(), 2);
        // id-only matching still applies: re-applying the existing id refreshes, never duplicates.
        let mut same = target();
        same.hosts.truncate(1);
        same.hosts[0].address = "127.0.0.1".into();
        same.hosts[0].port = 2222;
        let (_, created, _) = apply(&mut profiles, &same).unwrap();
        assert!(!created);
        assert_eq!(profiles.len(), 2);
    }

    fn other_profile() -> Profile {
        Profile {
            id: "other".into(),
            name: "o".into(),
            hosts: vec![],
            bastion: None,
            bootstrap: BootstrapPolicy::default(),
            kubeconfig: None,
            manage_hosts_file: false,
            trusted_cas: vec![],
            local_runtime: None,
        }
    }

    #[test]
    fn creates_profile_and_keeps_others() {
        let mut profiles = vec![other_profile()];
        // worker-1 listed first: master-1 must still be picked as the control plane.
        let mut t = target();
        t.hosts.reverse();
        let (id, created, _) = apply(&mut profiles, &t).unwrap();
        assert_eq!((id.as_str(), created), ("vagrant-lab-111111", true));
        assert_eq!(profiles[0].name, "o");
        let new_profile = profiles.iter().find(|p| p.id == id).unwrap();
        assert_eq!(
            new_profile.kubeconfig.as_ref().unwrap().control_plane,
            "master-1"
        );
    }

    #[test]
    fn existing_profile_only_refreshes_addresses() {
        let mut profiles = vec![];
        apply(&mut profiles, &target()).unwrap();
        profiles[0]
            .trusted_cas
            .push(crate::services::ca_trust::TrustedCa {
                secret_ref: "ns/ca".into(),
                fingerprint_sha256: "a".into(),
                fingerprint_sha1: "b".into(),
                subject_cn: "ca".into(),
                not_after: "x".into(),
                trusted_at: "y".into(),
            });
        let kubeconfig_before = profiles[0].kubeconfig.clone();
        profiles[0].hosts[1].user = "custom".into();
        let mut moved = target();
        moved.hosts[1].address = "192.168.1.99".into();
        // A changed kubeconfig target must not rewrite the user's kubeconfig settings either.
        moved.kubeconfig_path = "/other/path".into();

        let (_, created, updated) = apply(&mut profiles, &moved).unwrap();
        assert!(!created);
        assert_eq!(updated, vec!["worker-1"]);
        assert_eq!(profiles[0].hosts[1].address, "192.168.1.99");
        assert_eq!(profiles[0].hosts[1].user, "custom");
        assert_eq!(profiles[0].trusted_cas[0].subject_cn, "ca");
        assert_eq!(
            serde_json::to_value(&profiles[0].kubeconfig).unwrap(),
            serde_json::to_value(&kubeconfig_before).unwrap()
        );
    }

    #[test]
    fn rejects_unsafe_values() {
        let mut t = target();
        t.hosts[0].user = "-oProxyCommand=x".into();
        assert!(apply(&mut vec![], &t).is_err());
        let mut t = target();
        t.id = "../x".into();
        assert!(apply(&mut vec![], &t).is_err());
    }
}
