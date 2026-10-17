//! Fix round 4 (#99): regression tests for the independent review of fdacf4a.

use super::test_support::{ep, profile, Fake};
use super::*;
use crate::commands::connection::{
    discover_cluster_endpoints_internal, merge_discovered_endpoints, sync_hosts_file_with_deps,
    write_connect_hosts_block,
};
use crate::services::k8s_endpoints::{discover_cluster_endpoints_detailed, DiscoveryResult};
use crate::services::process::{CommandOutput, CommandRunner};
use async_trait::async_trait;
use std::sync::atomic::{AtomicBool, Ordering};

fn out(success: bool, stdout: &str, stderr: &str) -> Result<CommandOutput, String> {
    Ok(CommandOutput {
        stdout: stdout.into(),
        stderr: stderr.into(),
        success,
    })
}

fn put_hosts_block(f: &Fake, p: &Profile, eps: &[DiscoveredEndpoint]) {
    let block = render_hosts_block_with_endpoints(p, eps).unwrap();
    std::fs::write(
        &f.hosts,
        compute_updated_hosts_content("127.0.0.1 localhost\n", &p.id, Some(&block)),
    )
    .unwrap();
}

fn has_block(f: &Fake) -> bool {
    std::fs::read_to_string(&f.hosts)
        .unwrap()
        .contains("ClusterDeck BEGIN")
}

fn write_kubeconfig(paths: &ClusterDeckPaths, id: &str) {
    paths.ensure_dirs().unwrap();
    std::fs::create_dir_all(paths.base.join("dns")).unwrap();
    std::fs::write(
        paths.kubeconfig_file(id),
        "clusters:\n- cluster:\n    server: https://192.0.2.1:6443\n",
    )
    .unwrap();
}

type Reply = Box<dyn Fn(&str) -> Result<CommandOutput, String> + Send + Sync>;

/// Delegates everything but kubectl/curl to the fake (osascript must still edit the hosts file).
struct Disc {
    fake: Arc<Fake>,
    kubectl: Reply,
    curl: Reply,
}

#[async_trait]
impl CommandRunner for Disc {
    async fn run(&self, bin: &str, args: &[String]) -> Result<CommandOutput, String> {
        let path = args.last().map(String::as_str).unwrap_or("");
        match bin {
            "kubectl" => (self.kubectl)(path),
            "curl" => (self.curl)(path),
            _ => self.fake.run(bin, args).await,
        }
    }
}

fn unreachable_cluster(f: &Arc<Fake>) -> Disc {
    Disc {
        fake: Arc::clone(f),
        kubectl: Box::new(|_| out(false, "", "Unable to connect to the server")),
        curl: Box::new(|_| out(false, "", "curl: (7) connection refused")),
    }
}

// ---------- 1: a save never prompts for another profile's pending zones ----------

async fn cancelled_pending_b(f: &Fake, paths: &ClusterDeckPaths) {
    let b = profile("ckb", HostMapping::LocalDns, &["lab.example.internal"]);
    store::upsert_profile(paths, b).unwrap();
    f.set_cancel_osascript(true);
    assert!(
        reapply(paths, &f.env(), None, None, None)
            .await
            .unwrap()
            .cancelled
    );
}

#[tokio::test(start_paused = true)]
async fn r4_item1_hosts_rename_save_does_not_prompt_for_other_profiles_zones() {
    let f = Fake::new();
    let paths = f.cd_paths();
    cancelled_pending_b(&f, &paths).await;
    let before = f.count("osascript");

    let a = profile("cka", HostMapping::Hosts, &[]);
    store::upsert_profile(&paths, a.clone()).unwrap();
    put_hosts_block(&f, &a, &[]);
    let mut renamed = a.clone();
    renamed.name = "renamed".into();
    store::upsert_profile(&paths, renamed.clone()).unwrap();

    let res = reapply_after_save(&paths, &f.env(), &f.hosts, Some(&a), &renamed)
        .await
        .unwrap();
    assert!(res.is_none(), "a Hosts->Hosts rename needs no DNS work");
    assert_eq!(
        f.count("osascript"),
        before,
        "Hosts rename prompted for profile B's pending zone"
    );

    // The production entry point must not even resolve the DNS environment.
    let guard = store::ProfileWriteGuard;
    let mut renamed2 = renamed.clone();
    renamed2.name = "renamed-again".into();
    let res = crate::commands::profiles::save_profile_internal(
        &paths,
        &guard,
        renamed2,
        || Err("must not resolve DNS for a Hosts->Hosts save".to_string()),
        &f.hosts,
    )
    .await;
    assert!(res.is_ok(), "save_profile_internal: {res:?}");
}

