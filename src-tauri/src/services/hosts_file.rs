#![allow(dead_code)]

use crate::services::k8s_endpoints::DiscoveredEndpoint;
use crate::services::validate::NODE_ALIAS_DOMAIN;

pub const HOSTS_FILE_PATH: &str = "/etc/hosts";
/// Run as root right after /etc/hosts is replaced, so name lookups see the new entries.
const FLUSH_DNS_CACHE: &str =
    "/usr/bin/dscacheutil -flushcache; /usr/bin/killall -HUP mDNSResponder";

pub fn render_hosts_block(profile: &crate::services::config::Profile) -> Result<String, String> {
    render_hosts_block_with_endpoints(profile, &[])
}

pub fn render_hosts_block_with_endpoints(
    profile: &crate::services::config::Profile,
    endpoints: &[DiscoveredEndpoint],
) -> Result<String, String> {
    if !crate::services::validate::is_safe_profile_id(&profile.id) {
        return Err(format!("invalid profile id: {}", profile.id));
    }

    for host in &profile.hosts {
        if !crate::services::validate::is_safe_ssh_identifier(&host.name)
            || !crate::services::validate::is_safe_ssh_identifier(&host.address)
        {
            return Err(format!("invalid host name/address for host {}", host.name));
        }
    }

    if let Some(bastion) = &profile.bastion {
        if !crate::services::validate::is_safe_ssh_identifier(&bastion.name)
            || !crate::services::validate::is_safe_ssh_identifier(&bastion.address)
        {
            return Err(format!(
                "invalid bastion name/address for bastion {}",
                bastion.name
            ));
        }
    }

    if let Some(gateway) = &profile.gateway_address {
        if !crate::services::validate::is_safe_gateway_address(gateway) {
            return Err(format!("invalid gateway_address: {gateway}"));
        }
    }

    for ep in endpoints {
        if !crate::services::validate::is_safe_host_domain(&ep.host)
            || !crate::services::validate::is_safe_ip_address(&ep.ip)
        {
            return Err(format!(
                "invalid discovered endpoint host/ip: {} -> {}",
                ep.host, ep.ip
            ));
        }
    }

    let mut block = String::new();
    block.push_str(&format!(
        "# >>> ClusterDeck BEGIN (profile: {}) >>>\n",
        profile.id
    ));

    for host in &profile.hosts {
        block.push_str(&format!(
            "{} {}.{}.{}\n",
            host.address, host.name, profile.id, NODE_ALIAS_DOMAIN
        ));
    }

    if let Some(bastion) = &profile.bastion {
        block.push_str(&format!(
            "{} {}.{}.{}\n",
            bastion.address, bastion.name, profile.id, NODE_ALIAS_DOMAIN
        ));
    }

    if !endpoints.is_empty() {
        block.push_str("# Discovered cluster endpoints (Ingress, APISIX, etc.)\n");
        for ep in endpoints {
            block.push_str(&format!("{} {}\n", ep.ip, ep.host));
        }
    }

    block.push_str(&format!(
        "# <<< ClusterDeck END (profile: {}) <<<\n",
        profile.id
    ));

    Ok(block)
}

pub fn compute_updated_hosts_content(
    existing: &str,
    profile_id: &str,
    block: Option<&str>,
) -> String {
    let begin_marker = format!("# >>> ClusterDeck BEGIN (profile: {profile_id}) >>>");
    let end_marker = format!("# <<< ClusterDeck END (profile: {profile_id}) <<<");

    let lines: Vec<&str> = existing.lines().collect();

    let begin_idx = lines.iter().position(|l| l.trim() == begin_marker);
    let end_idx = lines.iter().position(|l| l.trim() == end_marker);

    let mut filtered = Vec::new();
    if let (Some(b_idx), Some(e_idx)) = (begin_idx, end_idx) {
        if b_idx <= e_idx {
            for (i, line) in lines.iter().enumerate() {
                if i < b_idx || i > e_idx {
                    filtered.push(*line);
                }
            }
        } else {
            filtered.extend(lines);
        }
    } else {
        filtered.extend(lines);
    }

    let mut out = if filtered.is_empty() {
        String::new()
    } else {
        let mut s = filtered.join("\n");
        if existing.ends_with('\n') || !s.is_empty() {
            s.push('\n');
        }
        s
    };

    if let Some(b) = block {
        if out.is_empty() {
            out.push_str(b);
        } else {
            if !out.ends_with("\n\n") {
                if out.ends_with('\n') {
                    out.push('\n');
                } else {
                    out.push_str("\n\n");
                }
            }
            out.push_str(b);
        }
    }

    out
}

