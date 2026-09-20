# ADR-0005: Local VM detection prefills Profile fields instead of an observed runtime view

- Status: Proposed
- Date: 2026-09-20
- Issue: #14
- Note: if accepted, this supersedes [ADR-0003](0003-colima-lima-local-runtime-provider.md) in part — see the "Relationship to ADR-0003" table below for exactly which parts.

## Context

[ADR-0003](0003-colima-lima-local-runtime-provider.md) (Accepted, 2026-09-17) decided a **read-only
observation** feature for issue #14: a `LocalRuntimeProvider` trait implemented by `ColimaProvider`
and `LimaProvider`, a vendor-neutral `LocalRuntime` domain type, two Tauri commands
(`list_local_runtimes`, `open_local_runtime_shell`), a sidebar `LOCAL` section, and an explicit rule
that discovered state is never persisted into a `Profile` and `Profile` gains no fields for it. It
also explicitly rejected fetching/normalizing Colima's kubeconfig and rejected ingesting
`colima ssh-config` into a ClusterDeck-owned file.

The code present in the working tree implements a different feature. There is no
`LocalRuntimeProvider` trait, no `ColimaProvider`/`LimaProvider`, no `LocalRuntime` type, no
`list_local_runtimes`/`open_local_runtime_shell` command, and no sidebar `LOCAL` section anywhere in
`src-tauri/src` or `src`. Instead, `services/local_runtime.rs` exposes one async function,
`detect_local_hosts`, behind one Tauri command of the same name (`commands/local_runtime.rs`,
registered in `lib.rs`), returning `Vec<DiscoveredLocalHost>`. Its only caller is
`src/components/ProfileEditor.tsx`, which uses the result to prefill an in-progress, user-authored
`Profile` form. ADR-0003 specified that discovered state is never persisted into a `Profile`; this
feature's purpose is for a user to turn discovered state into `Profile` content by saving it. A third
source, Vagrant, was also added; ADR-0003 only ever scoped Colima and Lima.

AGENTS.md requires product-boundary and design changes to be recorded as an ADR. This ADR records the
as-built design, states plainly where it agrees and disagrees with ADR-0003, and leaves the product
decision — which design ClusterDeck actually wants — to the project owner.

## Decision

### D1 — Detection is an on-demand profile-editor helper; nothing persists until Save

All of this feature's UI lives inside `ProfileEditor.tsx`. Clicking "Detect local VM" calls
`handleDetectLocal`, which invokes the `detectLocalHosts` wrapper in `src/api/tauri.ts` around the
`detect_local_hosts` Tauri command, and stores the result in local component state
(`detectedHosts`). That state renders as a dismissible, provider-grouped "Detected Local VMs" panel.
`applyDetectedHost` (one host) and `applyAllFromGroup` (every host in a group) copy fields onto the
in-progress form via ordinary React state setters (`setId`, `setName`, `setHosts`, and — through
`applyKubeconfigIfPresent` — `setUseKubeconfig`/`setKubeControlPlane`/`setKubeContext`/
`setKubeRemotePath`). None of this touches disk.

The user must still press Save. `handleSave` builds a `Profile` and calls `api.saveProfile`, which
invokes the `save_profile` command in `commands/profiles.rs` → `store::upsert_profile` in
`services/store.rs` → `validate::validate_profile` in `services/validate.rs`, then writes
`profiles.yaml`. `Profile`'s struct definition in `services/config.rs` is byte-for-byte unchanged
from before this feature — no field for discovered/detected state was added, so ADR-0003 D3's literal
claim ("`Profile` gains no fields") still holds. What no longer holds is the *purpose* behind that
claim: ADR-0003 treated discovered state and `Profile` as permanently separate; this feature's only
reason to exist is for a human to turn discovered state into `Profile` content by pressing Save.

### D2 — Three independent sources: Colima, Lima, and Vagrant (new)