#[tokio::test(start_paused = true)]
async fn r4_item1_local_dns_record_edit_save_does_not_prompt_for_other_profiles_zones() {
    let f = Fake::new();
    let paths = f.cd_paths();
    let a = profile("cka", HostMapping::LocalDns, &[]);
    store::upsert_profile(&paths, a.clone()).unwrap();
    reapply(&paths, &f.env(), None, None, None).await.unwrap();
    cancelled_pending_b(&f, &paths).await;
    let before = f.count("osascript");

    let mut edited = a.clone();
    edited.hosts[0].name = "m2".into();
    store::upsert_profile(&paths, edited.clone()).unwrap();
    let res = reapply_after_save(&paths, &f.env(), &f.hosts, Some(&a), &edited)
        .await
        .unwrap();
    assert!(res.is_some());
    assert_eq!(
        f.count("osascript"),
        before,
        "saving A prompted for profile B's cancelled zone"
    );
    assert!(
        records_of(&f, "cka.clusterdeck.local").contains(&"m2.cka.clusterdeck.local".to_string()),
        "A's own records must still be refreshed: {:?}",
        f.served()
    );
}

// ---------- 2: Connect must not report Ok while the responder is dead ----------

#[tokio::test(start_paused = true)]
async fn r4_item2_connect_reports_dead_responder_without_removing_files() {
    let f = Fake::new();
    let paths = f.cd_paths();
    let p = profile("cka", HostMapping::LocalDns, &["lab.example.internal"]);
    store::upsert_profile(&paths, p.clone()).unwrap();
    sync_local_dns_profile(&paths, &f.env(), &f.hosts, &p, Ok(vec![]))
        .await
        .unwrap();
    assert!(f.resolver.join("lab.example.internal").exists());
    let prompts = f.count("osascript");

    f.run("/bin/launchctl", &["bootout".to_string()])
        .await
        .unwrap();

    let mut errors = vec![];
    refresh_local_dns_on_connect(
        &paths,
        &f.env(),
        "cka",
        Ok(&[ep("api.lab.example.internal", "192.0.2.60")]),
        &mut errors,
    )
    .await;
    assert!(
        errors
            .iter()
            .any(|e| e.contains("Local DNS responder unavailable; press Sync to local DNS")),
        "dead responder was reported as Ok: {errors:?}"
    );
    assert_eq!(f.count("osascript"), prompts, "Connect must never prompt");
    assert!(
        f.resolver.join("lab.example.internal").exists(),
        "removal needs privilege and belongs to Sync"
    );
}

// ---------- 3: total discovery failure in Hosts mode keeps the records ----------

#[tokio::test(start_paused = true)]
async fn r4_item3_hosts_sync_total_discovery_failure_keeps_records() {
    let f = Fake::new();
    let paths = f.cd_paths();
    let p = profile("cka", HostMapping::Hosts, &[]);
    store::upsert_profile(&paths, p.clone()).unwrap();
    write_kubeconfig(&paths, "cka");
    let old = ep("api.lab.example.internal", "192.0.2.60");
    update_endpoint_cache(
        &paths.endpoints_cache_file(),
        "cka",
        std::slice::from_ref(&old),
        None,
    )
    .unwrap();
    put_hosts_block(&f, &p, std::slice::from_ref(&old));

    let runner = unreachable_cluster(&f);
    let res = sync_hosts_file_with_deps(&runner, &paths, &f.hosts, "cka")
        .await
        .unwrap();
    let hosts = std::fs::read_to_string(&f.hosts).unwrap();
    assert!(
        hosts.contains("192.0.2.60 api.lab.example.internal"),
        "total discovery failure wiped the records: {hosts}"
    );
    assert!(!res.success, "total failure must not report success");
    assert!(res.warning.is_some());
}

