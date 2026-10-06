# ADR-0008: VM inventory as an observed environment source and one-click profile creation

- Status: Proposed
- Date: 2026-10-04
- Issue: #14
- Supersedes: [ADR-0005](0005-local-host-detection-prefills-profiles.md) D1 ("nothing persisted before Save") for the one-click "Set up cluster" flow only.
- Relates to: [ADR-0003](0003-colima-lima-local-runtime-provider.md), [ADR-0005](0005-local-host-detection-prefills-profiles.md), [ADR-0007](0007-local-runtime-lifecycle-actions.md)

## Context

`AGENTS.md` fixes the product boundary at `Discovery → SSH Bootstrap → SSH/ProxyJump → kubeconfig Fetch → Normalize → Verify`, and forbids turning ClusterDeck into a general Kubernetes administration console unless explicitly changed through an Architecture Decision Record. [ADR-0003](0003-colima-lima-local-runtime-provider.md) widened this boundary by one axis — observing local runtimes as environment sources — and [ADR-0007](0007-local-runtime-lifecycle-actions.md) added guarded lifecycle actions for Colima and Lima instances.

InfraDeck was originally started as a companion macOS dashboard focused on multi-provider VM inventory (Colima, VirtualBox, VMware Fusion, Vagrant), cross-provider relationship mapping, and host resource aggregation. Its integration with ClusterDeck relied on an external file-editing handoff: writing a cluster definition into `~/.clusterdeck/profiles.yaml` (with backup `profiles.yaml.infradeck.bak`) and shelling out to `open -a ClusterDeck.app`. This decoupled handoff introduced significant operational hazards: concurrent file modification, unvalidated profile injection, and lack of direct deep-linking in ClusterDeck.

Merging InfraDeck directly into ClusterDeck unites local infrastructure observation and cluster connection in a single application. This requires an architectural decision on how VM inventory fits into ClusterDeck's product boundary, execution model, domain identity, and persistence guarantees.

## Decision

### D1 — Widen product boundary by exactly one axis: observe VM inventory as an environment source

ClusterDeck incorporates the Infrastructure inventory feature from InfraDeck as an environment source. The product boundary expands to observe local VM inventory across four providers:
- **Colima** (Lima-backed containers/VMs)
- **VirtualBox** (registered VMs via `VBoxManage`)
- **VMware Fusion** (running VMs via `vmrun` and `.vmx` inspection)
- **Vagrant** (multi-machine project environments via `vagrant global-status` and `.vagrant` metadata)

Observation remains **read-only**:
- No lifecycle mutation actions (start, stop, pause, resume, destroy) are introduced for VirtualBox, VMware Fusion, or Vagrant. Lifecycle actions remain strictly scoped to Colima and Lima under [ADR-0007](0007-local-runtime-lifecycle-actions.md).
- ClusterDeck does not become a general Kubernetes administration console. Kubernetes status is observed solely for connectivity (API endpoint presence and reachability), leaving workload and resource management to dedicated tools.

### D2 — Single detection path and shared parser reuse

Rather than maintaining duplicate CLI execution and parsing logic, `services/inventory/` reuses and extracts shared parsing functions from `services/local_runtime.rs`:
- Colima listing parser supports both JSON array and NDJSON streams.
- Vagrant `global-status` parser and machine inspection logic are shared.
- The inventory service enriches these base records with inventory-specific fields: `disk_used_gib`, `created_at`, and normalized memory/disk resource capacities in GiB.

### D3 — Bounded command execution via `CommandRunner` (`run_bounded`)

All external provider CLI executions must strictly use ClusterDeck's `CommandRunner` trait (`services/process.rs`). Direct usage of `tokio::process::Command` is prohibited.

To preserve the defensive process execution guarantees proven in InfraDeck, `CommandRunner` introduces `run_bounded(bin, args, limits)`:
- Default implementation delegates to `run` to ensure `FakeRunner` and existing callers remain unbroken.
- `SystemRunner` enforces:
  - `LC_ALL=C` environment for deterministic, locale-independent output parsing.
  - Standard input redirected to `null`.
  - Process termination on drop (`kill_on_drop(true)`).
  - Bounded stream reads: standard output and standard error are each capped at 2 MiB during collection, aborting with an error if exceeded.
  - Strict 10-second process execution timeout.
- Scope: every external command in `services/inventory/` goes through `run_bounded`, or `run_bounded_with_env` where an environment variable is needed (Vagrant `ssh-config`); `grep '\.run('` over that directory finds no unbounded call. The bounds apply to `SystemRunner`; the trait defaults delegate to `run`, so test fakes are not bounded. Kubernetes detection and VMX/disk inspection use no external command (a 500 ms TCP probe and filesystem reads).
- VMware Fusion's `vmrun` binary is resolved via an explicit absolute path (`/Applications/VMware Fusion.app/Contents/Library/vmrun`) in addition to system `PATH` searches.

**Vagrant `ssh-config` address behavior.** The runner has no working-directory support, so `vagrant ssh-config <machine>` runs with `VAGRANT_CWD` set to the validated project directory. Address, port, user, and identity file are accepted only after `validate.rs` checks. During inventory a forwarded `127.0.0.1` address is not recorded as a machine IP. During "Set up cluster", if the machine has no usable address or only `127.0.0.1`, the ssh-config address and port are used for the profile host.

**Consequence under R1 (collision refusal).** A VirtualBox NAT-forwarded endpoint such as `127.0.0.1:<port>` (often the default 2222) can match an address and port already used by a different profile. `apply.rs` then refuses the write with an error naming the existing profile; it never silently updates or overwrites that profile. The user must resolve the conflict (for example, change the forwarded port) before the profile can be created.