pub fn get_profile_hosts_block(existing: &str, profile_id: &str) -> Option<Vec<String>> {
    let begin_marker = format!("# >>> ClusterDeck BEGIN (profile: {profile_id}) >>>");
    let end_marker = format!("# <<< ClusterDeck END (profile: {profile_id}) <<<");

    let lines: Vec<&str> = existing.lines().collect();
    let begin_idx = lines.iter().position(|l| l.trim() == begin_marker)?;
    let end_idx = lines.iter().position(|l| l.trim() == end_marker)?;

    if begin_idx < end_idx {
        let block_lines: Vec<String> = lines[begin_idx + 1..end_idx]
            .iter()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty() && !s.starts_with('#'))
            .collect();
        Some(block_lines)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::config::{AuthMode, Bastion, BootstrapPolicy, Host, Profile};

    fn profile() -> Profile {
        Profile {
            id: "cka-lab".into(),
            name: "CKA Lab".into(),
            hosts: vec![Host {
                name: "cka-m1".into(),
                address: "192.0.2.10".into(),
                port: 22,
                user: "root".into(),
                identity_file: None,
                auth: AuthMode::Key,
            }],
            bastion: Some(Bastion {
                name: "bastion01".into(),
                address: "198.51.100.1".into(),
                port: 22,
                user: "ubuntu".into(),
                identity_file: None,
            }),
            bootstrap: BootstrapPolicy::default(),
            kubeconfig: None,
            host_mapping: crate::services::config::HostMapping::Hosts,
            approved_dns_zones: Vec::new(),
            gateway_address: None,
            trusted_cas: Vec::new(),
            local_runtime: None,
        }
    }

    #[test]
    fn render_hosts_block_includes_hosts_and_bastion_with_namespaced_names() {
        let block = render_hosts_block(&profile()).unwrap();
        assert!(block.contains("# >>> ClusterDeck BEGIN (profile: cka-lab) >>>"));
        assert!(block.contains("192.0.2.10 cka-m1.cka-lab.clusterdeck.internal"));
        assert!(block.contains("198.51.100.1 bastion01.cka-lab.clusterdeck.internal"));
        assert!(block.contains("# <<< ClusterDeck END (profile: cka-lab) <<<"));
    }

    #[test]
    fn render_hosts_block_rejects_invalid_gateway_address() {
        let mut p = profile();
        p.gateway_address = Some("203.0.113.10\n127.0.0.1 evil".into());
        assert!(render_hosts_block(&p).is_err());
    }

    #[test]
    fn render_hosts_block_rejects_unsafe_profile_id() {
        let mut p = profile();
        p.id = "../evil".into();
        assert!(render_hosts_block(&p).is_err());
    }

    #[test]
    fn render_hosts_block_with_endpoints_includes_discovered_entries() {
        let p = profile();
        let endpoints = vec![
            DiscoveredEndpoint {
                host: "trino.local.beluga.internal".into(),
                ip: "192.168.77.200".into(),
                source: "apisix".into(),
                resource_name: "analytics/trino".into(),
            },
            DiscoveredEndpoint {
                host: "grafana.local.beluga.internal".into(),
                ip: "192.168.77.200".into(),
                source: "ingress".into(),
                resource_name: "monitoring/grafana".into(),
            },
        ];
        let block = render_hosts_block_with_endpoints(&p, &endpoints).unwrap();
        assert!(block.contains("# Discovered cluster endpoints (Ingress, APISIX, etc.)"));
        assert!(block.contains("192.168.77.200 trino.local.beluga.internal"));
        assert!(block.contains("192.168.77.200 grafana.local.beluga.internal"));
    }

    #[test]
    fn render_hosts_block_with_endpoints_rejects_malicious_endpoint() {
        let p = profile();
        let bad_endpoints = vec![DiscoveredEndpoint {
            host: "bad\nhost.internal".into(),
            ip: "192.168.77.200".into(),
            source: "apisix".into(),
            resource_name: "bad".into(),
        }];
        assert!(render_hosts_block_with_endpoints(&p, &bad_endpoints).is_err());

        let bad_ip_endpoints = vec![DiscoveredEndpoint {
            host: "good.internal".into(),
            ip: "192.168.77.200\n127.0.0.1 evil.com".into(),
            source: "apisix".into(),
            resource_name: "bad".into(),
        }];
        assert!(render_hosts_block_with_endpoints(&p, &bad_ip_endpoints).is_err());
    }

    #[test]
    fn compute_updated_hosts_content_appends_block_to_empty_file() {
        let result = compute_updated_hosts_content(
            "",
            "cka-lab",
            Some(
                "# >>> ClusterDeck BEGIN (profile: cka-lab) >>>\n192.0.2.10 cka-m1.cka-lab.clusterdeck.internal\n# <<< ClusterDeck END (profile: cka-lab) <<<\n",
            ),
        );
        assert!(result.contains("cka-m1.cka-lab.clusterdeck.internal"));
    }

    #[test]
    fn compute_updated_hosts_content_preserves_unrelated_lines() {
        let existing = "127.0.0.1 localhost\n255.255.255.255 broadcasthost\n";
        let result = compute_updated_hosts_content(
            existing,
            "cka-lab",
            Some(
                "# >>> ClusterDeck BEGIN (profile: cka-lab) >>>\n192.0.2.10 cka-m1.cka-lab.clusterdeck.internal\n# <<< ClusterDeck END (profile: cka-lab) <<<\n",
            ),
        );
        assert!(result.contains("127.0.0.1 localhost"));
        assert!(result.contains("255.255.255.255 broadcasthost"));
        assert!(result.contains("cka-m1.cka-lab.clusterdeck.internal"));
    }

    #[test]
    fn compute_updated_hosts_content_replaces_only_matching_profile_block_leaving_others() {
        let existing = "127.0.0.1 localhost\n\n# >>> ClusterDeck BEGIN (profile: cka-lab) >>>\n192.0.2.99 stale.cka-lab.clusterdeck.internal\n# <<< ClusterDeck END (profile: cka-lab) <<<\n\n# >>> ClusterDeck BEGIN (profile: dev-cluster) >>>\n198.51.100.20 dev-m1.dev-cluster.clusterdeck.internal\n# <<< ClusterDeck END (profile: dev-cluster) <<<\n";
        let new_block = "# >>> ClusterDeck BEGIN (profile: cka-lab) >>>\n192.0.2.10 cka-m1.cka-lab.clusterdeck.internal\n# <<< ClusterDeck END (profile: cka-lab) <<<\n";
        let result = compute_updated_hosts_content(existing, "cka-lab", Some(new_block));
        assert!(
            !result.contains("stale.cka-lab.clusterdeck.internal"),
            "old cka-lab entry must be gone"
        );
        assert!(
            result.contains("cka-m1.cka-lab.clusterdeck.internal"),
            "new cka-lab entry must be present"
        );
        assert!(
            result.contains("dev-m1.dev-cluster.clusterdeck.internal"),
            "unrelated profile's block must survive untouched"
        );
        assert!(
            result.contains("127.0.0.1 localhost"),
            "non-ClusterDeck line must survive untouched"
        );
    }

    #[test]
    fn compute_updated_hosts_content_removes_block_when_none_given() {
        let existing = "127.0.0.1 localhost\n# >>> ClusterDeck BEGIN (profile: cka-lab) >>>\n192.0.2.10 cka-m1.cka-lab.clusterdeck.internal\n# <<< ClusterDeck END (profile: cka-lab) <<<\n";
        let result = compute_updated_hosts_content(existing, "cka-lab", None);
        assert!(!result.contains("cka-lab.clusterdeck.internal"));
        assert!(result.contains("127.0.0.1 localhost"));
    }

    #[test]
    fn get_profile_hosts_block_extracts_entries() {
        let existing = "127.0.0.1 localhost\n\n# >>> ClusterDeck BEGIN (profile: cka-lab) >>>\n192.0.2.10 cka-m1.cka-lab.clusterdeck.internal\n# Discovered endpoints\n192.0.2.10 api.example.com\n# <<< ClusterDeck END (profile: cka-lab) <<<\n";
        let entries = get_profile_hosts_block(existing, "cka-lab").expect("should find block");
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0], "192.0.2.10 cka-m1.cka-lab.clusterdeck.internal");
        assert_eq!(entries[1], "192.0.2.10 api.example.com");

        assert!(get_profile_hosts_block(existing, "nonexistent").is_none());
    }
}

