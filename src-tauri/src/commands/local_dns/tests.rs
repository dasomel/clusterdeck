//! Command-level tests: fake runner + temp dirs, no real launchctl/osascript.

use super::*;
use std::sync::Arc;

mod test_support {
    use super::super::*;
    use crate::services::config::{AuthMode, BootstrapPolicy, Host};
    use crate::services::local_dns::zone_file::{write_status, StatusFile};
    use crate::services::process::{CommandOutput, CommandRunner};
    use async_trait::async_trait;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    pub const PID: u32 = 4242;
    static SEQ: AtomicU64 = AtomicU64::new(0);

    /// Stand-in for id/launchctl/lsof/osascript plus a responder that mirrors zones.json into
    /// status.json while the agent is "loaded". It records every call so tests can assert on
    /// what the wiring asked the manager to do.
    pub struct Fake {
        pub home: PathBuf,
        pub agent_paths: AgentPaths,
        pub resolver: PathBuf,
        pub exe: PathBuf,
        pub hosts: PathBuf,
        log: Mutex<Vec<String>>,
        scripts: Mutex<Vec<String>>,
        loaded: Mutex<bool>,
        cancel_osascript: Mutex<bool>,
    }

    impl Fake {
        pub fn new() -> Arc<Self> {
            let seq = SEQ.fetch_add(1, Ordering::Relaxed);
            let home = std::env::temp_dir().join(format!(
                "clusterdeck-dnscmd-test-{}-{seq}",
                std::process::id()
            ));
            let _ = std::fs::remove_dir_all(&home);
            let resolver = home.join("resolver");
            std::fs::create_dir_all(&resolver).unwrap();
            let exe = home.join("current-exe");
            std::fs::write(&exe, b"binary-v1").unwrap();
            let hosts = home.join("hosts");
            std::fs::write(&hosts, "127.0.0.1 localhost\n").unwrap();
            let f = Arc::new(Self {
                agent_paths: AgentPaths::new(&home),
                home,
                resolver,
                exe,
                hosts,
                log: Mutex::new(Vec::new()),
                scripts: Mutex::new(Vec::new()),
                loaded: Mutex::new(false),
                cancel_osascript: Mutex::new(false),
            });
            let responder = Arc::clone(&f);
            tokio::spawn(async move {
                loop {
                    tokio::time::sleep(Duration::from_millis(30)).await;
                    responder.mirror_status();
                }
            });
            f
        }

        pub fn env(&self) -> ManagerEnv<'_> {
            ManagerEnv::new(
                self,
                &self.agent_paths,
                &self.resolver,
                &self.exe,
                DEFAULT_DNS_PORT,
            )
        }

        pub fn cd_paths(&self) -> ClusterDeckPaths {
            ClusterDeckPaths::at(self.home.join(".clusterdeck"))
        }

        pub fn count(&self, prefix: &str) -> usize {
            self.log
                .lock()
                .unwrap()
                .iter()
                .filter(|c| c.starts_with(prefix))
                .count()
        }

        pub fn total_calls(&self) -> usize {
            self.log.lock().unwrap().len()
        }

        pub fn last_script(&self) -> Option<String> {
            self.scripts.lock().unwrap().last().cloned()
        }

        pub fn set_cancel_osascript(&self, cancel: bool) {
            *self.cancel_osascript.lock().unwrap() = cancel;
        }

        fn mirror_status(&self) {
            if !*self.loaded.lock().unwrap() {
                return;
            }
            if let Some(z) = std::fs::read_to_string(&self.agent_paths.zones)
                .ok()
                .and_then(|s| serde_json::from_str::<ZonesFile>(&s).ok())
            {
                let _ = write_status(
                    &self.agent_paths.status,
                    StatusFile {
                        generation: z.generation,
                        loaded_at: "t".into(),
                        error: None,
                        pid: PID,
                        instance: "inst".into(),
                    },
                );
            }
        }

        pub fn served(&self) -> Vec<(String, Vec<String>)> {
            std::fs::read_to_string(&self.agent_paths.zones)
                .ok()
                .and_then(|s| serde_json::from_str::<ZonesFile>(&s).ok())
                .map(|z| {
                    z.zones
                        .into_iter()
                        .map(|e| (e.zone, e.records.into_iter().map(|r| r.name).collect()))
                        .collect()
                })
                .unwrap_or_default()
        }
    }

    fn ok(stdout: &str) -> Result<CommandOutput, String> {
        Ok(CommandOutput {
            stdout: stdout.into(),
            stderr: String::new(),
            success: true,
        })
    }

    #[async_trait]
    impl CommandRunner for Fake {
        async fn run(&self, bin: &str, args: &[String]) -> Result<CommandOutput, String> {
            let sub = args.first().map(String::as_str).unwrap_or("");
            match bin {
                "/usr/bin/id" => {
                    self.log.lock().unwrap().push("id".into());
                    ok("501")
                }
                "/bin/launchctl" => {
                    self.log.lock().unwrap().push(format!("launchctl {sub}"));
                    let mut loaded = self.loaded.lock().unwrap();
                    match sub {
                        "bootstrap" => {
                            *loaded = true;
                            ok("")
                        }
                        "bootout" if *loaded => {
                            *loaded = false;
                            ok("")
                        }
                        "print" if *loaded => ok(&format!("{{\n\tpid = {PID}\n}}")),
                        _ => Ok(CommandOutput {
                            stdout: String::new(),
                            stderr: "not loaded".into(),
                            success: false,
                        }),
                    }
                }
                "/usr/sbin/lsof" => {
                    self.log.lock().unwrap().push("lsof".into());
                    ok(&PID.to_string())
                }
                "osascript" => {
                    self.log.lock().unwrap().push("osascript".into());
                    self.scripts.lock().unwrap().push(args[1].clone());
                    if *self.cancel_osascript.lock().unwrap() {
                        Ok(CommandOutput {
                            stdout: String::new(),
                            stderr: "execution error: User cancelled. (-128)".into(),
                            success: false,
                        })
                    } else {
                        if let Ok(entries) = std::fs::read_dir(&self.resolver) {
                            for entry in entries.flatten() {
                                let path = entry.path();
                                if path.is_file() {
                                    let rm = format!("/bin/rm -f '{path:?}'").replace("\"", "");
                                    let rm_direct = format!("/bin/rm -f '{}'", path.display());
                                    if args[1].contains(&rm) || args[1].contains(&rm_direct) {
                                        let _ = std::fs::remove_file(path);
                                    }
                                }
                            }
                        }
                        if let Some(start) = args[1].find("/bin/cp -P '") {
                            let rest = &args[1][start + "/bin/cp -P '".len()..];
                            if let Some(end) = rest.find('\'') {
                                let tmp_path = &rest[..end];
                                if let Ok(content) = std::fs::read_to_string(tmp_path) {
                                    let _ = std::fs::write(&self.hosts, content);
                                }
                            }
                        }
                        let mut search = args[1].as_str();
                        while let Some(mv_pos) = search.find("/bin/mv -f ") {
                            let rest = &search[mv_pos + "/bin/mv -f ".len()..];
                            if let Some(target_start) = rest.find('\'') {
                                let after_target = &rest[target_start + 1..];
                                if let Some(end_quote) = after_target.find('\'') {
                                    let target_path = &after_target[..end_quote];
                                    let prefix = &search[..mv_pos];
                                    let marker_opt = prefix
                                        .rfind("/usr/bin/printf '%s\\\\n' '")
                                        .map(|p| p + "/usr/bin/printf '%s\\\\n' '".len())
                                        .or_else(|| {
                                            prefix.rfind("# managed by ClusterDeck (profile: ")
                                        });
                                    if let Some(marker_pos) = marker_opt {
                                        let rest_marker = &prefix[marker_pos..];
                                        if let Some(marker_end) = rest_marker.find('\'') {
                                            let marker_line = &rest_marker[..marker_end];
                                            let content = format!(
                                                "{marker_line}\nnameserver 127.0.0.1\nport {}\n",
                                                DEFAULT_DNS_PORT
                                            );
                                            let _ = std::fs::write(target_path, content);
                                        }
                                    }
                                    search = &after_target[end_quote + 1..];
                                    continue;
                                }
                            }
                            break;
                        }
                        ok("")
                    }
                }
                other => panic!("unexpected command {other}"),
            }
        }
    }

    pub fn host(name: &str, address: &str) -> Host {
        Host {
            name: name.into(),
            address: address.into(),
            port: 22,
            user: "root".into(),
            identity_file: None,
            auth: AuthMode::Key,
        }
    }

    pub fn profile(id: &str, mapping: HostMapping, zones: &[&str]) -> Profile {
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

    pub fn ep(host: &str, ip: &str) -> DiscoveredEndpoint {
        DiscoveredEndpoint {
            host: host.into(),
            ip: ip.into(),
            source: "ingress".into(),
            resource_name: "ns/app".into(),
        }
    }
}