### D4 — Path-segment validation on filesystem read paths

Local inspection of provider files (e.g., reading `.vagrant/machines/<name>/<provider>/id`, Lima instance configs under `~/.colima/_lima`, or VMware VMX configurations) involves joining dynamic names into filesystem paths.

Before any filesystem access, all path components (machine names, provider identifiers, project directories) must pass strict path-segment validation via `services/validate.rs` (`is_safe_path_segment`). Validation strictly forbids path separators (`/`, `\`), directory traversal elements (`..`), leading hyphens (`-`), NUL bytes, and newline characters.

### D5 — Environment identity and collision-safe profile matching

To prevent identity collisions when generating profiles from discovered environments:
- **Vagrant profile IDs** are deterministically derived as `vagrant-<slug(basename)>-<6 hex chars of sha256(canonical project path)>`. This ensures distinct projects sharing identical folder names (e.g. `/path/a/lab` vs `/path/b/lab`) produce distinct IDs.
- **Colima profile IDs** follow `colima-<slug(name)>`.
- When creating or refreshing a profile from an environment, matching against existing profiles is evaluated **by profile ID only**.
- If a different existing profile already uses the target host's IP address and port, the operation **refuses to proceed** with an explicit collision error, preventing silent overwrites of foreign profiles.

### D6 — Reconciliation, Kubernetes detection, and synthetic demo mode

- **Reconciliation:** VM observations from hypervisors and orchestrators are merged strictly by exact `runtime:runtime_id` (VirtualBox UUIDs match directly; VMware merges only when Vagrant's ID matches the VMX path exactly). Merging by VM name is prohibited. Stale Vagrant entries are reconciled: missing `.vagrant` metadata marks an entry as `stale`, and unmatched running entries become `stopped`.
- **Kubernetes detection:** API server presence is determined evidence-first: parsing local kubeconfig server hostnames followed by a non-blocking 500 ms TCP probe on port 6443. Reported states: `"kubeconfig server"`, `"API server reachable (6443)"`, or `"not detected"`.
- **Demo mode:** A synthetic inventory mode (presenting 3 Vagrant/VMware "Narwhal" control-plane and worker nodes) is retained in-app for demonstration and testing without requiring hypervisor installations or executing host processes.

### D7 — Typed-level profile merge, process-wide mutex, and store integrity guard

Creating or updating a profile from an observed environment:
- Loads the existing profile from `ProfileStore` and performs a typed-level merge: only `hosts[].address` and `hosts[].port` are updated, missing hosts are appended, and the `local_runtime` origin is attached. Custom user fields, trusted CAs, and kubeconfig configurations are preserved.
- All profile creations and updates are serialized behind a process-wide `tokio::sync::Mutex` held in Tauri managed state.
- **Store integrity guard:** `store::upsert_profile` drops invalid entries upon loading. If loading profiles detects that any invalid profiles were dropped from the store, `create_profile_from_environment` refuses to write and returns an error naming the dropped count, preventing unintended data loss.

### D8 — One-click "Set up cluster" auto-saves profile (superseding ADR-0005 D1 for this flow only)

[ADR-0005](0005-local-host-detection-prefills-profiles.md) D1 established that detected VM hosts are prefilled into `ProfileEditor` component state and nothing persists until the user explicitly clicks Save.

This decision **explicitly supersedes ADR-0005 D1 for the one-click "Set up cluster" flow only**:
- Clicking "Set up cluster" on an observed environment directly creates and persists a valid `Profile` via `store::upsert_profile`, selecting it immediately in the navigation view.
- It never replaces an existing profile; existing profiles with matching IDs are only refreshed on host address/port.
- The standard manual Profile Editor flow defined in ADR-0005 remains unchanged: manual edits and ad-hoc local VM detections in `ProfileEditor.tsx` still require the user to click Save.

### D9 — Standalone CLI dropped and external repository retired

- The standalone `infradeck` CLI binary from the InfraDeck repository is deliberately dropped. Including a CLI binary in ClusterDeck would inappropriately widen the public Rust library API of `src-tauri`. All inventory capabilities are exposed via Tauri IPC commands (`discover_inventory`, `create_profile_from_environment`).
- The standalone InfraDeck repository is archived/deleted. All out-of-band file-hacking handoffs (`profiles.yaml.infradeck.bak`, deep links) are eliminated in favor of direct in-app state management.

## Consequences

### Positive
- Users gain full visibility into local VM and hypervisor inventory directly alongside remote cluster profiles.
- One-click setup eliminates manual profile configuration for local Colima and Vagrant clusters.
- All process executions inherit ClusterDeck's audited `CommandRunner` with strict timeout and buffer limits.
- Path-segment validation and deterministic project hashing eliminate path traversal and identity collisions.
- Eliminates fragile external file modifications and IPC deep links between separate applications.

### Neutral / Trade-offs
- Rust codebase expands to include provider-specific adapters for VirtualBox, VMware Fusion, and Vagrant.
- `CommandRunner` requires a new `run_bounded` method with bounded buffer collection.
- Stored profile count must be checked before writing to avoid triggering `store::upsert_profile` data loss.

### Negative / Risks
- Hypervisor CLI outputs (especially VirtualBox and VMware Fusion) are unversioned and may change across host OS or hypervisor updates.
- VMware Fusion file inspection requires reading `.vmx` files on the host filesystem, requiring defensive async file operations.