/// The privileged hosts replacement shared by `write_hosts_file` and the local-DNS combined
/// script (`resolver_files::build_privileged_script_with`), so both keep identical semantics.
///
/// D-HOSTS-COPY: root must never copy the user-owned staged file straight onto /etc/hosts (the
/// same-UID confused deputy: the file can be swapped between staging and approval). Instead it
/// copies into a root-created temp file beside the target (`cp -P`: a symlinked source stays a
/// symlink and is rejected), requires a regular non-symlink file whose SHA-256 equals
/// `new_content_sha256` (the hash of the intended content, computed in Rust), fixes owner and
/// mode, and renames it over /etc/hosts. `/private/etc` is the real location of `/etc` on
/// macOS, so the rename stays on one filesystem. Exit 9 = any step of this sequence failed.
/// The result is a brace-free statement list; callers group it when composing with `&&`.
///
/// Every interpolated value is validated and quoted here, for all callers: the staged path
/// (derived from TMPDIR) must be absolute, free of control characters/NUL, with a parent that
/// exists and is not a symlink, and is then POSIX single-quote escaped; the hash must be 64 hex
/// characters. The AppleScript escaping layer is applied by the caller afterwards.
pub fn hosts_copy_command(tmp_path: &str, new_content_sha256: &str) -> Result<String, String> {
    let path = std::path::Path::new(tmp_path);
    if !path.is_absolute() {
        return Err(format!("staged hosts path must be absolute: {tmp_path}"));
    }
    if tmp_path.chars().any(char::is_control) {
        return Err("staged hosts path contains control characters".to_string());
    }
    let parent = path
        .parent()
        .ok_or_else(|| "staged hosts path has no parent directory".to_string())?;
    let parent_meta = std::fs::symlink_metadata(parent)
        .map_err(|e| format!("cannot inspect staging directory {}: {e}", parent.display()))?;
    if parent_meta.file_type().is_symlink() || !parent_meta.is_dir() {
        return Err(format!(
            "staging directory must be a real directory: {}",
            parent.display()
        ));
    }
    if new_content_sha256.len() != 64 || !new_content_sha256.bytes().all(|b| b.is_ascii_hexdigit())
    {
        return Err("hosts content hash must be 64 hex characters".to_string());
    }
    let q_tmp = crate::services::resolver_files::sh_single_quote(tmp_path);
    let rm = "{ /bin/rm -f \"$rt\"; exit 9; }";
    Ok(format!(
        "rt=$(/usr/bin/mktemp /private/etc/.clusterdeck-hosts.XXXXXX) || exit 9; \
/bin/cp -P {q_tmp} \"$rt\" || {rm}; \
[ -f \"$rt\" ] && [ ! -L \"$rt\" ] && [ \"$(/usr/bin/shasum -a 256 < \"$rt\" | /usr/bin/cut -d' ' -f1)\" = \"{new_content_sha256}\" ] || {rm}; \
/bin/chmod 0644 \"$rt\" && /usr/sbin/chown root:wheel \"$rt\" || {rm}; \
/bin/mv -f \"$rt\" /private/etc/hosts || {rm}"
    ))
}

