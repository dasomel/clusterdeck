//! Regression tests for issue #102.
use super::*;

fn partial(f: &Arc<Fake>, services_fail: bool) -> Disc {
    Disc {
        fake: Arc::clone(f),
        kubectl: Box::new(move |path| {
            let services = path.ends_with("/api/v1/services");
            if services == services_fail {
                out(
                    true,
                    r#"{"kind":"Status","reason":"Forbidden","code":403}"#,
                    "",
                )
            } else if services_fail && path.ends_with("/ingresses") {
                out(
                    true,
                    r#"{"items":[{"metadata":{"name":"app"},"spec":{"rules":[{"host":"api.lab.example.internal"}]}}]}"#,
                    "",
                )
            } else {
                out(true, r#"{"items":[]}"#, "")
            }
        }),
        curl: Box::new(|_| panic!("unexpected curl")),
    }
}

#[tokio::test(start_paused = true)]
async fn postmerge_services_failure_without_ip_preserves_ingress() {
    let f = Fake::new();
    let paths = f.cd_paths();
    write_kubeconfig(&paths, "cka");
    let res = discover_cluster_endpoints_detailed(
        &partial(&f, true),
        &paths.kubeconfig_file("cka"),
        Some("node.example.internal"),
        None,
    )
    .await
    .unwrap();
    let cache = paths.endpoints_cache_file();
    update_endpoint_cache(
        &cache,
        "cka",
        &[ep("api.lab.example.internal", "192.0.2.60")],
        None,
    )
    .unwrap();
    let merged =
        update_endpoint_cache(&cache, "cka", &res.endpoints, Some(&res.succeeded_kinds)).unwrap();
    assert_eq!(merged.len(), 1, "unresolved Ingress erased: {res:?}");
    assert!(
        res.succeeded_kinds.contains(&"apisix".to_string()),
        "empty lists remain authoritative"
    );
}

#[tokio::test(start_paused = true)]
async fn postmerge_legacy_hosts_survive_repeated_partial_sync() {
    let f = Fake::new();
    let paths = f.cd_paths();
    let p = profile("cka", HostMapping::Hosts, &[]);
    put_hosts_block(&f, &p, &[ep("old.lab.example.internal", "192.0.2.60")]);
    std::fs::create_dir_all(paths.base.join("dns")).unwrap();
    let mut fresh = ep("new.lab.example.internal", "192.0.2.61");
    fresh.source = "service".into();
    for _ in 0..3 {
        let merged = merge_discovered_endpoints(
            &paths.endpoints_cache_file(),
            &f.hosts,
            "cka",
            &[fresh.clone()],
            &["service".into()],
            &["ingress".into()],
        );
        assert!(merged.iter().any(|e| e.host == "old.lab.example.internal"));
        put_hosts_block(&f, &p, &merged);
    }
    let succeeded = crate::services::k8s_endpoints::DISCOVERY_KINDS.map(String::from);
    let merged = merge_discovered_endpoints(
        &paths.endpoints_cache_file(),
        &f.hosts,
        "cka",
        &[],
        &succeeded,
        &[],
    );
    assert!(merged.is_empty());
    assert!(cached_endpoints(&paths.endpoints_cache_file(), "cka").is_empty());
}

#[tokio::test(start_paused = true)]
async fn postmerge_sync_racing_off_save_must_not_write_hosts() {
    struct SaveDuringDiscovery {
        fake: Arc<Fake>,
        paths: ClusterDeckPaths,
        off: Profile,
        saved: AtomicBool,
    }
    #[async_trait]
    impl CommandRunner for SaveDuringDiscovery {
        async fn run(&self, bin: &str, args: &[String]) -> Result<CommandOutput, String> {
            if bin == "kubectl" {
                if !self.saved.swap(true, Ordering::SeqCst) {
                    crate::commands::profiles::save_profile_internal(
                        &self.paths,
                        &store::ProfileWriteGuard,
                        self.off.clone(),
                        || panic!("Off save must not need DNS"),
                        &self.fake.hosts,
                    )
                    .await
                    .unwrap();
                }
                out(true, r#"{"items":[]}"#, "")
            } else {
                self.fake.run(bin, args).await
            }
        }
    }
    let f = Fake::new();
    let paths = f.cd_paths();
    let mut p = profile("cka", HostMapping::Hosts, &[]);
    store::upsert_profile(&paths, p.clone()).unwrap();
    write_kubeconfig(&paths, "cka");
    p.host_mapping = HostMapping::Off;
    let runner = SaveDuringDiscovery {
        fake: Arc::clone(&f),
        paths: f.cd_paths(),
        off: p,
        saved: AtomicBool::new(false),
    };
    let result = sync_hosts_file_with_deps(&runner, &paths, &f.hosts, "cka").await;
    assert!(
        !has_block(&f),
        "Sync resurrected hosts block after Off save"
    );
    assert!(result.is_err());
    assert_eq!(f.count("osascript"), 0);
}

#[tokio::test(start_paused = true)]
async fn postmerge_scan_partial_failure_must_warn() {
    let f = Fake::new();
    let paths = f.cd_paths();
    store::upsert_profile(&paths, profile("cka", HostMapping::LocalDns, &[])).unwrap();
    write_kubeconfig(&paths, "cka");
    let res = discover_cluster_endpoints_internal(
        &partial(&f, false),
        &paths,
        Ok(paths.endpoints_cache_file()),
        "cka",
    )
    .await
    .unwrap();
    assert!(
        res.warning
            .as_deref()
            .is_some_and(|w| w.contains("Forbidden")),
        "{res:?}"
    );
}

#[tokio::test(start_paused = true)]
async fn postmerge_sync_waits_for_lane_and_rejects_deleted_profile() {
    let f = Fake::new();
    let paths = f.cd_paths();
    store::upsert_profile(&paths, profile("cka", HostMapping::Hosts, &[])).unwrap();
    let lane = lock_lane().await;
    let sync = sync_hosts_file_with_deps(&*f, &paths, &f.hosts, "cka");
    tokio::pin!(sync);
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(20), &mut sync)
            .await
            .is_err()
    );
    assert_eq!(f.count("osascript"), 0);
    store::delete_profile(&paths, "cka").unwrap();
    drop(lane);
    assert!(sync.await.is_err());
    assert!(!has_block(&f));
    assert!(!paths.endpoints_cache_file().exists());
}