`detect_local_hosts` in `services/local_runtime.rs` runs three detectors concurrently via
`tokio::join!` and concatenates colima → lima → vagrant, same union-with-no-dedup approach ADR-0003
chose for Colima/Lima.

- **Colima** (`detect_colima`): runs `colima list --json` (NDJSON), parses each line into a narrow
  `ColimaListRow { name, status, runtime }`, skipping lines that fail to deserialize. For each entry
  it then runs `colima ssh-config` (adding `--profile <name>` when the instance isn't `default`) and
  parses the output **in memory** with `parse_ssh_config_block` to fill
  `address`/`port`/`user`/`identity_file` over hardcoded defaults (`127.0.0.1`/22/`root`). A `runtime`
  string containing `k3s`/`kubernetes` sets `kube_context` to `colima`/`colima-<profile>` by naming
  convention (no lookup against a real kubeconfig) and `kube_remote_path` to
  `/etc/rancher/k3s/k3s.yaml`.
- **Lima** (`detect_lima`): runs `limactl list --json` (NDJSON) and reads
  `sshAddress`/`sshLocalPort`/`IdentityFile`/`config.user.name` straight off the JSON (`LimaListRow`)
  — no second CLI call, no ssh-config parsing, and `kube_context`/`kube_remote_path` are always
  `None`.
- **Vagrant** (`detect_vagrant`, not part of ADR-0003): runs `vagrant global-status` (a human-readable
  table, not JSON) and parses it with `parse_vagrant_global_status` into
  `VagrantEntry { id, name, provider, state, directory }`. `find_static_vagrant_info` statically reads
  `<dir>/configs/cluster.env`, `<dir>/cluster.env`, `<dir>/.env`, and the instance's `Vagrantfile`
  from local disk (no CLI) for a declared IP and a `k3s` marker (`extract_ip_from_vagrantfile`). For a
  running instance, `vagrant ssh-config <id>` is parsed the same way as Colima's; if still no identity
  file, well-known Vagrant key paths under `~/.vagrant.d/` and the per-machine
  `.vagrant/machines/.../private_key` are checked with `Path::exists`. `kube_context` is resolved
  only for machines whose name contains `master`/`control`, by matching the Vagrant project directory
  name against `kube_import::list_local_kube_contexts()` in `services/kube_import.rs` (which reads
  the user's own `$KUBECONFIG`/`~/.kube/config`), falling back to the bare project name if nothing
  matches.

### D3 — Vagrant's live SSH probe reuses the shared, hardened arg builder

When a Vagrant instance is `running` and its `address`/`user` (and `identity_file`, if present) pass
`validate::is_safe_ssh_identifier`, the code builds a throwaway `config::Host` and calls
`services::ssh::build_ssh_target_args` to run
`ip -4 -o addr show; test -f /etc/rancher/k3s/k3s.yaml && echo HAS_K3S || true` over `ssh`
(`runner.run("ssh", &probe_args)`). `build_ssh_target_args` in `services/ssh.rs` unconditionally
includes `-o BatchMode=yes` and `-o StrictHostKeyChecking=accept-new`, so this probe follows the same
trust-on-first-use rule AGENTS.md requires for every SSH invocation, using the existing shared helper
rather than a bespoke argv. The probe's stdout feeds `extract_private_network_ip`, which scores
candidate interfaces (global + private + non-dynamic scoring) to prefer a real LAN address over the
NAT/loopback address `vagrant ssh-config` reports, with the D2 static-file IP as a fallback if the
result still looks like NAT/loopback.

### D4 — Kubernetes contexts stay referenced-only; `kube_remote_path` hands off to the existing pipeline

No provider fetches, copies, or writes a kubeconfig anywhere in this feature — `kube_import.rs` only
reads the user's existing local kubeconfig (`std::fs::read_to_string`), and nothing in
`local_runtime.rs` writes to `~/.kube/config`. This is consistent with
[ADR-0002](0002-kubeconfig-stays-isolated-from-user-kube-config.md) and retains ADR-0003's "referenced,
never copied" rule. Separately, when a detected host carries a `kube_remote_path` (Colima: always
`/etc/rancher/k3s/k3s.yaml` when Kubernetes is enabled; Vagrant master nodes: k3s or
`/etc/kubernetes/admin.conf`), `applyKubeconfigIfPresent` prefills `kubeControlPlane` /
`kubeRemotePath` / `kubeContext`, which line up field-for-field with `Profile`'s existing
`KubeconfigSource { remote_path, control_plane, local_path, context }` type in `services/config.rs`.
If the user saves that profile, the host is wired into the same remote fetch/normalize pipeline every
other SSH-reachable profile already uses — no new kubeconfig transport was built for this feature.

### D5 — One Tauri command: `detect_local_hosts`

`commands/local_runtime.rs` (8 lines total) exposes exactly
`detect_local_hosts() -> Result<Vec<DiscoveredLocalHost>, String>`, registered once in `lib.rs`.
ADR-0003's two commands, `list_local_runtimes` and `open_local_runtime_shell`, do not exist anywhere
in the tree; there is no "open a shell into a local runtime" affordance at all.

### D6 — Provider seam: keep free functions per provider; defer the trait

Proposed position, not yet adopted anywhere else in the codebase: keep one `async fn` per provider
returning an owned `Vec<DiscoveredLocalHost>`, joined concurrently in `detect_local_hosts` exactly as
today, rather than introducing ADR-0003's `LocalRuntimeProvider` trait now. Reasoning:

- Nothing in the current call graph needs polymorphism — there is exactly one call site
  (`detect_local_hosts`), it always wants "all providers' results," and nothing chooses, mocks, or
  enables/disables a provider dynamically. A trait earns its cost when a call site needs to select or
  substitute an implementation at runtime; introducing one preemptively here would be an abstraction
  with a single, fixed set of callers.
- Do **not** adopt `available()` yet either. Listing already degrades gracefully — a missing or
  failing CLI makes that provider contribute zero rows, not an error — so a separate availability
  probe would only add an extra process spawn per provider on every detection, for a display-only
  signal nothing currently reads. That cost is not free: `vagrant`'s CLI is already the heaviest of
  the three to shell out to, and a dedicated availability check would spawn it an additional time on
  every click of "Detect local VM."
- If a future call site does need polymorphism (for example, a per-provider enable/disable setting,
  or a provider chosen/mocked dynamically in tests beyond today's `FakeRunner` fixtures), that is the
  trigger to introduce the trait — not this ADR.

## Relationship to ADR-0003

| ADR-0003 item | Status | Note |
| --- | --- | --- |
| D1: separate `services/local_runtime.rs`, not merged into `discovery.rs` | Retained | `discovery.rs` has zero references to local-runtime code; still pure network probing |
| D1: `LocalRuntimeProvider` trait + `ColimaProvider`/`LimaProvider` | Not implemented | three free `async fn`s instead (`detect_colima`/`detect_lima`/`detect_vagrant`); see D6 |
| D1: tolerant per-line NDJSON parse into a narrow DTO | Retained | `ColimaListRow`/`LimaListRow` still skip a bad line rather than failing the listing |
| D2: Docker context ↔ runtime correlated via `DockerEndpoint` socket path | Not implemented | no Docker context handling of any kind exists in `local_runtime.rs` |
| D2: Kubernetes context referenced only, never copied | Retained | Colima uses a naming convention, Vagrant uses `kube_import` lookup; neither fetches a kubeconfig |
| D2: missing `docker`/`kubectl` binary yields "unknown" | Not implemented | that concept doesn't exist as-built; a missing/failing provider CLI (`colima`/`limactl`/`vagrant` itself) instead makes the whole provider silently contribute zero rows |
| D3: `list_local_runtimes` / `open_local_runtime_shell` commands | Replaced | single `detect_local_hosts` command with a different, prefill-oriented contract |
| D3: `Profile` gains no fields | Retained | `services/config.rs` `Profile` struct is unchanged |
| D3: discovered state is never persisted into a `Profile` | Replaced | the feature's entire purpose is for a user to turn discovered state into `Profile` content via Save; see Open Questions |
| D3: sidebar `LOCAL` section | Not implemented | `src/components/` has no sidebar component of any kind |
| D3: untrusted provider names validated before reaching a privileged sink | Retained | both provider-derived name/id sinks (Colima profile name, Vagrant instance id) are guarded by `validate::is_safe_ssh_identifier`, skipping the row on failure; see Security |
| Rejected: extending `discovery.rs` | Retained | confirmed untouched |
| Rejected: a Colima-shaped domain model with Lima as a special case | Retained | `DiscoveredLocalHost` is one flat, vendor-neutral struct with a `provider: String` field for all three sources |
| Rejected: fetching/normalizing Colima's kubeconfig into `~/.clusterdeck/kubeconfigs/` | Retained | no kubeconfig fetch happens anywhere in this feature (D4) |
| Rejected: ingesting `colima ssh-config` into a ClusterDeck-owned `~/.clusterdeck/ssh/*.conf` | Retained, but a narrower new behavior was added | `colima ssh-config` (and, new, `vagrant ssh-config`) output *is* parsed, but only in memory, only for four prefill fields, and it is never written to any file ClusterDeck owns or claims — the specifically-rejected act of file ingestion/ownership still doesn't happen |
| Rejected: a background polling daemon | Retained | detection only runs on an explicit button click; no timer or interval exists |

## Security

Per ADR-0003 D3 and AGENTS.md, provider CLI output is untrusted input and must be validated via
`services/validate.rs` before it reaches a privileged sink (subprocess argv, a file ClusterDeck owns,
etc.). Tracing every string that originates in external CLI/file output through to a sink:

- **VALIDATED** — `detect_colima` in `services/local_runtime.rs` checks
  `validate::is_safe_ssh_identifier` on `entry.name` (the Colima instance/profile name, parsed from
  untrusted `colima list --json` NDJSON) immediately after parsing the row, before `name` can reach
  the `ssh_args` built for `colima ssh-config --profile <name>` or anywhere else. A row whose name
  fails the check is skipped with `continue` — that one row is dropped, not the whole listing,
  matching the function's existing tolerant per-line parsing.
- **VALIDATED** — `detect_vagrant` in `services/local_runtime.rs` checks
  `validate::is_safe_ssh_identifier` on `entry.id` (the Vagrant instance id, parsed from untrusted
  `vagrant global-status` table text) at the top of the per-entry loop, before `id` can reach
  `runner.run("vagrant", ["ssh-config", id])`. An entry whose id fails the check is skipped with
  `continue`, the same drop-one-row behavior as the Colima guard.
- **VALIDATED** — `detect_vagrant` also checks `validate::is_safe_ssh_identifier` on `address`,
  `user`, and `identity_file` (if present) before they are used; only if all pass does it build a
  throwaway `Host` and call `ssh::build_ssh_target_args` in `services/ssh.rs`, whose output is run
  over `ssh`. `entry.name` also flows into that same throwaway `Host.name`, but
  `build_ssh_target_args` never reads `Host.name` into argv (only address/port/user/identity_file), so
  it was never itself a sink.
- **VALIDATED (save-time backstop)** — any field a user keeps and saves (host name, address, user,
  identity_file) passes through `store::upsert_profile` in `services/store.rs` →
  `validate::validate_profile` in `services/validate.rs` (itself calling
  `is_safe_ssh_identifier`/`is_safe_profile_id`) before `profiles.yaml` is written. This is in
  addition to, not a replacement for, the detection-time guards above.

Net: all four traced sinks are now validated. The two provider-derived identifiers (Colima profile
name, Vagrant instance id) are checked immediately after they are parsed, before either can reach any
argv, closing the gap this ADR originally reported against ADR-0003 D3 and AGENTS.md's
validate-at-every-sink rule.

## Open questions for the owner

1. Is the ADR-0003 read-only sidebar observation feature (`LOCAL` section, `list_local_runtimes` /
   `open_local_runtime_shell`, no `Profile` interaction at all) still wanted alongside this prefill
   workflow, or has it been superseded/abandoned by product direction?
2. Should Vagrant be in the product boundary at all? ADR-0003 scoped only Colima/Lima; Vagrant support
   plus a live SSH probe during detection is a further widening AGENTS.md's boundary rule hasn't
   explicitly blessed.
3. Should `LocalRuntimeProvider` be adopted now anyway — for example because a third provider already
   exists and ssh-config parsing is now duplicated between the Colima and Vagrant paths — even without
   a call site that needs runtime polymorphism (D6)?
4. Is it acceptable for a live SSH probe (network round-trip, `known_hosts` trust-on-first-use write)
   to run automatically as a side effect of clicking "Detect local VM," before the user has decided to
   adopt that host into a profile at all?

## Consequences

Positive:

- Fast start for the common case (a freshly recreated local VM) without hand-typing
  address/port/user/identity_file.
- Because nothing is written until Save, and Save always re-validates via `validate_profile`, a
  malformed or hostile detected value cannot silently reach `profiles.yaml`, `~/.ssh/config`, or
  `/etc/hosts` — the pre-existing validation gate still applies to whatever a user chooses to keep.
- Vagrant reuses the same UX as Colima/Lima (one button, one grouped result list) instead of a new
  mental model per tool.
- The frontend reuses the existing Patch Panel design tokens (`local-detect-*` classes in
  `src/styles.css` use `var(--border-strong)`, `var(--bg-sunken)`, `var(--text-secondary)`,
  `var(--text-primary)`) — no new design system, per AGENTS.md.

Trade-offs:

- CLI/text-output drift now spans three external tools (`colima`, `limactl`, `vagrant`) instead of
  two. `vagrant global-status`'s output is a human-readable table, not JSON/NDJSON like Colima/Lima,
  which is inherently more fragile to parse and more likely to drift across Vagrant versions.
- Static Vagrantfile/`.env` parsing (`extract_ip_from_vagrantfile`, `find_static_vagrant_info`) is
  pattern-matching over arbitrary Ruby/env text, not a Vagrantfile evaluator; it will silently miss or
  mis-extract IPs for Vagrantfiles that don't follow the conventions it looks for.
- Clicking "Detect local VM" now costs a live network round-trip (SSH connect, `ConnectTimeout=5`) per
  running Vagrant machine, not just local CLI/file reads — a cost ADR-0003's evidence-gathering never
  anticipated, since it scoped Colima/Lima only and neither does a live probe.

## Risks

1. Vagrant table parsing and the static Vagrantfile heuristics are the most drift-prone code in the
   feature. A `vagrant` CLI format change degrades silently to an empty list (detectable — nothing
   shows up); a Vagrantfile convention outside the matched patterns degrades to a wrong or missing IP
   with no error surfaced to the user (not detectable from the UI alone).
2. The live SSH probe's trust-on-first-use write to `known_hosts` happens as a side effect of a
   "detect" button click, before the user has decided to adopt that host into any profile — a
   different consent shape than every other SSH-touching action in the app today, which only runs
   after a host is already part of a saved profile.

## Remaining

If this ADR is accepted as-is (as-built design kept, ADR-0003's unimplemented pieces formally
dropped), follow-up work in order:

1. Resolve Open Questions 1-2 with the owner; if the ADR-0003 sidebar observation feature is still
   wanted, it needs its own follow-up issue since none of it exists today.
2. Decide Open Question 3 (provider trait) only if/when a concrete polymorphic call site appears.
