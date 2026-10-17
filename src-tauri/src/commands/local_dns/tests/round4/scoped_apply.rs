//! Regression tests for issue #100: scoping resolver changes to the saved profile.
use super::*;

#[tokio::test(start_paused = true)]
async fn issue_100_save_profile_changing_approved_zones_does_not_apply_other_profiles_pending_zones(
) {
    let f = Fake::new();
    let paths = f.cd_paths();

    let a = profile("cka", HostMapping::LocalDns, &["a1.example.internal"]);
    store::upsert_profile(&paths, a.clone()).unwrap();
    reapply_after_save(&paths, &f.env(), &f.hosts, None, &a)
        .await
        .unwrap();
    assert!(f.resolver.join("a1.example.internal").exists());

    // Profile B tries to approve lab.example.internal, but cancels
    cancelled_pending_b(&f, &paths).await;
    f.set_cancel_osascript(false);
    assert!(!f.resolver.join("lab.example.internal").exists());

    // Now A adds a second approved zone a2
    let mut a2 = a.clone();
    a2.approved_dns_zones = vec!["a1.example.internal".into(), "a2.example.internal".into()];
    store::upsert_profile(&paths, a2.clone()).unwrap();

    let res = reapply_after_save(&paths, &f.env(), &f.hosts, Some(&a), &a2)
        .await
        .unwrap();
    assert!(res.is_some());
    assert!(f.resolver.join("a2.example.internal").exists());
    assert!(
        !f.resolver.join("lab.example.internal").exists(),
        "saving profile A carried over profile B's pending zone"
    );
}

#[tokio::test(start_paused = true)]
async fn issue_100_save_profile_changing_mapping_to_hosts_does_not_apply_other_profiles_pending_zones(
) {
    let f = Fake::new();
    let paths = f.cd_paths();
    let a = profile("cka", HostMapping::LocalDns, &["a1.example.internal"]);
    store::upsert_profile(&paths, a.clone()).unwrap();
    reapply_after_save(&paths, &f.env(), &f.hosts, None, &a)
        .await
        .unwrap();
    assert!(f.resolver.join("a1.example.internal").exists());

    cancelled_pending_b(&f, &paths).await;
    f.set_cancel_osascript(false);
    assert!(!f.resolver.join("lab.example.internal").exists());

    // Now A switches to Hosts mapping
    let mut a_hosts = a.clone();
    a_hosts.host_mapping = HostMapping::Hosts;
    store::upsert_profile(&paths, a_hosts.clone()).unwrap();

    let res = reapply_after_save(&paths, &f.env(), &f.hosts, Some(&a), &a_hosts)
        .await
        .unwrap();
    assert!(res.is_some());
    assert!(!f.resolver.join("a1.example.internal").exists());
    assert!(
        !f.resolver.join("lab.example.internal").exists(),
        "changing A's mapping to Hosts carried over profile B's pending zone"
    );
}

#[tokio::test(start_paused = true)]
async fn issue_100_sync_local_dns_profile_does_not_apply_other_profiles_pending_zones() {
    let f = Fake::new();
    let paths = f.cd_paths();
    cancelled_pending_b(&f, &paths).await;
    f.set_cancel_osascript(false);
    assert!(!f.resolver.join("lab.example.internal").exists());

    let a = profile("cka", HostMapping::LocalDns, &["a1.example.internal"]);
    store::upsert_profile(&paths, a.clone()).unwrap();

    let res = sync_local_dns_profile(&paths, &f.env(), &f.hosts, &a, Ok(vec![]))
        .await
        .unwrap();
    assert!(res.success);
    assert!(f.resolver.join("a1.example.internal").exists());
    assert!(
        !f.resolver.join("lab.example.internal").exists(),
        "syncing A to local DNS carried over profile B's pending zone"
    );
}

#[tokio::test(start_paused = true)]
async fn issue_100_delete_profile_removes_own_zones_without_applying_other_pending_zones() {
    let f = Fake::new();
    let paths = f.cd_paths();

    let a = profile("cka", HostMapping::LocalDns, &["a1.example.internal"]);
    store::upsert_profile(&paths, a.clone()).unwrap();
    reapply_after_save(&paths, &f.env(), &f.hosts, None, &a)
        .await
        .unwrap();
    assert!(f.resolver.join("a1.example.internal").exists());

    cancelled_pending_b(&f, &paths).await;
    f.set_cancel_osascript(false);
    assert!(!f.resolver.join("lab.example.internal").exists());

    // Delete profile A from store and reapply
    store::delete_profile(&paths, "cka").unwrap();
    reapply_after_delete(&paths, &f.env(), "cka").await.unwrap();

    assert!(!f.resolver.join("a1.example.internal").exists());
    assert!(
        !f.resolver.join("lab.example.internal").exists(),
        "deleting profile A carried over profile B's pending zone"
    );
}

#[tokio::test(start_paused = true)]
async fn issue_100_switch_zone_owner_applies_switched_zone_without_applying_other_pending_zones() {
    let f = Fake::new();
    let paths = f.cd_paths();

    let b = profile(
        "ckb",
        HostMapping::LocalDns,
        &["shared.example.internal", "pending.example.internal"],
    );
    store::upsert_profile(&paths, b).unwrap();
    // Only shared is on disk initially
    std::fs::write(
        f.resolver.join("shared.example.internal"),
        "# managed by ClusterDeck (profile: ckb)\nnameserver 127.0.0.1\nport 15353\n",
    )
    .unwrap();
    assert!(f.resolver.join("shared.example.internal").exists());
    assert!(!f.resolver.join("pending.example.internal").exists());

    let a = profile("cka", HostMapping::LocalDns, &[]);
    store::upsert_profile(&paths, a).unwrap();

    // Switch shared from ckb to cka
    store::switch_dns_zone_owner(&paths, "shared.example.internal", "cka").unwrap();

    // Reapply scoped to cka (as mutate_then_apply does)
    let (view, _) = reapply_locked(&paths, &f.env(), None, None, None, Some("cka"))
        .await
        .unwrap();
    assert!(!view.cancelled);
    // shared must now have marker cka
    let content = std::fs::read_to_string(f.resolver.join("shared.example.internal")).unwrap();
    assert!(content.contains("profile: cka"));
    // pending must NOT have been written
    assert!(
        !f.resolver.join("pending.example.internal").exists(),
        "switching shared zone carried over profile B's pending zone"
    );
}