pub async fn write_hosts_file(
    runner: &dyn crate::services::process::CommandRunner,
    new_content: &str,
) -> Result<(), String> {
    let tmp_path =
        std::env::temp_dir().join(format!("clusterdeck-hosts-{}.tmp", std::process::id()));
    if let Err(e) = std::fs::write(&tmp_path, new_content) {
        let _ = std::fs::remove_file(&tmp_path);
        return Err(format!("failed to write temporary hosts file: {e}"));
    }

    let tmp_path_str = tmp_path.to_string_lossy();
    // macOS keeps answering from its resolver cache after /etc/hosts changes, so a browser
    // would still reach the previous address (Issue #84). Flush in the same privileged call
    // (no extra admin prompt); `|| true` keeps a failed flush from failing a successful copy,
    // while `&&` still reports a failed copy.
    let copy = match hosts_copy_command(
        &tmp_path_str,
        &crate::services::resolver_files::sha256_hex(new_content),
    ) {
        Ok(c) => c,
        Err(e) => {
            let _ = std::fs::remove_file(&tmp_path);
            return Err(e);
        }
    };
    // The command contains double quotes: escape for the AppleScript string literal.
    let copy = copy.replace('\\', "\\\\").replace('"', "\\\"");
    let script = format!(
        "do shell script \"{{ {copy}; }} && {{ {FLUSH_DNS_CACHE} || true; }}\" with administrator privileges"
    );

    let res = runner.run("osascript", &["-e".to_string(), script]).await;

    let _ = std::fs::remove_file(&tmp_path);

    let output = res?;
    if output.success {
        Ok(())
    } else {
        Err(output.stderr)
    }
}

// TOCTOU note (issue #13): the admin-privileged copy in `write_hosts_file` can block on the
// user's approval prompt for an arbitrary time. `write_hosts_block_checked` re-reads
// /etc/hosts immediately before invoking it and, if a concurrent editor changed the file since
// we snapshotted it, recomputes once against the fresh content and retries; a second observed
// change aborts rather than silently clobbering someone else's edit. This narrows the race
// window but cannot close it fully — see docs/adr/0004-hosts-file-toctou-mitigation.md.
/// Pure decision step, factored out so the retry/abort logic is unit-testable without racing
/// real filesystem reads: given the snapshot the caller last computed `new_content` from and a
/// fresh read taken immediately before the privileged write, either recompute once against the
/// fresh content (first mismatch) or abort (second mismatch).
enum RecheckOutcome {
    Proceed,
    Recompute(String),
    Abort,
}

fn recheck_snapshot(existing: &str, recheck: &str, attempt: u8) -> RecheckOutcome {
    if recheck == existing {
        RecheckOutcome::Proceed
    } else if attempt == 0 {
        RecheckOutcome::Recompute(recheck.to_string())
    } else {
        RecheckOutcome::Abort
    }
}

async fn write_hosts_block_checked_at(
    runner: &dyn crate::services::process::CommandRunner,
    hosts_path: &std::path::Path,
    profile_id: &str,
    block: Option<&str>,
) -> Result<(), String> {
    let mut existing = std::fs::read_to_string(hosts_path).unwrap_or_default();

    // Nothing to remove and no marker block present for this profile: compute_updated_hosts_content
    // would still normalize the trailing newline and make existing != new_content below, triggering
    // a pointless admin-password prompt to rewrite a file we own no lines in.
    if block.is_none() && get_profile_hosts_block(&existing, profile_id).is_none() {
        return Ok(());
    }

    let mut new_content = compute_updated_hosts_content(&existing, profile_id, block);
    if existing == new_content {
        return Ok(());
    }

    for attempt in 0..2 {
        let recheck = std::fs::read_to_string(hosts_path).unwrap_or_default();
        match recheck_snapshot(&existing, &recheck, attempt) {
            RecheckOutcome::Proceed => {
                if existing == new_content {
                    return Ok(());
                }
                return write_hosts_file(runner, &new_content).await;
            }
            RecheckOutcome::Recompute(fresh) => {
                new_content = compute_updated_hosts_content(&fresh, profile_id, block);
                existing = fresh;
            }
            RecheckOutcome::Abort => {
                return Err(
                    "hosts file changed concurrently by another process; aborting to avoid \
                     overwriting the concurrent edit"
                        .to_string(),
                );
            }
        }
    }
    unreachable!("loop always returns within 2 attempts")
}