#[tokio::test(start_paused = true)]
async fn r4_item3_hosts_connect_total_discovery_failure_keeps_records() {
    let f = Fake::new();
    let paths = f.cd_paths();
    let p = profile("cka", HostMapping::Hosts, &[]);
    store::upsert_profile(&paths, p.clone()).unwrap();
    std::fs::create_dir_all(paths.base.join("dns")).unwrap();
    let old = ep("api.lab.example.internal", "192.0.2.60");
    update_endpoint_cache(
        &paths.endpoints_cache_file(),
        "cka",
        std::slice::from_ref(&old),
        None,
    )
    .unwrap();
    put_hosts_block(&f, &p, std::slice::from_ref(&old));

    write_connect_hosts_block(&*f, &paths, &f.hosts, &p, &Err("kubeconfig missing".into()))
        .await
        .unwrap();
    let hosts = std::fs::read_to_string(&f.hosts).unwrap();
    assert!(
        hosts.contains("192.0.2.60 api.lab.example.internal"),
        "Connect wrote an empty record list: {hosts}"
    );
}

// ---------- 4: hosts merge only keeps lines of FAILED kinds ----------

fn hosts_merge_fixture() -> (Arc<Fake>, ClusterDeckPaths, PathBuf) {
    let f = Fake::new();
    let paths = f.cd_paths();
    let cache = f.home.join("merge-cache.json");
    let mut old_ing = ep("old.lab.example.internal", "192.0.2.60");
    old_ing.source = "ingress".into();
    let mut gw = ep("gw.lab.example.internal", "192.0.2.61");
    gw.source = "gateway-api".into();
    update_endpoint_cache(&cache, "cka", &[old_ing.clone(), gw.clone()], None).unwrap();
    let p = profile("cka", HostMapping::Hosts, &[]);
    put_hosts_block(&f, &p, &[old_ing, gw]);
    (f, paths, cache)
}

fn hosts_of(eps: &[DiscoveredEndpoint]) -> Vec<String> {
    let mut h: Vec<String> = eps.iter().map(|e| e.host.clone()).collect();
    h.sort();
    h
}

#[tokio::test(start_paused = true)]
async fn r4_item4_removed_host_of_a_succeeded_kind_disappears() {
    let (f, _paths, cache) = hosts_merge_fixture();
    let merged = merge_discovered_endpoints(
        &cache,
        &f.hosts,
        "cka",
        &[],
        &["ingress".to_string()],
        &["gateway-api".to_string()],
    );
    assert_eq!(
        hosts_of(&merged),
        vec!["gw.lab.example.internal"],
        "ingress succeeded without old.lab...: it must be gone, the failed gateway kept"
    );
}

#[tokio::test(start_paused = true)]
async fn r4_item4_host_of_a_failed_kind_stays() {
    let (f, _paths, cache) = hosts_merge_fixture();
    let merged = merge_discovered_endpoints(
        &cache,
        &f.hosts,
        "cka",
        &[],
        &["gateway-api".to_string()],
        &["ingress".to_string()],
    );
    assert_eq!(hosts_of(&merged), vec!["old.lab.example.internal"]);
}

// ---------- 5: discovery classification ----------

