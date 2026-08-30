# /etc/hosts Managed-Block Feature Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking. **Dispatch note:** this project's workers are `agy` lanes, not native Claude subagents.

**Goal:** Let a profile optionally map its hosts/bastion to stable, unique hostnames in `/etc/hosts` (`<name>.<profile_id>.clusterdeck.local`), owned exclusively by a ClusterDeck marker block per profile, written via a single macOS admin-privilege prompt, wired into `connect_profile` behind an opt-in per-profile flag, and cleaned up on profile delete.

**Architecture:** A new pure-computation core (`compute_updated_hosts_content`) takes the existing `/etc/hosts` text and produces new text with exactly one profile's marker block upserted or removed — fully unit-testable with no privilege and no real file I/O. A thin privileged-write wrapper (`write_hosts_file`) does the one `osascript ... with administrator privileges` call, kept separate so it can stay untested (it cannot be tested without a real root prompt) while the logic that decides *what* to write is fully covered.

**Tech Stack:** Same Rust/Tauri stack as the rest of the backend. No new external dependency — `osascript` is already reachable via the existing `SEARCH_PATHS`/`CommandRunner` machinery in `services/process.rs`.

**Spec:** User decisions confirmed 2026-08-24: hostname scheme is `<host-or-bastion-name>.<profile_id>.clusterdeck.local` (fully namespaced, no cross-profile collision); the feature triggers as part of `connect_profile`, gated by a new opt-in `Profile.manage_hosts_file: bool` field (default `false`).

## Global Constraints

- Never touch any `/etc/hosts` line outside this profile's own marker block. Other profiles' blocks and the user's pre-existing entries must survive byte-for-byte.
- The privileged write must be a single `osascript ... with administrator privileges` invocation per `connect_profile` call (or per explicit management action) — never prompt more than once per user action.
- Marker block format, exact and machine-parseable:
  ```
  # >>> ClusterDeck BEGIN (profile: <profile_id>) >>>
  <address> <name>.<profile_id>.clusterdeck.local
  ...
  # <<< ClusterDeck END (profile: <profile_id>) <<<
  ```
