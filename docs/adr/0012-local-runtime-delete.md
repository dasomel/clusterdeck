# ADR-0012: Local runtime delete (Colima/Lima/Vagrant)

- Status: Accepted
- Date: 2026-10-31
- Amends: [ADR-0007](0007-local-runtime-lifecycle-actions.md) D7 ("Create/delete/reconfigure ... out of bounds")

## Context

ADR-0007 deferred delete. Users who recreate VMs frequently (ClusterDeck's core scenario) need to
remove discovered instances without leaving the app. Delete is irreversible, so the design is
driven by two concerns: never let an unvalidated string reach argv, and make accidental deletion
hard.

## Decision

### D1 — Exact CLI

Verified against `colima 0.10.3` (`delete [profile] -f/--force -d/--data`), `limactl 2.2.1`
(`delete INSTANCE -f/--force`), Vagrant 2.4.9 (`destroy [name|id] -f/--force`).

| Provider | argv |
| --- | --- |
| Colima | `colima delete --force --profile <name>` |
| Lima | `limactl delete --force <name>` |
| Vagrant | `vagrant destroy -f <machine name>` with env `VAGRANT_CWD=<canonical project dir>` (D12) |

`--force` skips the CLIs' own interactive prompt (the app has already confirmed) and also stops a
running instance first.

### D2 — `--data` is omitted for Colima

`colima delete --data` additionally removes the container-runtime data disk. The default preserves
it: least-destructive choice, and the user can still remove it through the CLI.

### D3 — Separate `DeletableRuntimeProvider` enum

`LocalRuntimeProvider` is persisted in `profiles.yaml` (`LocalRuntimeSource`) and gates
start/stop/shell, none of which Vagrant supports. Widening it would leak "vagrant" into both. A
new enum `{Colima, Lima, Vagrant}` (exact lowercase wire strings) is used for delete. The
`LifecycleGuard` is keyed by a small `InstanceKey { provider display name, name }`
(`LocalRuntimeProvider::key` / `DeletableRuntimeProvider::key`), so no conversion between the two
enums is needed and the same provider/name pair still locks identically.

### D4 — Vagrant identity (superseded by D11/D12)

Originally the caller sent `basename(project)/machine`, which was re-resolved against fresh
`global-status` and passed to `vagrant destroy <id>`. A security review showed three wrong-target
paths: Vagrant resolves an id by *prefix* (two ids sharing a prefix hit the first VM), a basename
is not unique across projects (`/a/proj/master` vs `/b/proj/master`), and nothing bound the
confirmed target to the executed one. A Vagrant machine is now identified by `(project directory,
machine name)` and deleted by D11/D12; the id is only recorded in the plan.

### D5 — Two-step confirmation

Step 1 explains the consequence; step 2 requires typing the instance name exactly
(`ConfirmModal.confirmText`). Cancel at either step aborts.

### D6 — Guard sharing

`delete_local_runtime` acquires the same `LifecycleGuard` `InstanceKey` (provider display name +
name), so a delete and a start/stop/restart on the same instance exclude each other.

### D7 — Group delete (Infrastructure screen)

`delete_environment_group` removes every machine of one `Machine.environment` group. The caller
sends only the environment string; it is compared against a **fresh live listing** and never
reaches argv. Zero matches is an error.

- **Narrow fresh discovery.** `vagrant global-status` is fetched once (`fetch_vagrant_entries`,
  bounded by `DISCOVERY_TIMEOUT`) and reused for membership and id resolution. Membership is built
  by `inventory::discover_delete_candidates` from those entries plus `colima list` only, with no
  VirtualBox/VMware probes, detail reads or k8s detection. A standalone VirtualBox/VMware VM never
  shares a Vagrant project directory as its `environment`, so the member set is unchanged. If
  `global-status` fails, its error is appended to the "environment not found" message.
- **Backend-computed deletability (`Machine.delete_kind`).** `inventory::annotate_delete_kinds` sets
  `delete_kind` (`"destroy" | "prune" | null`, serde default `null`) once in Rust: Vagrant machines
  get `destroy`, or `prune` when `vagrant_delete_kind` finds the project directory gone; Colima
  profiles with a safe name get `destroy`; everything else `null`. `select_targets` and the UI
  (`EnvironmentGroup` delete button, `InfrastructureScreen` "only removes stale entries" text) read
  this one field; the UI no longer infers anything from `orchestrator`/`runtime_id`.
- **All-or-nothing eligibility.** One member with `delete_kind == null` (standalone
  VirtualBox/VMware, ...) refuses the whole group, so the UI never offers a partial delete.
- **Ids come from the CLI, not the inventory row (D12: now only recorded in the plan, never argv).** Vagrant ids are resolved from the fetched
  `global-status` entries by `(directory, machine name)` (exactly one match,
  `validate::is_safe_vagrant_id`), because `Machine.id` keeps the first provider's id when a Vagrant
  machine merges with a standalone VirtualBox row. Colima names must pass
  `is_safe_local_runtime_instance_name`.
- **Lock everything first.** `LifecycleGuard` is acquired for every member (same keys as single
  delete: `Colima/name`, `Vagrant/basename(dir)/machine`) before any command runs; if any acquire
  fails nothing runs and the locks drop.
- **Concurrent Colima, sequential Vagrant, continue on failure.** Colima profile deletes run
  concurrently in chunks of 4 (`join_all`; locks are already held), then Vagrant destroys one at a
  time, then one `vagrant global-status --prune` (best-effort for live machines). Each machine
  yields `{name, success, message}`; results keep the machine order regardless of completion order.
  A destroyed VM cannot be rolled back, so failures are reported, not aborted on.
- **Stale Vagrant entries are pruned, not destroyed - from either button.** A real report:
  destroying a group whose project directory was gone failed every machine ("The working directory
  for Vagrant doesn't exist!") although the end state was success. The single rule lives in
  `vagrant_delete_kind(directory)` (directory missing or not a directory => prune) and is used by
  group delete, single delete and `delete_kind`. The previous extra condition "inventory has no
  `runtime_id`" was dropped: `runtime_id` is itself derived from the same directory check, so the
  directory test alone is equivalent apart from a transient race. A stale machine gets no
  `destroy`; the prune (`prune_vagrant_cache`) is its delete: success ("stale entry removed ...")
  only if prune succeeded, otherwise failed with prune's stderr tail. Single delete of a stale
  Vagrant machine therefore runs only `global-status --prune`, which (like the group prune) clears
  every stale cache entry, not just that one (D13 makes the confirm dialog list them all). Because the environment string is the project
  directory, a group is all-stale or all-live in practice; mixed handling is unit-tested on the
  executor.
- **Timeouts.** `detect_vagrant` (Discovery) now also goes through `fetch_vagrant_entries`, so its
  `vagrant global-status` is bounded by `DISCOVERY_TIMEOUT` (30s) instead of being able to hang;
  failure or timeout still means "no Vagrant entries". Its id check is the same
  `validate::is_safe_vagrant_id` as delete (ASCII alphanumeric, <=64; real Vagrant ids are short
  hex).
- Implemented in `services/local_runtime_group_delete.rs` (the lifecycle module was ~1100 lines).

### D8 — Two-step confirmation for groups

Same two-step flow as D5; step 2 requires typing the environment string (a long project path for
Vagrant). Group names are not Colima-style short names, so the dialog's `confirmText` UI is D9.

### D9 — Copyable confirm text

`ConfirmModal` shows a short instruction, then the exact `confirmText` in its own mono `<code>`
block (`user-select: all`, `word-break: break-all`) with a Copy button
(`navigator.clipboard.writeText`, brief "Copied" state), then the input. Copy/paste is never
blocked anywhere; Enter still confirms only on an exact match.


### D10 — Per-machine Delete button (Infrastructure inventory rows)

Each concrete row of the "Virtual Machine Inventory by Environment" table shows a trash button
next to its state pill whenever the backend `Machine.delete_kind` is non-null (Vagrant `destroy` or
`prune`, Colima `destroy`); standalone VirtualBox/VMware VMs and demo data have `null` and get no
button. Collapsed "x N identical machines" summary rows get none (expand to delete one).

- **Same backend path.** The button calls `plan_local_runtime_delete` with `colima` + profile name
  (`runtime_id`) or `vagrant` + machine name + project directory (`Machine.environment`), then
  `delete_local_runtime` with the confirmed plan (D11). The frontend no longer recomputes Vagrant
  names (`src/lib/machineDelete.ts` removed). Fresh re-check (D11/D6),
  `LifecycleGuard` key, validators and CommandRunner-only apply unchanged; nothing new reaches argv.
- **Running machines are deletable.** `--force` stops them first (D1), so the button is not
  disabled for Running; the explain step says so. A stale Vagrant row (`prune`) only clears the
  cache entry, and the button tooltip reads "Remove stale entry".
- **Same two-step typed confirm** (D5): type the machine name. The button is disabled while that
  machine, or its whole group, has a delete in flight. On result the modal closes, a status is
  pushed and the inventory refreshes without being awaited.
- **ClusterDeck profiles are not removed** (unchanged); a deleted VM leaves its profile in place.
- Lima instances are not part of the inventory table (no Lima inventory provider); they remain
  deletable from the "Local runtimes" panel.

### D11 — Confirmed-plan binding

Delete is two backend calls. `plan_local_runtime_delete(provider, name, project_dir?)` and
`plan_environment_delete(environment)` are read-only and return a `DeletePlan`: sorted `targets`
(`provider`, `name` = Colima/Lima instance or Vagrant machine name, `project_dir` = canonical
absolute dir for a destroy / as-reported dir for a stale entry, `vagrant_id` as reported,
`delete_kind`) plus `stale_entries` (D13). The confirm modal renders it. `delete_local_runtime(plan)`
and `delete_environment_group(environment, plan)` lock every confirmed target, re-derive a fresh
plan from live CLI output and require exact equality; any difference (member added/removed, project
replaced or moved, id changed, stale to live flip or back, extra stale entry) returns "State changed
since you confirmed — review again" with nothing run. The frontend plan is only compared, never
used for argv/env, and the sink re-validates names and directories (`delete_command`). Fixes the
review's wrong-target findings: replaced/moved project (identity is directory + machine name + id,
not a basename), group gaining a member after confirm, and a confirmed "stale entry" becoming a real
`destroy` when the directory reappears (`delete_kind` is part of the plan).
`services/local_runtime_delete_plan.rs` holds the plan, `local_runtime_group_delete.rs` the group
plan and executor.

### D12 — Vagrant destroy by project directory

`vagrant destroy -f <machine name>` runs with `VAGRANT_CWD=<canonical project dir>` through
`CommandRunner::run_bounded_with_env` (the runner has no cwd support). The directory must be
absolute (`validate::is_safe_vagrant_project_dir`), exist, be canonicalized and contain a
`Vagrantfile`; the machine name passes `is_safe_local_runtime_instance_name`. The id never reaches
argv, so Vagrant's id-prefix resolution (measured on 2.4.9: two ids sharing a prefix resolve to the
first VM) cannot select another machine. The `LocalRuntimePanel` no longer offers Vagrant delete
(`DiscoveredLocalHost` has no project directory); use the inventory row or group.