#[tokio::test(start_paused = true)]
async fn r4_item5a_services_failure_does_not_let_fallback_ips_overwrite_cache() {
    let f = Fake::new();
    let paths = f.cd_paths();
    write_kubeconfig(&paths, "cka");
    let runner = Disc {
        fake: Arc::clone(&f),
        kubectl: Box::new(|path| {
            if path.ends_with("/api/v1/services") {
                out(
                    false,
                    r#"{"kind":"Status","status":"Failure","reason":"InternalError","code":500,"message":"boom"}"#,
                    "",
                )
            } else if path.ends_with("/ingresses") {
                out(
                    true,
                    r#"{"items":[{"metadata":{"name":"i","namespace":"ns"},"spec":{"rules":[{"host":"api.lab.example.internal"}]}}]}"#,
                    "",
                )
            } else {
                out(false, "", "Error from server (NotFound): the server could not find the requested resource")
            }
        }),
        curl: Box::new(|_| out(false, "", "curl: (7) connection refused")),
    };
    let res = discover_cluster_endpoints_detailed(
        &runner,
        &paths.kubeconfig_file("cka"),
        Some("192.0.2.10"),
        None,
    )
    .await
    .unwrap();
    assert!(
        res.failed_kinds.contains(&"ingress".to_string()),
        "ingress took a fallback IP while Services failed: {res:?}"
    );
    let cache = f.home.join("cache-5a.json");
    update_endpoint_cache(
        &cache,
        "cka",
        &[ep("api.lab.example.internal", "192.0.2.60")],
        None,
    )
    .unwrap();
    let updated =
        update_endpoint_cache(&cache, "cka", &res.endpoints, Some(&res.succeeded_kinds)).unwrap();
    assert_eq!(updated.len(), 1);
    assert_eq!(updated[0].ip, "192.0.2.60", "good cached IP overwritten");
}