- Reuse `crate::services::validate::is_safe_profile_id` / `is_safe_ssh_identifier` — do not write a second validator. A profile already fails to persist (per the existing `store::upsert_profile` gate) if any field is unsafe, so by the time `hosts_file.rs` runs, `profile.id`/host names/addresses are already known-safe; still call the validators again in `render_hosts_block` as defense-in-depth (matches the `debug_assert!` pattern already used in `services/paths.rs`), since `/etc/hosts` corruption is more severe than a bad SSH config entry.
- Failure to write `/etc/hosts` (e.g. user cancels the admin password prompt) must be non-fatal to `connect_profile`, exactly like the existing alias-write and kubeconfig-fetch failure handling — push a message to `ConnectionResult.errors` and continue.
- `cargo fmt --all -- --check`, `cargo clippy --all-targets --all-features -- -D warnings`, `cargo test --all-targets --all-features` (i.e. `make verify`'s Rust steps) must pass. Do not use a bare `cargo clippy -- -D warnings` without `--all-targets` — this repo's Makefile lint target is the authoritative one and has caught issues a narrower invocation missed twice already.

---

## File Structure

```text
src-tauri/src/
  services/
    hosts_file.rs          # NEW: compute_updated_hosts_content, render_hosts_block, write_hosts_file, upsert_hosts_block, remove_hosts_block
    config.rs               # MODIFY: add Profile.manage_hosts_file: bool (default false)
    mod.rs                  # MODIFY: + pub mod hosts_file;
  commands/
    connection.rs            # MODIFY: connect_profile calls hosts_file::upsert_hosts_block when profile.manage_hosts_file
    profiles.rs               # MODIFY: delete_profile_cmd also calls hosts_file::remove_hosts_block
src/
  api/tauri.ts                # MODIFY: add manage_hosts_file to the Profile type
  App.tsx                      # not touched by this plan -- no UI toggle yet, field defaults false and is only settable by hand-editing profiles.yaml for this first slice (same deferral pattern as Profile-creation UI)
```

## Global Interfaces

```rust
// services/hosts_file.rs
pub const HOSTS_FILE_PATH: &str = "/etc/hosts";

pub fn render_hosts_block(profile: &crate::services::config::Profile) -> Result<String, String>;
// One line per host (name.profile_id.clusterdeck.local) plus one for bastion if present
// (bastion.name.profile_id.clusterdeck.local), wrapped in the exact BEGIN/END marker
// lines from Global Constraints. Returns Err if profile.id or any host/bastion
// name/address fails is_safe_profile_id / is_safe_ssh_identifier (defense-in-depth
// re-check; the store layer should already have rejected these before persistence).

pub fn compute_updated_hosts_content(existing: &str, profile_id: &str, block: Option<&str>) -> String;
// Pure function, no I/O. Finds this profile's existing BEGIN/END block (matched by the
// "(profile: <profile_id>)" marker text) anywhere in `existing` and removes it if present.
// If `block` is Some(rendered_block), appends it (with a leading blank line separator if
// `existing` doesn't already end in one) to the end of the resulting content. If `block`
// is None, the profile's block is simply absent from the result (removal). Never touches
// any other profile's block or any non-ClusterDeck line.

pub async fn write_hosts_file(runner: &dyn crate::services::process::CommandRunner, new_content: &str) -> Result<(), String>;
// Writes `new_content` to a temp file (std::env::temp_dir()), then runs:
//   osascript -e "do shell script \"cp '<tmp_path>' /etc/hosts\" with administrator privileges"
// via runner.run("osascript", &[...]) -- a SINGLE privileged call. Deletes the temp file
// afterward regardless of success/failure (same cleanup-on-every-path discipline as
// kubeconfig.rs::fetch_and_store's tmp_path handling). Maps a non-success CommandOutput
// (e.g. user cancelled the password prompt) to Err with the stderr message, never panics.

pub async fn upsert_hosts_block(runner: &dyn crate::services::process::CommandRunner, profile: &crate::services::config::Profile) -> Result<(), String>;
// existing = std::fs::read_to_string(HOSTS_FILE_PATH) (world-readable, no privilege needed)
// block = render_hosts_block(profile)?
// new_content = compute_updated_hosts_content(&existing, &profile.id, Some(&block))
// write_hosts_file(runner, &new_content).await

pub async fn remove_hosts_block(runner: &dyn crate::services::process::CommandRunner, profile_id: &str) -> Result<(), String>;
// existing = std::fs::read_to_string(HOSTS_FILE_PATH)
// new_content = compute_updated_hosts_content(&existing, profile_id, None)
// write_hosts_file(runner, &new_content).await
// If profile_id's block wasn't present in `existing` to begin with, this is a harmless no-op
// (compute_updated_hosts_content still runs, new_content == existing content-wise for that
// profile, and write_hosts_file still executes -- acceptable for MVP simplicity; do NOT
// special-case "block not found -> skip write", since detecting that reliably from the pure
// function's return alone requires it to report whether it found the block, which is an
// unnecessary interface change for a rare, harmless case).
```

Every later task's Rust code imports these from `crate::services::hosts_file`.

---

### Task 1: `Profile.manage_hosts_file` field + `hosts_file.rs` pure content-computation core

**Files:**
- Modify: `src-tauri/src/services/config.rs`
- Modify: `src-tauri/src/services/store.rs` (the `ProfileBody` (de)serialization wrapper needs the new field, `#[serde(default)]`, plus the `Profile <-> ProfileBody` conversion in `load_profiles`/`upsert_profile`)
- Create: `src-tauri/src/services/hosts_file.rs`
- Modify: `src-tauri/src/services/mod.rs`
- Test: `#[cfg(test)]` in `hosts_file.rs`, plus one added test in `store.rs`'s existing test module

**Interfaces:**
- Produces: `Profile.manage_hosts_file: bool`, `render_hosts_block`, `compute_updated_hosts_content` (see Global Interfaces — `write_hosts_file`/`upsert_hosts_block`/`remove_hosts_block` are Task 2).

- [ ] **Step 1: Add the field to `Profile` in `config.rs`**

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Profile {
    pub id: String,
    pub name: String,
    pub hosts: Vec<Host>,
    pub bastion: Option<Bastion>,
    pub bootstrap: BootstrapPolicy,
    pub kubeconfig: Option<KubeconfigSource>,
    #[serde(default)]
    pub manage_hosts_file: bool,
}
```

- [ ] **Step 2: Thread the field through `store.rs`'s `ProfileBody`**

Read the current `store.rs` first — it has a private `ProfileBody` struct mirroring `Profile` minus `id` (id is the YAML map key). Add `#[serde(default)] manage_hosts_file: bool` to `ProfileBody`, and copy it in both directions in whatever `From`/manual-mapping code already converts `Profile <-> (String, ProfileBody)` in `load_profiles`/`upsert_profile`/`save_profiles`. Every existing `store.rs` test that constructs a `Profile` literal (e.g. `upsert_then_load_roundtrips`) needs `manage_hosts_file: false` added to its struct literal to keep compiling — do this for every such test, not just new ones.

- [ ] **Step 3: Write failing tests for `hosts_file.rs`**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::config::{Bastion, BootstrapPolicy, Host, Profile};

    fn profile() -> Profile {
        Profile {
            id: "cka-lab".into(),
            name: "CKA Lab".into(),
            hosts: vec![Host { name: "cka-m1".into(), address: "192.0.2.10".into(), port: 22, user: "root".into(), identity_file: None }],
            bastion: Some(Bastion { name: "bastion01".into(), address: "198.51.100.1".into(), port: 22, user: "ubuntu".into(), identity_file: None }),
            bootstrap: BootstrapPolicy::default(),
            kubeconfig: None,
            manage_hosts_file: true,
        }
    }

    #[test]
    fn render_hosts_block_includes_hosts_and_bastion_with_namespaced_names() {
        let block = render_hosts_block(&profile()).unwrap();
        assert!(block.contains("# >>> ClusterDeck BEGIN (profile: cka-lab) >>>"));
        assert!(block.contains("192.0.2.10 cka-m1.cka-lab.clusterdeck.local"));
        assert!(block.contains("198.51.100.1 bastion01.cka-lab.clusterdeck.local"));
        assert!(block.contains("# <<< ClusterDeck END (profile: cka-lab) <<<"));
    }

    #[test]
    fn render_hosts_block_rejects_unsafe_profile_id() {
        let mut p = profile();
        p.id = "../evil".into();
        assert!(render_hosts_block(&p).is_err());
    }

    #[test]
    fn compute_updated_hosts_content_appends_block_to_empty_file() {
        let result = compute_updated_hosts_content("", "cka-lab", Some("# >>> ClusterDeck BEGIN (profile: cka-lab) >>>\n192.0.2.10 cka-m1.cka-lab.clusterdeck.local\n# <<< ClusterDeck END (profile: cka-lab) <<<\n"));
        assert!(result.contains("cka-m1.cka-lab.clusterdeck.local"));
    }

    #[test]
    fn compute_updated_hosts_content_preserves_unrelated_lines() {
        let existing = "127.0.0.1 localhost\n255.255.255.255 broadcasthost\n";
        let result = compute_updated_hosts_content(existing, "cka-lab", Some("# >>> ClusterDeck BEGIN (profile: cka-lab) >>>\n192.0.2.10 cka-m1.cka-lab.clusterdeck.local\n# <<< ClusterDeck END (profile: cka-lab) <<<\n"));
        assert!(result.contains("127.0.0.1 localhost"));
        assert!(result.contains("255.255.255.255 broadcasthost"));
        assert!(result.contains("cka-m1.cka-lab.clusterdeck.local"));
    }

    #[test]
    fn compute_updated_hosts_content_replaces_only_matching_profile_block_leaving_others() {
        let existing = "127.0.0.1 localhost\n\n# >>> ClusterDeck BEGIN (profile: cka-lab) >>>\n192.0.2.99 stale.cka-lab.clusterdeck.local\n# <<< ClusterDeck END (profile: cka-lab) <<<\n\n# >>> ClusterDeck BEGIN (profile: dev-cluster) >>>\n198.51.100.20 dev-m1.dev-cluster.clusterdeck.local\n# <<< ClusterDeck END (profile: dev-cluster) <<<\n";
        let new_block = "# >>> ClusterDeck BEGIN (profile: cka-lab) >>>\n192.0.2.10 cka-m1.cka-lab.clusterdeck.local\n# <<< ClusterDeck END (profile: cka-lab) <<<\n";
        let result = compute_updated_hosts_content(existing, "cka-lab", Some(new_block));
        assert!(!result.contains("stale.cka-lab.clusterdeck.local"), "old cka-lab entry must be gone");
        assert!(result.contains("cka-m1.cka-lab.clusterdeck.local"), "new cka-lab entry must be present");
        assert!(result.contains("dev-m1.dev-cluster.clusterdeck.local"), "unrelated profile's block must survive untouched");
        assert!(result.contains("127.0.0.1 localhost"), "non-ClusterDeck line must survive untouched");
    }

    #[test]
    fn compute_updated_hosts_content_removes_block_when_none_given() {
        let existing = "127.0.0.1 localhost\n# >>> ClusterDeck BEGIN (profile: cka-lab) >>>\n192.0.2.10 cka-m1.cka-lab.clusterdeck.local\n# <<< ClusterDeck END (profile: cka-lab) <<<\n";
        let result = compute_updated_hosts_content(existing, "cka-lab", None);
        assert!(!result.contains("cka-lab.clusterdeck.local"));
        assert!(result.contains("127.0.0.1 localhost"));
    }
}
```

- [ ] **Step 4: Run to verify failure.** Run: `cargo test --manifest-path src-tauri/Cargo.toml services::hosts_file` — Expected: FAIL.

- [ ] **Step 5: Implement `render_hosts_block` and `compute_updated_hosts_content`** per the Global Interfaces spec and to satisfy the tests above. Implementation notes:
  - `render_hosts_block`: validate `profile.id` via `is_safe_profile_id`, and every `host.name`/`host.address` and (if present) `bastion.name`/`bastion.address` via `is_safe_ssh_identifier`, returning `Err` on the first failure. Build the block as a `String` with the exact BEGIN/END marker lines (including the literal `(profile: <id>)` text the removal logic matches on), one `<address> <name>.<profile_id>.clusterdeck.local` line per host, then one more for the bastion if `profile.bastion.is_some()`.
  - `compute_updated_hosts_content`: search `existing` for a line exactly equal to `format!("# >>> ClusterDeck BEGIN (profile: {profile_id}) >>>")` and a later line exactly equal to `format!("# <<< ClusterDeck END (profile: {profile_id}) <<<")`; if both found, remove that whole line range (inclusive) from the content. Then, if `block` is `Some(b)`, append `b` (ensuring exactly one blank-line separator before it if the content is non-empty and doesn't already end in a blank line). Return the reassembled content. Implement with plain line-based `Vec<&str>`/`String` manipulation — no regex dependency needed.

- [ ] **Step 6: Add `pub mod hosts_file;` to `services/mod.rs`.**

- [ ] **Step 7: Run to verify pass.** Run: `cargo test --manifest-path src-tauri/Cargo.toml services::hosts_file services::store` — Expected: PASS (including the `manage_hosts_file: false` literal additions to existing `store.rs` tests compiling).

- [ ] **Step 8: `cargo check` full workspace, format, lint.**

```bash
cargo check --manifest-path src-tauri/Cargo.toml
cd src-tauri && cargo fmt --all && cd ..
cd src-tauri && cargo clippy --all-targets --all-features -- -D warnings && cd ..
```

- [ ] **Step 9: Commit**

```bash
git add src-tauri/src/services/config.rs src-tauri/src/services/store.rs src-tauri/src/services/hosts_file.rs src-tauri/src/services/mod.rs
git commit -m "feat(backend): add Profile.manage_hosts_file flag and pure /etc/hosts block computation"
```

---

### Task 2: privileged write + upsert/remove wrappers, wired into `connect_profile` and profile delete

**Files:**
- Modify: `src-tauri/src/services/hosts_file.rs` (add `write_hosts_file`, `upsert_hosts_block`, `remove_hosts_block`)
- Modify: `src-tauri/src/commands/connection.rs` (`connect_profile` calls `upsert_hosts_block` when `profile.manage_hosts_file`)
- Modify: `src-tauri/src/commands/profiles.rs` (`delete_profile_cmd` calls `remove_hosts_block` before/after `store::delete_profile`)
- Modify: `src/api/tauri.ts` (add `manage_hosts_file: boolean` to the `Profile` type)
- Test: one new test in `hosts_file.rs` for `write_hosts_file`'s error path using a `FakeRunner` (the success path cannot be tested without real root — do not attempt to)

**Interfaces:**
- Consumes: `render_hosts_block`, `compute_updated_hosts_content` (Task 1); `CommandRunner`, `CommandOutput` (existing, `services/process.rs`); `Profile` (Task 1, now with `manage_hosts_file`).
- Produces: `write_hosts_file`, `upsert_hosts_block`, `remove_hosts_block` (see Global Interfaces) — consumed by `connection.rs` and `profiles.rs`.

- [ ] **Step 1: Write a failing test for `write_hosts_file`'s failure path**

```rust
#[cfg(test)]
mod write_tests {
    use super::*;
    use crate::services::process::CommandOutput;
    use async_trait::async_trait;