#[tokio::test(start_paused = true)]
async fn postmerge_local_dns_sync_rejects_stale_mapping() {
    let f = Fake::new();
    let paths = f.cd_paths();
    let snapshot = profile("cka", HostMapping::LocalDns, &[]);
    let current = profile("cka", HostMapping::Hosts, &[]);
    store::upsert_profile(&paths, current.clone()).unwrap();
    put_hosts_block(&f, &current, &[]);
    let before = std::fs::read_to_string(&f.hosts).unwrap();
    assert!(
        sync_local_dns_profile(&paths, &f.env(), &f.hosts, &snapshot, Ok(vec![]))
            .await
            .is_err()
    );
    assert_eq!(std::fs::read_to_string(&f.hosts).unwrap(), before);
    assert_eq!(f.total_calls(), 0);
}

#[tokio::test(start_paused = true)]
async fn postmerge_scan_retains_discovery_and_cache_warnings() {
    let f = Fake::new();
    let paths = f.cd_paths();
    store::upsert_profile(&paths, profile("cka", HostMapping::LocalDns, &[])).unwrap();
    write_kubeconfig(&paths, "cka");
    let result = discover_cluster_endpoints_internal(
        &partial(&f, false),
        &paths,
        Err("test cache unavailable".into()),
        "cka",
    )
    .await
    .unwrap();
    let warning = result.warning.unwrap();
    assert!(warning.contains("Forbidden"));
    assert!(warning.contains("test cache unavailable"));
}
