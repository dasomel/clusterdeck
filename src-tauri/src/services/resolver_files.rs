#![allow(dead_code)]

use std::collections::{BTreeMap, HashSet};

use crate::services::validate::{is_allowed_private_zone, is_safe_profile_id};

pub const RESOLVER_DIR: &str = "/etc/resolver";
pub const DEFAULT_DNS_PORT: u16 = 15353;
/// Each command carries its own `|| true`: a failing flush must never fail the script after the
/// resolver files were already applied.
pub const FLUSH_DNS_CACHE: &str =
    "/usr/bin/dscacheutil -flushcache || true; /usr/bin/killall -HUP mDNSResponder || true";

const MARKER_PREFIX: &str = "# managed by ClusterDeck (profile: ";
const MARKER_SUFFIX: &str = ")";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConflictKind {
    Unowned,
    OwnedByOtherProfile {
        owner: String,
    },
    InvalidZone,
    /// The `/etc/resolver` entry for this zone is a symlink, not a regular file, or unreadable:
    /// ownership cannot be established, so it is never written or served.
    Unreadable,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Conflict {
    pub zone: String,
    pub kind: ConflictKind,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ResolverPlan {
    pub write: Vec<(String, String)>,
    pub remove: Vec<(String, String)>,
    pub conflicts: Vec<Conflict>,
    /// Records the prior owner on disk for writes authorized via `approved_switches`, so the
    /// privileged script checks the on-disk owner's marker before replacing the file.
    pub switched_from: BTreeMap<String, String>,
}

pub fn render_resolver_file(profile_id: &str, port: u16) -> Result<String, String> {
    if !is_safe_profile_id(profile_id) {
        return Err(format!("invalid profile id: {profile_id}"));
    }
    if port == 0 {
        return Err("invalid port: 0".to_string());
    }
    Ok(format!(
        "# managed by ClusterDeck (profile: {profile_id})\nnameserver 127.0.0.1\nport {port}\n"
    ))
}

/// Extracts the owning `profile_id` only when the exact first line is a valid ClusterDeck marker.
/// Uses `split('\n')` rather than `lines()` so a trailing `\r` or leading whitespace is rejected.
pub fn parse_marker(content: &str) -> Option<String> {
    let first_line = content.split('\n').next()?;
    let profile_id = first_line
        .strip_prefix(MARKER_PREFIX)?
        .strip_suffix(MARKER_SUFFIX)?;
    is_safe_profile_id(profile_id).then(|| profile_id.to_string())
}

pub fn plan_changes(
    desired: &[(String, String)],
    existing: &[(String, String)],
    approved_switches: &[(String, String, String)],
) -> ResolverPlan {
    plan_changes_for_port(desired, existing, approved_switches, DEFAULT_DNS_PORT)
}

pub fn plan_changes_for_port(
    desired: &[(String, String)],
    existing: &[(String, String)],
    approved_switches: &[(String, String, String)],
    port: u16,
) -> ResolverPlan {
    let mut plan = ResolverPlan::default();

    // Existing filenames that fail `is_allowed_private_zone` can never belong to ClusterDeck and
    // must be ignored completely (neither removed nor reported).
    let mut existing_by_zone: BTreeMap<&str, &str> = BTreeMap::new();
    for (name, content) in existing {
        if is_allowed_private_zone(name) {
            existing_by_zone.insert(name.as_str(), content.as_str());
        }
    }

    let mut claimants_by_zone: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for (zone, owner) in desired {
        if is_allowed_private_zone(zone) && is_safe_profile_id(owner) {
            let claimants = claimants_by_zone.entry(zone.as_str()).or_default();
            if !claimants.contains(&owner.as_str()) {
                claimants.push(owner.as_str());
            }
        }
    }

    let desired_valid_zones: HashSet<&str> = claimants_by_zone.keys().copied().collect();
    let mut processed_zones: HashSet<&str> = HashSet::new();

    for (zone_str, owner) in desired {
        if !is_allowed_private_zone(zone_str) || !is_safe_profile_id(owner) {
            plan.conflicts.push(Conflict {
                zone: zone_str.clone(),
                kind: ConflictKind::InvalidZone,
            });
            continue;
        }
        let zone = zone_str.as_str();
        if !processed_zones.insert(zone) {
            continue;
        }

        let claimants = &claimants_by_zone[zone];
        let is_switch_approved = |from: &str, to: &str| {
            approved_switches
                .iter()
                .any(|(z, f, t)| z == zone && f == from && t == to)
        };

        match existing_by_zone.get(zone) {
            None => {
                let switched = claimants.iter().find_map(|&from| {
                    claimants
                        .iter()
                        .copied()
                        .find(|&to| to != from && is_switch_approved(from, to))
                        .map(|to| (from, to))
                });
                let (from_opt, winner) = match switched {
                    Some((from, to)) => (Some(from), to),
                    None => (None, claimants[0]),
                };
                plan.write.push((zone.to_string(), winner.to_string()));
                for &c in claimants {
                    if c != winner && Some(c) != from_opt {
                        plan.conflicts.push(Conflict {
                            zone: zone.to_string(),
                            kind: ConflictKind::OwnedByOtherProfile {
                                owner: winner.to_string(),
                            },
                        });
                    }
                }
            }
            Some(&content) => match parse_marker(content) {
                None => {
                    plan.conflicts.push(Conflict {
                        zone: zone.to_string(),
                        kind: ConflictKind::Unowned,
                    });
                }
                Some(existing_owner) => {
                    if let Some(&new_owner) = claimants
                        .iter()
                        .find(|&&c| c != existing_owner && is_switch_approved(&existing_owner, c))
                    {
                        plan.write.push((zone.to_string(), new_owner.to_string()));
                        plan.switched_from
                            .insert(zone.to_string(), existing_owner.clone());
                        for &c in claimants {
                            if c != new_owner && c != existing_owner {
                                plan.conflicts.push(Conflict {
                                    zone: zone.to_string(),
                                    kind: ConflictKind::OwnedByOtherProfile {
                                        owner: new_owner.to_string(),
                                    },
                                });
                            }
                        }
                    } else {
                        if claimants.contains(&existing_owner.as_str()) {
                            if let Ok(expected) = render_resolver_file(&existing_owner, port) {
                                if content != expected {
                                    plan.write.push((zone.to_string(), existing_owner.clone()));
                                }
                            }
                        }
                        for &c in claimants {
                            if c != existing_owner {
                                plan.conflicts.push(Conflict {
                                    zone: zone.to_string(),
                                    kind: ConflictKind::OwnedByOtherProfile {
                                        owner: existing_owner.clone(),
                                    },
                                });
                            }
                        }
                    }
                }
            },
        }
    }

    for (name, content) in existing {
        if !is_allowed_private_zone(name) || desired_valid_zones.contains(name.as_str()) {
            continue;
        }
        if let Some(existing_owner) = parse_marker(content) {
            plan.remove.push((name.clone(), existing_owner));
        }
    }

    plan
}

fn validate_dir_path(path: &str, label: &str) -> Result<(), String> {
    if path.is_empty() || !path.starts_with('/') || path == "/" {
        return Err(format!("{label} must be a non-root absolute path: {path}"));
    }
    if path.split('/').any(|seg| seg == ".." || seg == ".") {
        return Err(format!(
            "{label} must not contain traversal segments: {path}"
        ));
    }
    if path.chars().any(|c| {
        c.is_control()
            || matches!(
                c,
                '\'' | '"' | '`' | '$' | '\\' | ';' | '&' | '|' | '<' | '>'
            )
    }) {
        return Err(format!("{label} contains unsafe characters: {path}"));
    }
    Ok(())
}

/// POSIX single-quote escaping for `/bin/sh`: wraps in `'...'` and replaces each `'` with `'\''`.
pub(crate) fn sh_single_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// Escapes `\` and `"` for embedding inside an AppleScript double-quoted string literal.
fn escape_applescript_string(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

/// Builds a single `osascript` `do shell script ... with administrator privileges` expression.
///
/// The script runs as root and never reads user-writable input: resolver file content is generated
/// inside the script from a re-validated profile id and port (no staging directory, so there is no
/// confused-deputy copy). Each write goes `mktemp` -> `printf` -> `chmod` -> ownership guard -> `mv`
/// inside the resolver directory so the final rename is atomic. Every step stops the script with its
/// own exit code instead of relying on `set -e`, which does not cover `&&`/`||` lists:
/// 2 mkdir, 3 write guard, 4 remove guard, 5 temp-file write steps, 6 rm.
pub fn build_privileged_script(
    resolver_dir: &str,
    plan: &ResolverPlan,
    port: u16,
) -> Result<String, String> {
    build_privileged_script_with(resolver_dir, plan, port, &ScriptOptions::default())
}

/// Optional extra steps for `build_privileged_script_with`. Extra exit code: 7 = listener
/// ownership check failed (the hosts copy propagates `cp`'s own status).
#[derive(Debug, Clone, Default)]
pub struct ScriptOptions<'a> {
    /// When set, the script first requires the UDP listener and the TCP LISTEN socket on `port`
    /// to both belong to exactly this pid (R8: re-checked as root right before writing, closing
    /// the gap between the unprivileged ownership gate and the prompt being approved).
    pub expected_pid: Option<u32>,
    /// A user-staged hosts file copied to /etc/hosts FIRST (R11: one prompt for a hosts<->DNS
    /// mode switch). Same `hosts_file::hosts_copy_command` as `write_hosts_file`.
    ///
    /// D-HOSTS-COPY: the user-owned staged file is never copied straight onto /etc/hosts; root
    /// copies it to its own temp file, verifies the content hash and type, and renames it, so a
    /// same-UID swap after staging cannot inject different content.
    pub hosts_tmp: Option<&'a std::path::Path>,
    /// SHA-256 (hex) of the /etc/hosts snapshot `hosts_tmp` was computed from; exit 8 when the
    /// live file no longer matches. The hosts step runs after every resolver step.
    pub hosts_sha256: Option<&'a str>,
    /// D-MBB: the hosts change keeps every existing line (adds only). Placed before resolver
    /// removals when true, after them otherwise. Default false (= the safer "last").
    pub hosts_additive: bool,
    /// SHA-256 (hex) of the intended NEW hosts content; required with `hosts_tmp`. The script
    /// verifies its private copy against it (D-HOSTS-COPY, exit 9).
    pub hosts_content_sha256: Option<String>,
}

/// SHA-256 hex of a hosts snapshot, as compared by the script's `shasum` check.
pub fn sha256_hex(content: &str) -> String {
    use sha2::{Digest, Sha256};
    Sha256::digest(content.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

pub fn build_privileged_script_with(
    resolver_dir: &str,
    plan: &ResolverPlan,
    port: u16,
    opts: &ScriptOptions<'_>,
) -> Result<String, String> {
    if port == 0 {
        return Err("invalid port: 0".to_string());
    }
    validate_dir_path(resolver_dir, "resolver_dir")?;

    for (zone, from_owner) in &plan.switched_from {
        if !is_allowed_private_zone(zone) {
            return Err(format!("invalid zone in switched_from: {zone}"));
        }
        if !is_safe_profile_id(from_owner) {
            return Err(format!("invalid profile id in switched_from: {from_owner}"));
        }
    }

    let resolver_base = resolver_dir.trim_end_matches('/');

    let mut cmds: Vec<String> = vec!["set -e".to_string()];

    if let Some(pid) = opts.expected_pid {
        cmds.push(format!(
            "[ \"$(/usr/sbin/lsof -nP -iUDP:{port} -t)\" = \"{pid}\" ] || exit 7"
        ));
        cmds.push(format!(
            "[ \"$(/usr/sbin/lsof -nP -iTCP:{port} -sTCP:LISTEN -t)\" = \"{pid}\" ] || exit 7"
        ));
    }

    // D-MBB (make-before-break): steps are ordered by EFFECT so a failure at any point leaves at
    // least one working name mapping:
    //   (1) resolver writes, (2) hosts step if purely additive, (3) resolver removals,
    //   (4) hosts step if it removes anything.
    // LocalDns->Hosts therefore adds the hosts block before removing resolver files, and
    // Hosts->LocalDns writes resolver files before removing the hosts block. Any failure
    // (including exit 8) aborts the remaining steps; the caller reconciles from disk.
    let hosts_additive = opts.hosts_additive;
    let mut hosts_cmds: Vec<String> = Vec::new();
    if let Some(tmp) = opts.hosts_tmp {
        let tmp = tmp
            .to_str()
            .ok_or_else(|| "hosts staging path is not valid UTF-8".to_string())?;
        if let Some(hash) = opts.hosts_sha256 {
            if hash.len() != 64 || !hash.bytes().all(|b| b.is_ascii_hexdigit()) {
                return Err("hosts_sha256 must be 64 hex characters".to_string());
            }
            // ADR-0004 snapshot discipline, enforced as root: the staged content was computed
            // from a snapshot with this hash; if /etc/hosts changed since, exit 8 and skip
            // every remaining step rather than clobber the concurrent edit.
            hosts_cmds.push(format!(
                "[ \"$(/usr/bin/shasum -a 256 < {} | /usr/bin/cut -d' ' -f1)\" = \"{hash}\" ] || exit 8",
                crate::services::hosts_file::HOSTS_FILE_PATH
            ));
        }
        let content_hash = opts
            .hosts_content_sha256
            .as_deref()
            .ok_or_else(|| "hosts_content_sha256 is required with hosts_tmp".to_string())?;
        if content_hash.len() != 64 || !content_hash.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err("hosts_content_sha256 must be 64 hex characters".to_string());
        }
        // Path and hash validation plus shell quoting happen inside `hosts_copy_command`.
        hosts_cmds.push(crate::services::hosts_file::hosts_copy_command(
            tmp,
            content_hash,
        )?);
    }

    if !plan.write.is_empty() {
        let q_resolver_dir = sh_single_quote(resolver_base);
        cmds.push(format!("/bin/mkdir -p {q_resolver_dir} || exit 2"));
        cmds.push(format!(
            "{{ [ -d {q_resolver_dir} ] && [ ! -L {q_resolver_dir} ]; }} || exit 3"
        ));
    }

    for (zone, owner) in &plan.write {
        if !is_allowed_private_zone(zone) {
            return Err(format!("invalid zone in write plan: {zone}"));
        }
        if !is_safe_profile_id(owner) {
            return Err(format!("invalid profile id in write plan: {owner}"));
        }
        let current_owner = plan
            .switched_from
            .get(zone)
            .map(String::as_str)
            .unwrap_or(owner.as_str());
        if !is_safe_profile_id(current_owner) {
            return Err(format!("invalid current owner profile id: {current_owner}"));
        }

        // Re-validates the id and port; the lines below are the only data that reaches printf.
        let content = render_resolver_file(owner, port)?;
        let q_lines: Vec<String> = content.lines().map(sh_single_quote).collect();

        let target = format!("{resolver_base}/{zone}");
        let template = format!("{resolver_base}/.clusterdeck.XXXXXX");
        let expected_marker = format!("# managed by ClusterDeck (profile: {current_owner})");

        let q_target = sh_single_quote(&target);
        let q_template = sh_single_quote(&template);
        let q_marker = sh_single_quote(&expected_marker);
        let q_content = q_lines.join(" ");
        let cleanup = "{ /bin/rm -f \"$tmp\"; exit 5; }";

        cmds.push(format!("tmp=$(/usr/bin/mktemp {q_template}) || exit 5"));
        cmds.push(format!(
            "/usr/bin/printf '%s\\n' {q_content} > \"$tmp\" || {cleanup}"
        ));
        cmds.push(format!("/bin/chmod 0644 \"$tmp\" || {cleanup}"));
        // Check both `-e` and `-L` so a dangling symlink still enters the guard and triggers exit 3;
        // the temp file is removed first so a refused write leaves nothing behind.
        cmds.push(format!(
            "if [ -e {q_target} ] || [ -L {q_target} ]; then {{ [ -f {q_target} ] && [ ! -L {q_target} ] && IFS= read -r line < {q_target} && [ \"$line\" = {q_marker} ]; }} || {{ /bin/rm -f \"$tmp\"; exit 3; }}; fi"
        ));
        cmds.push(format!("/bin/mv -f \"$tmp\" {q_target} || {cleanup}"));
    }

    if hosts_additive {
        cmds.extend(hosts_cmds.iter().cloned());
    }

    for (zone, expected_owner) in &plan.remove {
        if !is_allowed_private_zone(zone) {
            return Err(format!("invalid zone in remove plan: {zone}"));
        }
        if !is_safe_profile_id(expected_owner) {
            return Err(format!(
                "invalid profile id in remove plan: {expected_owner}"
            ));
        }

        let target = format!("{resolver_base}/{zone}");
        let expected_marker = format!("# managed by ClusterDeck (profile: {expected_owner})");

        let q_target = sh_single_quote(&target);
        let q_marker = sh_single_quote(&expected_marker);

        cmds.push(format!(
            "{{ [ -f {q_target} ] && [ ! -L {q_target} ] && IFS= read -r line < {q_target} && [ \"$line\" = {q_marker} ]; }} || exit 4"
        ));
        cmds.push(format!("/bin/rm -f {q_target} || exit 6"));
    }

    if !hosts_additive {
        cmds.extend(hosts_cmds);
    }

    cmds.push(FLUSH_DNS_CACHE.to_string());

    let sh_script = cmds.join("; ");
    let escaped = escape_applescript_string(&sh_script);
    Ok(format!(
        "do shell script \"{escaped}\" with administrator privileges"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};

    static TEST_DIR_SEQ: AtomicU64 = AtomicU64::new(0);

    /// Per-test scratch directory standing in for `/etc/resolver`, removed on drop.
    struct TestDir(PathBuf);

    impl TestDir {
        fn new() -> Self {
            let seq = TEST_DIR_SEQ.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "clusterdeck-resolver-test-{}-{seq}",
                std::process::id()
            ));
            std::fs::create_dir_all(&path).unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
            TestDir(path)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TestDir {
        fn drop(&mut self) {
            let _ = std::fs::set_permissions(&self.0, std::fs::Permissions::from_mode(0o700));
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn unescape_applescript_do_shell_script(script: &str) -> String {
        let inner = script
            .strip_prefix("do shell script \"")
            .expect("must start with do shell script")
            .strip_suffix("\" with administrator privileges")
            .expect("must end with administrator privileges");
        let mut out = String::with_capacity(inner.len());
        let mut chars = inner.chars();
        while let Some(c) = chars.next() {
            if c == '\\' {
                let next = chars
                    .next()
                    .expect("escaped char must follow backslash in AppleScript string");
                out.push(next);
            } else {
                out.push(c);
            }
        }
        out
    }

    #[test]
    fn marker_render_and_parse_round_trip_and_rejects_near_misses() {
        let rendered = render_resolver_file("lab-profile_1", 15353).unwrap();
        assert_eq!(
            rendered,
            "# managed by ClusterDeck (profile: lab-profile_1)\nnameserver 127.0.0.1\nport 15353\n"
        );
        assert_eq!(parse_marker(&rendered).as_deref(), Some("lab-profile_1"));

        // Invalid profile IDs rejected by render_resolver_file
        assert!(render_resolver_file("", 15353).is_err());
        assert!(render_resolver_file("../evil", 15353).is_err());
        assert!(render_resolver_file("bad space", 15353).is_err());
        assert!(render_resolver_file("lab", 0).is_err());

        // Near-miss markers rejected by parse_marker
        assert_eq!(parse_marker(""), None);
        assert_eq!(
            parse_marker(
                "\n# managed by ClusterDeck (profile: lab)\nnameserver 127.0.0.1\nport 15353\n"
            ),
            None
        );
        assert_eq!(
            parse_marker(
                "nameserver 127.0.0.1\n# managed by ClusterDeck (profile: lab)\nport 15353\n"
            ),
            None
        );
        assert_eq!(
            parse_marker(" # managed by ClusterDeck (profile: lab)\nnameserver 127.0.0.1\n"),
            None
        );
        assert_eq!(
            parse_marker("# managed by ClusterDeck (profile: lab) \nnameserver 127.0.0.1\n"),
            None
        );
        assert_eq!(
            parse_marker("# managed by ClusterDeck (profile: lab)\r\nnameserver 127.0.0.1\n"),
            None
        );
        assert_eq!(
            parse_marker("# managed by ClusterDeck (profile: )\nnameserver 127.0.0.1\n"),
            None
        );
        assert_eq!(
            parse_marker("# managed by ClusterDeck (profile: ../evil)\nnameserver 127.0.0.1\n"),
            None
        );
        assert_eq!(
            parse_marker("# managed by ClusterDeck (profile: lab) extra\nnameserver 127.0.0.1\n"),
            None
        );
    }

    #[test]
    fn plan_changes_handles_missing_noop_diff_and_removal() {
        let up_to_date = render_resolver_file("prof-a", DEFAULT_DNS_PORT).unwrap();
        let outdated_port = render_resolver_file("prof-a", 15354).unwrap();
        let stale_zone = render_resolver_file("prof-b", DEFAULT_DNS_PORT).unwrap();

        let desired = vec![
            ("new.example.internal".to_string(), "prof-a".to_string()),
            ("same.example.internal".to_string(), "prof-a".to_string()),
            ("diff.example.internal".to_string(), "prof-a".to_string()),
        ];
        let existing = vec![
            ("same.example.internal".to_string(), up_to_date),
            ("diff.example.internal".to_string(), outdated_port),
            ("old.example.internal".to_string(), stale_zone),
            // Non-private or invalid zone filenames in /etc/resolver are ignored completely.
            (
                "github.com".to_string(),
                render_resolver_file("prof-a", DEFAULT_DNS_PORT).unwrap(),
            ),
            (
                "corp.internal".to_string(),
                "nameserver 10.0.0.1\n".to_string(),
            ),
        ];

        let plan = plan_changes(&desired, &existing, &[]);
        assert_eq!(
            plan.write,
            vec![
                ("new.example.internal".to_string(), "prof-a".to_string()),
                ("diff.example.internal".to_string(), "prof-a".to_string()),
            ]
        );
        assert_eq!(
            plan.remove,
            vec![("old.example.internal".to_string(), "prof-b".to_string())]
        );
        assert!(plan.conflicts.is_empty());
    }

    #[test]
    fn plan_changes_handles_unowned_switches_and_invalid_desired_zones() {
        let owned_by_a = render_resolver_file("prof-a", DEFAULT_DNS_PORT).unwrap();
        let owned_by_b = render_resolver_file("prof-b", DEFAULT_DNS_PORT).unwrap();

        let desired = vec![
            ("unowned.example.internal".to_string(), "prof-a".to_string()),
            ("blocked.example.internal".to_string(), "prof-b".to_string()),
            (
                "switched.example.internal".to_string(),
                "prof-c".to_string(),
            ),
            ("github.com".to_string(), "prof-a".to_string()),
            ("two.internal".to_string(), "prof-a".to_string()),
        ];
        let existing = vec![
            (
                "unowned.example.internal".to_string(),
                "nameserver 192.0.2.1\n".to_string(),
            ),
            ("blocked.example.internal".to_string(), owned_by_a),
            ("switched.example.internal".to_string(), owned_by_b),
            (
                "keep-unowned.example.internal".to_string(),
                "nameserver 192.0.2.2\n".to_string(),
            ),
        ];
        let approved_switches = vec![(
            "switched.example.internal".to_string(),
            "prof-b".to_string(),
            "prof-c".to_string(),
        )];

        let plan = plan_changes(&desired, &existing, &approved_switches);

        assert_eq!(
            plan.write,
            vec![(
                "switched.example.internal".to_string(),
                "prof-c".to_string()
            )]
        );
        assert_eq!(
            plan.switched_from
                .get("switched.example.internal")
                .map(String::as_str),
            Some("prof-b")
        );
        assert!(plan.remove.is_empty());
        assert_eq!(
            plan.conflicts,
            vec![
                Conflict {
                    zone: "unowned.example.internal".to_string(),
                    kind: ConflictKind::Unowned,
                },
                Conflict {
                    zone: "blocked.example.internal".to_string(),
                    kind: ConflictKind::OwnedByOtherProfile {
                        owner: "prof-a".to_string(),
                    },
                },
                Conflict {
                    zone: "github.com".to_string(),
                    kind: ConflictKind::InvalidZone,
                },
                Conflict {
                    zone: "two.internal".to_string(),
                    kind: ConflictKind::InvalidZone,
                },
            ]
        );
    }

    #[test]
    fn plan_changes_resolves_multi_profile_claims_for_same_zone() {
        let desired = vec![
            ("shared.example.internal".to_string(), "prof-a".to_string()),
            ("shared.example.internal".to_string(), "prof-b".to_string()),
        ];

        // Without an approved switch, first/existing owner wins and the second is reported.
        let plan_conflict = plan_changes(&desired, &[], &[]);
        assert_eq!(
            plan_conflict.write,
            vec![("shared.example.internal".to_string(), "prof-a".to_string())]
        );
        assert_eq!(
            plan_conflict.conflicts,
            vec![Conflict {
                zone: "shared.example.internal".to_string(),
                kind: ConflictKind::OwnedByOtherProfile {
                    owner: "prof-a".to_string(),
                },
            }]
        );

        // With an approved switch from prof-a to prof-b, prof-b replaces prof-a cleanly.
        let existing = vec![(
            "shared.example.internal".to_string(),
            render_resolver_file("prof-a", DEFAULT_DNS_PORT).unwrap(),
        )];
        let switches = vec![(
            "shared.example.internal".to_string(),
            "prof-a".to_string(),
            "prof-b".to_string(),
        )];
        let plan_switched = plan_changes(&desired, &existing, &switches);
        assert_eq!(
            plan_switched.write,
            vec![("shared.example.internal".to_string(), "prof-b".to_string())]
        );
        assert_eq!(
            plan_switched
                .switched_from
                .get("shared.example.internal")
                .map(String::as_str),
            Some("prof-a")
        );
        assert!(plan_switched.conflicts.is_empty());
    }

    #[test]
    fn build_privileged_script_rejects_attack_inputs() {
        let valid_plan = ResolverPlan {
            write: vec![("lab.example.internal".to_string(), "prof-a".to_string())],
            remove: Vec::new(),
            conflicts: Vec::new(),
            switched_from: BTreeMap::new(),
        };

        let attack_zones = [
            "lab'quote.example.internal",
            "lab\"quote.example.internal",
            "$(id).example.internal",
            "`id`.example.internal",
            "lab\n.example.internal",
            "../lab.example.internal",
            "-lab.example.internal",
            "apps.github.com",
            "example.internal",
        ];
        for bad_zone in attack_zones {
            let mut p = valid_plan.clone();
            p.write = vec![(bad_zone.to_string(), "prof-a".to_string())];
            assert!(
                build_privileged_script(RESOLVER_DIR, &p, DEFAULT_DNS_PORT).is_err(),
                "write zone should be rejected: {bad_zone}"
            );

            let mut p_rem = valid_plan.clone();
            p_rem.write.clear();
            p_rem.remove = vec![(bad_zone.to_string(), "prof-a".to_string())];
            assert!(
                build_privileged_script(RESOLVER_DIR, &p_rem, DEFAULT_DNS_PORT).is_err(),
                "remove zone should be rejected: {bad_zone}"
            );
        }

        let attack_owners = [
            "prof'a",
            "prof\"a",
            "$(id)",
            "`id`",
            "prof\na",
            "../prof",
            "prof space",
            "",
        ];
        for bad_owner in attack_owners {
            let mut p = valid_plan.clone();
            p.write = vec![("lab.example.internal".to_string(), bad_owner.to_string())];
            assert!(
                build_privileged_script(RESOLVER_DIR, &p, DEFAULT_DNS_PORT).is_err(),
                "write owner should be rejected: {bad_owner}"
            );

            let mut p_rem = valid_plan.clone();
            p_rem.write.clear();
            p_rem.remove = vec![("lab.example.internal".to_string(), bad_owner.to_string())];
            assert!(
                build_privileged_script(RESOLVER_DIR, &p_rem, DEFAULT_DNS_PORT).is_err(),
                "remove owner should be rejected: {bad_owner}"
            );

            let mut p_sw = valid_plan.clone();
            p_sw.switched_from =
                BTreeMap::from([("lab.example.internal".to_string(), bad_owner.to_string())]);
            assert!(
                build_privileged_script(RESOLVER_DIR, &p_sw, DEFAULT_DNS_PORT).is_err(),
                "switched_from owner should be rejected: {bad_owner}"
            );
        }

        let attack_dirs = [
            "/tmp/quote'dir",
            "/tmp/quote\"dir",
            "/tmp/$(id)",
            "/tmp/`id`",
            "/tmp/new\nline",
            "/tmp/../etc/resolver",
            "-/tmp/leading-dash",
            "relative/dir",
            "/",
            "",
        ];
        for bad_dir in attack_dirs {
            assert!(
                build_privileged_script(bad_dir, &valid_plan, DEFAULT_DNS_PORT).is_err(),
                "resolver_dir should be rejected: {bad_dir}"
            );
        }

        assert!(build_privileged_script(RESOLVER_DIR, &valid_plan, 0).is_err());
    }

    #[test]
    fn script_has_no_staging_and_uses_absolute_binaries() {
        let plan = ResolverPlan {
            write: vec![("lab.example.internal".to_string(), "prof-a".to_string())],
            remove: vec![("old.example.internal".to_string(), "prof-b".to_string())],
            conflicts: Vec::new(),
            switched_from: BTreeMap::new(),
        };
        let applescript =
            build_privileged_script("/etc/resolver", &plan, DEFAULT_DNS_PORT).unwrap();
        let sh = unescape_applescript_do_shell_script(&applescript);

        // The only filesystem paths the script may mention live in the resolver dir or are system
        // binaries; in particular no user-writable temp/staging location.
        for token in sh.split(|c: char| c.is_whitespace() || matches!(c, '\'' | '"' | '(' | ')')) {
            if token.starts_with('/') {
                assert!(
                    token.starts_with("/etc/resolver")
                        || token.starts_with("/bin/")
                        || token.starts_with("/usr/bin/"),
                    "unexpected path in privileged script: {token}"
                );
            }
        }
        assert!(
            !sh.contains("/tmp")
                && !sh.contains("/var/folders")
                && !sh.contains("clusterdeck-resolver-")
        );
        assert!(!sh.contains("cp "), "root must not copy user-owned files");

        // Every external command is invoked by absolute path.
        for cmd in ["mkdir", "mktemp", "printf", "chmod", "mv", "rm"] {
            assert!(
                !sh.split(|c: char| c.is_whitespace() || c == '(' || c == ';')
                    .any(|w| w == cmd),
                "bare {cmd} in script"
            );
        }
    }

    fn run_sh(script: &str) -> std::process::ExitStatus {
        let sh = unescape_applescript_do_shell_script(script).replace(FLUSH_DNS_CACHE, "true");
        std::process::Command::new("/bin/sh")
            .args(["-c", &sh])
            .status()
            .unwrap()
    }

    fn no_temp_leftovers(dir: &Path) -> bool {
        !std::fs::read_dir(dir)
            .unwrap()
            .filter_map(Result::ok)
            .any(|e| e.file_name().to_string_lossy().starts_with(".clusterdeck."))
    }

    #[test]
    fn real_sh_executes_writes_preserves_unowned_and_switches_owners() {
        let dir = TestDir::new();
        let resolver_dir = dir.path();
        let resolver_dir_str = resolver_dir.to_str().unwrap();

        let unowned_path = resolver_dir.join("unowned.example.internal");
        std::fs::write(&unowned_path, "nameserver 192.0.2.53\n").unwrap();
        let switched_path = resolver_dir.join("switch.example.internal");
        std::fs::write(
            &switched_path,
            render_resolver_file("prof-a", DEFAULT_DNS_PORT).unwrap(),
        )
        .unwrap();
        let stale_path = resolver_dir.join("stale.example.internal");
        std::fs::write(
            &stale_path,
            render_resolver_file("prof-old", DEFAULT_DNS_PORT).unwrap(),
        )
        .unwrap();

        let existing = vec![
            (
                "unowned.example.internal".to_string(),
                "nameserver 192.0.2.53\n".to_string(),
            ),
            (
                "switch.example.internal".to_string(),
                render_resolver_file("prof-a", DEFAULT_DNS_PORT).unwrap(),
            ),
            (
                "stale.example.internal".to_string(),
                render_resolver_file("prof-old", DEFAULT_DNS_PORT).unwrap(),
            ),
        ];
        let desired = vec![
            ("lab.example.internal".to_string(), "prof-a".to_string()),
            ("unowned.example.internal".to_string(), "prof-a".to_string()),
            ("switch.example.internal".to_string(), "prof-b".to_string()),
        ];
        let switches = vec![(
            "switch.example.internal".to_string(),
            "prof-a".to_string(),
            "prof-b".to_string(),
        )];

        let plan = plan_changes(&desired, &existing, &switches);
        let applescript =
            build_privileged_script(resolver_dir_str, &plan, DEFAULT_DNS_PORT).unwrap();
        assert_eq!(
            applescript.matches("with administrator privileges").count(),
            1
        );

        let status = run_sh(&applescript);
        assert!(status.success(), "sh exited with {status:?}");

        let lab_path = resolver_dir.join("lab.example.internal");
        assert_eq!(
            std::fs::read_to_string(&lab_path).unwrap(),
            render_resolver_file("prof-a", DEFAULT_DNS_PORT).unwrap()
        );
        let lab_mode = std::fs::metadata(&lab_path).unwrap().permissions().mode() & 0o777;
        assert_eq!(lab_mode, 0o644);

        assert_eq!(
            std::fs::read_to_string(&switched_path).unwrap(),
            render_resolver_file("prof-b", DEFAULT_DNS_PORT).unwrap()
        );
        let switch_mode = std::fs::metadata(&switched_path)
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(switch_mode, 0o644);

        assert_eq!(
            std::fs::read_to_string(&unowned_path).unwrap(),
            "nameserver 192.0.2.53\n"
        );
        assert!(!stale_path.exists());
        assert!(no_temp_leftovers(resolver_dir), "temp file left behind");
    }

    #[test]
    fn real_sh_symlink_target_exits_3_with_nothing_written_after() {
        let dir = TestDir::new();
        let resolver_dir = dir.path();
        let resolver_dir_str = resolver_dir.to_str().unwrap();

        let decoy = resolver_dir.join("decoy.txt");
        std::fs::write(
            &decoy,
            render_resolver_file("prof-a", DEFAULT_DNS_PORT).unwrap(),
        )
        .unwrap();
        let symlink_target = resolver_dir.join("first.example.internal");
        std::os::unix::fs::symlink(&decoy, &symlink_target).unwrap();

        let plan = ResolverPlan {
            write: vec![
                ("first.example.internal".to_string(), "prof-a".to_string()),
                ("second.example.internal".to_string(), "prof-a".to_string()),
            ],
            remove: Vec::new(),
            conflicts: Vec::new(),
            switched_from: BTreeMap::new(),
        };
        let applescript =
            build_privileged_script(resolver_dir_str, &plan, DEFAULT_DNS_PORT).unwrap();

        assert_eq!(run_sh(&applescript).code(), Some(3));
        assert!(
            !resolver_dir.join("second.example.internal").exists(),
            "nothing must be written after the symlink failure"
        );
        assert!(
            std::fs::symlink_metadata(&symlink_target)
                .unwrap()
                .file_type()
                .is_symlink(),
            "symlink must be left in place"
        );
        assert!(no_temp_leftovers(resolver_dir), "temp file left behind");
    }

    #[test]
    fn real_sh_removing_file_with_wrong_owner_exits_4() {
        let dir = TestDir::new();
        let resolver_dir = dir.path();
        let resolver_dir_str = resolver_dir.to_str().unwrap();

        let wrong_owner_file = resolver_dir.join("lab.example.internal");
        std::fs::write(
            &wrong_owner_file,
            render_resolver_file("prof-other", DEFAULT_DNS_PORT).unwrap(),
        )
        .unwrap();

        let plan = ResolverPlan {
            write: Vec::new(),
            remove: vec![("lab.example.internal".to_string(), "prof-a".to_string())],
            conflicts: Vec::new(),
            switched_from: BTreeMap::new(),
        };
        let applescript =
            build_privileged_script(resolver_dir_str, &plan, DEFAULT_DNS_PORT).unwrap();

        assert_eq!(run_sh(&applescript).code(), Some(4));
        assert!(
            wrong_owner_file.exists(),
            "file with wrong owner must not be removed"
        );
    }

    /// A read-only resolver dir makes `mktemp` fail (exit 5); the script must stop right there
    /// rather than report success with files missing (the old `cp && chmod` set -e hole).
    #[test]
    fn real_sh_unwritable_dir_fails_with_5_and_skips_later_steps() {
        let dir = TestDir::new();
        let resolver_dir = dir.path();
        let resolver_dir_str = resolver_dir.to_str().unwrap();

        let stale = resolver_dir.join("stale.example.internal");
        std::fs::write(
            &stale,
            render_resolver_file("prof-old", DEFAULT_DNS_PORT).unwrap(),
        )
        .unwrap();

        let plan = ResolverPlan {
            write: vec![
                ("first.example.internal".to_string(), "prof-a".to_string()),
                ("second.example.internal".to_string(), "prof-a".to_string()),
            ],
            remove: vec![("stale.example.internal".to_string(), "prof-old".to_string())],
            conflicts: Vec::new(),
            switched_from: BTreeMap::new(),
        };
        let applescript =
            build_privileged_script(resolver_dir_str, &plan, DEFAULT_DNS_PORT).unwrap();

        std::fs::set_permissions(resolver_dir, std::fs::Permissions::from_mode(0o500)).unwrap();
        // Root bypasses directory permissions, which would make this test vacuous.
        let writable_anyway = std::fs::write(resolver_dir.join("probe"), "x").is_ok();
        let status = run_sh(&applescript);
        std::fs::set_permissions(resolver_dir, std::fs::Permissions::from_mode(0o700)).unwrap();
        if writable_anyway {
            return;
        }

        assert_eq!(status.code(), Some(5));
        assert!(!resolver_dir.join("first.example.internal").exists());
        assert!(!resolver_dir.join("second.example.internal").exists());
        assert!(
            stale.exists(),
            "remove step must not run after a failed write"
        );
    }

    /// A failing final `mv` (simulated by swapping the binary for `false`) must exit 5 and remove its
    /// temp file, leaving the target absent.
    #[test]
    fn legacy_clusterdeck_local_owned_file_is_removed_when_zone_moves_to_internal() {
        // D3 (issue #119): the alias zone moved `.local` -> `.internal`; the owned legacy file
        // is removed, an unowned `.local` file is never touched.
        let owned = render_resolver_file("lab", DEFAULT_DNS_PORT).unwrap();
        let existing = vec![
            ("lab.clusterdeck.local".to_string(), owned),
            (
                "foreign.clusterdeck.local".to_string(),
                "nameserver 10.0.0.1\n".to_string(),
            ),
        ];
        let desired = vec![("lab.clusterdeck.internal".to_string(), "lab".to_string())];
        let plan = plan_changes(&desired, &existing, &[]);
        assert_eq!(
            plan.write,
            vec![("lab.clusterdeck.internal".to_string(), "lab".to_string())]
        );
        assert_eq!(
            plan.remove,
            vec![("lab.clusterdeck.local".to_string(), "lab".to_string())]
        );
    }

    #[test]
    fn real_sh_failing_mv_exits_5_and_removes_temp_file() {
        let dir = TestDir::new();
        let resolver_dir = dir.path();
        let resolver_dir_str = resolver_dir.to_str().unwrap();

        let plan = ResolverPlan {
            write: vec![("lab.example.internal".to_string(), "prof-a".to_string())],
            remove: Vec::new(),
            conflicts: Vec::new(),
            switched_from: BTreeMap::new(),
        };
        let applescript =
            build_privileged_script(resolver_dir_str, &plan, DEFAULT_DNS_PORT).unwrap();
        let sh = unescape_applescript_do_shell_script(&applescript);
        assert!(sh.contains("/bin/mv -f \"$tmp\" "));
        assert!(sh.contains("|| { /bin/rm -f \"$tmp\"; exit 5; }"));

        // Force mv to fail at runtime: swap the real mv for one that always fails.
        let failing = sh.replace("/bin/mv -f", "/usr/bin/false");
        let status = std::process::Command::new("/bin/sh")
            .args(["-c", &failing.replace(FLUSH_DNS_CACHE, "true")])
            .status()
            .unwrap();
        assert_eq!(status.code(), Some(5));
        assert!(!resolver_dir.join("lab.example.internal").exists());
        assert!(no_temp_leftovers(resolver_dir), "temp file left behind");
    }

    #[test]
    fn real_sh_failing_flush_still_exits_0() {
        let dir = TestDir::new();
        let resolver_dir = dir.path();
        let resolver_dir_str = resolver_dir.to_str().unwrap();

        let plan = ResolverPlan {
            write: vec![("lab.example.internal".to_string(), "prof-a".to_string())],
            remove: Vec::new(),
            conflicts: Vec::new(),
            switched_from: BTreeMap::new(),
        };
        let applescript =
            build_privileged_script(resolver_dir_str, &plan, DEFAULT_DNS_PORT).unwrap();
        let sh = unescape_applescript_do_shell_script(&applescript);

        // Both flush commands fail; the files were already applied, so the script must exit 0.
        let failing_flush = FLUSH_DNS_CACHE
            .replace("/usr/bin/dscacheutil -flushcache", "/usr/bin/false")
            .replace("/usr/bin/killall -HUP mDNSResponder", "/usr/bin/false");
        assert_ne!(failing_flush, FLUSH_DNS_CACHE);
        assert!(sh.contains(FLUSH_DNS_CACHE));
        let status = std::process::Command::new("/bin/sh")
            .args(["-c", &sh.replace(FLUSH_DNS_CACHE, &failing_flush)])
            .status()
            .unwrap();
        assert!(
            status.success(),
            "flush failure must not fail the script: {status:?}"
        );
        assert!(resolver_dir.join("lab.example.internal").exists());
    }

    /// Runs the extracted script with `lsof` and `/etc/hosts` redirected to scratch paths.
    /// The stub prints `listener_pid` for every query, standing in for the real listener.
    fn run_ext(
        dir: &TestDir,
        plan: &ResolverPlan,
        opts: &ScriptOptions<'_>,
        listener_pid: &str,
        hosts_target: &Path,
    ) -> Option<i32> {
        let stub = dir.path().join("lsof-stub");
        std::fs::write(&stub, format!("#!/bin/sh\necho {listener_pid}\n")).unwrap();
        std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755)).unwrap();
        let resolver = dir.path().join("resolver");
        let applescript =
            build_privileged_script_with(resolver.to_str().unwrap(), plan, DEFAULT_DNS_PORT, opts)
                .unwrap();
        let sh = unescape_applescript_do_shell_script(&applescript)
            .replace(FLUSH_DNS_CACHE, "true")
            .replace("/usr/sbin/lsof", stub.to_str().unwrap())
            // D-HOSTS-COPY paths: /private/etc -> the scratch dir, chown (needs root) stubbed.
            .replace(
                "/private/etc/hosts",
                &format!("'{}'", hosts_target.display()),
            )
            .replace(
                "/private/etc/",
                &format!("{}/", hosts_target.parent().unwrap().display()),
            )
            .replace("/usr/sbin/chown root:wheel", "/usr/bin/true")
            .replace(" /etc/hosts", &format!(" '{}'", hosts_target.display()));
        std::process::Command::new("/bin/sh")
            .args(["-c", &sh])
            .status()
            .unwrap()
            .code()
    }

    fn one_write_plan() -> ResolverPlan {
        ResolverPlan {
            write: vec![("lab.example.internal".to_string(), "prof-a".to_string())],
            ..ResolverPlan::default()
        }
    }

    #[test]
    fn real_sh_expected_pid_mismatch_exits_7_before_any_write() {
        let dir = TestDir::new();
        let hosts_src = dir.path().join("hosts.staged");
        let hosts_dst = dir.path().join("hosts.live");
        std::fs::write(&hosts_src, "192.0.2.1 a.example.internal\n").unwrap();
        std::fs::write(&hosts_dst, "original\n").unwrap();
        let opts = ScriptOptions {
            expected_pid: Some(4242),
            hosts_tmp: Some(&hosts_src),
            hosts_sha256: None,
            hosts_additive: false,
            hosts_content_sha256: Some(file_sha(&hosts_src)),
        };

        let code = run_ext(&dir, &one_write_plan(), &opts, "9999", &hosts_dst);
        assert_eq!(code, Some(7));
        assert!(!dir.path().join("resolver/lab.example.internal").exists());
        assert_eq!(std::fs::read_to_string(&hosts_dst).unwrap(), "original\n");
    }

    #[test]
    fn real_sh_expected_pid_match_applies_resolver_files_then_hosts() {
        let dir = TestDir::new();
        let hosts_src = dir.path().join("hosts.staged");
        let hosts_dst = dir.path().join("hosts.live");
        std::fs::write(&hosts_src, "192.0.2.1 a.example.internal\n").unwrap();
        std::fs::write(&hosts_dst, "original\n").unwrap();
        let opts = ScriptOptions {
            expected_pid: Some(4242),
            hosts_tmp: Some(&hosts_src),
            hosts_sha256: None,
            hosts_additive: false,
            hosts_content_sha256: Some(file_sha(&hosts_src)),
        };

        let code = run_ext(&dir, &one_write_plan(), &opts, "4242", &hosts_dst);
        assert_eq!(code, Some(0));
        assert!(dir.path().join("resolver/lab.example.internal").exists());
        assert_eq!(
            std::fs::read_to_string(&hosts_dst).unwrap(),
            "192.0.2.1 a.example.internal\n"
        );

        let script = build_privileged_script_with(
            "/etc/resolver",
            &one_write_plan(),
            DEFAULT_DNS_PORT,
            &opts,
        )
        .unwrap();
        let udp = script.find("-iUDP:15353").expect("udp check");
        let tcp = script.find("-sTCP:LISTEN").expect("tcp check");
        let cp = script.find("/bin/cp -P '").expect("hosts copy");
        let mkdir = script.find("/bin/mkdir").expect("mkdir");
        assert!(udp < tcp && tcp < mkdir && mkdir < cp, "{script}");
    }

    #[test]
    fn real_sh_hosts_snapshot_mismatch_exits_8_and_match_copies() {
        let dir = TestDir::new();
        let hosts_src = dir.path().join("hosts.staged");
        let hosts_dst = dir.path().join("hosts.live");
        std::fs::write(&hosts_src, "192.0.2.1 a.example.internal\n").unwrap();
        std::fs::write(&hosts_dst, "edited concurrently\n").unwrap();
        let stale = sha256_hex("original\n");
        let opts = ScriptOptions {
            expected_pid: None,
            hosts_tmp: Some(&hosts_src),
            hosts_sha256: Some(&stale),
            hosts_additive: false,
            hosts_content_sha256: Some(file_sha(&hosts_src)),
        };
        let code = run_ext(&dir, &ResolverPlan::default(), &opts, "1", &hosts_dst);
        assert_eq!(code, Some(8));
        assert_eq!(
            std::fs::read_to_string(&hosts_dst).unwrap(),
            "edited concurrently\n"
        );

        let fresh = sha256_hex("edited concurrently\n");
        let opts = ScriptOptions {
            hosts_sha256: Some(&fresh),
            ..opts
        };
        let code = run_ext(&dir, &ResolverPlan::default(), &opts, "1", &hosts_dst);
        assert_eq!(code, Some(0));
        assert_eq!(
            std::fs::read_to_string(&hosts_dst).unwrap(),
            "192.0.2.1 a.example.internal\n"
        );
    }

    #[test]
    fn real_sh_resolver_failure_leaves_hosts_untouched() {
        let dir = TestDir::new();
        let hosts_src = dir.path().join("hosts.staged");
        let hosts_dst = dir.path().join("hosts.live");
        std::fs::write(&hosts_src, "192.0.2.1 a.example.internal\n").unwrap();
        std::fs::write(&hosts_dst, "original\n").unwrap();
        // Unowned file already at the target: the write guard exits 3.
        let resolver = dir.path().join("resolver");
        std::fs::create_dir_all(&resolver).unwrap();
        std::fs::write(
            resolver.join("lab.example.internal"),
            "nameserver 192.0.2.53\n",
        )
        .unwrap();
        let hash = sha256_hex("original\n");
        let opts = ScriptOptions {
            expected_pid: None,
            hosts_tmp: Some(&hosts_src),
            hosts_sha256: Some(&hash),
            hosts_additive: false,
            hosts_content_sha256: Some(file_sha(&hosts_src)),
        };
        let code = run_ext(&dir, &one_write_plan(), &opts, "1", &hosts_dst);
        assert_eq!(code, Some(3));
        assert_eq!(std::fs::read_to_string(&hosts_dst).unwrap(), "original\n");
    }

    #[test]
    fn script_options_default_adds_no_lsof_or_hosts_step() {
        let plain =
            build_privileged_script("/etc/resolver", &one_write_plan(), DEFAULT_DNS_PORT).unwrap();
        assert!(!plain.contains("lsof"));
        assert!(!plain.contains("/etc/hosts"));
    }

    #[test]
    fn hosts_tmp_that_is_relative_or_has_control_chars_is_rejected() {
        for bad in ["relative/path", "/tmp/a\nb", "/tmp/a\0b"] {
            let opts = ScriptOptions {
                expected_pid: None,
                hosts_tmp: Some(Path::new(bad)),
                hosts_sha256: None,
                hosts_additive: false,
                hosts_content_sha256: Some(file_sha(Path::new(bad))),
            };
            assert!(
                build_privileged_script_with(
                    "/etc/resolver",
                    &one_write_plan(),
                    DEFAULT_DNS_PORT,
                    &opts
                )
                .is_err(),
                "{bad:?}"
            );
        }
    }

    /// Parent must exist and not be a symlink (`/tmp` is one on macOS), so use the temp dir.
    const HOSTS_STAGED_DUMMY: &str = "/private/tmp/h.tmp";

    fn file_sha(p: &Path) -> String {
        std::fs::read_to_string(p)
            .map(|c| sha256_hex(&c))
            .unwrap_or_else(|_| "0".repeat(64))
    }

    // D-MBB: both switch directions, failure injected at each step; a mapping must remain.
    const HOST_LINE: &str = "192.0.2.1 h.example.internal\n";

    struct Switch {
        dir: TestDir,
        hosts_src: PathBuf,
        hosts_dst: PathBuf,
    }

    impl Switch {
        fn new(hosts_live: &str, hosts_staged: &str) -> Self {
            let dir = TestDir::new();
            std::fs::create_dir_all(dir.path().join("resolver")).unwrap();
            let hosts_src = dir.path().join("hosts.staged");
            let hosts_dst = dir.path().join("hosts.live");
            std::fs::write(&hosts_src, hosts_staged).unwrap();
            std::fs::write(&hosts_dst, hosts_live).unwrap();
            Switch {
                dir,
                hosts_src,
                hosts_dst,
            }
        }

        fn zone_file(&self) -> PathBuf {
            self.dir.path().join("resolver/lab.example.internal")
        }

        fn has_mapping(&self) -> bool {
            self.zone_file().exists()
                || std::fs::read_to_string(&self.hosts_dst)
                    .unwrap()
                    .contains("h.example.internal")
        }

        fn run(&self, plan: &ResolverPlan, snapshot: &str, additive: bool) -> Option<i32> {
            let hash = sha256_hex(snapshot);
            let opts = ScriptOptions {
                expected_pid: None,
                hosts_tmp: Some(&self.hosts_src),
                hosts_sha256: Some(&hash),
                hosts_additive: additive,
                hosts_content_sha256: Some(file_sha(&self.hosts_src)),
            };
            run_ext(&self.dir, plan, &opts, "1", &self.hosts_dst)
        }
    }

    fn remove_plan(owner: &str) -> ResolverPlan {
        ResolverPlan {
            remove: vec![("lab.example.internal".to_string(), owner.to_string())],
            ..ResolverPlan::default()
        }
    }

    #[test]
    fn mbb_local_dns_to_hosts_adds_hosts_before_removing_resolver_file() {
        let live = "orig\n";
        let staged = format!("orig\n{HOST_LINE}");

        // Success.
        let sw = Switch::new(live, &staged);
        std::fs::write(
            sw.zone_file(),
            render_resolver_file("prof-a", DEFAULT_DNS_PORT).unwrap(),
        )
        .unwrap();
        assert_eq!(sw.run(&remove_plan("prof-a"), live, true), Some(0));
        assert!(!sw.zone_file().exists());
        assert!(sw.has_mapping());

        // Hosts snapshot mismatch (exit 8) aborts before the removal: resolver file stays.
        let sw = Switch::new("edited\n", &staged);
        std::fs::write(
            sw.zone_file(),
            render_resolver_file("prof-a", DEFAULT_DNS_PORT).unwrap(),
        )
        .unwrap();
        assert_eq!(sw.run(&remove_plan("prof-a"), live, true), Some(8));
        assert!(sw.zone_file().exists() && sw.has_mapping());

        // Removal guard fails (exit 4) AFTER the hosts block was added: hosts maps the name.
        let sw = Switch::new(live, &staged);
        std::fs::write(
            sw.zone_file(),
            render_resolver_file("someone-else", DEFAULT_DNS_PORT).unwrap(),
        )
        .unwrap();
        assert_eq!(sw.run(&remove_plan("prof-a"), live, true), Some(4));
        assert!(sw.has_mapping());
        assert!(std::fs::read_to_string(&sw.hosts_dst)
            .unwrap()
            .contains("h.example.internal"));

        let script = build_privileged_script_with(
            "/etc/resolver",
            &remove_plan("prof-a"),
            DEFAULT_DNS_PORT,
            &ScriptOptions {
                expected_pid: None,
                hosts_tmp: Some(Path::new(HOSTS_STAGED_DUMMY)),
                hosts_sha256: None,
                hosts_additive: true,
                hosts_content_sha256: Some(file_sha(Path::new(HOSTS_STAGED_DUMMY))),
            },
        )
        .unwrap();
        assert!(script.find("/bin/cp -P '").unwrap() < script.find("/bin/rm -f '").unwrap());
    }

    #[test]
    fn mbb_hosts_to_local_dns_writes_resolver_file_before_removing_hosts_block() {
        let live = format!("orig\n{HOST_LINE}");
        let staged = "orig\n";

        // Success.
        let sw = Switch::new(&live, staged);
        assert_eq!(sw.run(&one_write_plan(), &live, false), Some(0));
        assert!(sw.zone_file().exists());
        assert!(sw.has_mapping());
        assert!(!std::fs::read_to_string(&sw.hosts_dst)
            .unwrap()
            .contains("h.example"));

        // Resolver write refused (unowned file at the target, exit 3): hosts untouched.
        let sw = Switch::new(&live, staged);
        std::fs::write(sw.zone_file(), "nameserver 192.0.2.53\n").unwrap();
        assert_eq!(sw.run(&one_write_plan(), &live, false), Some(3));
        assert_eq!(std::fs::read_to_string(&sw.hosts_dst).unwrap(), live);

        // Hosts snapshot mismatch (exit 8) after the write: resolver file is the mapping.
        let sw = Switch::new(&format!("{live}extra\n"), staged);
        assert_eq!(sw.run(&one_write_plan(), &live, false), Some(8));
        assert!(sw.zone_file().exists() && sw.has_mapping());

        let script = build_privileged_script_with(
            "/etc/resolver",
            &one_write_plan(),
            DEFAULT_DNS_PORT,
            &ScriptOptions {
                expected_pid: None,
                hosts_tmp: Some(Path::new(HOSTS_STAGED_DUMMY)),
                hosts_sha256: None,
                hosts_additive: false,
                hosts_content_sha256: Some(file_sha(Path::new(HOSTS_STAGED_DUMMY))),
            },
        )
        .unwrap();
        assert!(script.find("/bin/mv -f").unwrap() < script.find("/bin/cp -P '").unwrap());
    }

    fn hosts_copy_opts<'a>(src: &'a Path, snapshot: &'a str, content: &str) -> ScriptOptions<'a> {
        ScriptOptions {
            expected_pid: None,
            hosts_tmp: Some(src),
            hosts_sha256: Some(snapshot),
            hosts_additive: false,
            hosts_content_sha256: Some(sha256_hex(content)),
        }
    }

    fn hosts_leftovers(dir: &Path) -> usize {
        std::fs::read_dir(dir)
            .unwrap()
            .filter_map(Result::ok)
            .filter(|e| {
                e.file_name()
                    .to_string_lossy()
                    .starts_with(".clusterdeck-hosts.")
            })
            .count()
    }

    #[test]
    fn hosts_copy_happy_path_replaces_via_private_temp_without_leftovers() {
        use std::os::unix::fs::PermissionsExt;
        let dir = TestDir::new();
        let src = dir.path().join("hosts.staged");
        let dst = dir.path().join("hosts.live");
        std::fs::write(&src, "192.0.2.1 a.example.internal\n").unwrap();
        std::fs::write(&dst, "original\n").unwrap();
        let snap = sha256_hex("original\n");
        let opts = hosts_copy_opts(&src, &snap, "192.0.2.1 a.example.internal\n");

        let code = run_ext(&dir, &ResolverPlan::default(), &opts, "1", &dst);
        assert_eq!(code, Some(0));
        assert_eq!(
            std::fs::read_to_string(&dst).unwrap(),
            "192.0.2.1 a.example.internal\n"
        );
        assert_eq!(
            std::fs::metadata(&dst).unwrap().permissions().mode() & 0o777,
            0o644
        );
        assert_eq!(hosts_leftovers(dir.path()), 0);
    }

    #[test]
    fn hosts_copy_rejects_a_staged_symlink_with_exit_9_and_leaves_target_untouched() {
        let dir = TestDir::new();
        let real = dir.path().join("real-content");
        let src = dir.path().join("hosts.staged");
        let dst = dir.path().join("hosts.live");
        std::fs::write(&real, "192.0.2.1 a.example.internal\n").unwrap();
        // Hash matches the symlink's TARGET: only the symlink check can stop it.
        std::os::unix::fs::symlink(&real, &src).unwrap();
        std::fs::write(&dst, "original\n").unwrap();
        let snap = sha256_hex("original\n");
        let opts = hosts_copy_opts(&src, &snap, "192.0.2.1 a.example.internal\n");

        let code = run_ext(&dir, &ResolverPlan::default(), &opts, "1", &dst);
        assert_eq!(code, Some(9));
        assert_eq!(std::fs::read_to_string(&dst).unwrap(), "original\n");
        assert_eq!(hosts_leftovers(dir.path()), 0);
    }

    #[test]
    fn hosts_copy_rejects_content_that_does_not_match_the_hash() {
        let dir = TestDir::new();
        let src = dir.path().join("hosts.staged");
        let dst = dir.path().join("hosts.live");
        // The staged file was swapped after the hash of the intended content was computed.
        std::fs::write(&src, "6.6.6.6 evil.example.internal\n").unwrap();
        std::fs::write(&dst, "original\n").unwrap();
        let snap = sha256_hex("original\n");
        let opts = hosts_copy_opts(&src, &snap, "192.0.2.1 a.example.internal\n");

        let code = run_ext(&dir, &ResolverPlan::default(), &opts, "1", &dst);
        assert_eq!(code, Some(9));
        assert_eq!(std::fs::read_to_string(&dst).unwrap(), "original\n");
        assert_eq!(hosts_leftovers(dir.path()), 0);
    }

    #[test]
    fn hosts_tmp_without_a_content_hash_is_rejected() {
        let opts = ScriptOptions {
            hosts_tmp: Some(Path::new(HOSTS_STAGED_DUMMY)),
            ..ScriptOptions::default()
        };
        assert!(build_privileged_script_with(
            "/etc/resolver",
            &ResolverPlan::default(),
            DEFAULT_DNS_PORT,
            &opts
        )
        .is_err());
    }

    #[test]
    fn hosts_copy_survives_a_staging_dir_name_with_quote_and_command_substitution() {
        let dir = TestDir::new();
        let evil_dir = dir.path().join("it's$(touch pwned)'\"x");
        std::fs::create_dir_all(&evil_dir).unwrap();
        let src = evil_dir.join("hosts.staged");
        let dst = dir.path().join("hosts.live");
        std::fs::write(&src, "192.0.2.1 a.example.internal\n").unwrap();
        std::fs::write(&dst, "original\n").unwrap();
        let snap = sha256_hex("original\n");
        let opts = hosts_copy_opts(&src, &snap, "192.0.2.1 a.example.internal\n");
        let _ = std::fs::remove_file("pwned");

        let code = run_ext(&dir, &ResolverPlan::default(), &opts, "1", &dst);
        assert_eq!(code, Some(0), "quoted path must copy safely");
        assert_eq!(
            std::fs::read_to_string(&dst).unwrap(),
            "192.0.2.1 a.example.internal\n"
        );
        assert!(!Path::new("pwned").exists(), "command substitution ran");
        assert!(!evil_dir.join("pwned").exists());
        assert!(!dir.path().join("pwned").exists());
    }
}