    struct DenyingRunner;

    #[async_trait]
    impl crate::services::process::CommandRunner for DenyingRunner {
        async fn run(&self, _bin: &str, _args: &[String]) -> Result<CommandOutput, String> {
            Ok(CommandOutput { stdout: String::new(), stderr: "User canceled.".into(), success: false })
        }
    }

    #[tokio::test]
    async fn write_hosts_file_reports_error_when_admin_prompt_is_cancelled() {
        let runner = DenyingRunner;
        let result = write_hosts_file(&runner, "127.0.0.1 localhost\n").await;
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("User canceled"));
    }
}
```

- [ ] **Step 2: Run to verify failure.** Run: `cargo test --manifest-path src-tauri/Cargo.toml services::hosts_file::write_tests` — Expected: FAIL.

- [ ] **Step 3: Implement `write_hosts_file`** per the Global Interfaces spec: write `new_content` to `std::env::temp_dir().join(format!("clusterdeck-hosts-{}.tmp", std::process::id()))`, build the osascript command string with the temp path and `HOSTS_FILE_PATH` interpolated (both are either constant or already-validated by the time this is called — `HOSTS_FILE_PATH` is a compile-time constant, and the temp path is one we generated ourselves, so no injection risk there), call `runner.run("osascript", &[...]).await`, always `let _ = std::fs::remove_file(&tmp_path);` before returning on every branch (success or error), map `!output.success` to `Err(output.stderr)`.

- [ ] **Step 4: Implement `upsert_hosts_block` and `remove_hosts_block`** per the Global Interfaces spec (thin wrappers composing `std::fs::read_to_string(HOSTS_FILE_PATH)` + `render_hosts_block`/`None` + `compute_updated_hosts_content` + `write_hosts_file`).

- [ ] **Step 5: Run to verify pass.** Run: `cargo test --manifest-path src-tauri/Cargo.toml services::hosts_file` — Expected: PASS.

- [ ] **Step 6: Wire into `connect_profile` in `commands/connection.rs`**

Read the current `connect_profile` body first (it already has the `errors: Vec<String>` pattern from a prior fix, and now runs host probing concurrently via `futures::future::join_all` from a prior fix too — match the current real code, not an older version). After the existing alias-write block (the one that sets `aliases_written` and pushes to `errors` on failure) and before or after the kubeconfig-fetch block (either position is fine, they're independent), add:

```rust
if profile.manage_hosts_file {
    if let Err(e) = crate::services::hosts_file::upsert_hosts_block(&runner, &profile).await {
        errors.push(format!("hosts file update failed: {e}"));
    }
}
```

- [ ] **Step 7: Wire into `delete_profile_cmd` in `commands/profiles.rs`**

```rust
#[tauri::command]
pub async fn delete_profile_cmd(profile_id: String) -> Result<(), String> {
    let paths = ClusterDeckPaths::resolve()?;
    let runner = crate::services::process::SystemRunner;
    let _ = crate::services::hosts_file::remove_hosts_block(&runner, &profile_id).await;
    store::delete_profile(&paths, &profile_id)
}
```

(The hosts-file removal failure is intentionally swallowed with `let _ =` here, not propagated as a command error — deleting the Profile record itself must still succeed even if the privileged `/etc/hosts` cleanup fails or the user cancels the prompt; this mirrors `connect_profile`'s existing "non-fatal side effect" pattern.) Note: `delete_profile_cmd` was a sync `pub fn` before — confirm its current signature in the real file (it may already be effectively fine to make `async fn` since Tauri commands support both; if it's currently `pub fn` non-async, change it to `pub async fn` to allow the `.await` here, and update its registration in `lib.rs`'s `generate_handler!` list is NOT needed since `tauri::generate_handler!` handles async commands transparently — no signature change needed there).

- [ ] **Step 8: Add `manage_hosts_file: boolean` to the `Profile` type in `src/api/tauri.ts`** (the only frontend change in this plan — no UI toggle yet, per Global Constraints/File Structure).

- [ ] **Step 9: Full verification**

```bash
cargo test --manifest-path src-tauri/Cargo.toml
cargo check --manifest-path src-tauri/Cargo.toml
cd src-tauri && cargo fmt --all && cd ..
cd src-tauri && cargo clippy --all-targets --all-features -- -D warnings && cd ..
pnpm exec tsc --noEmit -p tsconfig.json
pnpm build
```

- [ ] **Step 10: Commit**

```bash
git add src-tauri/src/services/hosts_file.rs src-tauri/src/commands/connection.rs src-tauri/src/commands/profiles.rs src/api/tauri.ts
git commit -m "feat(backend): write/remove ClusterDeck-owned /etc/hosts block via connect_profile and profile delete"
```

---

### Task 3: manual real-world verification (not committed) + docs update

**Files:**
- No source changes beyond what Tasks 1–2 already made.
- Modify: `docs/ARCHITECTURE.md` — add one short subsection documenting the `/etc/hosts` managed-block behavior (naming scheme, opt-in flag, marker format) near the existing SSH-config-ownership section (§7 "SSH Configuration Ownership"), following that section's style.

**Interfaces:** none new.

- [ ] **Step 1: Real end-to-end manual check.** Using the same temporary-`#[ignore]`-test-then-revert technique used earlier in this project's history for real-infrastructure checks (append a throwaway `#[tokio::test] #[ignore]` to `connection.rs` or `hosts_file.rs` that sets `manage_hosts_file: true` on a real local test profile — e.g. the existing `colima-local` profile in the developer's real `~/.clusterdeck/profiles.yaml` if present — and calls `connect_profile`, or more narrowly just `hosts_file::upsert_hosts_block` directly against a `Profile` literal), run it with `cargo test -- --ignored --nocapture`, confirm the macOS admin password prompt appears and, after entering it, that `/etc/hosts` actually gained the expected `# >>> ClusterDeck BEGIN ...` block (`cat /etc/hosts`), then confirm `remove_hosts_block` cleans it back up. Remove the temporary test afterward and revert `/etc/hosts` to its pre-test state if the test run leaves a stray entry (`sudo` edit or re-run the removal path). Report the actual prompt behavior and file diff observed — this is the one part of the feature that categorically cannot be verified by any unit test, so this manual pass is the real completion gate for Task 2, not optional polish.
- [ ] **Step 2: Update `docs/ARCHITECTURE.md`** with the short subsection described above.
- [ ] **Step 3: Commit**

