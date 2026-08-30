# ClusterDeck MVP Connect Pipeline Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking. **Dispatch note:** this project's workers are `agy` lanes (`agyp "<task prompt>" --model "..."`), not native Claude subagents — see the orchestrator's `agy_cli` playbook. Each task below is sized to be one `agyp` dispatch.

**Goal:** Implement GitHub Issues #2–#5 — multi-VM IP discovery + SSH bootstrap, remote kubeconfig fetch + local integration, Bastion/ProxyJump support, and Kubernetes connectivity verification + Profile status — turning the current UI-mock/stub-backend scaffold into a working local connect pipeline.

**Architecture:** Rust owns all privileged operations (filesystem, SSH/SCP/kubectl process execution, kubeconfig parsing) behind a `CommandRunner` trait so services are unit-testable without touching real infrastructure. Tauri commands expose granular pipeline steps (discover → probe → bootstrap → alias → kubeconfig fetch → verify) plus one orchestrating `connect_profile` command. React calls these via a typed `src/api/tauri.ts` client and replaces today's hardcoded mock state.

**Tech Stack:** Tauri 2, Rust (tokio, serde, serde_yaml, async-trait, thiserror, chrono), React 19 + TypeScript, existing `ssh`/`scp`/`ssh-copy-id`/`sshpass`/`kubectl` system binaries.

**Spec:**
- `docs/ARCHITECTURE.md`
- `docs/03-mvp-design.md`
- `AGENTS.md`
- GitHub Issues #1 (baseline), #2, #3, #4, #5 (`gh issue view <n> --repo dasomel/clusterdeck`)

## Global Constraints

- macOS-first only; do not add cross-platform abstractions (per ADR-0001).
- Frontend (`src/`) is presentation only — never execute SSH/SCP/kubectl or touch the filesystem from TypeScript. All privileged work lives in `src-tauri/src/`.
- Never store passwords in any file; a bootstrap password lives only in memory for the duration of a single Tauri command call.
- Never print secrets (passwords, private key contents, kubeconfig payloads) in logs, error strings, or test fixtures. Errors must state *what* failed, not raw command output that could contain a payload.
- ClusterDeck must never overwrite the user's entire `~/.ssh/config` or `~/.kube/config` — only ever append/update its own `Include ~/.clusterdeck/ssh/*.conf` line and write exclusively inside `~/.clusterdeck/`.
- Local state layout is fixed: `~/.clusterdeck/profiles.yaml`, `~/.clusterdeck/ssh/<profile_id>.conf`, `~/.clusterdeck/kubeconfigs/<profile_id>.yaml`, `~/.clusterdeck/state.json`.
- All filesystem-path-producing services take a `ClusterDeckPaths` base directory as a parameter (never hardcode `$HOME` inline) so tests run against a temp dir, never the real home directory.
- All process-execution services take `&dyn CommandRunner` as a parameter (never call `tokio::process::Command` directly from service logic) so tests run against a `FakeRunner`, never real `ssh`/`scp`/`kubectl`.
- Prefer mature system tools (`ssh`, `scp`, `ssh-copy-id`, `sshpass`, `kubectl`); do not implement SSH protocol logic in Rust.
- `cargo fmt --check`, `cargo clippy -D warnings`, `cargo test`, and `pnpm build` (`tsc && vite build`) must all pass before any task is considered done — this mirrors `docs/CI.md`.
- Conventional Commits for every commit.

---

## File Structure

```text
src-tauri/
  Cargo.toml                        # + async-trait, serde_yaml, chrono
  src/
    lib.rs                          # register all new commands
    commands/
      mod.rs                        # + discovery, connection already present
      app.rs                        # unchanged
      profiles.rs                   # real CRUD backed by ProfileStore
      discovery.rs                  # NEW: discover_hosts command
      connection.rs                 # rewritten: granular + orchestrating commands
    services/
      mod.rs                        # + new modules
      paths.rs                      # NEW: ClusterDeckPaths
      process.rs                    # + CommandRunner trait, SystemRunner, FakeRunner (test-only)
      config.rs                     # extended Profile/Host/Bastion/Bootstrap domain types
      store.rs                      # NEW: profiles.yaml load/save
      discovery.rs                  # NEW: CIDR/IP expansion + TCP reachability probe
      ssh.rs                        # NEW: probe / bootstrap / retry
      ssh_config.rs                 # NEW: alias + ProxyJump config rendering/writing
      kubeconfig.rs                 # NEW: fetch + normalize + write local kubeconfig
      verify.rs                     # NEW: kubectl-based verification
      state.rs                      # NEW: state.json (profile verification/status) load/save
src/
  api/
    tauri.ts                        # NEW: typed invoke() wrappers + shared types
  App.tsx                           # rewritten to use real data instead of mock profiles
```

## Global Interfaces (defined in Task 1, consumed by every later task)

```rust
// services/process.rs
#[derive(Debug, Clone)]
pub struct CommandOutput {
    pub stdout: String,
    pub stderr: String,
    pub success: bool,
}

#[async_trait::async_trait]
pub trait CommandRunner: Send + Sync {
    async fn run(&self, bin: &str, args: &[String]) -> Result<CommandOutput, String>;
}

pub struct SystemRunner;
// impl CommandRunner for SystemRunner using resolve_cli_path + tokio::process::Command,
// always returning CommandOutput (never a bare Err on non-zero exit) so callers can inspect
// success/stderr instead of pattern-matching on Result.
```

```rust
// services/paths.rs
pub struct ClusterDeckPaths { pub base: std::path::PathBuf }
impl ClusterDeckPaths {
    pub fn resolve() -> Result<Self, String>;       // base = $HOME/.clusterdeck
    pub fn at(base: std::path::PathBuf) -> Self;     // test constructor
    pub fn profiles_file(&self) -> std::path::PathBuf;
    pub fn ssh_dir(&self) -> std::path::PathBuf;
    pub fn ssh_conf(&self, profile_id: &str) -> std::path::PathBuf;
    pub fn kubeconfigs_dir(&self) -> std::path::PathBuf;
    pub fn kubeconfig_file(&self, profile_id: &str) -> std::path::PathBuf;
    pub fn state_file(&self) -> std::path::PathBuf;
    pub fn ensure_dirs(&self) -> Result<(), String>; // mkdir -p base, ssh_dir, kubeconfigs_dir
}
```

Every later task's Rust code imports `CommandRunner`/`CommandOutput`/`SystemRunner` from `crate::services::process` and `ClusterDeckPaths` from `crate::services::paths`.

---

### Task 1: Command runner abstraction, path resolver, and extended domain types

**Files:**
- Modify: `src-tauri/Cargo.toml`
- Modify: `src-tauri/src/services/process.rs`
- Create: `src-tauri/src/services/paths.rs`
- Modify: `src-tauri/src/services/config.rs`
- Modify: `src-tauri/src/services/mod.rs`
- Test: inline `#[cfg(test)]` modules in `process.rs`, `paths.rs`, `config.rs`

**Interfaces:**
- Produces: `CommandOutput`, `CommandRunner`, `SystemRunner` (see Global Interfaces), `ClusterDeckPaths` (see Global Interfaces), and the extended `Profile`/`Host`/`Bastion` structs below. Every later task depends on these exact names.

Extend `services/config.rs`:

```rust
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Profile {
    pub id: String,
    pub name: String,
    pub hosts: Vec<Host>,
    pub bastion: Option<Bastion>,
    pub bootstrap: BootstrapPolicy,
    pub kubeconfig: Option<KubeconfigSource>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Host {
    pub name: String,
    pub address: String,
    #[serde(default = "default_port")]
    pub port: u16,
    pub user: String,
    pub identity_file: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Bastion {
    pub name: String,
    pub address: String,
    #[serde(default = "default_port")]
    pub port: u16,
    pub user: String,
    pub identity_file: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BootstrapPolicy {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_retries")]
    pub retries: u32,
    #[serde(default = "default_retry_delay_secs")]
    pub retry_delay_secs: u64,
}

impl Default for BootstrapPolicy {
    fn default() -> Self {
        Self { enabled: false, retries: default_retries(), retry_delay_secs: default_retry_delay_secs() }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KubeconfigSource {
    pub remote_path: String,
    pub control_plane: String, // Host.name of the source host
    pub local_path: String,
    pub context: String,
}

fn default_port() -> u16 { 22 }
fn default_retries() -> u32 { 3 }
fn default_retry_delay_secs() -> u64 { 5 }
```

- [ ] **Step 1: Add dependencies**

Edit `src-tauri/Cargo.toml`, add to `[dependencies]`:

```toml
async-trait = "0.1"
serde_yaml = "0.9"
chrono = { version = "0.4", features = ["serde"] }
```

- [ ] **Step 2: Write failing tests for `CommandRunner`/`SystemRunner` and `ClusterDeckPaths`**