use crate::services::resolver_files::render_resolver_file;
use test_support::*;

mod round4;

fn zone_names(f: &Fake) -> Vec<String> {
    f.served().into_iter().map(|(z, _)| z).collect()
}

fn records_of(f: &Fake, zone: &str) -> Vec<String> {
    f.served()
        .into_iter()
        .find(|(z, _)| z == zone)
        .map(|(_, r)| r)
        .unwrap_or_default()
}

#[tokio::test(start_paused = true)]
async fn sync_applies_zones_built_from_all_inputs_in_one_prompt() {
    let f = Fake::new();
    let paths = f.cd_paths();
    store::upsert_profile(
        &paths,
        profile("cka", HostMapping::LocalDns, &["lab.example.internal"]),
    )
    .unwrap();
    let p = store::get_profile(&paths, "cka").unwrap();

    let res = sync_local_dns_profile(
        &paths,
        &f.env(),
        &f.hosts,
        &p,
        Ok(vec![
            ep("api.lab.example.internal", "192.0.2.50"),
            ep("api.other.example.internal", "192.0.2.51"),
        ]),
    )
    .await
    .unwrap();

    assert!(res.success && res.dns_prompted);
    assert_eq!(f.count("osascript"), 1, "exactly one admin prompt");
    assert_eq!(f.count("launchctl bootstrap"), 1);
    assert_eq!(
        zone_names(&f),
        vec!["cka.clusterdeck.local", "lab.example.internal"]
    );
    assert_eq!(
        records_of(&f, "lab.example.internal"),
        vec!["api.lab.example.internal"]
    );
    // A host outside every approved zone is reported with an approvable parent.
    assert_eq!(res.skipped_hosts.len(), 1);
    assert_eq!(res.skipped_hosts[0].host, "api.other.example.internal");
    assert_eq!(
        res.skipped_hosts[0].suggested_zones,
        vec!["other.example.internal"]
    );
}

#[tokio::test(start_paused = true)]
async fn syncing_one_profile_keeps_the_other_profiles_records() {
    let f = Fake::new();
    let paths = f.cd_paths();
    store::upsert_profile(
        &paths,
        profile("cka", HostMapping::LocalDns, &["a.example.internal"]),
    )
    .unwrap();
    store::upsert_profile(
        &paths,
        profile("ckb", HostMapping::LocalDns, &["b.example.internal"]),
    )
    .unwrap();
    let a = store::get_profile(&paths, "cka").unwrap();
    let b = store::get_profile(&paths, "ckb").unwrap();

    sync_local_dns_profile(
        &paths,
        &f.env(),
        &f.hosts,
        &a,
        Ok(vec![ep("x.a.example.internal", "192.0.2.1")]),
    )
    .await
    .unwrap();
    sync_local_dns_profile(
        &paths,
        &f.env(),
        &f.hosts,
        &b,
        Ok(vec![ep("y.b.example.internal", "192.0.2.2")]),
    )
    .await
    .unwrap();

    assert_eq!(
        records_of(&f, "a.example.internal"),
        vec!["x.a.example.internal"]
    );
    assert_eq!(
        records_of(&f, "b.example.internal"),
        vec!["y.b.example.internal"]
    );
}

async fn two_synced_profiles(f: &Fake) -> ClusterDeckPaths {
    let paths = f.cd_paths();
    for (id, zone) in [("cka", "a.example.internal"), ("ckb", "b.example.internal")] {
        store::upsert_profile(&paths, profile(id, HostMapping::LocalDns, &[zone])).unwrap();
    }
    let a = store::get_profile(&paths, "cka").unwrap();
    let b = store::get_profile(&paths, "ckb").unwrap();
    for (p, host) in [(&a, "x.a.example.internal"), (&b, "y.b.example.internal")] {
        sync_local_dns_profile(
            &paths,
            &f.env(),
            &f.hosts,
            p,
            Ok(vec![ep(host, "192.0.2.1")]),
        )
        .await
        .unwrap();
    }
    paths
}

#[tokio::test(start_paused = true)]
async fn removing_the_last_local_dns_profile_tears_down_but_not_before() {
    let f = Fake::new();
    let paths = two_synced_profiles(&f).await;

    remove_local_dns_profile(&paths, &f.env(), &f.hosts, "cka")
        .await
        .unwrap();
    assert!(f.agent_paths.plist.exists(), "ckb still needs the agent");
    assert_eq!(
        records_of(&f, "b.example.internal"),
        vec!["y.b.example.internal"]
    );
    assert!(!zone_names(&f).contains(&"a.example.internal".to_string()));

    // Removal keeps the profile configured; model "none left" by switching cka off.
    store::upsert_profile(&paths, profile("cka", HostMapping::Off, &[])).unwrap();
    remove_local_dns_profile(&paths, &f.env(), &f.hosts, "ckb")
        .await
        .unwrap();
    assert!(!f.agent_paths.plist.exists());
    assert!(!f.agent_paths.binary.exists());
}

#[tokio::test(start_paused = true)]
async fn delete_reapplies_without_the_deleted_profile() {
    let f = Fake::new();
    let paths = two_synced_profiles(&f).await;

    let was = crate::commands::profiles::delete_stored_profile(&paths, "cka").unwrap();
    assert!(was);
    reapply_after_delete(&paths, &f.env(), "cka").await.unwrap();
    assert_eq!(
        zone_names(&f),
        vec!["b.example.internal", "ckb.clusterdeck.local"]
    );
    assert!(f.agent_paths.plist.exists());

    let was = crate::commands::profiles::delete_stored_profile(&paths, "ckb").unwrap();
    assert!(was);
    reapply_after_delete(&paths, &f.env(), "ckb").await.unwrap();
    assert!(
        !f.agent_paths.plist.exists(),
        "last LocalDns profile gone: agent removed"
    );
    assert!(!f.agent_paths.binary.exists());
}