### D13 — Prune scope is global and shown as such

`vagrant global-status --prune` removes every invalid entry, not one. A stale plan therefore lists
all entries whose project directory is missing (`stale_entries`, same rule as `vagrant_delete_kind`)
and the modal says "This removes every stale Vagrant cache entry, not only the one above". Equality
covers that list, so an entry going stale during the dialog aborts. Limit: staleness is judged by the
directory test only; Vagrant may also drop entries it considers invalid for other reasons, which the
plan cannot predict.

Colima/Lima have no stable id: a profile deleted and recreated under the same name during the dialog
is the same target (same name) and is deleted. Accepted: the user confirmed that name.

### D14 — Name-prefix guard against Vagrant's global id resolution

Measured by Codex on Vagrant 2.4.9: when the machine name (e.g. `a`) is a prefix of *another*
machine's global index id, the resolver picks that id ahead of `VAGRANT_CWD`. Planning a destroy
(`vagrant_target`, so also the fresh re-plan right before execute) reads the full ids from
Vagrant's machine index (`$VAGRANT_HOME` or `~/.vagrant.d`, `data/machine-index/index`, `machines`
keys; read-only) and refuses with a fixed error, nothing run, if the machine name is a
case-insensitive prefix of any id other than the target's own. An unreadable, oversized or
unparsable index also refuses (fail closed). Code: `services/local_runtime_vagrant_identity.rs`.