In `src-tauri/src/services/process.rs`, append:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn system_runner_reports_failure_without_erroring() {
        let runner = SystemRunner;
        let result = runner.run("true", &[]).await;
        // `true` exists on macOS default PATH search dirs
        assert!(result.is_ok());
    }

    #[test]
    fn resolve_cli_path_errors_on_unknown_binary() {
        let err = resolve_cli_path("definitely-not-a-real-binary-xyz").unwrap_err();
        assert!(err.contains("not found"));
    }
}
```

In `src-tauri/src/services/paths.rs` (new file), write the struct plus:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_are_scoped_under_base() {
        let paths = ClusterDeckPaths::at("/tmp/clusterdeck-test-fixture".into());
        assert_eq!(paths.profiles_file(), std::path::PathBuf::from("/tmp/clusterdeck-test-fixture/profiles.yaml"));
        assert_eq!(paths.ssh_conf("cka"), std::path::PathBuf::from("/tmp/clusterdeck-test-fixture/ssh/cka.conf"));
        assert_eq!(paths.kubeconfig_file("cka"), std::path::PathBuf::from("/tmp/clusterdeck-test-fixture/kubeconfigs/cka.yaml"));
    }

    #[test]
    fn ensure_dirs_creates_expected_tree() {
        let dir = std::env::temp_dir().join(format!("clusterdeck-test-{}", std::process::id()));
        let paths = ClusterDeckPaths::at(dir.clone());
        paths.ensure_dirs().unwrap();
        assert!(paths.ssh_dir().is_dir());
        assert!(paths.kubeconfigs_dir().is_dir());
        std::fs::remove_dir_all(&dir).ok();
    }
}
```

- [ ] **Step 3: Run tests to verify they fail**

Run: `cargo test --manifest-path src-tauri/Cargo.toml services::process services::paths`
Expected: FAIL (module/trait/struct not defined yet).

- [ ] **Step 4: Implement `CommandRunner`/`CommandOutput`/`SystemRunner` in `process.rs`**

Keep existing `resolve_cli_path` and `run_cli` (still used nowhere critical, but keep for compatibility — actually remove `run_cli` usages are none yet, so replace it with the trait-based approach and delete the old free function once nothing references it). Implement:

```rust
use async_trait::async_trait;
use tokio::process::Command;

#[derive(Debug, Clone)]
pub struct CommandOutput {
    pub stdout: String,
    pub stderr: String,
    pub success: bool,
}

#[async_trait]
pub trait CommandRunner: Send + Sync {
    async fn run(&self, bin: &str, args: &[String]) -> Result<CommandOutput, String>;
}

pub struct SystemRunner;

#[async_trait]
impl CommandRunner for SystemRunner {
    async fn run(&self, bin: &str, args: &[String]) -> Result<CommandOutput, String> {
        let path = resolve_cli_path(bin)?;
        let output = Command::new(path)
            .args(args)
            .output()
            .await
            .map_err(|err| format!("{bin} execution failed: {err}"))?;
        Ok(CommandOutput {
            stdout: String::from_utf8_lossy(&output.stdout).trim().to_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).trim().to_owned(),
            success: output.status.success(),
        })
    }
}
```

