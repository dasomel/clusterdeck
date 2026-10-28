#![allow(dead_code)]

//! Pure derivation of `zones.json` content from profile settings and discovered endpoints
//! (ADR-0011 phase 2, lane C). No I/O: callers gather endpoints and write the result.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use serde::{Deserialize, Serialize};

use crate::services::config::{HostMapping, Profile};
use crate::services::k8s_endpoints::DiscoveredEndpoint;
use crate::services::local_dns::zone_file::{
    is_name_in_zone, validate_record_ipv4, RecordEntry, ZoneEntry, ZonesFile, ZONES_FILE_VERSION,
};
use crate::services::validate::{
    is_allowed_private_zone, is_reserved_clusterdeck_zone, is_safe_gateway_address,
    is_safe_host_domain, profile_dns_label, NODE_ALIAS_DOMAIN,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SkippedReasonKind {
    OutsideZones,
    CoveredByOtherProfile,
    InvalidDnsName,
    InvalidIpv4,
}

/// A name that could not be published, surfaced to the UI instead of silently dropped.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SkippedHost {
    pub profile_id: String,
    pub host: String,
    pub reason: String,
    pub reason_kind: SkippedReasonKind,
}

/// A zone claimed by more than one profile. It is never served, so neither claimant silently
/// wins; the user resolves it with an explicit zone-owner switch.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ZoneConflict {
    pub zone: String,
    pub profile_ids: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BuildOutput {
    pub zones: ZonesFile,
    pub skipped: Vec<SkippedHost>,
    pub conflicts: Vec<ZoneConflict>,
}

fn skip(profile_id: &str, host: &str, reason: &str, reason_kind: SkippedReasonKind) -> SkippedHost {
    SkippedHost {
        profile_id: profile_id.to_string(),
        host: host.to_string(),
        reason: reason.to_string(),
        reason_kind,
    }
}