#[tokio::test(start_paused = true)]
async fn hosts_profiles_never_reach_the_dns_machinery() {
    let f = Fake::new();
    let paths = f.cd_paths();
    let hosts_profile = profile("ckh", HostMapping::Hosts, &[]);
    store::upsert_profile(&paths, hosts_profile.clone()).unwrap();

    // Saving a Hosts profile (even with changed settings) does nothing DNS-related.
    let mut edited = hosts_profile.clone();
    edited.gateway_address = Some("192.0.2.9".into());
    let out = reapply_after_save(&paths, &f.env(), &f.hosts, Some(&hosts_profile), &edited)
        .await
        .unwrap();
    assert!(out.is_none());
    assert!(!crate::commands::profiles::delete_stored_profile(&paths, "ckh").unwrap());
    assert_eq!(f.total_calls(), 0, "no id/launchctl/osascript call at all");
}

#[tokio::test(start_paused = true)]
async fn save_reapplies_only_when_a_local_dns_setting_changed() {
    let f = Fake::new();
    let paths = f.cd_paths();
    let old = profile("cka", HostMapping::LocalDns, &[]);
    store::upsert_profile(&paths, old.clone()).unwrap();

    // Unrelated edit: no re-apply.
    let mut renamed = old.clone();
    renamed.name = "renamed".into();
    assert!(
        reapply_after_save(&paths, &f.env(), &f.hosts, Some(&old), &renamed)
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(f.total_calls(), 0);

    // Approving a zone: re-apply.
    let mut approved = old.clone();
    approved.approved_dns_zones = vec!["lab.example.internal".into()];
    store::upsert_profile(&paths, approved.clone()).unwrap();
    assert!(
        reapply_after_save(&paths, &f.env(), &f.hosts, Some(&old), &approved)
            .await
            .unwrap()
            .is_some()
    );
    assert!(zone_names(&f).contains(&"lab.example.internal".to_string()));
}

#[tokio::test(start_paused = true)]
async fn hosts_to_local_dns_switch_removes_the_block_in_the_same_script() {
    let f = Fake::new();
    let paths = f.cd_paths();
    let old = profile("cka", HostMapping::Hosts, &[]);
    let new = profile("cka", HostMapping::LocalDns, &[]);
    store::upsert_profile(&paths, new.clone()).unwrap();
    let block = render_hosts_block_with_endpoints(&old, &[]).unwrap();
    std::fs::write(
        &f.hosts,
        compute_updated_hosts_content("127.0.0.1 localhost\n", "cka", Some(&block)),
    )
    .unwrap();

    reapply_after_save(&paths, &f.env(), &f.hosts, Some(&old), &new)
        .await
        .unwrap();

    assert_eq!(
        f.count("osascript"),
        1,
        "resolver write and hosts change share one prompt"
    );
    let script = f.last_script().unwrap();
    assert!(script.contains("/private/etc/hosts"), "{script}");
    assert!(script.contains("cka.clusterdeck.local"), "{script}");
}

#[tokio::test(start_paused = true)]
async fn startup_sweeps_our_files_when_no_profile_uses_local_dns_and_prompts_only_then() {
    let f = Fake::new();
    let paths = f.cd_paths();
    store::upsert_profile(&paths, profile("ckh", HostMapping::Hosts, &[])).unwrap();

    startup_with(&paths, &f.env()).await.unwrap();
    assert_eq!(
        f.count("osascript"),
        0,
        "nothing of ours on disk: no prompt"
    );

    std::fs::write(
        f.resolver.join("lab.example.internal"),
        render_resolver_file("cka", DEFAULT_DNS_PORT).unwrap(),
    )
    .unwrap();
    startup_with(&paths, &f.env()).await.unwrap();
    assert_eq!(f.count("osascript"), 1);
    assert!(f.last_script().unwrap().contains("lab.example.internal"));
}

#[tokio::test(start_paused = true)]
async fn startup_refreshes_a_changed_binary_and_reloads_a_loaded_agent() {
    let f = Fake::new();
    let paths = f.cd_paths();
    store::upsert_profile(
        &paths,
        profile("cka", HostMapping::LocalDns, &["lab.example.internal"]),
    )
    .unwrap();
    let p = store::get_profile(&paths, "cka").unwrap();
    sync_local_dns_profile(&paths, &f.env(), &f.hosts, &p, Ok(vec![]))
        .await
        .unwrap();
    assert_eq!(f.count("launchctl bootstrap"), 1);

    std::fs::write(&f.exe, b"binary-v2").unwrap();
    startup_with(&paths, &f.env()).await.unwrap();
    assert_eq!(std::fs::read(&f.agent_paths.binary).unwrap(), b"binary-v2");
    assert_eq!(
        f.count("launchctl bootstrap"),
        2,
        "reloaded after the copy changed"
    );
}

#[tokio::test(start_paused = true)]
async fn sync_of_a_former_hosts_profile_removes_its_block_after_the_resolver_writes() {
    let f = Fake::new();
    let paths = f.cd_paths();
    let p = profile("cka", HostMapping::LocalDns, &[]);
    store::upsert_profile(&paths, p.clone()).unwrap();
    let block =
        render_hosts_block_with_endpoints(&profile("cka", HostMapping::Hosts, &[]), &[]).unwrap();
    let other =
        render_hosts_block_with_endpoints(&profile("ckb", HostMapping::Hosts, &[]), &[]).unwrap();
    let with_blocks = compute_updated_hosts_content(
        &compute_updated_hosts_content("127.0.0.1 localhost\n", "ckb", Some(&other)),
        "cka",
        Some(&block),
    );
    std::fs::write(&f.hosts, &with_blocks).unwrap();

    sync_local_dns_profile(&paths, &f.env(), &f.hosts, &p, Ok(vec![]))
        .await
        .unwrap();

    assert_eq!(f.count("osascript"), 1, "one privileged action");
    let script = f.last_script().unwrap();
    let write_at = script
        .find("cka.clusterdeck.local")
        .expect("resolver write");
    let hosts_at = script.find("/private/etc/hosts").expect("hosts removal");
    assert!(
        write_at < hosts_at,
        "resolver writes first, subtractive hosts step after"
    );

    // The update handed to the manager is "remove this profile's block, keep the rest".
    let expected = compute_updated_hosts_content(&with_blocks, "cka", None);
    assert!(!expected.contains("profile: cka"));
    assert!(expected.contains("profile: ckb"));
}

#[tokio::test(start_paused = true)]
async fn sync_without_a_stale_block_never_touches_hosts() {
    let f = Fake::new();
    let paths = f.cd_paths();
    let p = profile("cka", HostMapping::LocalDns, &[]);
    store::upsert_profile(&paths, p.clone()).unwrap();
    sync_local_dns_profile(&paths, &f.env(), &f.hosts, &p, Ok(vec![]))
        .await
        .unwrap();
    assert!(!f.last_script().unwrap().contains("/private/etc/hosts"));
}

#[test]
fn suggested_zones_are_private_most_specific_first_and_never_reserved() {
    assert_eq!(
        suggested_zones("API.Lab.Example.Internal"),
        vec!["lab.example.internal"]
    );
    assert!(suggested_zones("host.example.com").is_empty());
    assert!(suggested_zones("m1.cka.clusterdeck.local").is_empty());
}

#[tokio::test(start_paused = true)]
async fn concurrent_reapply_ordering_last_request_wins() {
    let f = Fake::new();
    let paths = f.cd_paths();
    let p = profile("cka", HostMapping::LocalDns, &["lab.example.internal"]);
    store::upsert_profile(&paths, p.clone()).unwrap();

    let ep1 = vec![DiscoveredEndpoint {
        host: "first.lab.example.internal".into(),
        ip: "192.0.2.11".into(),
        source: "ingress".into(),
        resource_name: "ing1".into(),
    }];
    let ep2 = vec![DiscoveredEndpoint {
        host: "second.lab.example.internal".into(),
        ip: "192.0.2.22".into(),
        source: "ingress".into(),
        resource_name: "ing2".into(),
    }];

    let env1 = f.env();
    let env2 = f.env();
    let (res1, res2) = tokio::join!(
        reapply(&paths, &env1, None, None, Some(("cka", &ep1))),
        reapply(&paths, &env2, None, None, Some(("cka", &ep2))),
    );
    res1.unwrap();
    res2.unwrap();

    let cache = cache_load(&cache_path(&f.env()));
    let cached_endpoints = cache.get("cka").unwrap();
    let served = f.served();
    let lab_zone = served
        .iter()
        .find(|(z, _)| z == "lab.example.internal")
        .unwrap();
    assert_eq!(lab_zone.1.len(), cached_endpoints.len());
    for ep in cached_endpoints {
        assert!(lab_zone.1.contains(&ep.host));
    }
}

#[tokio::test(start_paused = true)]
async fn lane_mutex_serializes_reapply() {
    let f = Fake::new();
    let paths = f.cd_paths();
    let p = profile("cka", HostMapping::LocalDns, &["lab.example.internal"]);
    store::upsert_profile(&paths, p).unwrap();

    let guard = lock_lane().await;

    let paths_clone = paths.clone();
    let f_clone = Arc::clone(&f);
    let handle =
        tokio::spawn(async move { reapply(&paths_clone, &f_clone.env(), None, None, None).await });

    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    assert!(
        !handle.is_finished(),
        "reapply must block while lane mutex is held"
    );
    assert_eq!(
        f.total_calls(),
        0,
        "no manager operations should start while lane mutex is held"
    );

    drop(guard);
    let res = handle
        .await
        .expect("task panicked")
        .expect("reapply failed");
    assert!(!res.cancelled);
}

#[tokio::test(start_paused = true)]
async fn lane_mutex_serializes_startup_with() {
    let f = Fake::new();
    let paths = f.cd_paths();

    let guard = lock_lane().await;

    let paths_clone = paths.clone();
    let f_clone = Arc::clone(&f);
    let handle = tokio::spawn(async move { startup_with(&paths_clone, &f_clone.env()).await });

    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    assert!(
        !handle.is_finished(),
        "startup_with must block while lane mutex is held"
    );

    drop(guard);
    handle
        .await
        .expect("task panicked")
        .expect("startup_with failed");
}

#[tokio::test(start_paused = true)]
async fn startup_sweep_does_not_delete_files_written_by_prior_apply() {
    let f = Fake::new();
    let paths = f.cd_paths();
    let p = profile("cka", HostMapping::LocalDns, &[]);
    store::upsert_profile(&paths, p.clone()).unwrap();

    reapply(&paths, &f.env(), None, None, None).await.unwrap();

    let resolver_file = f.resolver.join("cka.clusterdeck.local");
    std::fs::write(
        &resolver_file,
        render_resolver_file("cka", DEFAULT_DNS_PORT).unwrap(),
    )
    .unwrap();

    let osascript_before = f.count("osascript");

    startup_with(&paths, &f.env()).await.unwrap();
    assert!(
        resolver_file.exists(),
        "startup sweep must not delete active resolver file"
    );
    assert_eq!(
        f.count("osascript"),
        osascript_before,
        "startup sweep must not prompt to delete resolver files when local DNS is active"
    );
}

#[tokio::test(start_paused = true)]
async fn host_edit_triggers_reapply() {
    let f = Fake::new();
    let paths = f.cd_paths();
    let old = profile("cka", HostMapping::LocalDns, &[]);
    store::upsert_profile(&paths, old.clone()).unwrap();
    reapply(&paths, &f.env(), None, None, None).await.unwrap();

    // 1. Host address edit
    let mut edited_host = old.clone();
    edited_host.hosts[0].address = "192.0.2.99".into();
    store::upsert_profile(&paths, edited_host.clone()).unwrap();

    let out = reapply_after_save(&paths, &f.env(), &f.hosts, Some(&old), &edited_host)
        .await
        .unwrap();
    assert!(out.is_some(), "host address edit must trigger reapply");

    // 2. Host name edit
    let mut edited_name = old.clone();
    edited_name.hosts[0].name = "m2".into();
    store::upsert_profile(&paths, edited_name.clone()).unwrap();
    let out_name = reapply_after_save(&paths, &f.env(), &f.hosts, Some(&old), &edited_name)
        .await
        .unwrap();
    assert!(out_name.is_some(), "host name edit must trigger reapply");

    // 3. Bastion edit
    let mut edited_bastion = old.clone();
    edited_bastion.bastion = Some(crate::services::config::Bastion {
        name: "jump".into(),
        address: "192.0.2.100".into(),
        port: 22,
        user: "root".into(),
        identity_file: None,
    });
    store::upsert_profile(&paths, edited_bastion.clone()).unwrap();

    let out_bastion = reapply_after_save(&paths, &f.env(), &f.hosts, Some(&old), &edited_bastion)
        .await
        .unwrap();
    assert!(out_bastion.is_some(), "bastion edit must trigger reapply");
}

#[tokio::test(start_paused = true)]
async fn cancel_returns_warning() {
    let f = Fake::new();
    let paths = f.cd_paths();
    let p = profile("cka", HostMapping::LocalDns, &["lab.example.internal"]);
    store::upsert_profile(&paths, p.clone()).unwrap();

    f.set_cancel_osascript(true);
    let view = reapply(&paths, &f.env(), None, None, None).await.unwrap();
    assert!(
        view.cancelled,
        "cancelling admin prompt must set cancelled=true"
    );
    assert!(
        view.prompted,
        "cancelled admin prompt must set prompted=true"
    );

    let sync_res = sync_local_dns_profile(&paths, &f.env(), &f.hosts, &p, Ok(vec![]))
        .await
        .unwrap();
    assert!(!sync_res.success, "cancelled sync must not report success");
    assert!(sync_res.message.contains("cancelled"));
}

#[tokio::test(start_paused = true)]
async fn local_dns_to_hosts_switch() {
    let f = Fake::new();
    let paths = f.cd_paths();
    let old = profile("cka", HostMapping::LocalDns, &[]);
    store::upsert_profile(&paths, old.clone()).unwrap();

    let ep = vec![DiscoveredEndpoint {
        host: "app.lab.example.internal".into(),
        ip: "192.0.2.33".into(),
        source: "ingress".into(),
        resource_name: "ing".into(),
    }];
    sync_local_dns_profile(&paths, &f.env(), &f.hosts, &old, Ok(ep))
        .await
        .unwrap();

    std::fs::write(
        f.resolver.join("cka.clusterdeck.local"),
        render_resolver_file("cka", DEFAULT_DNS_PORT).unwrap(),
    )
    .unwrap();

    // Switch to Hosts mapping
    let mut new = old.clone();
    new.host_mapping = HostMapping::Hosts;
    store::upsert_profile(&paths, new.clone()).unwrap();

    let out = reapply_after_save(&paths, &f.env(), &f.hosts, Some(&old), &new)
        .await
        .unwrap();
    assert!(out.is_some());

    let hosts_content = std::fs::read_to_string(&f.hosts).unwrap();
    assert!(
        hosts_content.contains(">>> ClusterDeck BEGIN (profile: cka) >>>"),
        "hosts file must contain cka block: {hosts_content}"
    );
    assert!(
        hosts_content.contains("192.0.2.33"),
        "hosts file must have cached endpoint IP: {hosts_content}"
    );
    assert!(
        hosts_content.contains("app.lab.example.internal"),
        "hosts file must have cached endpoint host: {hosts_content}"
    );
    let script = f.last_script().expect("osascript was run");
    assert!(
        script.contains("/private/etc/hosts"),
        "script must update hosts: {script}"
    );
    assert!(
        script.contains("cka.clusterdeck.local"),
        "script must remove local DNS resolver file: {script}"
    );
    assert!(
        !f.agent_paths.plist.exists(),
        "agent removed when last LocalDns profile switched away"
    );
}

#[tokio::test(start_paused = true)]
async fn saving_hosts_profile_works_when_system_dns_resolution_would_fail() {
    let f = Fake::new();
    let paths = f.cd_paths();
    let guard = store::ProfileWriteGuard;

    let hosts_profile = profile("ckh", HostMapping::Hosts, &[]);
    let failing_dns = || Err("simulated SystemDns::resolve failure".to_string());

    let res = crate::commands::profiles::save_profile_internal(
        &paths,
        &guard,
        hosts_profile.clone(),
        failing_dns,
        &f.hosts,
    )
    .await;
    assert!(
        res.is_ok(),
        "saving Hosts profile must succeed even if SystemDns resolution fails"
    );
    assert!(res.unwrap().is_none());

    let local_dns_profile = profile("ckl", HostMapping::LocalDns, &[]);
    let failing_dns_again = || Err("simulated SystemDns::resolve failure".to_string());
    let err_res = crate::commands::profiles::save_profile_internal(
        &paths,
        &guard,
        local_dns_profile,
        failing_dns_again,
        &f.hosts,
    )
    .await;
    assert!(
        err_res.is_err(),
        "saving LocalDns profile must fail if SystemDns resolution fails"
    );
    assert!(err_res
        .unwrap_err()
        .contains("simulated SystemDns::resolve failure"));
}

#[tokio::test(start_paused = true)]
async fn sync_racing_delete_does_not_reinsert_into_cache() {
    let f = Fake::new();
    let paths = f.cd_paths();

    let ep = vec![DiscoveredEndpoint {
        host: "app.lab.example.internal".into(),
        ip: "192.0.2.33".into(),
        source: "ingress".into(),
        resource_name: "ing".into(),
    }];

    // Profile "cka" does NOT exist in store
    reapply(&paths, &f.env(), None, None, Some(("cka", &ep)))
        .await
        .unwrap();

    let cache = cache_load(&cache_path(&f.env()));
    assert!(
        !cache.contains_key("cka"),
        "deleted profile must not be re-inserted into endpoints cache"
    );
}

#[tokio::test(start_paused = true)]
async fn connect_refreshes_local_dns_and_updates_cache() {
    let f = Fake::new();
    let paths = f.cd_paths();
    let p = profile("cka", HostMapping::LocalDns, &["lab.example.internal"]);
    store::upsert_profile(&paths, p.clone()).unwrap();

    // First, sync to establish the resolver files so zones are known.
    sync_local_dns_profile(&paths, &f.env(), &f.hosts, &p, Ok(vec![]))
        .await
        .unwrap();
    let initial_prompts = f.count("osascript");
    assert_eq!(initial_prompts, 1);

    let endpoints = vec![ep("api.lab.example.internal", "192.0.2.60")];
    let mut errors = Vec::new();

    refresh_local_dns_on_connect(&paths, &f.env(), &p.id, Ok(&endpoints), &mut errors).await;

    assert!(
        errors.is_empty(),
        "expected no errors on connect refresh: {errors:?}"
    );
    let cache = cache_load(&cache_path(&f.env()));
    assert_eq!(
        cache.get("cka").map(|v| v.as_slice()),
        Some(endpoints.as_slice())
    );
    assert_eq!(
        records_of(&f, "lab.example.internal"),
        vec!["api.lab.example.internal"]
    );
    assert_eq!(
        f.count("osascript"),
        initial_prompts,
        "Connect with unchanged zones must not trigger an admin prompt"
    );
}

#[tokio::test(start_paused = true)]
async fn connect_discovery_result_branches() {
    let f = Fake::new();
    let paths = f.cd_paths();
    let p = profile("cka", HostMapping::LocalDns, &["lab.example.internal"]);
    store::upsert_profile(&paths, p.clone()).unwrap();

    // First sync to establish the resolver files so zones are installed.
    sync_local_dns_profile(&paths, &f.env(), &f.hosts, &p, Ok(vec![]))
        .await
        .unwrap();

    // 1. Success branch with endpoints: cache updated, records served, no errors
    let eps = vec![ep("api.lab.example.internal", "192.0.2.60")];
    let mut errors = Vec::new();
    refresh_local_dns_on_connect(&paths, &f.env(), &p.id, Ok(&eps), &mut errors).await;
    assert!(errors.is_empty());
    let cache = cache_load(&cache_path(&f.env()));
    assert_eq!(cache.get("cka").map(|v| v.as_slice()), Some(eps.as_slice()));
    assert_eq!(
        records_of(&f, "lab.example.internal"),
        vec!["api.lab.example.internal"]
    );

    // 2. Failure branch: previous cache kept, warning emitted with error text and WITHOUT "(Connect first)"
    let mut fail_errors = Vec::new();
    refresh_local_dns_on_connect(
        &paths,
        &f.env(),
        &p.id,
        Err("api unreachable: dial tcp 10.0.0.1:6443"),
        &mut fail_errors,
    )
    .await;
    assert_eq!(fail_errors.len(), 1);
    assert!(fail_errors[0].contains("Could not discover endpoints; serving the last known records"));
    assert!(fail_errors[0].contains("api unreachable: dial tcp 10.0.0.1:6443"));
    assert!(!fail_errors[0].contains("(Connect first)"));
    let cache_after_fail = cache_load(&cache_path(&f.env()));
    assert_eq!(
        cache_after_fail.get("cka").map(|v| v.as_slice()),
        Some(eps.as_slice())
    );

    // 3. Empty branch (e.g. successful discovery with 0 endpoints): cache stores [], no errors
    let mut empty_errors = Vec::new();
    refresh_local_dns_on_connect(&paths, &f.env(), &p.id, Ok(&[]), &mut empty_errors).await;
    assert!(empty_errors.is_empty());
    let cache_after_empty = cache_load(&cache_path(&f.env()));
    assert_eq!(
        cache_after_empty.get("cka").map(|v| v.as_slice()),
        Some([].as_slice())
    );
    assert!(records_of(&f, "lab.example.internal").is_empty());
}

#[tokio::test(start_paused = true)]
async fn connect_local_dns_is_non_fatal_on_cancel_or_error() {
    let f = Fake::new();
    let paths = f.cd_paths();
    let p = profile("cka", HostMapping::LocalDns, &["lab.example.internal"]);
    store::upsert_profile(&paths, p.clone()).unwrap();

    f.set_cancel_osascript(true);
    let mut errors = Vec::new();

    // Resolver files are not installed; Connect must not prompt and must report pending state without crashing.
    refresh_local_dns_on_connect(
        &paths,
        &f.env(),
        &p.id,
        Ok(&[ep("api.lab.example.internal", "192.0.2.60")]),
        &mut errors,
    )
    .await;

    assert_eq!(errors.len(), 1);
    assert!(errors[0].contains("Local DNS changes pending: press Sync to local DNS"));
    assert_eq!(
        f.count("osascript"),
        0,
        "must not prompt for uninstalled resolver files"
    );
}

#[tokio::test(start_paused = true)]
async fn connect_leaves_hosts_and_off_profiles_untouched() {
    let f = Fake::new();
    let paths = f.cd_paths();
    let p_hosts = profile("ckh", HostMapping::Hosts, &["lab.example.internal"]);
    store::upsert_profile(&paths, p_hosts.clone()).unwrap();

    let mut errors = Vec::new();
    refresh_local_dns_on_connect(
        &paths,
        &f.env(),
        &p_hosts.id,
        Ok(&[ep("api.lab.example.internal", "192.0.2.60")]),
        &mut errors,
    )
    .await;

    assert!(errors.is_empty());
    let cache = cache_load(&cache_path(&f.env()));
    assert!(!cache.contains_key("ckh"));
    assert_eq!(f.count("osascript"), 0);
    assert!(f.served().is_empty());
}

#[tokio::test(start_paused = true)]
async fn connect_profile_switched_to_hosts_results_in_no_reapply_and_no_prompt() {
    let f = Fake::new();
    let paths = f.cd_paths();
    // Simulate user switching profile from LocalDns to Hosts during Connect
    let p_hosts = profile("cka", HostMapping::Hosts, &["lab.example.internal"]);
    store::upsert_profile(&paths, p_hosts).unwrap();

    let mut errors = Vec::new();
    refresh_local_dns_on_connect(
        &paths,
        &f.env(),
        "cka",
        Ok(&[ep("api.lab.example.internal", "192.0.2.60")]),
        &mut errors,
    )
    .await;

    assert!(errors.is_empty());
    let cache = cache_load(&cache_path(&f.env()));
    assert!(
        !cache.contains_key("cka"),
        "profile switched to Hosts must not touch endpoints cache"
    );
    assert_eq!(
        f.count("osascript"),
        0,
        "profile switched to Hosts must not trigger admin prompt"
    );
    assert!(f.served().is_empty());
}

#[tokio::test(start_paused = true)]
async fn connect_profile_switched_to_localdns_records_pending_without_prompt() {
    let f = Fake::new();
    let paths = f.cd_paths();
    // Simulate user switching profile from Hosts to LocalDns during Connect
    let p_localdns = profile("cka", HostMapping::LocalDns, &["lab.example.internal"]);
    store::upsert_profile(&paths, p_localdns).unwrap();

    let eps = vec![ep("api.lab.example.internal", "192.0.2.60")];
    let mut errors = Vec::new();
    refresh_local_dns_on_connect(&paths, &f.env(), "cka", Ok(&eps), &mut errors).await;

    assert_eq!(errors.len(), 1);
    assert!(errors[0].contains("Local DNS changes pending: press Sync to local DNS"));
    assert_eq!(
        f.count("osascript"),
        0,
        "Connect must never prompt for uninstalled resolver files"
    );
    let cache = cache_load(&cache_path(&f.env()));
    assert_eq!(
        cache.get("cka").map(|v| v.as_slice()),
        Some(eps.as_slice()),
        "endpoints are cached so sync can serve them immediately"
    );
}

#[tokio::test(start_paused = true)]
async fn sync_without_kubeconfig_gives_no_warning() {
    let f = Fake::new();
    let paths = f.cd_paths();
    let mut p = profile("cka", HostMapping::LocalDns, &["lab.example.internal"]);
    p.kubeconfig = None;
    store::upsert_profile(&paths, p.clone()).unwrap();

    let res = sync_local_dns_profile(&paths, &f.env(), &f.hosts, &p, Ok(vec![]))
        .await
        .unwrap();

    assert!(res.success, "sync without kubeconfig must report success");
    assert!(
        res.warning.is_none(),
        "sync without kubeconfig must have no warning"
    );
    assert_ne!(res.message, DISCOVERY_FAILED_WARNING);
    assert!(!res.message.contains("Could not discover"));
}

#[tokio::test(start_paused = true)]
async fn failed_discovery_result_carries_warning_and_error_text() {
    let f = Fake::new();
    let paths = f.cd_paths();
    let p = profile("cka", HostMapping::LocalDns, &["lab.example.internal"]);
    store::upsert_profile(&paths, p.clone()).unwrap();

    // Initial successful sync stores cached record.
    let initial_eps = vec![ep("api.lab.example.internal", "192.0.2.50")];
    sync_local_dns_profile(&paths, &f.env(), &f.hosts, &p, Ok(initial_eps.clone()))
        .await
        .unwrap();

    let cache = cache_load(&cache_path(&f.env()));
    assert_eq!(
        cache.get("cka").map(|v| v.as_slice()),
        Some(initial_eps.as_slice())
    );

    // Sync with discovery failure error text
    let res = sync_local_dns_profile(
        &paths,
        &f.env(),
        &f.hosts,
        &p,
        Err("api cluster.local unreachable: port 6443"),
    )
    .await
    .unwrap();

    assert!(
        !res.success,
        "failed discovery must not claim plain success"
    );
    assert!(
        res.warning.is_some(),
        "failed discovery must set warning field"
    );
    let warning = res.warning.unwrap();
    assert!(warning
        .contains("Could not discover endpoints (Connect first); serving the last known records"));
    assert!(warning.contains("api cluster.local unreachable: port 6443"));
    assert_eq!(res.message, warning);
    assert_eq!(res.endpoints, initial_eps);

    let cache_after_sync = cache_load(&cache_path(&f.env()));
    assert_eq!(
        cache_after_sync.get("cka").map(|v| v.as_slice()),
        Some(initial_eps.as_slice()),
        "cache must be preserved on discovery failure"
    );
    assert_eq!(
        records_of(&f, "lab.example.internal"),
        vec!["api.lab.example.internal"]
    );
    assert!(zone_names(&f).contains(&"cka.clusterdeck.local".to_string()));
}

#[tokio::test(start_paused = true)]
async fn failed_discovery_without_connection_down_shows_discovery_error() {
    let f = Fake::new();
    let paths = f.cd_paths();
    let p = profile("cka", HostMapping::LocalDns, &["lab.example.internal"]);
    store::upsert_profile(&paths, p.clone()).unwrap();

    let initial_eps = vec![ep("api.lab.example.internal", "192.0.2.50")];
    sync_local_dns_profile(&paths, &f.env(), &f.hosts, &p, Ok(initial_eps.clone()))
        .await
        .unwrap();

    // Discovery fails because of RBAC forbidden / api error, NOT connection down
    let res = sync_local_dns_profile(
        &paths,
        &f.env(),
        &f.hosts,
        &p,
        Err("services is forbidden: User system:serviceaccount cannot list resource services"),
    )
    .await
    .unwrap();

    assert!(!res.success);
    let warning = res.warning.expect("warning should be present");
    assert!(
        warning.contains("Endpoint discovery failed: services is forbidden: User system:serviceaccount cannot list resource services; serving the last known records."),
        "warning was: {warning}"
    );
    assert!(
        !warning.contains("Connect first"),
        "warning must not say Connect first when connection is not down"
    );
    assert_eq!(res.message, warning);
    assert_eq!(res.endpoints, initial_eps);
}

#[tokio::test(start_paused = true)]
async fn empty_successful_discovery_stores_empty() {
    let f = Fake::new();
    let paths = f.cd_paths();
    let p = profile("cka", HostMapping::LocalDns, &["lab.example.internal"]);
    store::upsert_profile(&paths, p.clone()).unwrap();

    // Initial sync with records.
    let initial_eps = vec![ep("api.lab.example.internal", "192.0.2.50")];
    sync_local_dns_profile(&paths, &f.env(), &f.hosts, &p, Ok(initial_eps))
        .await
        .unwrap();

    // Now empty successful discovery.
    let res = sync_local_dns_profile(&paths, &f.env(), &f.hosts, &p, Ok(vec![]))
        .await
        .unwrap();

    assert!(res.success);
    assert!(res.warning.is_none());
    assert_ne!(res.message, DISCOVERY_FAILED_WARNING);
    assert!(res.endpoints.is_empty());
    let cache = cache_load(&cache_path(&f.env()));
    assert_eq!(
        cache.get("cka").map(|v| v.as_slice()),
        Some([].as_slice()),
        "cache must store [] on successful empty discovery"
    );
    assert!(records_of(&f, "lab.example.internal").is_empty());
    // Node alias zone is still served
    assert!(zone_names(&f).contains(&"cka.clusterdeck.local".to_string()));
}

#[test]
fn suggest_dns_zones_command_deduplicates_and_maps_by_host() {
    let res = suggest_dns_zones(vec![
        "app.lab.example.internal".into(),
        "api.lab.example.internal".into(),
        "metrics.prod.example.internal".into(),
        "external.com".into(),
    ]);

    assert_eq!(
        res.zones,
        vec!["lab.example.internal", "prod.example.internal"]
    );
    assert_eq!(
        res.by_host.get("app.lab.example.internal"),
        Some(&vec!["lab.example.internal".to_string()])
    );
    assert_eq!(
        res.by_host.get("metrics.prod.example.internal"),
        Some(&vec!["prod.example.internal".to_string()])
    );
    assert_eq!(res.by_host.get("external.com"), Some(&vec![]));
}

#[test]
fn suggest_dns_zones_caps_input_and_drops_invalid_hosts() {
    let mut hosts = Vec::new();
    for i in 0..1005 {
        hosts.push(format!("app{i}.lab.example.internal"));
    }
    hosts.push("invalid domain with spaces".to_string());
    hosts.push("../malicious.example.internal".to_string());
    hosts.push("".to_string());
    hosts.push("foo;bar".to_string());

    let res = suggest_dns_zones(hosts);
    assert_eq!(
        res.by_host.len(),
        1000,
        "must cap input to at most 1000 hosts"
    );
    assert!(!res.by_host.contains_key("invalid domain with spaces"));
    assert!(!res.by_host.contains_key("../malicious.example.internal"));
    assert!(!res.by_host.contains_key(""));
    assert!(!res.by_host.contains_key("foo;bar"));
}

#[tokio::test(start_paused = true)]
async fn critic_partial_discovery_erases_last_known_records() {
    struct Partial;
    #[async_trait::async_trait]
    impl crate::services::process::CommandRunner for Partial {
        async fn run(
            &self,
            bin: &str,
            args: &[String],
        ) -> Result<crate::services::process::CommandOutput, String> {
            if bin == "kubectl" && args.last().map(String::as_str) == Some("/api/v1/services") {
                Ok(crate::services::process::CommandOutput {
                    stdout: "{\"items\":[]}".into(),
                    stderr: String::new(),
                    success: true,
                })
            } else {
                Err("connection reset".into())
            }
        }
    }
    let f = Fake::new();
    let paths = f.cd_paths();
    let p = profile("cka", HostMapping::LocalDns, &["lab.example.internal"]);
    store::upsert_profile(&paths, p.clone()).unwrap();
    sync_local_dns_profile(
        &paths,
        &f.env(),
        &f.hosts,
        &p,
        Ok(vec![ep("api.lab.example.internal", "192.0.2.60")]),
    )
    .await
    .unwrap();
    let detailed = crate::services::k8s_endpoints::discover_cluster_endpoints_detailed(
        &Partial,
        &f.home.join("missing-kubeconfig"),
        None,
        None,
    )
    .await
    .unwrap();
    assert!(detailed.endpoints.is_empty());
    assert_eq!(detailed.succeeded_kinds, vec!["service"]);
    assert!(!detailed.failed_kinds.is_empty());

    let mut errors = vec![];
    refresh_local_dns_on_connect_detailed(&paths, &f.env(), "cka", Ok(&detailed), &mut errors)
        .await;

    assert!(
        errors
            .iter()
            .any(|e| e.contains("Partial endpoint discovery: failed querying")),
        "errors should warn about failed queries: {errors:?}"
    );
    assert_eq!(
        records_of(&f, "lab.example.internal"),
        vec!["api.lab.example.internal"],
        "records of failed kinds must be preserved"
    );
}

#[tokio::test(start_paused = true)]
async fn critic_stale_save_reintroduces_hosts_after_off() {
    let f = Fake::new();
    let paths = f.cd_paths();
    let old = profile("cka", HostMapping::LocalDns, &[]);
    store::upsert_profile(&paths, old.clone()).unwrap();
    sync_local_dns_profile(&paths, &f.env(), &f.hosts, &old, Ok(vec![]))
        .await
        .unwrap();
    let mut intermediate = old.clone();
    intermediate.host_mapping = HostMapping::Hosts;
    store::upsert_profile(&paths, intermediate.clone()).unwrap();
    // A second save completes while the first is between the write lock and DNS lane lock.
    let mut latest = intermediate.clone();
    latest.host_mapping = HostMapping::Off;
    store::upsert_profile(&paths, latest).unwrap();
    reapply_after_save(&paths, &f.env(), &f.hosts, Some(&old), &intermediate)
        .await
        .unwrap();
    assert_eq!(
        store::get_profile(&paths, "cka").unwrap().host_mapping,
        HostMapping::Off
    );
    assert!(!std::fs::read_to_string(&f.hosts)
        .unwrap()
        .contains("ClusterDeck BEGIN"));
}

#[tokio::test(start_paused = true)]
async fn critic_scan_approval_has_no_records() {
    let f = Fake::new();
    let paths = f.cd_paths();
    let mut p = profile("cka", HostMapping::LocalDns, &[]);
    store::upsert_profile(&paths, p.clone()).unwrap();
    // Scan returns data to React only; approve mutates the store and reapplies with no record.
    p.approved_dns_zones.push("lab.example.internal".into());
    store::upsert_profile(&paths, p).unwrap();
    let view = reapply(&paths, &f.env(), None, None, None).await.unwrap();
    assert!(!view.cancelled && view.conflicts.is_empty());
    assert_eq!(
        view.zone_records.get("lab.example.internal").copied(),
        Some(0)
    );
    assert!(records_of(&f, "lab.example.internal").is_empty());
}

#[tokio::test(start_paused = true)]
async fn critic_connect_retries_cancelled_approval_prompt() {
    let f = Fake::new();
    let paths = f.cd_paths();
    let p = profile("cka", HostMapping::LocalDns, &[]);
    store::upsert_profile(&paths, p).unwrap();
    f.set_cancel_osascript(true);
    let cancelled = reapply(&paths, &f.env(), None, None, None).await.unwrap();
    assert!(cancelled.cancelled);
    let before = f.count("osascript");
    let mut errors = vec![];
    refresh_local_dns_on_connect(&paths, &f.env(), "cka", Ok(&[]), &mut errors).await;
    assert_eq!(
        f.count("osascript"),
        before,
        "Connect must not retry prompt"
    );
    assert!(
        errors
            .iter()
            .any(|e| e.contains("Local DNS changes pending: press Sync to local DNS")),
        "must report pending state: {errors:?}"
    );
}

#[tokio::test(start_paused = true)]
async fn scan_stores_endpoints_in_cache_for_subsequent_approval() {
    let f = Fake::new();
    let paths = f.cd_paths();
    let mut p = profile("cka", HostMapping::LocalDns, &[]);
    store::upsert_profile(&paths, p.clone()).unwrap();

    // Scan persists validated endpoints into cache
    let eps = vec![ep("api.lab.example.internal", "192.0.2.50")];
    update_endpoint_cache(&cache_path(&f.env()), "cka", &eps, None).unwrap();

    // Now user approves the zone
    p.approved_dns_zones.push("lab.example.internal".into());
    store::upsert_profile(&paths, p).unwrap();
    let view = reapply(&paths, &f.env(), None, None, None).await.unwrap();
    assert_eq!(
        view.zone_records.get("lab.example.internal").copied(),
        Some(1)
    );
    assert_eq!(
        records_of(&f, "lab.example.internal"),
        vec!["api.lab.example.internal"]
    );
}

#[tokio::test(start_paused = true)]
async fn rereview_connect_after_cancelled_revoke_must_not_prompt() {
    let f = Fake::new();
    let paths = f.cd_paths();
    let mut p = profile("cka", HostMapping::LocalDns, &["lab.example.internal"]);
    store::upsert_profile(&paths, p.clone()).unwrap();
    reapply(&paths, &f.env(), None, None, None).await.unwrap();
    p.approved_dns_zones.clear();
    store::upsert_profile(&paths, p).unwrap();
    f.set_cancel_osascript(true);
    assert!(
        reapply(&paths, &f.env(), None, None, None)
            .await
            .unwrap()
            .cancelled
    );
    let before = f.count("osascript");
    let mut errors = vec![];
    refresh_local_dns_on_connect(&paths, &f.env(), "cka", Ok(&[]), &mut errors).await;
    assert_eq!(
        f.count("osascript"),
        before,
        "Connect retried cancelled revoke: {errors:?}"
    );
}

#[tokio::test(start_paused = true)]
async fn rereview_forbidden_must_preserve_cached_ingress() {
    struct Forbidden;
    #[async_trait::async_trait]
    impl crate::services::process::CommandRunner for Forbidden {
        async fn run(
            &self,
            bin: &str,
            args: &[String],
        ) -> Result<crate::services::process::CommandOutput, String> {
            let services = args.last().is_some_and(|s| s.ends_with("/api/v1/services"));
            Ok(crate::services::process::CommandOutput {
                success: services || bin == "curl",
                stdout: if services {
                    r#"{"items":[]}"#
                } else {
                    r#"{"kind":"Status","status":"Failure","reason":"Forbidden","code":403}"#
                }
                .into(),
                stderr: String::new(),
            })
        }
    }
    let f = Fake::new();
    let paths = f.cd_paths();
    let p = profile("cka", HostMapping::LocalDns, &["lab.example.internal"]);
    store::upsert_profile(&paths, p).unwrap();
    update_endpoint_cache(
        &cache_path(&f.env()),
        "cka",
        &[ep("api.lab.example.internal", "192.0.2.60")],
        None,
    )
    .unwrap();
    let kc = f.home.join("test-kubeconfig");
    std::fs::write(
        &kc,
        "clusters:\n- cluster:\n    server: https://192.0.2.1:6443\n",
    )
    .unwrap();
    let res = crate::services::k8s_endpoints::discover_cluster_endpoints_detailed(
        &Forbidden, &kc, None, None,
    )
    .await
    .unwrap();
    let mut errors = vec![];
    refresh_local_dns_on_connect_detailed(&paths, &f.env(), "cka", Ok(&res), &mut errors).await;
    assert_eq!(
        cache_load(&cache_path(&f.env()))["cka"].len(),
        1,
        "Forbidden classified as success: {res:?}"
    );
    assert!(res.failed_kinds.contains(&"ingress".to_string()));
}

#[tokio::test(start_paused = true)]
async fn malformed_list_must_preserve_cached_records() {
    struct MalformedRunner;
    #[async_trait::async_trait]
    impl crate::services::process::CommandRunner for MalformedRunner {
        async fn run(
            &self,
            bin: &str,
            args: &[String],
        ) -> Result<crate::services::process::CommandOutput, String> {
            let services = args.last().is_some_and(|s| s.ends_with("/api/v1/services"));
            Ok(crate::services::process::CommandOutput {
                success: services || bin == "curl",
                stdout: if services {
                    r#"{"items":[]}"#
                } else {
                    r#"{"kind":"IngressList","metadata":{}}"#
                }
                .into(),
                stderr: String::new(),
            })
        }
    }
    let f = Fake::new();
    let paths = f.cd_paths();
    let p = profile("cka", HostMapping::LocalDns, &["lab.example.internal"]);
    store::upsert_profile(&paths, p).unwrap();
    update_endpoint_cache(
        &cache_path(&f.env()),
        "cka",
        &[ep("api.lab.example.internal", "192.0.2.60")],
        None,
    )
    .unwrap();
    let kc = f.home.join("test-kubeconfig-malformed");
    std::fs::write(
        &kc,
        "clusters:\n- cluster:\n    server: https://192.0.2.1:6443\n",
    )
    .unwrap();
    let res = crate::services::k8s_endpoints::discover_cluster_endpoints_detailed(
        &MalformedRunner,
        &kc,
        None,
        None,
    )
    .await
    .unwrap();
    let mut errors = vec![];
    refresh_local_dns_on_connect_detailed(&paths, &f.env(), "cka", Ok(&res), &mut errors).await;
    assert_eq!(
        cache_load(&cache_path(&f.env()))["cka"].len(),
        1,
        "Malformed list classified as success: {res:?}"
    );
    assert!(res.failed_kinds.contains(&"ingress".to_string()));
}