(Extend `SEARCH_PATHS` to include `sshpass`'s typical Homebrew locations — it already covers `/opt/homebrew/bin` and `/usr/local/bin`, so no change needed there.)

- [ ] **Step 5: Implement `ClusterDeckPaths` in `paths.rs`**

```rust
use std::path::PathBuf;

pub struct ClusterDeckPaths {
    pub base: PathBuf,
}

impl ClusterDeckPaths {
    pub fn resolve() -> Result<Self, String> {
        let home = std::env::var("HOME").map_err(|_| "HOME is not set".to_string())?;
        Ok(Self { base: PathBuf::from(home).join(".clusterdeck") })
    }

    pub fn at(base: PathBuf) -> Self {
        Self { base }
    }

    pub fn profiles_file(&self) -> PathBuf { self.base.join("profiles.yaml") }
    pub fn ssh_dir(&self) -> PathBuf { self.base.join("ssh") }
    pub fn ssh_conf(&self, profile_id: &str) -> PathBuf { self.ssh_dir().join(format!("{profile_id}.conf")) }
    pub fn kubeconfigs_dir(&self) -> PathBuf { self.base.join("kubeconfigs") }
    pub fn kubeconfig_file(&self, profile_id: &str) -> PathBuf { self.kubeconfigs_dir().join(format!("{profile_id}.yaml")) }
    pub fn state_file(&self) -> PathBuf { self.base.join("state.json") }

    pub fn ensure_dirs(&self) -> Result<(), String> {
        std::fs::create_dir_all(self.ssh_dir()).map_err(|e| e.to_string())?;
        std::fs::create_dir_all(self.kubeconfigs_dir()).map_err(|e| e.to_string())?;
        Ok(())
    }
}
```

- [ ] **Step 6: Update `services/mod.rs`**

```rust
pub mod process;
pub mod paths;
pub mod config;
```

(Task 2 will add `store`, Task 3 `discovery`, Task 4/5 `ssh`/`ssh_config`, Task 6 `kubeconfig`, Task 7 `verify`, Task 8 `state`.)

- [ ] **Step 7: Update `config.rs` domain types as specified above, run `cargo check`**

Run: `cargo check --manifest-path src-tauri/Cargo.toml`
Expected: compiles (nothing references the old two-field `Bastion`/`Host` shape incompatibly yet since `commands/*` only construct `Vec::new()`).

- [ ] **Step 8: Run full test suite for this task**

Run: `cargo test --manifest-path src-tauri/Cargo.toml services::process services::paths`
Expected: PASS.

- [ ] **Step 9: Format, lint, commit**

```bash
cargo fmt --manifest-path src-tauri/Cargo.toml
cargo clippy --manifest-path src-tauri/Cargo.toml -- -D warnings
git add src-tauri/Cargo.toml src-tauri/Cargo.lock src-tauri/src/services/process.rs src-tauri/src/services/paths.rs src-tauri/src/services/config.rs src-tauri/src/services/mod.rs
git commit -m "feat(backend): add CommandRunner abstraction, ClusterDeckPaths, and extended profile domain types"
```

---

### Task 2: Profile store (YAML load/save) and real Profile CRUD commands

**Files:**
- Create: `src-tauri/src/services/store.rs`
- Modify: `src-tauri/src/services/mod.rs`
- Modify: `src-tauri/src/commands/profiles.rs`
- Modify: `src-tauri/src/lib.rs`
- Test: `#[cfg(test)]` in `store.rs`

**Interfaces:**
- Consumes: `ClusterDeckPaths` (Task 1), `Profile`/`Host`/`Bastion`/`BootstrapPolicy`/`KubeconfigSource` (Task 1).
- Produces:
  ```rust
  // services/store.rs
  pub fn load_profiles(paths: &ClusterDeckPaths) -> Result<Vec<Profile>, String>;
  pub fn save_profiles(paths: &ClusterDeckPaths, profiles: &[Profile]) -> Result<(), String>;
  pub fn upsert_profile(paths: &ClusterDeckPaths, profile: Profile) -> Result<(), String>;
  pub fn delete_profile(paths: &ClusterDeckPaths, profile_id: &str) -> Result<(), String>;
  pub fn get_profile(paths: &ClusterDeckPaths, profile_id: &str) -> Result<Profile, String>;
  ```
- Produces Tauri commands consumed by the frontend in Task 9/10:
  ```rust
  // commands/profiles.rs
  #[tauri::command] pub fn list_profiles() -> Result<Vec<Profile>, String>;
  #[tauri::command] pub fn get_profile_cmd(profile_id: String) -> Result<Profile, String>;
  #[tauri::command] pub fn save_profile(profile: Profile) -> Result<(), String>;
  #[tauri::command] pub fn delete_profile_cmd(profile_id: String) -> Result<(), String>;
  ```

YAML file format (`profiles.yaml`) is a top-level map keyed by profile id, matching `docs/03-mvp-design.md` §7:

```yaml
profiles:
  cka-lab:
    name: CKA Lab
    hosts: [...]
    bastion: null
    bootstrap: { enabled: false, retries: 3, retry_delay_secs: 5 }
    kubeconfig: null
```

So `store.rs` needs a private wrapper type for (de)serialization:

```rust
#[derive(Serialize, Deserialize, Default)]
struct ProfilesFile {
    #[serde(default)]
    profiles: std::collections::BTreeMap<String, ProfileBody>,
}

#[derive(Serialize, Deserialize)]
struct ProfileBody {
    name: String,
    #[serde(default)]
    hosts: Vec<Host>,
    #[serde(default)]
    bastion: Option<Bastion>,
    #[serde(default)]
    bootstrap: BootstrapPolicy,
    #[serde(default)]
    kubeconfig: Option<KubeconfigSource>,
}
```

`load_profiles` converts each `(id, ProfileBody)` entry into a `Profile { id, name, hosts, bastion, bootstrap, kubeconfig }`; `save_profiles` does the inverse and writes with `serde_yaml::to_string`, creating parent dirs via `paths.ensure_dirs()` first. A missing file is not an error — `load_profiles` returns `Ok(vec![])` when the file does not exist.

- [ ] **Step 1: Write failing tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::config::{Host, Profile, BootstrapPolicy};

    fn temp_paths(tag: &str) -> ClusterDeckPaths {
        let dir = std::env::temp_dir().join(format!("clusterdeck-store-test-{tag}-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        ClusterDeckPaths::at(dir)
    }

    #[test]
    fn load_profiles_returns_empty_when_file_missing() {
        let paths = temp_paths("missing");
        assert_eq!(load_profiles(&paths).unwrap().len(), 0);
    }

    #[test]
    fn upsert_then_load_roundtrips() {
        let paths = temp_paths("roundtrip");
        let profile = Profile {
            id: "cka".into(),
            name: "CKA Lab".into(),
            hosts: vec![Host { name: "cka-m1".into(), address: "192.0.2.10".into(), port: 22, user: "root".into(), identity_file: None }],
            bastion: None,
            bootstrap: BootstrapPolicy::default(),
            kubeconfig: None,
        };
        upsert_profile(&paths, profile.clone()).unwrap();
        let loaded = get_profile(&paths, "cka").unwrap();
        assert_eq!(loaded.name, "CKA Lab");
        assert_eq!(loaded.hosts.len(), 1);
    }

    #[test]
    fn delete_profile_removes_entry() {
        let paths = temp_paths("delete");
        let profile = Profile { id: "x".into(), name: "X".into(), hosts: vec![], bastion: None, bootstrap: BootstrapPolicy::default(), kubeconfig: None };
        upsert_profile(&paths, profile).unwrap();
        delete_profile(&paths, "x").unwrap();
        assert!(get_profile(&paths, "x").is_err());
    }
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test --manifest-path src-tauri/Cargo.toml services::store`
Expected: FAIL (module does not exist).

- [ ] **Step 3: Implement `store.rs`** per the interfaces and YAML shape above; add `pub mod store;` to `services/mod.rs`.

- [ ] **Step 4: Run to verify pass**

Run: `cargo test --manifest-path src-tauri/Cargo.toml services::store`
Expected: PASS.

- [ ] **Step 5: Rewrite `commands/profiles.rs`**

```rust
use crate::services::{config::Profile, paths::ClusterDeckPaths, store};

#[tauri::command]
pub fn list_profiles() -> Result<Vec<Profile>, String> {
    let paths = ClusterDeckPaths::resolve()?;
    store::load_profiles(&paths)
}

#[tauri::command]
pub fn get_profile_cmd(profile_id: String) -> Result<Profile, String> {
    let paths = ClusterDeckPaths::resolve()?;
    store::get_profile(&paths, &profile_id)
}

#[tauri::command]
pub fn save_profile(profile: Profile) -> Result<(), String> {
    let paths = ClusterDeckPaths::resolve()?;
    store::upsert_profile(&paths, profile)
}

#[tauri::command]
pub fn delete_profile_cmd(profile_id: String) -> Result<(), String> {
    let paths = ClusterDeckPaths::resolve()?;
    store::delete_profile(&paths, &profile_id)
}
```

Remove the old `ProfileSummary` struct and `list_profiles` stub entirely — `Profile` (full struct, `Serialize`) is now returned directly.

- [ ] **Step 6: Register new commands in `lib.rs`**

Add `commands::profiles::get_profile_cmd`, `commands::profiles::save_profile`, `commands::profiles::delete_profile_cmd` to the `tauri::generate_handler![...]` list (keep `list_profiles` and `get_app_info`; `commands::connection::test_connection` will be replaced in Task 8, leave as-is for now).

- [ ] **Step 7: `cargo check` full workspace**

Run: `cargo check --manifest-path src-tauri/Cargo.toml`
Expected: compiles clean.

- [ ] **Step 8: Format, lint, commit**

```bash
cargo fmt --manifest-path src-tauri/Cargo.toml
cargo clippy --manifest-path src-tauri/Cargo.toml -- -D warnings
git add src-tauri/src/services/store.rs src-tauri/src/services/mod.rs src-tauri/src/commands/profiles.rs src-tauri/src/lib.rs
git commit -m "feat(backend): implement profiles.yaml store and real Profile CRUD commands"
```

---

### Task 3: CIDR/IP discovery service + `discover_hosts` command

**Files:**
- Create: `src-tauri/src/services/discovery.rs`
- Create: `src-tauri/src/commands/discovery.rs`
- Modify: `src-tauri/src/services/mod.rs`, `src-tauri/src/commands/mod.rs`, `src-tauri/src/lib.rs`
- Test: `#[cfg(test)]` in `services/discovery.rs`

**Interfaces:**
- Produces:
  ```rust
  // services/discovery.rs
  #[derive(Debug, Clone, PartialEq, Eq)]
  pub struct DiscoveredHost {
      pub address: String,
      pub ssh_open: bool,
  }
  pub fn expand_targets(input: &str) -> Result<Vec<String>, String>;
  // Accepts either a single IPv4 CIDR ("192.0.2.0/29") or a comma-separated
  // explicit IP/hostname list ("192.0.2.10,192.0.2.11"). Caps CIDR expansion
  // at 1024 hosts (prefix >= /22), returns Err for anything larger or invalid.
  pub async fn probe_targets(targets: Vec<String>, port: u16, timeout_ms: u64) -> Vec<DiscoveredHost>;
  // TCP-connect probe per target (std::net::TcpStream, run via tokio::task::spawn_blocking
  // per target, joined with futures::future::join_all or tokio::join on a Vec of handles).
  ```
- Produces Tauri command:
  ```rust
  // commands/discovery.rs
  #[tauri::command]
  pub async fn discover_hosts(input: String, port: Option<u16>) -> Result<Vec<discovery::DiscoveredHost>, String>;
  ```

`expand_targets` implementation notes: if `input` contains `/`, parse as `a.b.c.d/prefix` using `std::net::Ipv4Addr::from_str` on the base and a manual mask computation (`u32::MAX << (32 - prefix)`); iterate host addresses only (exclude network/broadcast for prefix < 31); prefix must be in `22..=32` (reject `>` /22 i.e. more than 1024 addresses, and reject prefix `0` bogus input) — return `Err("CIDR range too large (max 1024 hosts)")` otherwise. If no `/`, split on `,`, trim whitespace, drop empty entries, return the list as-is (hostnames allowed, no validation needed beyond non-empty).

- [ ] **Step 1: Write failing tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expand_targets_parses_comma_list() {
        let out = expand_targets("192.0.2.10, 192.0.2.11").unwrap();
        assert_eq!(out, vec!["192.0.2.10", "192.0.2.11"]);
    }

    #[test]
    fn expand_targets_parses_small_cidr() {
        let out = expand_targets("192.0.2.0/30").unwrap();
        // /30 = 4 addresses, 2 usable hosts (.1, .2)
        assert_eq!(out, vec!["192.0.2.1", "192.0.2.2"]);
    }

    #[test]
    fn expand_targets_rejects_oversized_cidr() {
        assert!(expand_targets("10.0.0.0/8").is_err());
    }

    #[tokio::test]
    async fn probe_targets_marks_unreachable_localhost_port_closed() {
        // Port 1 is reserved/unlikely to be listening in CI.
        let results = probe_targets(vec!["127.0.0.1".into()], 1, 200).await;
        assert_eq!(results.len(), 1);
        assert!(!results[0].ssh_open);
    }
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test --manifest-path src-tauri/Cargo.toml services::discovery`
Expected: FAIL.

- [ ] **Step 3: Implement `services/discovery.rs`** per the spec above.

- [ ] **Step 4: Run to verify pass**

Run: `cargo test --manifest-path src-tauri/Cargo.toml services::discovery`
Expected: PASS.

- [ ] **Step 5: Implement `commands/discovery.rs`**

```rust
use crate::services::discovery::{self, DiscoveredHost};

#[tauri::command]
pub async fn discover_hosts(input: String, port: Option<u16>) -> Result<Vec<DiscoveredHost>, String> {
    let targets = discovery::expand_targets(&input)?;
    Ok(discovery::probe_targets(targets, port.unwrap_or(22), 1500).await)
}
```

Add `pub mod discovery;` to both `services/mod.rs` and `commands/mod.rs`; register `commands::discovery::discover_hosts` in `lib.rs`'s handler list.

- [ ] **Step 6: `cargo check`, format, lint, commit**

```bash
cargo check --manifest-path src-tauri/Cargo.toml
cargo fmt --manifest-path src-tauri/Cargo.toml
cargo clippy --manifest-path src-tauri/Cargo.toml -- -D warnings
git add src-tauri/src/services/discovery.rs src-tauri/src/commands/discovery.rs src-tauri/src/services/mod.rs src-tauri/src/commands/mod.rs src-tauri/src/lib.rs
git commit -m "feat(backend): add CIDR/IP host discovery service and command"
```

---

### Task 4: SSH probe, password bootstrap, and retry logic

**Files:**
- Create: `src-tauri/src/services/ssh.rs`
- Modify: `src-tauri/src/services/mod.rs`
- Test: `#[cfg(test)]` in `services/ssh.rs` using a `FakeRunner`

**Interfaces:**
- Consumes: `CommandRunner`, `CommandOutput` (Task 1); `Host`, `Bastion` (Task 1).
- Produces:
  ```rust
  // services/ssh.rs
  #[derive(Debug, Clone, serde::Serialize)]
  pub struct ProbeResult { pub host: String, pub reachable: bool, pub detail: String }

  #[derive(Debug, Clone, serde::Serialize)]
  pub struct BootstrapResult { pub host: String, pub key_deployed: bool, pub verified: bool, pub detail: String }

  pub fn build_ssh_target_args(host: &Host, bastion: Option<&Bastion>, extra: &[&str]) -> Vec<String>;
  // Builds ["-o","BatchMode=yes","-o","ConnectTimeout=5", "-p", port, "-i", identity(if any),
  //         "-J", "user@bastion:port" (if bastion present), "user@address", ...extra]

  pub async fn probe_key_auth(runner: &dyn CommandRunner, host: &Host, bastion: Option<&Bastion>) -> ProbeResult;
  pub async fn probe_password_auth(runner: &dyn CommandRunner, host: &Host, bastion: Option<&Bastion>, password: &str) -> ProbeResult;
  pub async fn deploy_public_key(runner: &dyn CommandRunner, host: &Host, bastion: Option<&Bastion>, password: &str) -> Result<(), String>;
  // uses sshpass -p <password> ssh-copy-id <same target args as probe, minus BatchMode>
  pub async fn bootstrap_host(runner: &dyn CommandRunner, host: &Host, bastion: Option<&Bastion>, password: &str, retries: u32, retry_delay: std::time::Duration) -> BootstrapResult;
  // deploy_public_key, then probe_key_auth with retry loop up to `retries` times
  pub async fn probe_with_retry(runner: &dyn CommandRunner, host: &Host, bastion: Option<&Bastion>, retries: u32, retry_delay: std::time::Duration) -> ProbeResult;
  ```

`FakeRunner` (test-only, defined once here and reused conceptually by later tasks' own local fakes — do not try to share a single test fixture module across files; each file's `#[cfg(test)]` defines its own minimal fake to keep tasks independent):

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::config::Host;
    use async_trait::async_trait;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct FakeRunner {
        // returns success on the Nth call (1-indexed), failure before that
        succeed_on_call: usize,
        calls: AtomicUsize,
    }

    #[async_trait]
    impl CommandRunner for FakeRunner {
        async fn run(&self, _bin: &str, _args: &[String]) -> Result<CommandOutput, String> {
            let n = self.calls.fetch_add(1, Ordering::SeqCst) + 1;
            Ok(CommandOutput {
                stdout: String::new(),
                stderr: if n >= self.succeed_on_call { String::new() } else { "Permission denied".into() },
                success: n >= self.succeed_on_call,
            })
        }
    }

    fn host() -> Host {
        Host { name: "cka-m1".into(), address: "192.0.2.10".into(), port: 22, user: "root".into(), identity_file: None }
    }

    #[tokio::test]
    async fn probe_key_auth_reports_reachable_on_success() {
        let runner = FakeRunner { succeed_on_call: 1, calls: AtomicUsize::new(0) };
        let result = probe_key_auth(&runner, &host(), None).await;
        assert!(result.reachable);
    }

    #[tokio::test]
    async fn probe_with_retry_succeeds_after_transient_failures() {
        let runner = FakeRunner { succeed_on_call: 3, calls: AtomicUsize::new(0) };
        let result = probe_with_retry(&runner, &host(), None, 3, std::time::Duration::from_millis(1)).await;
        assert!(result.reachable);
    }

    #[tokio::test]
    async fn probe_with_retry_gives_up_after_max_retries() {
        let runner = FakeRunner { succeed_on_call: 99, calls: AtomicUsize::new(0) };
        let result = probe_with_retry(&runner, &host(), None, 2, std::time::Duration::from_millis(1)).await;
        assert!(!result.reachable);
    }

    #[tokio::test]
    async fn build_ssh_target_args_includes_proxy_jump_when_bastion_present() {
        use crate::services::config::Bastion;
        let bastion = Bastion { name: "b".into(), address: "10.0.0.10".into(), port: 22, user: "ubuntu".into(), identity_file: None };
        let args = build_ssh_target_args(&host(), Some(&bastion), &[]);
        assert!(args.iter().any(|a| a == "-J"));
        assert!(args.iter().any(|a| a.contains("ubuntu@10.0.0.10")));
    }
}
```

- [ ] **Step 1: Write the failing tests above in `services/ssh.rs`.**
- [ ] **Step 2: Run to verify failure.** Run: `cargo test --manifest-path src-tauri/Cargo.toml services::ssh` — Expected: FAIL.
- [ ] **Step 3: Implement `build_ssh_target_args`, `probe_key_auth`, `probe_password_auth`, `deploy_public_key`, `probe_with_retry`, `bootstrap_host`** per the interfaces above. `probe_with_retry` loops calling `probe_key_auth`, sleeping `retry_delay` between attempts (use `tokio::time::sleep`), returning the last `ProbeResult`. `bootstrap_host` calls `deploy_public_key` once, then `probe_with_retry`, and folds both outcomes into `BootstrapResult`.
- [ ] **Step 4: Add `pub mod ssh;` to `services/mod.rs`.**
- [ ] **Step 5: Run to verify pass.** Run: `cargo test --manifest-path src-tauri/Cargo.toml services::ssh` — Expected: PASS.
- [ ] **Step 6: `cargo check`, format, lint, commit.**

```bash
cargo check --manifest-path src-tauri/Cargo.toml
cargo fmt --manifest-path src-tauri/Cargo.toml
cargo clippy --manifest-path src-tauri/Cargo.toml -- -D warnings
git add src-tauri/src/services/ssh.rs src-tauri/src/services/mod.rs
git commit -m "feat(backend): add SSH probe, password bootstrap, and retry service"
```

---

### Task 5: SSH alias / ProxyJump config rendering and `~/.ssh/config` include management

**Files:**
- Create: `src-tauri/src/services/ssh_config.rs`
- Modify: `src-tauri/src/services/mod.rs`
- Test: `#[cfg(test)]` in `services/ssh_config.rs`

**Interfaces:**
- Consumes: `Profile`, `Host`, `Bastion` (Task 1), `ClusterDeckPaths` (Task 1).
- Produces:
  ```rust
  // services/ssh_config.rs
  pub fn render_profile_config(profile: &Profile) -> String;
  // One "Host <profile_id>-bastion" block (if bastion set) + one "Host <profile_id>-<host.name>"
  // block per host, each with HostName/User/Port/IdentityFile, and "ProxyJump <profile_id>-bastion"
  // on target blocks when a bastion is present.
  pub fn write_profile_config(paths: &ClusterDeckPaths, profile: &Profile) -> Result<std::path::PathBuf, String>;
  // ensures paths.ensure_dirs(), writes render_profile_config() to paths.ssh_conf(&profile.id)
  pub fn ssh_alias(profile_id: &str, host_name: &str) -> String; // format!("{profile_id}-{host_name}")
  pub fn ensure_ssh_include(home_ssh_config_path: &std::path::Path, paths: &ClusterDeckPaths) -> Result<(), String>;
  // Idempotently prepends `Include <paths.ssh_dir()>/*.conf` as the first line of the given
  // ssh config file if not already present anywhere in the file; creates the file (with the
  // Include line as its sole content) if it doesn't exist yet. Never touches existing lines.
  ```

- [ ] **Step 1: Write failing tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::config::{Bastion, Host, Profile, BootstrapPolicy};

    fn profile_with_bastion() -> Profile {
        Profile {
            id: "cka".into(),
            name: "CKA Lab".into(),
            hosts: vec![Host { name: "cka-m1".into(), address: "192.168.56.10".into(), port: 22, user: "vagrant".into(), identity_file: Some("~/.ssh/cka".into()) }],
            bastion: Some(Bastion { name: "bastion01".into(), address: "10.0.0.10".into(), port: 22, user: "ubuntu".into(), identity_file: Some("~/.ssh/lab".into()) }),
            bootstrap: BootstrapPolicy::default(),
            kubeconfig: None,
        }
    }

    #[test]
    fn render_includes_proxy_jump_for_target_hosts() {
        let rendered = render_profile_config(&profile_with_bastion());
        assert!(rendered.contains("Host cka-bastion"));
        assert!(rendered.contains("Host cka-cka-m1"));
        assert!(rendered.contains("ProxyJump cka-bastion"));
        assert!(rendered.contains("HostName 10.0.0.10"));
    }

    #[test]
    fn ssh_alias_formats_profile_and_host() {
        assert_eq!(ssh_alias("cka", "cka-m1"), "cka-cka-m1");
    }

    #[test]
    fn ensure_ssh_include_creates_file_when_missing() {
        let dir = std::env::temp_dir().join(format!("clusterdeck-sshcfg-test-a-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let ssh_config = dir.join("config");
        let paths = crate::services::paths::ClusterDeckPaths::at(dir.join("cdhome"));
        ensure_ssh_include(&ssh_config, &paths).unwrap();
        let content = std::fs::read_to_string(&ssh_config).unwrap();
        assert!(content.contains("Include"));
        assert!(content.contains("ssh/*.conf"));
    }

    #[test]
    fn ensure_ssh_include_is_idempotent_and_preserves_existing_content() {
        let dir = std::env::temp_dir().join(format!("clusterdeck-sshcfg-test-b-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let ssh_config = dir.join("config");
        std::fs::write(&ssh_config, "Host existing\n  HostName example.invalid\n").unwrap();
        let paths = crate::services::paths::ClusterDeckPaths::at(dir.join("cdhome"));
        ensure_ssh_include(&ssh_config, &paths).unwrap();
        ensure_ssh_include(&ssh_config, &paths).unwrap();
        let content = std::fs::read_to_string(&ssh_config).unwrap();
        assert_eq!(content.matches("Include").count(), 1);
        assert!(content.contains("Host existing"));
    }
}
```

- [ ] **Step 2: Run to verify failure.** Run: `cargo test --manifest-path src-tauri/Cargo.toml services::ssh_config` — Expected: FAIL.
- [ ] **Step 3: Implement `ssh_config.rs`** per the spec. `render_profile_config` skips `IdentityFile` lines when `identity_file` is `None`. `ensure_ssh_include` reads the file (empty string if missing), checks `content.contains("Include")` — if not found, writes `format!("Include {}/*.conf\n\n{}", paths.ssh_dir().display(), content)` back to the file, creating parent dirs if needed.
- [ ] **Step 4: Add `pub mod ssh_config;` to `services/mod.rs`.**
- [ ] **Step 5: Run to verify pass.** Run: `cargo test --manifest-path src-tauri/Cargo.toml services::ssh_config` — Expected: PASS.
- [ ] **Step 6: `cargo check`, format, lint, commit.**

```bash
cargo check --manifest-path src-tauri/Cargo.toml
cargo fmt --manifest-path src-tauri/Cargo.toml
cargo clippy --manifest-path src-tauri/Cargo.toml -- -D warnings
git add src-tauri/src/services/ssh_config.rs src-tauri/src/services/mod.rs
git commit -m "feat(backend): generate ClusterDeck-owned SSH alias/ProxyJump config and ssh_config Include management"
```

---

### Task 6: Remote kubeconfig fetch, normalization, and local storage

**Files:**
- Create: `src-tauri/src/services/kubeconfig.rs`
- Modify: `src-tauri/src/services/mod.rs`
- Test: `#[cfg(test)]` in `services/kubeconfig.rs`

**Interfaces:**
- Consumes: `CommandRunner`, `CommandOutput` (Task 1); `Profile`, `KubeconfigSource` (Task 1); `ClusterDeckPaths` (Task 1); `ssh_config::ssh_alias`, `ssh_config` module path (Task 5) — fetch uses the already-written alias config via `scp -F <ssh_conf_path> <alias>:<remote_path> <tmp_path>`.
- Produces:
  ```rust
  // services/kubeconfig.rs
  #[derive(Debug, Clone, serde::Serialize)]
  pub struct KubeconfigSummary { pub cluster_name: String, pub context_name: String, pub local_path: String }

  pub fn normalize(raw_yaml: &str, profile_id: &str) -> Result<String, String>;
  // Parses raw_yaml as serde_yaml::Value, renames clusters[0].name, contexts[0].name,
  // contexts[0].context.cluster, contexts[0].context.user, users[0].name to `profile_id`,
  // sets current-context to `profile_id`. Leaves certificate-authority-data / client
  // cert/key data and the `server` field untouched. Returns Err if the document does not
  // have exactly one entry in clusters/contexts/users (MVP scope: single-cluster kubeconfig).
  pub async fn fetch_and_store(runner: &dyn CommandRunner, paths: &ClusterDeckPaths, profile: &Profile) -> Result<KubeconfigSummary, String>;
  // Resolves the control_plane Host by name from profile.hosts, builds the ssh alias via
  // ssh_config::ssh_alias(&profile.id, &host.name), runs:
  //   scp -F <paths.ssh_conf(&profile.id)> <alias>:<kubeconfig.remote_path> <tmpfile>
  // reads the tmp file, calls normalize(), writes the result to
  // paths.kubeconfig_file(&profile.id) with 0600 permissions (std::os::unix::fs::PermissionsExt),
  // deletes the tmp file, and returns a KubeconfigSummary.
  ```

- [ ] **Step 1: Write failing tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"
apiVersion: v1
kind: Config
clusters:
  - name: original-cluster
    cluster:
      server: https://192.0.2.10:6443
      certificate-authority-data: ZmFrZS1jYQ==
contexts:
  - name: original-context
    context:
      cluster: original-cluster
      user: original-user
current-context: original-context
users:
  - name: original-user
    user:
      client-certificate-data: ZmFrZS1jZXJ0
      client-key-data: ZmFrZS1rZXk=
"#;

    #[test]
    fn normalize_renames_cluster_context_and_user_to_profile_id() {
        let normalized = normalize(SAMPLE, "cka").unwrap();
        let value: serde_yaml::Value = serde_yaml::from_str(&normalized).unwrap();
        assert_eq!(value["current-context"].as_str().unwrap(), "cka");
        assert_eq!(value["clusters"][0]["name"].as_str().unwrap(), "cka");
        assert_eq!(value["contexts"][0]["name"].as_str().unwrap(), "cka");
        assert_eq!(value["contexts"][0]["context"]["cluster"].as_str().unwrap(), "cka");
        assert_eq!(value["contexts"][0]["context"]["user"].as_str().unwrap(), "cka");
        assert_eq!(value["users"][0]["name"].as_str().unwrap(), "cka");
        // certificate data must survive untouched
        assert_eq!(value["clusters"][0]["cluster"]["certificate-authority-data"].as_str().unwrap(), "ZmFrZS1jYQ==");
    }

    #[test]
    fn normalize_rejects_multi_cluster_kubeconfig() {
        let multi = SAMPLE.replace(
            "clusters:\n  - name: original-cluster",
            "clusters:\n  - name: original-cluster\n    cluster:\n      server: https://x\n  - name: second",
        );
        assert!(normalize(&multi, "cka").is_err());
    }
}
```

- [ ] **Step 2: Run to verify failure.** Run: `cargo test --manifest-path src-tauri/Cargo.toml services::kubeconfig` — Expected: FAIL.
- [ ] **Step 3: Implement `normalize`** using `serde_yaml::Value` mutation as specified (guard: `clusters`/`contexts`/`users` sequences must each have length 1, else `Err("multi-cluster kubeconfig is not supported in MVP".into())`).
- [ ] **Step 4: Implement `fetch_and_store`** per the spec (async, uses `runner.run("scp", &[...])`, checks `CommandOutput.success`, maps failure to `Err(format!("kubeconfig fetch failed: {}", output.stderr))` — never includes file contents in the error).
- [ ] **Step 5: Add `pub mod kubeconfig;` to `services/mod.rs`.**
- [ ] **Step 6: Run to verify pass.** Run: `cargo test --manifest-path src-tauri/Cargo.toml services::kubeconfig` — Expected: PASS.
- [ ] **Step 7: `cargo check`, format, lint, commit.**

```bash
cargo check --manifest-path src-tauri/Cargo.toml
cargo fmt --manifest-path src-tauri/Cargo.toml
cargo clippy --manifest-path src-tauri/Cargo.toml -- -D warnings
git add src-tauri/src/services/kubeconfig.rs src-tauri/src/services/mod.rs
git commit -m "feat(backend): fetch, normalize, and locally store remote kubeconfig"
```

---

### Task 7: Kubernetes connectivity verification + Profile status persistence

**Files:**
- Create: `src-tauri/src/services/verify.rs`
- Create: `src-tauri/src/services/state.rs`
- Modify: `src-tauri/src/services/mod.rs`
- Test: `#[cfg(test)]` in both new files

**Interfaces:**
- Consumes: `CommandRunner`, `CommandOutput` (Task 1); `ClusterDeckPaths` (Task 1).
- Produces:
  ```rust
  // services/verify.rs
  #[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
  pub struct VerificationResult {
      pub ssh: bool,
      pub kubeconfig: bool,
      pub kubernetes: bool,
      pub node_count: Option<u32>,
      pub api_endpoint: Option<String>,
      pub last_verified: Option<String>, // RFC3339, chrono::Utc::now().to_rfc3339()
  }
  pub async fn verify_cluster(runner: &dyn CommandRunner, kubeconfig_path: &std::path::Path, context: &str) -> VerificationResult;
  // Runs `kubectl --kubeconfig <path> --context <context> get nodes -o json`, parses
  // `.items | length` for node_count via serde_json, sets kubernetes=true on success.
  // ssh/kubeconfig fields are filled in by the caller (commands/connection.rs in Task 8),
  // not by this function — verify_cluster only knows about the kubernetes step, so it always
  // returns ssh: false, kubeconfig: false as placeholders the caller overwrites.
  ```
  ```rust
  // services/state.rs
  #[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
  pub struct StateFile { pub profiles: std::collections::BTreeMap<String, crate::services::verify::VerificationResult> }

  pub fn load_state(paths: &ClusterDeckPaths) -> Result<StateFile, String>; // Ok(default) if file missing
  pub fn save_status(paths: &ClusterDeckPaths, profile_id: &str, result: crate::services::verify::VerificationResult) -> Result<(), String>;
  pub fn get_status(paths: &ClusterDeckPaths, profile_id: &str) -> Result<Option<crate::services::verify::VerificationResult>, String>;
  ```

- [ ] **Step 1: Write failing tests for `verify.rs`**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::process::CommandOutput;
    use async_trait::async_trait;

    struct FakeRunner { nodes_json: &'static str, success: bool }

    #[async_trait]
    impl crate::services::process::CommandRunner for FakeRunner {
        async fn run(&self, _bin: &str, _args: &[String]) -> Result<CommandOutput, String> {
            Ok(CommandOutput { stdout: self.nodes_json.into(), stderr: String::new(), success: self.success })
        }
    }

    #[tokio::test]
    async fn verify_cluster_counts_nodes_on_success() {
        let runner = FakeRunner { nodes_json: r#"{"items":[{},{},{}]}"#, success: true };
        let result = verify_cluster(&runner, std::path::Path::new("/tmp/kc.yaml"), "cka").await;
        assert!(result.kubernetes);
        assert_eq!(result.node_count, Some(3));
        assert!(result.last_verified.is_some());
    }

    #[tokio::test]
    async fn verify_cluster_reports_false_on_kubectl_failure() {
        let runner = FakeRunner { nodes_json: "", success: false };
        let result = verify_cluster(&runner, std::path::Path::new("/tmp/kc.yaml"), "cka").await;
        assert!(!result.kubernetes);
        assert_eq!(result.node_count, None);
    }
}
```

- [ ] **Step 2: Run to verify failure.** Run: `cargo test --manifest-path src-tauri/Cargo.toml services::verify` — Expected: FAIL.
- [ ] **Step 3: Implement `verify.rs`** per spec, using `chrono::Utc::now().to_rfc3339()` for `last_verified` only when the kubectl call itself succeeded (attempted), and `serde_json::from_str::<serde_json::Value>(&output.stdout)` then `.get("items").and_then(|v| v.as_array()).map(|a| a.len() as u32)` for `node_count`.
- [ ] **Step 4: Run to verify pass.** Run: `cargo test --manifest-path src-tauri/Cargo.toml services::verify` — Expected: PASS.
- [ ] **Step 5: Write failing tests for `state.rs`**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::paths::ClusterDeckPaths;
    use crate::services::verify::VerificationResult;

    fn temp_paths(tag: &str) -> ClusterDeckPaths {
        let dir = std::env::temp_dir().join(format!("clusterdeck-state-test-{tag}-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        ClusterDeckPaths::at(dir)
    }

    #[test]
    fn get_status_returns_none_when_absent() {
        let paths = temp_paths("absent");
        assert!(get_status(&paths, "cka").unwrap().is_none());
    }

    #[test]
    fn save_then_get_status_roundtrips() {
        let paths = temp_paths("roundtrip");
        let result = VerificationResult { ssh: true, kubeconfig: true, kubernetes: true, node_count: Some(3), api_endpoint: None, last_verified: Some("2026-08-24T00:00:00Z".into()) };
        save_status(&paths, "cka", result.clone()).unwrap();
        let loaded = get_status(&paths, "cka").unwrap().unwrap();
        assert_eq!(loaded.node_count, Some(3));
    }
}
```

- [ ] **Step 6: Run to verify failure.** Run: `cargo test --manifest-path src-tauri/Cargo.toml services::state` — Expected: FAIL.
- [ ] **Step 7: Implement `state.rs`** — `load_state` reads `paths.state_file()` via `serde_json::from_str`, returning `Ok(StateFile::default())` if the file is missing; `save_status` loads current state, inserts/overwrites the entry, writes back via `serde_json::to_string_pretty` after `paths.ensure_dirs()`; `get_status` loads state and returns `.profiles.get(profile_id).cloned()`.
- [ ] **Step 8: Add `pub mod verify;` and `pub mod state;` to `services/mod.rs`.**
- [ ] **Step 9: Run full test suite for this task.** Run: `cargo test --manifest-path src-tauri/Cargo.toml services::verify services::state` — Expected: PASS.
- [ ] **Step 10: `cargo check`, format, lint, commit.**

```bash
cargo check --manifest-path src-tauri/Cargo.toml
cargo fmt --manifest-path src-tauri/Cargo.toml
cargo clippy --manifest-path src-tauri/Cargo.toml -- -D warnings
git add src-tauri/src/services/verify.rs src-tauri/src/services/state.rs src-tauri/src/services/mod.rs
git commit -m "feat(backend): add kubectl-based cluster verification and profile status persistence"
```

---

### Task 8: Orchestrating `connection.rs` commands + `lib.rs` wiring

**Files:**
- Modify: `src-tauri/src/commands/connection.rs`
- Modify: `src-tauri/src/lib.rs`
- Test: none new (this task wires Tasks 1–7 together behind Tauri commands; correctness is covered by the service-level tests already written — this task is verified by `cargo check`/`cargo clippy` compiling the full call graph, per Task Right-Sizing: wiring has no independent logic to unit-test)

**Interfaces:**
- Consumes everything from Tasks 1–7: `ClusterDeckPaths::resolve`, `store::{get_profile, upsert_profile}`, `ssh::{probe_with_retry, bootstrap_host}`, `ssh_config::{write_profile_config, ensure_ssh_include, ssh_alias}`, `kubeconfig::fetch_and_store`, `verify::verify_cluster`, `state::save_status`, `discovery::{expand_targets, probe_targets}` (already exposed via `commands/discovery.rs` in Task 3 — not re-exposed here).
- Produces the commands the frontend (Tasks 9–10) calls:
  ```rust
  #[derive(Debug, Clone, serde::Serialize)]
  pub struct HostStageResult { pub host: String, pub reachable: bool, pub detail: String }

  #[derive(Debug, Clone, serde::Serialize)]
  pub struct ConnectionResult {
      pub hosts: Vec<HostStageResult>,
      pub aliases_written: bool,
      pub kubeconfig: Option<crate::services::kubeconfig::KubeconfigSummary>,
      pub verification: crate::services::verify::VerificationResult,
  }

  #[tauri::command] pub async fn probe_profile_hosts(profile_id: String) -> Result<Vec<HostStageResult>, String>;
  #[tauri::command] pub async fn bootstrap_profile(profile_id: String, password: String) -> Result<Vec<crate::services::ssh::BootstrapResult>, String>;
  #[tauri::command] pub async fn generate_aliases(profile_id: String) -> Result<(), String>;
  #[tauri::command] pub async fn fetch_kubeconfig(profile_id: String) -> Result<crate::services::kubeconfig::KubeconfigSummary, String>;
  #[tauri::command] pub async fn verify_profile(profile_id: String) -> Result<crate::services::verify::VerificationResult, String>;
  #[tauri::command] pub async fn get_profile_status(profile_id: String) -> Result<Option<crate::services::verify::VerificationResult>, String>;
  #[tauri::command] pub async fn connect_profile(profile_id: String, bootstrap_password: Option<String>) -> Result<ConnectionResult, String>;
  ```

  `connect_profile` runs the full pipeline in order (matches `docs/03-mvp-design.md` §2): for each host, `ssh::probe_with_retry`; for any host that failed and `bootstrap_password.is_some()` and `profile.bootstrap.enabled`, call `ssh::bootstrap_host` then re-probe; then `ssh_config::write_profile_config` + `ssh_config::ensure_ssh_include(&home_ssh_config_path, &paths)` (home path = `PathBuf::from(std::env::var("HOME")?).join(".ssh").join("config")`); then, only if `profile.kubeconfig.is_some()` and at least one host is reachable, `kubeconfig::fetch_and_store`; then, only if the kubeconfig step succeeded, `verify::verify_cluster`; finally `state::save_status` with `ssh`/`kubeconfig` fields filled in from the earlier steps (overwriting the placeholders `verify_cluster` returns) and the command returns the assembled `ConnectionResult`. Each stage's failure is caught and folded into the result rather than aborting the whole command (e.g. kubeconfig fetch failure still returns a `ConnectionResult` with `kubeconfig: None` and `verification.kubernetes: false`, not an `Err`) — the old `test_connection` command's "always returns Ok with structured falses" behavior is the model to follow, extended with the new fields.

- [ ] **Step 1: Delete the old stub in `commands/connection.rs`** (the `ConnectionResult { ssh, kubeconfig, kubernetes }` struct and `test_connection` fn) and replace the file with the implementation described above, importing all needed service modules.

- [ ] **Step 2: Implement `probe_profile_hosts`**

```rust
#[tauri::command]
pub async fn probe_profile_hosts(profile_id: String) -> Result<Vec<HostStageResult>, String> {
    let paths = crate::services::paths::ClusterDeckPaths::resolve()?;
    let profile = crate::services::store::get_profile(&paths, &profile_id)?;
    let runner = crate::services::process::SystemRunner;
    let mut results = Vec::new();
    for host in &profile.hosts {
        let probe = crate::services::ssh::probe_with_retry(&runner, host, profile.bastion.as_ref(), 1, std::time::Duration::from_secs(1)).await;
        results.push(HostStageResult { host: host.name.clone(), reachable: probe.reachable, detail: probe.detail });
    }
    Ok(results)
}
```

- [ ] **Step 3: Implement `bootstrap_profile`, `generate_aliases`, `fetch_kubeconfig`, `verify_profile`, `get_profile_status`** following the same pattern (load paths + profile, construct `SystemRunner`, call the relevant service function(s), map to the command's return type).

- [ ] **Step 4: Implement `connect_profile`** per the orchestration spec above.

- [ ] **Step 5: Update `lib.rs`**

```rust
mod commands;
mod services;

pub fn run() {
    tauri::Builder::default()
        .invoke_handler(tauri::generate_handler![
            commands::app::get_app_info,
            commands::profiles::list_profiles,
            commands::profiles::get_profile_cmd,
            commands::profiles::save_profile,
            commands::profiles::delete_profile_cmd,
            commands::discovery::discover_hosts,
            commands::connection::probe_profile_hosts,
            commands::connection::bootstrap_profile,
            commands::connection::generate_aliases,
            commands::connection::fetch_kubeconfig,
            commands::connection::verify_profile,
            commands::connection::get_profile_status,
            commands::connection::connect_profile,
        ])
        .run(tauri::generate_context!())
        .expect("error while running ClusterDeck");
}
```

- [ ] **Step 6: Full backend verification**

```bash
cargo check --manifest-path src-tauri/Cargo.toml
cargo test --manifest-path src-tauri/Cargo.toml
cargo fmt --manifest-path src-tauri/Cargo.toml
cargo clippy --manifest-path src-tauri/Cargo.toml -- -D warnings
```

Expected: all green — this is the integration point for every service written in Tasks 1–7, so a failure here means a signature mismatch against an earlier task's interface, not new logic.

- [ ] **Step 7: Commit**

```bash
git add src-tauri/src/commands/connection.rs src-tauri/src/lib.rs
git commit -m "feat(backend): wire full connect pipeline (probe, bootstrap, alias, kubeconfig, verify) into Tauri commands"
```

---

### Task 9: Frontend typed API client

**Files:**
- Create: `src/api/tauri.ts`
- Test: none (thin typed wrapper; correctness is exercised by Task 10's manual browser verification per the project's evidence rules — no frontend test runner is configured in `package.json` yet, and adding one is out of scope for this plan)

**Interfaces:**
- Consumes: `@tauri-apps/api/core`'s `invoke` (already a dependency).
- Produces (names/shapes the frontend in Task 10 imports verbatim; keep every field name identical to the Rust `serde::Serialize` struct it mirrors, since Tauri sends JSON as-is):

```typescript
import { invoke } from '@tauri-apps/api/core';

export type Host = {
  name: string;
  address: string;
  port: number;
  user: string;
  identity_file: string | null;
};

export type Bastion = {
  name: string;
  address: string;
  port: number;
  user: string;
  identity_file: string | null;
};

export type BootstrapPolicy = {
  enabled: boolean;
  retries: number;
  retry_delay_secs: number;
};

export type KubeconfigSource = {
  remote_path: string;
  control_plane: string;
  local_path: string;
  context: string;
};

export type Profile = {
  id: string;
  name: string;
  hosts: Host[];
  bastion: Bastion | null;
  bootstrap: BootstrapPolicy;
  kubeconfig: KubeconfigSource | null;
};

export type HostStageResult = { host: string; reachable: boolean; detail: string };

export type BootstrapResult = { host: string; key_deployed: boolean; verified: boolean; detail: string };

export type KubeconfigSummary = { cluster_name: string; context_name: string; local_path: string };

export type VerificationResult = {
  ssh: boolean;
  kubeconfig: boolean;
  kubernetes: boolean;
  node_count: number | null;
  api_endpoint: string | null;
  last_verified: string | null;
};

export type ConnectionResult = {
  hosts: HostStageResult[];
  aliases_written: boolean;
  kubeconfig: KubeconfigSummary | null;
  verification: VerificationResult;
};

export type DiscoveredHost = { address: string; ssh_open: boolean };

export const api = {
  listProfiles: () => invoke<Profile[]>('list_profiles'),
  getProfile: (profileId: string) => invoke<Profile>('get_profile_cmd', { profileId }),
  saveProfile: (profile: Profile) => invoke<void>('save_profile', { profile }),
  deleteProfile: (profileId: string) => invoke<void>('delete_profile_cmd', { profileId }),
  discoverHosts: (input: string, port?: number) => invoke<DiscoveredHost[]>('discover_hosts', { input, port }),
  probeProfileHosts: (profileId: string) => invoke<HostStageResult[]>('probe_profile_hosts', { profileId }),
  bootstrapProfile: (profileId: string, password: string) => invoke<BootstrapResult[]>('bootstrap_profile', { profileId, password }),
  generateAliases: (profileId: string) => invoke<void>('generate_aliases', { profileId }),
  fetchKubeconfig: (profileId: string) => invoke<KubeconfigSummary>('fetch_kubeconfig', { profileId }),
  verifyProfile: (profileId: string) => invoke<VerificationResult>('verify_profile', { profileId }),
  getProfileStatus: (profileId: string) => invoke<VerificationResult | null>('get_profile_status', { profileId }),
  connectProfile: (profileId: string, bootstrapPassword?: string) =>
    invoke<ConnectionResult>('connect_profile', { profileId, bootstrapPassword }),
};
```

- [ ] **Step 1: Create `src/api/tauri.ts`** with the exact content above (Tauri's `invoke` auto-converts the JS object's keys to the command's declared Rust parameter names — `profileId` → `profile_id` is handled by Tauri's camelCase-to-snake_case argument matching, which is already relied on nowhere else in this codebase yet, so this is the first place it matters; verify this in Step 2).
- [ ] **Step 2: Type-check in isolation**

Run: `pnpm exec tsc --noEmit -p tsconfig.json`
Expected: no errors referencing `src/api/tauri.ts` (errors elsewhere from the not-yet-updated `App.tsx` in this task are expected and fixed in Task 10).

- [ ] **Step 3: Commit**

```bash
git add src/api/tauri.ts
git commit -m "feat(frontend): add typed Tauri command client"
```

---

### Task 10: Wire `App.tsx` to the real backend

**Files:**
- Modify: `src/App.tsx`
- Manual verification: `pnpm tauri dev` (see Step 5)

**Interfaces:**
- Consumes: everything exported from `src/api/tauri.ts` (Task 9).

Replace the hardcoded `initialProfiles` and the mock `connect`/`refresh` handlers:

- [ ] **Step 1: Load profiles from the backend on mount**

```typescript
import { useEffect, useMemo, useState } from 'react';
import { api, type ConnectionResult, type Profile } from './api/tauri';
// keep existing lucide-react icon imports

export default function App() {
  const [profiles, setProfiles] = useState<Profile[]>([]);
  const [selectedId, setSelectedId] = useState<string | null>(null);
  const [connecting, setConnecting] = useState(false);
  const [lastResult, setLastResult] = useState<ConnectionResult | null>(null);
  const [loadError, setLoadError] = useState<string | null>(null);

  const loadProfiles = async () => {
    try {
      const loaded = await api.listProfiles();
      setProfiles(loaded);
      setSelectedId((current) => current ?? loaded[0]?.id ?? null);
      setLoadError(null);
    } catch (err) {
      setLoadError(String(err));
    }
  };

  useEffect(() => {
    loadProfiles();
  }, []);

  const selected = useMemo(() => profiles.find((profile) => profile.id === selectedId) ?? null, [profiles, selectedId]);
  // ... rest below
```

- [ ] **Step 2: Replace `connect`/`refresh` with real calls**

```typescript
  const connect = async () => {
    if (!selected) return;
    setConnecting(true);
    try {
      const result = await api.connectProfile(selected.id);
      setLastResult(result);
    } catch (err) {
      setLoadError(String(err));
    } finally {
      setConnecting(false);
    }
  };

  const refresh = () => loadProfiles();
```

- [ ] **Step 3: Replace host-list/status rendering to use `lastResult` instead of hardcoded strings**

In the "Hosts" panel, derive `reachable` per host from `lastResult?.hosts.find((h) => h.host === host.name)?.reachable`, defaulting to `false` when `lastResult` is `null` (no successful connect yet). In the "Kubernetes" panel, replace the four hardcoded `status-row` values with:

```tsx
<div className="status-row"><span>SSH</span><strong>{lastResult?.verification.ssh ? 'Ready' : '—'}</strong></div>
<div className="status-row"><span>Kubeconfig</span><strong>{lastResult?.verification.kubeconfig ? 'Synced' : '—'}</strong></div>
<div className="status-row"><span>Context</span><strong>{selected?.kubeconfig?.context ?? '—'}</strong></div>
<div className="status-row"><span>API</span><strong>{lastResult?.verification.kubernetes ? 'Verified' : '—'}</strong></div>
```

- [ ] **Step 4: Handle the empty-profiles and load-error states**

Where the sidebar currently assumes `profiles` is non-empty (e.g. `selected?.name ?? 'Cluster'` already guards this reasonably), add a one-line banner rendering `loadError` above `main-panel` when set: `{loadError && <div className="pill warning">{loadError}</div>}`. When `profiles.length === 0` and no error, render `<p>No profiles yet. Add one to ~/.clusterdeck/profiles.yaml.</p>` in place of the profile list (Profile-creation UI is out of scope for this plan — `AGENTS.md` change-rule 4 says keep each change scoped to one logical purpose, and issue #1's Profile CRUD UI is a separate concern from the connect pipeline this plan implements; profiles are created by hand-editing `profiles.yaml` or via a future UI task).

- [ ] **Step 5: Manual verification in the real app**

Run: `pnpm install && pnpm build`
Expected: `tsc && vite build` succeeds with zero errors.

Then, to observe actual behavior (per this project's evidence rules — a passing build is not a working feature):

```bash
mkdir -p ~/.clusterdeck
cat > ~/.clusterdeck/profiles.yaml <<'EOF'
profiles:
  demo:
    name: Demo
    hosts: []
    bastion: null
    bootstrap: { enabled: false, retries: 3, retry_delay_secs: 5 }
    kubeconfig: null
EOF
pnpm tauri dev
```

Confirm in the launched app window: the sidebar shows "Demo" (loaded from the real file, not the old hardcoded "CKA Lab"/"Dev Cluster"), and clicking "Connect / Sync" completes without throwing (an empty host list means `connect_profile` returns quickly with `hosts: []`, `kubeconfig: null`, `verification.kubernetes: false` — this exercises the full IPC round-trip end to end). Take a screenshot or describe what rendered; do not claim this step done from the build passing alone.

- [ ] **Step 6: Commit**

```bash
git add src/App.tsx
git commit -m "feat(frontend): replace mock profile data with real Tauri backend calls"
```

---

### Task 11: Full-repo CI parity check and documentation update

**Files:**
- Modify: `docs/03-mvp-design.md` (mark completed items in §11 "First implementation sequence")
- Modify: `docs/ARCHITECTURE.md` (mark completed items in §14 "MVP Boundaries") if its checklist-style prose needs a status note — only touch this if the doc's own convention supports marking progress; otherwise skip per "don't duplicate/restructure docs beyond what changed"
- No new source files

**Interfaces:** none — this task is a verification and documentation-sync gate, not new code.

- [ ] **Step 1: Run the exact CI check sequence from `docs/CI.md`**

```bash
cargo fmt --manifest-path src-tauri/Cargo.toml --check
cargo clippy --manifest-path src-tauri/Cargo.toml -- -D warnings
cargo test --manifest-path src-tauri/Cargo.toml
pnpm install --no-frozen-lockfile
pnpm build
```

Expected: every command exits 0. If `cargo fmt --check` fails, run `cargo fmt` (without `--check`) and re-commit the formatting fix as its own `style:` commit rather than folding it into a feature commit.

- [ ] **Step 2: Update `docs/03-mvp-design.md` §11**

Mark items 1–8 (build/launch through Kubernetes verification) as done and item 9 (Bastion/ProxyJump) as done given Task 5 implemented it; leave item 10 (richer IP discovery/status refresh beyond the basic CIDR probe in Task 3) unmarked — this plan implements CIDR/explicit-IP discovery but not provider-specific auto-discovery, which is explicitly deferred in `docs/ARCHITECTURE.md` §14. Use whatever the existing document's convention is for marking status (it is currently a plain numbered list with no checkboxes — add a trailing `— implemented` to completed lines rather than introducing a new checkbox convention the rest of the doc doesn't use).

- [ ] **Step 3: Verify no placeholder/stub code remains**

```bash
grep -rn "TODO" src-tauri/src src/App.tsx src/api
```

Expected: no output (the two `TODO` comments that existed in the original scaffold — in `connection.rs` and `App.tsx` — were removed in Tasks 8 and 10 respectively). If any remain, resolve them before closing this task; an unresolved `TODO` in changed code is a blocker per this project's completion rules, not something to leave for later.

- [ ] **Step 4: Commit**

```bash
git add docs/03-mvp-design.md
git commit -m "docs: mark MVP connect-pipeline sequence items implemented"
```

- [ ] **Step 5: Final report**

Summarize for the user: which of Issues #2/#3/#4/#5's MVP checklist items (from each issue's body) are now implemented vs. still open (e.g. `sshpass`-dependent password bootstrap requires `sshpass` installed — already confirmed present on this machine; multi-Bastion chains and VM-provider auto-discovery remain explicitly out of scope per `docs/ARCHITECTURE.md` §14's "later phases" list). Do not close the GitHub issues from this plan — leave that to the user's review (`gh issue view`/`gh issue close` are theirs to run).

---

## Self-Review Notes

- **Spec coverage:** Issue #2 (discovery §Task 3, bootstrap/retry/alias §Tasks 4–5, Profile CRUD §Task 2) — covered. Issue #3 (remote kubeconfig fetch + local integration §Task 6) — covered. Issue #4 (Bastion/ProxyJump §Tasks 4–6, ProxyJump built into `build_ssh_target_args` and `render_profile_config`, kubeconfig fetch reuses the bastion-aware alias) — covered. Issue #5 (verification §Task 7, Profile status persistence §Task 7's `state.rs`, orchestrated end-to-end §Task 8, UI status display §Task 10) — covered.
- **Deferred by design (not gaps):** VM-provider IP auto-discovery, multi-Bastion chains, SSH agent forwarding, per-profile env vars, menu-bar UI, Profile-creation UI (create/edit forms) — all explicitly listed as "추후 고려" in Issue #1 or "later phases" in `docs/ARCHITECTURE.md` §14. Profile CRUD *commands* are implemented (Task 2); a create/edit *UI* is not, per Task 10 Step 4's scoping note — flag this to the user in Task 11 Step 5.
- **Type consistency check performed:** `Profile`/`Host`/`Bastion`/`BootstrapPolicy`/`KubeconfigSource` (Task 1) are used with identical field names through Tasks 2, 5, 6, 8, 9. `CommandRunner`/`CommandOutput`/`SystemRunner` (Task 1) are the sole process-execution interface used by Tasks 4, 6, 7, 8 — no task calls `tokio::process::Command` directly outside `SystemRunner`. `ClusterDeckPaths` (Task 1) is the sole path-resolution interface used by Tasks 2, 5, 6, 7, 8. `ssh_config::ssh_alias` (Task 5) is the exact function Task 6's `fetch_and_store` calls to build the `scp` target — verified same name and signature in both tasks' interface blocks.