```bash
git add docs/ARCHITECTURE.md
git commit -m "docs: document /etc/hosts managed-block feature"
```

---

## Self-Review Notes

- **Spec coverage:** namespaced hostname scheme (`<name>.<profile_id>.clusterdeck.local`) — Task 1's `render_hosts_block`. Trigger inside `connect_profile`, opt-in flag — Task 2 Step 6, Task 1 Step 1. Cleanup on delete — Task 2 Step 7. Single admin prompt per action — Task 2 Step 3 (`write_hosts_file` is the only caller of the privileged command, called at most once per `upsert_hosts_block`/`remove_hosts_block` invocation, which are each called at most once per `connect_profile`/`delete_profile_cmd` call).
- **Deferred by design:** no UI toggle for `manage_hosts_file` (hand-edit `profiles.yaml`, same deferral as Profile-creation UI elsewhere in this project); no multi-hop /etc/hosts entries beyond host+bastion; no Windows/Linux hosts-file path (macOS `/etc/hosts` only, matching this whole project's macOS-first scope).
- **Type consistency check performed:** `Profile.manage_hosts_file` (Task 1) is the exact field name used in Task 2's `connect_profile`/`delete_profile_cmd` wiring and in `tauri.ts`'s type. `render_hosts_block`/`compute_updated_hosts_content` (Task 1) are the exact names `upsert_hosts_block`/`remove_hosts_block` (Task 2) call. `HOSTS_FILE_PATH` constant (Task 2) is the single source of truth for the path, used by both `upsert_hosts_block` and `remove_hosts_block` — no hardcoded `"/etc/hosts"` string duplicated elsewhere.