#[tokio::test(start_paused = true)]
async fn r4_item5b_kubectl_notfound_on_stderr_is_success_empty_even_if_curl_says_401() {
    let f = Fake::new();
    let paths = f.cd_paths();
    write_kubeconfig(&paths, "cka");
    let runner = Disc {
        fake: Arc::clone(&f),
        kubectl: Box::new(|path| {
            if path.ends_with("/api/v1/services") || path.ends_with("/ingresses") {
                out(true, r#"{"items":[]}"#, "")
            } else {
                out(
                    false,
                    "",
                    "Error from server (NotFound): the server could not find the requested resource",
                )
            }
        }),
        curl: Box::new(|_| {
            out(
                false,
                r#"{"kind":"Status","status":"Failure","reason":"Unauthorized","code":401}"#,
                "",
            )
        }),
    };
    let res = discover_cluster_endpoints_detailed(
        &runner,
        &paths.kubeconfig_file("cka"),
        Some("192.0.2.10"),
        None,
    )
    .await
    .unwrap();
    assert!(
        res.failed_kinds.is_empty(),
        "uninstalled CRDs classified as failed: {res:?}"
    );
    for k in ["apisix", "istio", "gateway-api"] {
        assert!(res.succeeded_kinds.contains(&k.to_string()), "{k}: {res:?}");
    }
}

// ---------- 6: Hosts-mode Connect writes inside the lane, on the fresh profile ----------

fn empty_discovery() -> Result<DiscoveryResult, String> {
    Ok(DiscoveryResult {
        endpoints: vec![],
        succeeded_kinds: vec!["service".into()],
        failed_kinds: vec![],
        errors_by_kind: vec![],
    })
}

#[tokio::test(start_paused = true)]
async fn r4_item6_connect_racing_a_save_to_off_ends_off_without_block() {
    let f = Fake::new();
    let paths = f.cd_paths();
    let hosts_profile = profile("cka", HostMapping::Hosts, &[]);
    store::upsert_profile(&paths, hosts_profile.clone()).unwrap();
    std::fs::create_dir_all(paths.base.join("dns")).unwrap();

    // Connect read the Hosts profile, then a save to Off completed before its write.
    let mut off = hosts_profile.clone();
    off.host_mapping = HostMapping::Off;
    store::upsert_profile(&paths, off).unwrap();

    write_connect_hosts_block(&*f, &paths, &f.hosts, &hosts_profile, &empty_discovery())
        .await
        .unwrap();
    assert!(!has_block(&f), "stale Connect resurrected the hosts block");
    assert_eq!(
        store::get_profile(&paths, "cka").unwrap().host_mapping,
        HostMapping::Off
    );
}

#[tokio::test(start_paused = true)]
async fn r4_item6_connect_hosts_write_waits_for_the_lane() {
    let f = Fake::new();
    let paths = f.cd_paths();
    let p = profile("cka", HostMapping::Hosts, &[]);
    store::upsert_profile(&paths, p.clone()).unwrap();
    std::fs::create_dir_all(paths.base.join("dns")).unwrap();

    let lane = lock_lane().await;
    let f2 = Arc::clone(&f);
    let p2 = p.clone();
    let task = tokio::spawn(async move {
        write_connect_hosts_block(&*f2, &f2.cd_paths(), &f2.hosts, &p2, &empty_discovery()).await
    });
    tokio::time::sleep(std::time::Duration::from_secs(1)).await;
    assert!(!has_block(&f), "hosts written while another lane op ran");
    drop(lane);
    task.await.unwrap().unwrap();
    assert!(has_block(&f));
}

// ---------- 7: scan never resurrects a deleted profile's cache entry ----------

#[tokio::test(start_paused = true)]
async fn r4_item7_scan_does_not_resurrect_deleted_profiles_cache_entry() {
    let f = Fake::new();
    let paths = f.cd_paths();
    let p = profile("cka", HostMapping::LocalDns, &[]);
    store::upsert_profile(&paths, p).unwrap();
    write_kubeconfig(&paths, "cka");

    struct DeletesOnFirstCall {
        paths: ClusterDeckPaths,
        done: AtomicBool,
    }
    #[async_trait]
    impl CommandRunner for DeletesOnFirstCall {
        async fn run(&self, _bin: &str, args: &[String]) -> Result<CommandOutput, String> {
            if !self.done.swap(true, Ordering::SeqCst) {
                store::delete_profile(&self.paths, "cka").unwrap();
            }
            let path = args.last().map(String::as_str).unwrap_or("");
            if path.ends_with("/ingresses") {
                out(
                    true,
                    r#"{"items":[{"metadata":{"name":"i","namespace":"ns"},"spec":{"rules":[{"host":"api.lab.example.internal"}]}}]}"#,
                    "",
                )
            } else {
                out(true, r#"{"items":[]}"#, "")
            }
        }
    }
    let runner = DeletesOnFirstCall {
        paths: paths.clone(),
        done: AtomicBool::new(false),
    };
    let cache = f.home.join("scan-cache.json");
    discover_cluster_endpoints_internal(&runner, &paths, Ok(cache.clone()), "cka")
        .await
        .unwrap();
    assert!(
        !cache_load(&cache).contains_key("cka"),
        "scan re-inserted a deleted profile into the endpoints cache"
    );
}

// ---------- 8: deleting a Hosts profile drops its endpoints-cache entry ----------

#[tokio::test(start_paused = true)]
async fn r4_item8_delete_hosts_profile_removes_cache_entry() {
    let f = Fake::new();
    let paths = f.cd_paths();
    let p = profile("ckh", HostMapping::Hosts, &[]);
    store::upsert_profile(&paths, p).unwrap();
    std::fs::create_dir_all(paths.base.join("dns")).unwrap();
    let cache = paths.endpoints_cache_file();
    update_endpoint_cache(
        &cache,
        "ckh",
        &[ep("api.lab.example.internal", "192.0.2.60")],
        None,
    )
    .unwrap();
    update_endpoint_cache(
        &cache,
        "other",
        &[ep("o.lab.example.internal", "192.0.2.61")],
        None,
    )
    .unwrap();

    let guard = store::ProfileWriteGuard;
    crate::commands::profiles::delete_profile_internal(&paths, &guard, "ckh".into())
        .await
        .unwrap();
    let left = cache_load(&cache);
    assert!(
        !left.contains_key("ckh"),
        "deleted Hosts profile's cache entry survived"
    );
    assert!(
        left.contains_key("other"),
        "other profiles keep their entry"
    );
}

mod postmerge;