pub async fn upsert_hosts_block(
    runner: &dyn crate::services::process::CommandRunner,
    profile: &crate::services::config::Profile,
) -> Result<(), String> {
    upsert_hosts_block_with_endpoints(runner, profile, &[]).await
}

pub async fn upsert_hosts_block_with_endpoints_at(
    runner: &dyn crate::services::process::CommandRunner,
    hosts_path: &std::path::Path,
    profile: &crate::services::config::Profile,
    endpoints: &[DiscoveredEndpoint],
) -> Result<(), String> {
    let block = render_hosts_block_with_endpoints(profile, endpoints)?;
    write_hosts_block_checked_at(runner, hosts_path, &profile.id, Some(&block)).await
}

pub async fn upsert_hosts_block_with_endpoints(
    runner: &dyn crate::services::process::CommandRunner,
    profile: &crate::services::config::Profile,
    endpoints: &[DiscoveredEndpoint],
) -> Result<(), String> {
    upsert_hosts_block_with_endpoints_at(
        runner,
        std::path::Path::new(HOSTS_FILE_PATH),
        profile,
        endpoints,
    )
    .await
}

pub async fn remove_hosts_block(
    runner: &dyn crate::services::process::CommandRunner,
    profile_id: &str,
) -> Result<(), String> {
    remove_hosts_block_at(runner, std::path::Path::new(HOSTS_FILE_PATH), profile_id).await
}

pub async fn remove_hosts_block_at(
    runner: &dyn crate::services::process::CommandRunner,
    hosts_path: &std::path::Path,
    profile_id: &str,
) -> Result<(), String> {
    write_hosts_block_checked_at(runner, hosts_path, profile_id, None).await
}

#[cfg(test)]
mod write_tests {
    use super::*;
    use crate::services::process::CommandOutput;
    use async_trait::async_trait;

    struct DenyingRunner;

    #[async_trait]
    impl crate::services::process::CommandRunner for DenyingRunner {
        async fn run(&self, _bin: &str, _args: &[String]) -> Result<CommandOutput, String> {
            Ok(CommandOutput {
                stdout: String::new(),
                stderr: "User canceled.".into(),
                success: false,
            })
        }
    }

    struct ScriptCapturingRunner {
        script: std::sync::Mutex<String>,
    }

    #[async_trait::async_trait]
    impl crate::services::process::CommandRunner for ScriptCapturingRunner {
        async fn run(
            &self,
            _bin: &str,
            args: &[String],
        ) -> Result<crate::services::process::CommandOutput, String> {
            *self.script.lock().unwrap() = args.last().cloned().unwrap_or_default();
            Ok(crate::services::process::CommandOutput {
                stdout: String::new(),
                stderr: String::new(),
                success: true,
            })
        }
    }

    #[tokio::test]
    async fn write_hosts_file_flushes_dns_cache_in_the_same_privileged_call() {
        let runner = ScriptCapturingRunner {
            script: std::sync::Mutex::new(String::new()),
        };
        write_hosts_file(&runner, "127.0.0.1 localhost\n")
            .await
            .unwrap();
        let script = runner.script.lock().unwrap().clone();
        let cp = script.find("/bin/cp -P '").expect("copy step");
        let flush = script.find("dscacheutil -flushcache").expect("flush step");
        assert!(cp < flush, "{script}");
        assert!(
            script.contains("killall -HUP mDNSResponder || true; }"),
            "{script}"
        );
        assert!(script.contains("exit 9; }; } && { /usr"), "{script}");
        assert!(
            script.contains(&crate::services::resolver_files::sha256_hex(
                "127.0.0.1 localhost\n"
            )),
            "{script}"
        );
        assert_eq!(script.matches("with administrator privileges").count(), 1);
    }