### D15 — Plan bound to the machine's local identity files

`DeleteTarget.machine_identity` = `index_uuid:provider:provider_id`, read from
`<project>/.vagrant/machines/<machine>/<provider>/{index_uuid,id}` and included in plan equality,
so a project or `.vagrant` replaced at the same path after the confirm aborts with "State changed".
Destroy is refused unless (D16 tightened the provider rule): no entry under
`.vagrant/machines/<machine>/` is a symlink, exactly one provider directory carries an `id` file
(Vagrant 2.4.9 treats any such directory as active, with or without `index_uuid`; leftover dirs
without an `id` are ignored), and that provider has a usable `index_uuid`; both files are usable (the
provider id may be a path such as VMware's `.vmx`; it is only compared, never argv); neither
canonicalizes outside the machine directory (no symlink escape); `index_uuid` is a key of the
index, is prefixed by the id `global-status` listed, and the index entry's `name` and
`local_data_path` (when present) match this machine and this project's `.vagrant`. Prune targets
have no identity (no destroy runs).

### D16 — Re-verify every destroy and the prune immediately before it runs

A group destroy takes minutes; validating once at the start left that whole window open. `execute`
now re-derives each Vagrant destroy target from its own planned fields (`reverify_target`: canonical
project dir, fresh identity files, machine index, name-prefix rule via `vagrant_target`) right
before its `vagrant destroy`, and requires exact equality with the planned target. Before the single
prune it re-lists `global-status` and requires the stale set to equal the planned `stale_entries`.
The first mismatch stops everything after it: that target reports `not deleted: <reason>`, the later
targets (and the prune) report `skipped: ...`, nothing else runs; targets already destroyed keep
their success result. A destroy command that itself fails does not stop the group (unchanged).
Colima/Lima have no local identity to re-check and are unchanged.

### D17 — Fully controlled Vagrant environment for destroy

Inherited Vagrant variables could redirect which state directory or index the child uses. The destroy
env (`destroy_env`) is exactly `VAGRANT_CWD=<canonical project>`, `VAGRANT_DOTFILE_PATH=<project>/.vagrant`,
`VAGRANT_VAGRANTFILE=Vagrantfile`, `VAGRANT_HOME=<home>`; `<home>` is the same `vagrant_home()` the
machine index is read from, and `<project>/.vagrant` is where the identity files are read, so
validation and execution use identical paths. `SystemRunner::run_bounded_with_env` removes every
other inherited `VAGRANT_*` variable whenever the caller passes any `VAGRANT_*` variable (this also
drops harmless ones such as `VAGRANT_DEFAULT_PROVIDER`; an existing machine's provider comes from its
state, not the environment). A non-absolute `VAGRANT_HOME` is refused.

Residual risks: (1) the window between the last check and the `vagrant destroy` process start is now
per target (milliseconds), under the lifecycle lock but not a Vagrant lock; it is not zero.
(2) Only the name-prefix resolver case is covered; behaviour of future Vagrant resolvers is
unverified. (3) A provider VM replaced under identical `index_uuid` and `id` is indistinguishable.
(4) Vagrant reads `Vagrantfile` content and plugins at destroy time; their content is not bound.
(5) Real-environment check was plan-only on the narwhal project; no destroy was executed. The D16/D17
rules were checked only against that project's on-disk layout (read-only `ls`), not re-run through
the code against the real index.

## Out of scope

- Removing ClusterDeck profiles that reference the deleted instance.
- Leftover Docker contexts / kube contexts created by Colima/Lima.
- Progress streaming and mid-flight cancel (same limits as ADR-0007 D7; bounded by
  `LIFECYCLE_TIMEOUT`).

## Evidence

- `cargo test --all-targets --all-features`: 559 passed, 0 failed, 3 ignored. New tests cover exact
  argv for Colima/Lima/Vagrant delete, unknown provider strings rejected ("Vagrant", "docker"),
  Vagrant not-found / ambiguous / non-alphanumeric id / implausible name, Colima absent from
  fresh discovery, and delete blocked while a stop holds the same guard key.
- `cargo fmt --check`, `cargo clippy --all-targets --all-features -- -D warnings`, `pnpm build`:
  clean (`make verify` exit 0).
- Real-binary run (temporary `#[ignore]`d test, `SystemRunner`, removed before commit), limactl
  2.2.1: a throwaway `cd-del-probe` Lima instance created with `limactl create` was deleted via
  `delete_instance` (`success: true`; `limactl list` no longer shows it, `~/.lima/cd-del-probe`
  gone); a second call was rejected as "not found in current discovery listing". A Vagrant call
  for an unknown `project/machine` was rejected before any `destroy` ran.
- Colima/Vagrant argv parsing only, against nonexistent targets: `colima delete --force --profile
  <missing>` (0.10.3) exits 0 with "Ignoring non-existent instance"; `vagrant destroy -f <unknown id>`
  (2.4.9) reports the machine not found — neither rejected a flag. No existing Colima profile or
  Vagrant machine was deleted.
- Group delete: `cargo test --all-targets --all-features` now 570 passed, 0 failed, 3 ignored (+11 in
  `local_runtime_group_delete`): Vagrant group -> two `destroy -f <id>` + one prune, Colima group ->
  one `delete --force --profile`, unknown environment, unsafe id, guard held by one member (nothing
  runs, locks released), mixed-runtime group refused, unsafe Colima name refused, plus stale-group tests (all-stale: zero destroys + one prune; mixed executor plan: destroy only live + one prune; failed prune fails stale machines; existing dir with no id stays on destroy). FakeRunner only;
  no real destroy/prune was run against real Colima/Vagrant.
- Follow-up round (delete_kind, shared stale rule, concurrent Colima, narrow discovery, InstanceKey):
  `make verify` exit 0, `cargo test` 576 passed, 0 failed, 3 ignored. New FakeRunner tests: single
  stale Vagrant delete -> prune only, Colima plan completes out of order but results keep plan
  order, group delete lists `global-status` once and touches only vagrant/colima, start/delete probe
  only their own provider, `delete_kind` rule, vagrant validators. Not run against real binaries.
- Not verified: the two-step modals, the copy button and the group delete in the running Tauri app.
- Plan-binding round (D11-D13): `cargo test --all-targets --all-features` 741 passed, 0 failed, 3
  ignored. FakeRunner tests assert `destroy -f <machine>` + `VAGRANT_CWD` (and no id in argv, ids with a
  shared prefix), id replaced / project moved / member added or removed / stale-to-live flip / extra
  stale entry all abort with no destroy or prune, prune plan lists every stale entry, tampered plan
  cannot redirect, relative/missing/no-Vagrantfile dirs rejected, Colima same-name recreate proceeds.
  Not run against real binaries.
- Identity round (D14/D15): `make verify` exit 0, `cargo test` 752 passed, 0 failed, 3 ignored. New
  tests (temp machine index): name prefixing another id refused (case-insensitive, also appearing
  after confirm), unreadable/corrupt/no-`machines` index refused, replaced `.vagrant` (other
  `index_uuid` or `id`) aborts, `index_uuid` unknown/mismatching listed id refused, index entry for
  another project/name refused, missing `id` / two provider states refused, symlinked identity file
  refused, `.vmx`-path provider id accepted. Real, plan-only (temporary `#[ignore]`d test, removed):
  narwhal `master-1` planned OK against the real index and `.vagrant` (index_uuid
  `f490d4c0e387...`, provider `vmware_desktop`; an empty leftover `virtualbox` dir is ignored);
  `worker-1` refused as not in the `global-status` cache. No destroy executed.
- Wrong-target round (D16/D17): `make verify` exit 0, `cargo test` 761 passed, 0 failed, 3 ignored.
  New tests: symlinked provider entry (active or not) refused, active provider without `index_uuid`
  refused, `id`-only second provider ambiguous, id-less leftover dirs ignored, destroy env is exactly
  the four pinned variables and `VAGRANT_HOME` holds the index that was read, `SystemRunner` drops
  inherited `VAGRANT_*` (real `sh`), identity swapped or name-prefix id appearing after the first
  destroy stops the rest (destroyed / not deleted / skipped), stale set growing before prune skips
  the prune. Not run against real Vagrant.