pub fn build_zones(
    profiles: &[Profile],
    endpoints_by_profile: &HashMap<String, Vec<DiscoveredEndpoint>>,
    generation: u64,
    port: u16,
) -> BuildOutput {
    let mut skipped = Vec::new();
    let mut conflicts = Vec::new();

    // Group LocalDns profiles by DNS label. `ZoneTable` rejects two profile ids that collapse
    // onto one label for the whole file, so such profiles are excluded outright and reported
    // as a conflict on their shared alias zone.
    let mut by_label: BTreeMap<String, Vec<&Profile>> = BTreeMap::new();
    for p in profiles
        .iter()
        .filter(|p| p.host_mapping == HostMapping::LocalDns)
    {
        if let Some(label) = profile_dns_label(&p.id) {
            by_label.entry(label).or_default().push(p);
        }
    }
    let mut active: Vec<(&Profile, String)> = Vec::new();
    for (label, group) in by_label {
        if group.len() > 1 {
            let mut ids: Vec<String> = group.iter().map(|p| p.id.clone()).collect();
            ids.sort();
            conflicts.push(ZoneConflict {
                zone: format!("{label}.{NODE_ALIAS_DOMAIN}"),
                profile_ids: ids,
            });
        } else {
            active.push((group[0], label));
        }
    }

    // zone -> claimant profile ids (approved zones plus each profile's implicit alias zone).
    let mut claims: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for (p, label) in &active {
        claims
            .entry(format!("{label}.{NODE_ALIAS_DOMAIN}"))
            .or_default()
            .insert(p.id.clone());
        for zone in p
            .approved_dns_zones
            .iter()
            .filter(|z| is_allowed_private_zone(z) && !is_reserved_clusterdeck_zone(z))
        {
            claims.entry(zone.clone()).or_default().insert(p.id.clone());
        }
    }
    let mut served: BTreeSet<String> = BTreeSet::new();
    for (zone, owners) in &claims {
        if owners.len() > 1 {
            conflicts.push(ZoneConflict {
                zone: zone.clone(),
                profile_ids: owners.iter().cloned().collect(),
            });
        } else {
            served.insert(zone.clone());
        }
    }

    let mut records: BTreeMap<String, BTreeSet<(String, String)>> = BTreeMap::new();
    // Ownership comes ONLY from the validated claims: reserved/invalid zones in loaded YAML
    // never enter `claims`, so they can never take over another profile's zone.
    let owner: BTreeMap<String, String> = claims
        .iter()
        .filter(|(z, _)| served.contains(*z))
        .filter_map(|(z, ids)| ids.iter().next().map(|id| (z.clone(), id.clone())))
        .collect();
    for zone in owner.keys() {
        records.entry(zone.clone()).or_default();
    }

    for (p, label) in &active {
        let alias_zone = format!("{label}.{NODE_ALIAS_DOMAIN}");
        if owner.get(&alias_zone) == Some(&p.id) {
            // Alias names use the DNS label, not the raw profile id (the hosts block uses the
            // id): `_` or uppercase in an id cannot appear in a zone name.
            let aliased = p
                .hosts
                .iter()
                .map(|h| (h.name.as_str(), h.address.as_str()))
                .chain(
                    p.bastion
                        .iter()
                        .map(|b| (b.name.as_str(), b.address.as_str())),
                );
            for (name, address) in aliased {
                let fqdn = format!("{name}.{alias_zone}").to_ascii_lowercase();
                if !is_safe_host_domain(&fqdn) {
                    skipped.push(skip(
                        &p.id,
                        name,
                        "alias is not a valid DNS name",
                        SkippedReasonKind::InvalidDnsName,
                    ));
                    continue;
                }
                if validate_record_ipv4(&fqdn, address).is_err() {
                    skipped.push(skip(
                        &p.id,
                        name,
                        "address is not a usable IPv4 literal",
                        SkippedReasonKind::InvalidIpv4,
                    ));
                    continue;
                }
                records
                    .entry(alias_zone.clone())
                    .or_default()
                    .insert((fqdn, address.to_string()));
            }
        }

        let gateway = p
            .gateway_address
            .as_deref()
            .filter(|g| is_safe_gateway_address(g));
        for ep in endpoints_by_profile.get(&p.id).into_iter().flatten() {
            let host = ep.host.to_ascii_lowercase();
            if !is_safe_host_domain(&host) {
                skipped.push(skip(
                    &p.id,
                    &ep.host,
                    "not a valid DNS name",
                    SkippedReasonKind::InvalidDnsName,
                ));
                continue;
            }
            // Same override rule as discovery (`k8s_endpoints`): the gateway address wins.
            let ip = gateway.unwrap_or(&ep.ip);
            if validate_record_ipv4(&host, ip).is_err() {
                skipped.push(skip(
                    &p.id,
                    &ep.host,
                    "target is not a usable IPv4 address",
                    SkippedReasonKind::InvalidIpv4,
                ));
                continue;
            }
            // The most specific claimed zone across ALL profiles decides: a parent zone owner
            // must never publish names under a child zone owned by someone else.
            let covering = claims
                .keys()
                .filter(|z| is_name_in_zone(&host, z))
                .max_by_key(|z| z.len());
            match covering {
                Some(zone) if served.contains(zone) && owner.get(zone) == Some(&p.id) => {
                    records
                        .entry(zone.clone())
                        .or_default()
                        .insert((host, ip.to_string()));
                }
                // Covered by a conflicted zone: reported as a conflict, not as a skip.
                Some(zone) if !served.contains(zone) => {}
                Some(zone) => skipped.push(skip(
                    &p.id,
                    &ep.host,
                    &format!("covered by zone {zone} of another profile"),
                    SkippedReasonKind::CoveredByOtherProfile,
                )),
                None => skipped.push(skip(
                    &p.id,
                    &ep.host,
                    "outside every approved zone",
                    SkippedReasonKind::OutsideZones,
                )),
            }
        }
    }

    let zones = records
        .into_iter()
        .map(|(zone, recs)| ZoneEntry {
            profile_id: owner.get(&zone).cloned().unwrap_or_default(),
            zone,
            records: recs
                .into_iter()
                .map(|(name, ipv4)| RecordEntry { name, ipv4 })
                .collect(),
        })
        .collect();

    BuildOutput {
        zones: ZonesFile {
            version: ZONES_FILE_VERSION,
            generation,
            port,
            zones,
        },
        skipped,
        conflicts,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::config::{AuthMode, Bastion, BootstrapPolicy, Host};
    use crate::services::local_dns::zone_file::{load_and_validate, write_zones};

    fn host(name: &str, address: &str) -> Host {
        Host {
            name: name.into(),
            address: address.into(),
            port: 22,
            user: "root".into(),
            identity_file: None,
            auth: AuthMode::Key,
        }
    }

    fn profile(id: &str, mapping: HostMapping, zones: &[&str]) -> Profile {
        Profile {
            id: id.into(),
            name: id.into(),
            hosts: vec![host("m1", "192.0.2.10")],
            bastion: None,
            bootstrap: BootstrapPolicy::default(),
            kubeconfig: None,
            host_mapping: mapping,
            approved_dns_zones: zones.iter().map(|z| z.to_string()).collect(),
            gateway_address: None,
            trusted_cas: Vec::new(),
            local_runtime: None,
        }
    }

    fn ep(host: &str, ip: &str) -> DiscoveredEndpoint {
        DiscoveredEndpoint {
            host: host.into(),
            ip: ip.into(),
            source: "ingress".into(),
            resource_name: "ns/app".into(),
        }
    }

    fn eps(
        profile_id: &str,
        list: Vec<DiscoveredEndpoint>,
    ) -> HashMap<String, Vec<DiscoveredEndpoint>> {
        HashMap::from([(profile_id.to_string(), list)])
    }

    fn assert_valid(out: &BuildOutput, tag: &str) {
        let path = std::env::temp_dir().join(format!(
            "clusterdeck-zones-builder-{tag}-{}.json",
            std::process::id()
        ));
        write_zones(&path, &out.zones).unwrap();
        let table = load_and_validate(&path).unwrap();
        std::fs::remove_file(&path).ok();
        assert_eq!(table.generation, out.zones.generation);
    }

    fn zone<'a>(out: &'a BuildOutput, name: &str) -> &'a ZoneEntry {
        out.zones.zones.iter().find(|z| z.zone == name).unwrap()
    }

    #[test]
    fn only_local_dns_profiles_are_included() {
        let profiles = vec![
            profile("a", HostMapping::LocalDns, &[]),
            profile("b", HostMapping::Hosts, &["lab.example.internal"]),
            profile("c", HostMapping::Off, &[]),
        ];
        let out = build_zones(&profiles, &HashMap::new(), 3, 15353);
        assert_eq!(out.zones.generation, 3);
        assert_eq!(out.zones.port, 15353);
        let names: Vec<_> = out.zones.zones.iter().map(|z| z.zone.as_str()).collect();
        assert_eq!(names, vec!["a.clusterdeck.internal"]);
        assert_valid(&out, "only-local");
    }

    #[test]
    fn endpoints_inside_approved_zone_are_served_and_others_skipped() {
        let profiles = vec![profile(
            "a",
            HostMapping::LocalDns,
            &["lab.example.internal"],
        )];
        let endpoints = eps(
            "a",
            vec![
                ep("app.lab.example.internal", "192.0.2.50"),
                ep("lab.example.internal", "192.0.2.51"),
                ep("other.example.test", "192.0.2.52"),
                ep("x.lab.example.internal.evil.example", "192.0.2.53"),
            ],
        );
        let out = build_zones(&profiles, &endpoints, 1, 15353);
        let z = zone(&out, "lab.example.internal");
        assert_eq!(z.profile_id, "a");
        let names: Vec<_> = z.records.iter().map(|r| r.name.as_str()).collect();
        assert_eq!(
            names,
            vec!["app.lab.example.internal", "lab.example.internal"]
        );
        let skipped: Vec<_> = out.skipped.iter().map(|s| s.host.as_str()).collect();
        assert_eq!(
            skipped,
            vec!["other.example.test", "x.lab.example.internal.evil.example"]
        );
        assert_valid(&out, "approved");
    }

    #[test]
    fn gateway_address_overrides_endpoint_target() {
        let mut p = profile("a", HostMapping::LocalDns, &["lab.example.internal"]);
        p.gateway_address = Some("192.0.2.99".into());
        let endpoints = eps("a", vec![ep("app.lab.example.internal", "192.0.2.50")]);
        let out = build_zones(&[p], &endpoints, 1, 15353);
        assert_eq!(
            zone(&out, "lab.example.internal").records[0].ipv4,
            "192.0.2.99"
        );
        assert_valid(&out, "gateway");
    }

    #[test]
    fn ipv6_targets_are_skipped() {
        let profiles = vec![profile(
            "a",
            HostMapping::LocalDns,
            &["lab.example.internal"],
        )];
        let endpoints = eps("a", vec![ep("v6.lab.example.internal", "2001:db8::1")]);
        let out = build_zones(&profiles, &endpoints, 1, 15353);
        assert!(zone(&out, "lab.example.internal").records.is_empty());
        assert_eq!(out.skipped.len(), 1);
        assert_valid(&out, "ipv6");
    }

    #[test]
    fn node_alias_zone_has_hosts_and_bastion() {
        let mut p = profile("a", HostMapping::LocalDns, &[]);
        p.bastion = Some(Bastion {
            name: "jump".into(),
            address: "192.0.2.5".into(),
            port: 22,
            user: "root".into(),
            identity_file: None,
        });
        let out = build_zones(&[p], &HashMap::new(), 1, 15353);
        let z = zone(&out, "a.clusterdeck.internal");
        let recs: Vec<_> = z
            .records
            .iter()
            .map(|r| (r.name.as_str(), r.ipv4.as_str()))
            .collect();
        assert_eq!(
            recs,
            vec![
                ("jump.a.clusterdeck.internal", "192.0.2.5"),
                ("m1.a.clusterdeck.internal", "192.0.2.10")
            ]
        );
        assert_valid(&out, "alias");
    }

    #[test]
    fn alias_uses_dns_label_when_id_differs() {
        let p = profile("My_Lab", HostMapping::LocalDns, &[]);
        let out = build_zones(&[p], &HashMap::new(), 1, 15353);
        let z = zone(&out, "my-lab.clusterdeck.internal");
        assert_eq!(z.profile_id, "My_Lab");
        assert_eq!(z.records[0].name, "m1.my-lab.clusterdeck.internal");
        assert_valid(&out, "label");
    }

    #[test]
    fn non_ipv4_host_address_is_skipped_from_aliases() {
        let mut p = profile("a", HostMapping::LocalDns, &[]);
        p.hosts.push(host("m2", "node2.example.internal"));
        let out = build_zones(&[p], &HashMap::new(), 1, 15353);
        assert_eq!(zone(&out, "a.clusterdeck.internal").records.len(), 1);
        assert_eq!(out.skipped[0].host, "m2");
        assert_valid(&out, "alias-skip");
    }

    #[test]
    fn duplicate_zone_across_profiles_is_a_conflict_and_not_served() {
        let profiles = vec![
            profile("a", HostMapping::LocalDns, &["lab.example.internal"]),
            profile("b", HostMapping::LocalDns, &["lab.example.internal"]),
        ];
        let mut endpoints = eps("a", vec![ep("app.lab.example.internal", "192.0.2.50")]);
        endpoints.insert(
            "b".into(),
            vec![ep("app.lab.example.internal", "192.0.2.60")],
        );
        let out = build_zones(&profiles, &endpoints, 1, 15353);
        assert_eq!(
            out.conflicts,
            vec![ZoneConflict {
                zone: "lab.example.internal".into(),
                profile_ids: vec!["a".into(), "b".into()],
            }]
        );
        assert!(out
            .zones
            .zones
            .iter()
            .all(|z| z.zone != "lab.example.internal"));
        assert!(out.skipped.is_empty());
        // Each profile's own alias zone is still served.
        assert_eq!(out.zones.zones.len(), 2);
        assert_valid(&out, "conflict");
    }

    #[test]
    fn profiles_sharing_a_dns_label_are_excluded_and_reported() {
        let profiles = vec![
            profile("my_lab", HostMapping::LocalDns, &["lab.example.internal"]),
            profile("my-lab", HostMapping::LocalDns, &[]),
            profile("ok", HostMapping::LocalDns, &[]),
        ];
        let out = build_zones(&profiles, &HashMap::new(), 1, 15353);
        assert_eq!(out.conflicts.len(), 1);
        assert_eq!(out.conflicts[0].zone, "my-lab.clusterdeck.internal");
        let names: Vec<_> = out.zones.zones.iter().map(|z| z.zone.as_str()).collect();
        assert_eq!(names, vec!["ok.clusterdeck.internal"]);
        assert_valid(&out, "label-collision");
    }

    #[test]
    fn nested_zone_prefers_most_specific() {
        let profiles = vec![profile(
            "a",
            HostMapping::LocalDns,
            &["lab.example.internal", "dev.lab.example.internal"],
        )];
        let endpoints = eps("a", vec![ep("app.dev.lab.example.internal", "192.0.2.50")]);
        let out = build_zones(&profiles, &endpoints, 1, 15353);
        assert_eq!(zone(&out, "dev.lab.example.internal").records.len(), 1);
        assert!(zone(&out, "lab.example.internal").records.is_empty());
        assert_valid(&out, "nested");
    }

    #[test]
    fn unspecified_and_invalid_targets_are_skipped_and_file_still_validates() {
        let mut p = profile("a", HostMapping::LocalDns, &["lab.example.internal"]);
        p.hosts.push(host("m0", "0.0.0.0"));
        let endpoints = eps(
            "a",
            vec![
                ep("zero.lab.example.internal", "0.0.0.0"),
                ep("junk.lab.example.internal", "not-an-ip"),
                ep("ok.lab.example.internal", "192.0.2.50"),
            ],
        );
        let out = build_zones(&[p], &endpoints, 1, 15353);
        let skipped: Vec<_> = out.skipped.iter().map(|s| s.host.as_str()).collect();
        assert_eq!(
            skipped,
            vec![
                "m0",
                "zero.lab.example.internal",
                "junk.lab.example.internal"
            ]
        );
        assert_eq!(zone(&out, "lab.example.internal").records.len(), 1);
        assert_valid(&out, "unspecified");
    }

    #[test]
    fn parent_zone_owner_cannot_publish_under_child_zone_of_another_profile() {
        let profiles = vec![
            profile("a", HostMapping::LocalDns, &["lab.example.internal"]),
            profile("b", HostMapping::LocalDns, &["dev.lab.example.internal"]),
        ];
        let mut endpoints = eps(
            "a",
            vec![
                ep("evil.dev.lab.example.internal", "192.0.2.66"),
                ep("fine.lab.example.internal", "192.0.2.50"),
            ],
        );
        endpoints.insert(
            "b".into(),
            vec![ep("app.dev.lab.example.internal", "192.0.2.60")],
        );
        let out = build_zones(&profiles, &endpoints, 1, 15353);
        let child = zone(&out, "dev.lab.example.internal");
        assert_eq!(child.profile_id, "b");
        assert_eq!(child.records.len(), 1);
        assert_eq!(child.records[0].ipv4, "192.0.2.60");
        assert_eq!(zone(&out, "lab.example.internal").records.len(), 1);
        assert_eq!(out.skipped.len(), 1);
        assert_eq!(out.skipped[0].host, "evil.dev.lab.example.internal");
        assert_valid(&out, "hijack");
    }

    #[test]
    fn approved_zone_under_clusterdeck_local_is_ignored() {
        let profiles = vec![profile(
            "a",
            HostMapping::LocalDns,
            &["x.clusterdeck.internal"],
        )];
        let endpoints = eps("a", vec![ep("app.x.clusterdeck.internal", "192.0.2.50")]);
        let out = build_zones(&profiles, &endpoints, 1, 15353);
        assert!(out
            .zones
            .zones
            .iter()
            .all(|z| z.zone != "x.clusterdeck.internal"));
        assert_valid(&out, "reserved");
    }

    #[test]
    fn profile_cannot_claim_another_profiles_alias_zone() {
        let profiles = vec![
            profile("a", HostMapping::LocalDns, &["b.clusterdeck.internal"]),
            profile("b", HostMapping::LocalDns, &[]),
        ];
        let endpoints = eps("a", vec![ep("evil.b.clusterdeck.internal", "192.0.2.66")]);
        let out = build_zones(&profiles, &endpoints, 1, 15353);
        let b = zone(&out, "b.clusterdeck.internal");
        assert_eq!(b.profile_id, "b");
        assert!(b.records.iter().all(|r| r.ipv4 != "192.0.2.66"));
        assert_eq!(out.skipped.len(), 1);
        assert_valid(&out, "alias-takeover");
    }

    #[test]
    fn loaded_overlapping_zones_never_cross_publish() {
        let profiles = vec![
            profile(
                "a",
                HostMapping::LocalDns,
                &["lab.example.internal", "x.clusterdeck.internal"],
            ),
            profile("b", HostMapping::LocalDns, &["dev.lab.example.internal"]),
        ];
        let endpoints = eps(
            "a",
            vec![
                ep("evil.dev.lab.example.internal", "192.0.2.66"),
                ep("evil.x.clusterdeck.internal", "192.0.2.67"),
            ],
        );
        let out = build_zones(&profiles, &endpoints, 1, 15353);
        assert_eq!(zone(&out, "dev.lab.example.internal").profile_id, "b");
        assert!(out
            .zones
            .zones
            .iter()
            .flat_map(|z| &z.records)
            .all(|r| r.ipv4 != "192.0.2.66" && r.ipv4 != "192.0.2.67"));
        assert_eq!(out.skipped.len(), 2);
        assert_valid(&out, "loaded-overlap");
    }
}