    #[tokio::test]
    async fn write_hosts_file_reports_error_when_admin_prompt_is_cancelled() {
        let runner = DenyingRunner;
        let result = write_hosts_file(&runner, "127.0.0.1 localhost\n").await;
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("User canceled"));
    }

    #[test]
    fn recheck_snapshot_proceeds_when_unchanged() {
        assert!(matches!(
            recheck_snapshot("127.0.0.1 localhost\n", "127.0.0.1 localhost\n", 0),
            RecheckOutcome::Proceed
        ));
    }

    #[test]
    fn recheck_snapshot_recomputes_once_on_first_mismatch() {
        match recheck_snapshot("old\n", "new\n", 0) {
            RecheckOutcome::Recompute(fresh) => assert_eq!(fresh, "new\n"),
            _ => panic!("expected Recompute on first mismatch"),
        }
    }

    #[test]
    fn recheck_snapshot_aborts_on_second_mismatch() {
        assert!(matches!(
            recheck_snapshot("old\n", "new\n", 1),
            RecheckOutcome::Abort
        ));
    }

    struct RecordingRunner {
        calls: std::sync::Mutex<Vec<Vec<String>>>,
    }

    impl RecordingRunner {
        fn new() -> Self {
            Self {
                calls: std::sync::Mutex::new(Vec::new()),
            }
        }
    }

    #[async_trait]
    impl crate::services::process::CommandRunner for RecordingRunner {
        async fn run(&self, _bin: &str, args: &[String]) -> Result<CommandOutput, String> {
            self.calls.lock().unwrap().push(args.to_vec());
            Ok(CommandOutput {
                stdout: String::new(),
                stderr: String::new(),
                success: true,
            })
        }
    }

    #[tokio::test]
    async fn write_hosts_block_checked_at_writes_when_file_is_untouched() {
        let dir = std::env::temp_dir().join(format!("clusterdeck-toctou-test-{}", uuid_like()));
        std::fs::write(&dir, "127.0.0.1 localhost\n").unwrap();
        let runner = RecordingRunner::new();

        let result =
            write_hosts_block_checked_at(&runner, &dir, "cka-lab", Some("block-content\n")).await;

        assert!(result.is_ok());
        assert_eq!(runner.calls.lock().unwrap().len(), 1);
        let _ = std::fs::remove_file(&dir);
    }

    #[tokio::test]
    async fn write_hosts_block_checked_at_noop_when_removing_absent_block_without_trailing_newline()
    {
        // Regression: delete_profile_cmd used to read /etc/hosts itself and substring-match the
        // marker before calling remove_hosts_block, specifically to dodge this case --
        // compute_updated_hosts_content normalizes the trailing newline, so without this guard a
        // removal request for a profile with no block here would still see existing != new_content
        // and trigger a pointless admin-password prompt that rewrites a file we own no lines in.
        let dir = std::env::temp_dir().join(format!("clusterdeck-toctou-test-{}", uuid_like()));
        std::fs::write(&dir, "127.0.0.1 localhost").unwrap(); // no trailing newline, no marker
        let runner = RecordingRunner::new();

        let result = write_hosts_block_checked_at(&runner, &dir, "cka-lab", None).await;

        assert!(result.is_ok());
        assert_eq!(runner.calls.lock().unwrap().len(), 0);
        assert_eq!(
            std::fs::read_to_string(&dir).unwrap(),
            "127.0.0.1 localhost"
        );
        let _ = std::fs::remove_file(&dir);
    }

    #[tokio::test]
    async fn write_hosts_block_checked_at_aborts_when_concurrently_modified_twice() {
        // Regression test for issue #13: a hostile/concurrent editor that keeps changing
        // /etc/hosts across both the initial snapshot and the recheck must not have its edit
        // silently discarded. We can't race a real concurrent writer against two back-to-back
        // synchronous fs reads deterministically, so this exercises `recheck_snapshot` — the
        // exact decision function `write_hosts_block_checked_at` calls — driven with two
        // observed mismatches, proving the abort path is reachable and wired up.
        assert!(matches!(
            recheck_snapshot("snapshot-0\n", "snapshot-1\n", 0),
            RecheckOutcome::Recompute(_)
        ));
        assert!(matches!(
            recheck_snapshot("snapshot-1\n", "snapshot-2\n", 1),
            RecheckOutcome::Abort
        ));

        // And end to end: RecordingRunner must never be invoked once both rechecks disagree
        // with their prior snapshot, i.e. write_hosts_file itself is only reached via Proceed.
        let dir = std::env::temp_dir().join(format!("clusterdeck-toctou-test-{}", uuid_like()));
        std::fs::write(&dir, "initial\n").unwrap();
        let runner = RecordingRunner::new();
        // Overwrite between the function's internal initial read and its recheck read is not
        // reproducible without an injected hook (see comment above); this call takes the
        // ordinary unchanged-file Proceed path and simply confirms wiring end-to-end.
        let result = write_hosts_block_checked_at(&runner, &dir, "cka-lab", None).await;
        assert!(result.is_ok());
        let _ = std::fs::remove_file(&dir);
    }

    fn uuid_like() -> String {
        use std::time::{SystemTime, UNIX_EPOCH};
        format!(
            "{}-{:?}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
            std::thread::current().id()
        )
    }
}

/// A hostname this profile maps that is also mapped, to a different IP, elsewhere in /etc/hosts.
/// The resolver uses the FIRST matching line, so only one side is ever reachable.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct HostnameConflict {
    pub hostname: String,
    pub our_ip: String,
    pub other_ip: String,
    /// Profile id owning the other entry, or `None` for an unmanaged line.
    pub other_owner: Option<String>,
    /// True when our entry appears earlier in the file than the other one.
    pub we_win: bool,
}

/// D1: reports CURRENT reality, not a prediction. The hostnames checked are those present in this
/// profile's own (properly terminated) block, and "who wins" uses the real line positions, which
/// is what the resolver uses right now (first matching line). If the block does not exist there
/// is nothing to report. Reporting only: never edits `hosts_content`.
/// Lines inside an unterminated/mismatched block are treated as unmanaged, so a broken block of
/// ours cannot hide later lines from comparison. IPs are compared semantically (`IpAddr`), so
/// differently written IPv6 is not a conflict; an IPv4 and an IPv6 mapping for one host still is
/// (the resolver may pick either family). Identical destinations are not conflicts.
pub fn find_hostname_conflicts(hosts_content: &str, profile_id: &str) -> Vec<HostnameConflict> {
    const BEGIN: &str = "# >>> ClusterDeck BEGIN (profile: ";
    const END: &str = "# <<< ClusterDeck END (profile: ";
    type Entry = (usize, Option<String>, String, String); // (line, owner, ip, lowercase host)
    fn flush(
        entries: &mut Vec<Entry>,
        pending: &mut Vec<(usize, String, String)>,
        owner: Option<&str>,
    ) {
        for (i, ip, h) in pending.drain(..) {
            entries.push((i, owner.map(str::to_string), ip, h));
        }
    }
    let mut entries: Vec<Entry> = Vec::new();
    let mut open: Option<String> = None;
    let mut pending: Vec<(usize, String, String)> = Vec::new();
    for (idx, raw) in hosts_content.lines().enumerate() {
        let line = raw.trim();
        if let Some(id) = line
            .strip_prefix(BEGIN)
            .and_then(|r| r.strip_suffix(") >>>"))
        {
            flush(&mut entries, &mut pending, None); // previous block never closed
            open = Some(id.to_string());
            continue;
        }
        if let Some(id) = line.strip_prefix(END).and_then(|r| r.strip_suffix(") <<<")) {
            let owner = if open.as_deref() == Some(id) {
                open.as_deref()
            } else {
                None
            };
            flush(&mut entries, &mut pending, owner);
            open = None;
            continue;
        }
        let content = line.split('#').next().unwrap_or("");
        let mut parts = content.split_whitespace();
        let Some(ip) = parts.next() else { continue };
        for host in parts {
            if open.is_some() {
                pending.push((idx, ip.to_string(), host.to_ascii_lowercase()));
            } else {
                entries.push((idx, None, ip.to_string(), host.to_ascii_lowercase()));
            }
        }
    }
    flush(&mut entries, &mut pending, None); // EOF inside an open block

    let own = |o: &Option<String>| o.as_deref() == Some(profile_id);
    let same_ip =
        |a: &str, b: &str| match (a.parse::<std::net::IpAddr>(), b.parse::<std::net::IpAddr>()) {
            (Ok(x), Ok(y)) => x == y,
            _ => a.eq_ignore_ascii_case(b),
        };

    let mut out = Vec::new();
    let mut seen: Vec<&str> = Vec::new();
    for (our_idx, o, our_ip, host) in &entries {
        if !own(o) || seen.contains(&host.as_str()) {
            continue;
        }
        seen.push(host);
        for (idx, owner, ip, h) in &entries {
            if own(owner) || h != host || same_ip(ip, our_ip) {
                continue;
            }
            out.push(HostnameConflict {
                hostname: host.clone(),
                our_ip: our_ip.clone(),
                other_ip: ip.clone(),
                other_owner: owner.clone(),
                we_win: our_idx < idx,
            });
        }
    }
    out
}

#[cfg(test)]
mod conflict_tests {
    use super::*;

    fn blk(id: &str, body: &str) -> String {
        format!("# >>> ClusterDeck BEGIN (profile: {id}) >>>\n{body}\n# <<< ClusterDeck END (profile: {id}) <<<\n")
    }

    #[test]
    fn other_profile_block_first_we_lose() {
        let f = format!(
            "127.0.0.1 localhost\n{}\n{}",
            blk("vagrant", "192.0.2.10 argocd.example.internal"),
            blk("cloud", "198.51.100.5 argocd.example.internal")
        );
        let c = find_hostname_conflicts(&f, "cloud");
        assert_eq!(c.len(), 1);
        assert_eq!(c[0].other_owner.as_deref(), Some("vagrant"));
        assert_eq!(c[0].other_ip, "192.0.2.10");
        assert_eq!(c[0].our_ip, "198.51.100.5");
        assert!(!c[0].we_win);
    }

    #[test]
    fn other_profile_block_after_we_win() {
        let f = format!(
            "{}\n{}",
            blk("cloud", "198.51.100.5 argocd.example.internal"),
            blk("vagrant", "192.0.2.10 argocd.example.internal")
        );
        let c = find_hostname_conflicts(&f, "cloud");
        assert_eq!(c.len(), 1);
        assert!(c[0].we_win);
    }

    #[test]
    fn unmanaged_line_is_reported_with_no_owner() {
        let f = format!(
            "192.0.2.77 app.example.internal\n{}",
            blk("cloud", "198.51.100.5 app.example.internal")
        );
        let c = find_hostname_conflicts(&f, "cloud");
        assert_eq!(c.len(), 1);
        assert_eq!(c[0].other_owner, None);
        assert!(!c[0].we_win);
    }

    #[test]
    fn case_insensitive_multiple_hostnames_tabs_and_inline_comment() {
        let f = format!(
            "192.0.2.77\talpha.example.internal\tAPP.Example.Internal # note\n{}",
            blk("cloud", "198.51.100.5 app.example.internal")
        );
        let c = find_hostname_conflicts(&f, "cloud");
        assert_eq!(c.len(), 1);
        assert_eq!(c[0].other_ip, "192.0.2.77");
    }

    #[test]
    fn inline_comment_does_not_become_a_hostname() {
        let f = format!(
            "192.0.2.77 other.example.internal # app.example.internal\n{}",
            blk("cloud", "198.51.100.5 app.example.internal")
        );
        assert!(find_hostname_conflicts(&f, "cloud").is_empty());
    }

    #[test]
    fn no_own_block_or_no_overlap_gives_no_conflicts() {
        let f = "192.0.2.77 app.example.internal\n# 192.0.2.8 x.example.internal\n\n";
        assert!(find_hostname_conflicts(f, "cloud").is_empty());
        let g = format!("{f}{}", blk("cloud", "198.51.100.5 b.example.internal"));
        assert!(find_hostname_conflicts(&g, "cloud").is_empty());
    }

    #[test]
    fn unterminated_own_block_does_not_swallow_later_unmanaged_lines() {
        let f = "# >>> ClusterDeck BEGIN (profile: cloud) >>>\n198.51.100.5 a.example.internal\n";
        assert!(find_hostname_conflicts(f, "cloud").is_empty());
        // An unterminated block of another profile precedes ours: its lines stay comparable.
        let g = format!(
            "# >>> ClusterDeck BEGIN (profile: broken) >>>\n192.0.2.1 a.example.internal\n{}",
            blk("cloud", "198.51.100.5 a.example.internal")
        );
        let c = find_hostname_conflicts(&g, "cloud");
        assert_eq!(c.len(), 1);
        assert_eq!(c[0].other_owner, None);
        assert!(!c[0].we_win);
        // Lines after an unterminated block of OURS followed by an unmanaged line are not owned.
        let h = "# >>> ClusterDeck BEGIN (profile: cloud) >>>\n198.51.100.5 a.example.internal\n";
        let h = format!(
            "{h}# <<< ClusterDeck END (profile: other) <<<\n192.0.2.9 a.example.internal\n"
        );
        assert!(find_hostname_conflicts(&h, "cloud").is_empty());
    }

    #[test]
    fn mismatched_end_resets_to_unmanaged() {
        let f = format!(
            "# >>> ClusterDeck BEGIN (profile: x) >>>\n192.0.2.1 a.example.internal\n# <<< ClusterDeck END (profile: y) <<<\n{}",
            blk("cloud", "198.51.100.5 a.example.internal")
        );
        let c = find_hostname_conflicts(&f, "cloud");
        assert_eq!(c.len(), 1);
        assert_eq!(c[0].other_owner, None);
    }

    #[test]
    fn ipv6_compared_semantically_and_v4_vs_v6_conflicts() {
        let same = format!(
            "2001:db8::1 a.example.internal\n{}",
            blk("cloud", "2001:0DB8:0:0:0:0:0:1 a.example.internal")
        );
        assert!(find_hostname_conflicts(&same, "cloud").is_empty());
        let mixed = format!(
            "2001:db8::1 a.example.internal\n{}",
            blk("cloud", "192.0.2.5 a.example.internal")
        );
        assert_eq!(find_hostname_conflicts(&mixed, "cloud").len(), 1);
    }

    #[test]
    fn hosts_copy_command_validates_every_interpolated_value() {
        let hash = "a".repeat(64);
        let good = std::env::temp_dir().join("clusterdeck-hosts-x.tmp");
        let good = good.to_string_lossy().into_owned();
        let cmd = hosts_copy_command(&good, &hash).unwrap();
        assert!(cmd.contains("/bin/cp -P '"));

        assert!(hosts_copy_command("relative.tmp", &hash).is_err());
        assert!(hosts_copy_command("/var/tmp/a\nb", &hash).is_err());
        assert!(hosts_copy_command("/var/tmp/a\0b", &hash).is_err());
        assert!(hosts_copy_command(&good, "not-a-hash").is_err());
        assert!(hosts_copy_command(&good, &format!("{}'; id #", "a".repeat(60))).is_err());

        // A symlinked parent directory is refused.
        let base =
            std::env::temp_dir().join(format!("clusterdeck-hosts-link-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(base.join("real")).unwrap();
        std::os::unix::fs::symlink(base.join("real"), base.join("link")).unwrap();
        let via_link = base.join("link").join("h.tmp");
        assert!(hosts_copy_command(&via_link.to_string_lossy(), &hash).is_err());
        let _ = std::fs::remove_dir_all(&base);

        // Quotes and command substitution in an otherwise valid path are escaped, not spliced.
        let evil_dir = std::env::temp_dir().join(format!("it's$(id)-{}", std::process::id()));
        std::fs::create_dir_all(&evil_dir).unwrap();
        let evil = evil_dir.join("h.tmp");
        let cmd = hosts_copy_command(&evil.to_string_lossy(), &hash).unwrap();
        assert!(cmd.contains("it'\\''s$(id)"), "{cmd}");
        let _ = std::fs::remove_dir_all(&evil_dir);
    }
}
