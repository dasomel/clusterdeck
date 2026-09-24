# ADR-0007: Local runtime lifecycle actions (Colima/Lima Start/Stop/Restart/Shell)

- Status: Accepted
- Date: 2026-09-23
- Issue: #14
- Amends: [ADR-0003](0003-colima-lima-local-runtime-provider.md)'s "Out of scope for this MVP" item,
  "Lifecycle actions (start/stop/restart) — ... Needs its own issue."

## Context

ADR-0003 explicitly deferred lifecycle actions on discovered local runtimes ("mutating and
long-running; the current command surface is fire-and-forget `Result<T, String>` with no
cancellation or progress streaming. Needs its own issue."). ADR-0005 subsequently recorded that the
as-built discovery feature diverged from ADR-0003's provider-trait design — it is three free
`async fn`s (`detect_colima`/`detect_lima`/`detect_vagrant`) in `services/local_runtime.rs`, behind a
single `detect_local_hosts` Tauri command, returning a flat `DiscoveredLocalHost` struct with a
`provider: String` field ("Colima" | "Lima" | "Vagrant"). The read-only discovery panel this ADR
builds on top of is `KubeconfigManager.tsx`'s "Local Runtime" section in Settings (issue #14 Phase 1,
commit `5e617d3`), not the sidebar `LOCAL` section ADR-0003 originally proposed and ADR-0005 confirmed
was never built.

Issue #14's Phase 2 checklist asks for: Start/Stop/Restart, opening a VM shell, copying runtime
info, opening the associated Docker/Kubernetes context, and confirmation on destructive operations.
This ADR is the "own issue" ADR-0003 deferred to, scoped by the product owner's decisions below.

## Decision

### D1 — Actions and exact CLI, scoped to Colima and Lima only

Vagrant is excluded from lifecycle actions (it has none in `DiscoveredLocalHost` today beyond
read-only fields, and issue #14's lifecycle checklist targets Colima/Lima). Confirmed against the
real CLIs (`colima 0.10.3`, `limactl 2.2.0`, macOS arm64):

| Action | Colima | Lima |
| --- | --- | --- |
| Start | `colima start --activate=false --profile <name>` | `limactl start <name>` |
| Stop | `colima stop --profile <name>` | `limactl stop <name>` |
| Restart | `colima stop --profile <name>` then `colima start --activate=false --profile <name>` | `limactl stop <name>` then `limactl start <name>` |
| Shell | `colima ssh --profile <name>` | `limactl shell <name>` |

**`--activate=false` (Colima start/restart only).** `colima start` defaults to
`--activate=true` ("set as active Docker/Kubernetes/Incus context on startup", confirmed via
`colima start --help` on 0.10.3). Real-UI testing during review caught this: starting `nqa-node2`
from the panel silently switched the user's **global** `docker context` from `desktop-linux` to
`colima-nqa-node2` (stdout: `Current context is now "colima-nqa-node2"`) — a direct violation of D3
and [ADR-0002](0002-kubeconfig-stays-isolated-from-user-kube-config.md)'s never-mutate-global-state
rule, and one the original real-binary evidence run for this ADR didn't catch because that run never
checked `docker context show`/`kubectl config current-context` before and after. Every Colima
`start` argv this module builds now passes `--activate=false` explicitly (a boolean flag, so it
must be one token, `--activate=false`, not two — pflag/cobra bool flags don't consume a following
bare argument as their value).

**Restart is stop-then-start for both providers, not either provider's native `restart`.**
`colima restart` has **no** `--activate` flag at all (confirmed via `colima restart --help` on
0.10.3), so it cannot be told to skip the same context switch — stop-then-start with
`--activate=false` on the start half is the only way to restart a Colima instance without that side
effect, so Colima restart was changed to use it. `limactl` does have a native `restart`, but Lima
restart is kept as stop-then-start too, so both providers share one restart shape (ADR-0007 D1's
original "deliberate simplicity trade-off, not a capability gap" reasoning, now also load-bearing
for Colima). Both providers' combined stop+start run under one `LIFECYCLE_TIMEOUT` deadline, not one
per call (worst case 10 minutes, not 20). `--profile` is always passed explicitly for Colima
(including the `default` profile) for uniform, predictable argv, unlike `detect_colima`'s existing
`ssh-config` call, which omits it for `default`.

**Lima has no equivalent flag or concept to guard against**, confirmed by grepping for "context" in
`limactl --help` and `limactl start --help` on 2.2.0 — no match in either. Lima's CLI has no
Docker-context-like mechanism at all; `limactl start`/`stop`/`shell` cannot mutate global Docker/kube
state, so no `--activate`-equivalent flag exists to pass. Net effect across both providers: this
module never runs `docker context use` or `kubectl config use-context`, directly or as a side effect
of a flag default — the "never mutate global state" half of D3 now holds for Start/Restart the same
way it already held for Shell/Open-in-context.

Provider dispatch is a new Rust enum, `LocalRuntimeProvider { Colima, Lima }`
(`services/local_runtime_lifecycle.rs`), parsed from the wire string via `FromStr` and rejected
outright if it is anything else (including `"vagrant"` or a differently-cased value) — never a free
string routed into argv. This is intentionally narrower than `DiscoveredLocalHost.provider: String`,
which still carries `"Vagrant"` for the read-only panel; the two are not unified because unifying them
would either let a Vagrant value reach lifecycle argv or force `services/local_runtime.rs`'s
discovery code (which predates this ADR) to be rewritten for no discovery-side benefit.

### D2 — Open VM shell reuses `open_with_system`'s error-handling shape, not its URL-scheme mechanism

`open_ssh_session` (`commands/connection.rs`) opens a Terminal session via `open ssh://<alias>`,
relying on macOS's URL-scheme handler for `ssh://`. There is no equivalent URL scheme for an
arbitrary shell command (`colima ssh --profile <name>` is not itself an SSH URL), so a new helper,
`process::open_terminal_with_command`, drives `Terminal.app` directly via
`osascript -e 'tell application "Terminal" to do script "<command>"'`. It mirrors
`open_with_system`'s call shape — goes through `CommandRunner`, returns `Result<(), String>` with the
child's stderr surfaced on failure — and lives next to it in `services/process.rs`. `osascript` is
already an approved sink in this codebase (`services/hosts_file.rs`'s admin-privileged write), so this
does not introduce a new class of external dependency, only a new script shape.

### D3 — Open in runtime context: host-side shell, additive-only environment

"Open in runtime context" opens a Terminal window on the **host** (not the VM, unlike D2's Shell
action) and, only for the parts that exist, runs:

```sh
export DOCKER_CONTEXT=<docker_context>
alias kubectl='kubectl --context <kube_context>'
```

It never runs `docker context use` or `kubectl config use-context` — nothing here mutates the user's
global Docker/kube state, consistent with [ADR-0002](0002-kubeconfig-stays-isolated-from-user-kube-config.md)'s
spirit of not silently rewriting configuration the user owns outside a ClusterDeck-marked block. If
neither context is available (or both fail validation — see Security), the command errors before
opening a Terminal window at all, rather than opening an empty one. D1's `--activate=false` fix
closes the only other place in this module that could have mutated the same global state (Colima's
own `start`/`restart` default), so this "never mutate global Docker/kube state" property now holds
for every action in this module, not just D3's.

### D4 — Copy runtime info is frontend-only

`KubeconfigManager.tsx`'s `copyRuntimeInfo` builds a plain-text summary (provider, name, status,
arch, cpu, memory, disk, runtime, address:port, docker/kube context) from the row already in React
state and writes it via `navigator.clipboard.writeText`. `identity_file` is excluded. No new Tauri
command — the data is already on the frontend from the last `detect_local_hosts` call, and clipboard
access needs no Rust-side privilege.

### D5 — Guards: confirmation, per-instance busy state, backend concurrency lock

Stop and Restart require the existing `ConfirmModal` (mutating/interrupting a possibly-in-use VM);
Start/Shell/Open-in-context/Copy do not. The Settings panel tracks a single `busyInstanceKey` and
disables that row's buttons while an action is in flight; the panel reloads (re-runs discovery) after
Start/Stop/Restart completes. On the backend, `LifecycleGuard` (`services/local_runtime_lifecycle.rs`,
a `Mutex<HashSet<(LocalRuntimeProvider, String)>>` held as Tauri managed state via `.manage(...)` in
`lib.rs`) rejects a second concurrent Start/Stop/Restart on the same `(provider, instance_name)` with
a clear error; the lock is released on `Drop`, including on early return or panic. The guard is scoped
to Start/Stop/Restart only — Shell and Open-in-context are instant, non-mutating, and do not hold or
check the lock.

### D6 — Security: strict instance-name validator, fresh-discovery re-check at every sink, context names validated-or-omitted

`services/validate.rs` gains two sink validators:

- `is_safe_local_runtime_instance_name`: anchors the entire charset (`^[A-Za-z0-9][A-Za-z0-9._-]*$`,
  max 64 chars) rather than only excluding a leading dash/newlines like `is_safe_ssh_identifier`
  does, because these names are provider-discovered (untrusted `colima`/`limactl` JSON output) and
  are embedded both in argv and in an AppleScript string literal — stricter than the existing SSH-sink
  validator is warranted for a second string-embedding context.
- `is_safe_shell_context_name`: for Docker/Kubernetes context names embedded in the D3 script.
  Context names are less constrained than instance names in practice (colons/slashes appear in real
  cluster ARNs), so this validator allows `[A-Za-z0-9._:/@-]`, but still excludes quotes, whitespace,
  and shell metacharacters. A context that fails this check is **omitted from the generated script**
  rather than quoted defensively, per the product owner's explicit instruction — the export/alias for
  that one context is simply dropped instead of attempting to safely quote an unbounded charset.

Every lifecycle/shell/context action in `services/local_runtime_lifecycle.rs` calls a shared
`find_fresh_instance` helper before doing anything else: it validates the instance name's charset,
then re-runs `local_runtime::detect_local_hosts` and requires the `(provider, instance_name)` pair to
still be present in that fresh listing, returning the freshly discovered row. Any docker/kube context
string used in D3 is read off that fresh row — never a caller-supplied value — closing off the
class of sink-trusts-unvalidated-input bug AGENTS.md calls out as a repeat offender in this codebase
(the SSH-config-injection and profile-id path-traversal findings ADR-0003/AGENTS.md both reference).

### D7 — Deferred

- **Progress streaming and user cancellation of an in-flight Start.** The current command surface
  is `Result<LifecycleActionResult, String>` with a 10-minute `tokio::time::timeout` (Start can take
  minutes: VM boot, container runtime init); there is no mid-flight cancellation or incremental
  progress. `kill_on_drop(true)` on the underlying `Command` (already set in `SystemRunner`) means a
  future timeout or app-shutdown drop does at least terminate the child process rather than leaking
  it.
- **Create/delete/reconfigure.** Unchanged from ADR-0003's scope; still out of bounds for this
  feature.
- **Vagrant lifecycle actions.** Not requested by issue #14's Phase 2 checklist for this
  increment; would need its own decision if wanted later (Vagrant's `up`/`halt`/`reload`/`ssh`
  equivalents are not currently wired into `LocalRuntimeProvider`).

## Security

Every string that reaches a privileged sink in this feature is accounted for:

- **Instance name → argv (`colima`/`limactl` subprocess) and → osascript string.** Validated by
  `is_safe_local_runtime_instance_name` in `find_fresh_instance` before any other work, and the value
  actually embedded is always the fresh-discovery row's `instance_name`, not a raw pass-through of
  whatever the frontend sent — the Tauri command signature does still accept an `instance_name:
  String` argument from the frontend, but it is used only to locate the matching fresh-discovery row,
  never embedded directly.
- **Docker/kube context → osascript string (D3).** Validated by `is_safe_shell_context_name`;
  read from the fresh-discovery row, not the frontend, and omitted (not quoted) on failure.
- **Provider → argv/routing.** Parsed via `LocalRuntimeProvider::FromStr`, which only accepts the
  literal strings `"colima"`/`"lima"`; anything else is rejected before any command runs.
- **osascript string escaping.** `process::open_terminal_with_command` escapes backslash and
  double-quote when embedding `command_line` into the `do script "..."` AppleScript string literal.
  This is defense-in-depth on top of, not a substitute for, the charset validators above — the
  validators already exclude the characters that would need escaping in practice.

## Consequences

Positive:

- Phase 2 of issue #14 is delivered without widening the product boundary further than ADR-0003
  already did (still "observe and act on a local runtime," never "administer its containers/pods").
- The fresh-discovery re-check pattern is reusable for any future lifecycle-style action on
  provider-discovered data.

Trade-offs:

- Every lifecycle/shell/context action pays the cost of a full `detect_local_hosts` re-run
  (Colima + Lima + Vagrant + `docker context ls`) before doing its own single CLI call, which is
  slower than trusting the frontend's last-fetched row would be. This was an explicit product-owner
  requirement (D6), not an oversight.
- `osascript`'s AppleScript string embedding is a less structured interface than passing argv
  directly to `CommandRunner`, unlike every other privileged sink in this codebase. The charset
  validators keep the embedded content to a known-safe subset, but this is a different shape of
  guarantee than "goes through `CommandRunner` with a `Vec<String>` argv," and is worth noting for
  anyone extending this Terminal-launch mechanism later.

## Risks

1. **CLI drift.** Argv shapes are pinned to `colima 0.10.3`/`limactl 2.2.0`, confirmed via
   `colima <cmd> --help`/`limactl <cmd> --help` at implementation time and exercised once against the
   real `colima` binary (see Evidence). A future CLI flag rename would surface as a lifecycle action
   failing with the CLI's own stderr, not a silent no-op.
2. **Terminal-launch mechanism is new to this codebase's privileged-sink surface.** Unlike every
   SSH/kubectl/git sink, which passes structured argv through `CommandRunner`, this feature embeds
   validated strings into an AppleScript string literal. If a future change relaxes
   `is_safe_local_runtime_instance_name` or `is_safe_shell_context_name`'s charset, the escaping in
   `open_terminal_with_command` must be re-reviewed for the newly allowed characters.
3. **"Never mutate global state" is a CLI-default property, not a structural guarantee, and the
   FakeRunner-only evidence gap that hid it once can hide a similar issue again.** The original
   `--activate=true` default was only caught by real-UI testing (running the actual app and checking
   `docker context show` before/after), not by this module's `FakeRunner` unit tests, which assert
   argv shape but have no opinion on what the real CLI *does* with that argv — the same category of
   gap AGENTS.md's SSH `BatchMode`/`accept-new` regression note describes. Any future CLI flag this
   module starts passing needs the same real-binary, context-observed check, not just an argv
   assertion.

## Evidence

- `cargo test --all-targets --all-features`: 182 passed (was 165 before this change), 0 failed, 3
  ignored (pre-existing real-binary tests unrelated to this feature) — covers exact argv per
  provider/action, validator boundary cases (normal/traversal/space/quote/semicolon/leading-dash),
  unknown-instance rejection, concurrent-op rejection, and that the generated Terminal script omits
  an invalid context rather than quoting it.
- `cargo fmt --check` and `cargo clippy --all-targets --all-features -- -D warnings`: clean.
- `pnpm build` (`tsc && vite build`): clean.
- Real-binary evidence (temporary `#[ignore]`d test, `SystemRunner`, removed before commit): on the
  local machine's `nqa-node2` Colima profile (Stopped beforehand) — `start_instance` succeeded and a
  fresh `detect_local_hosts` confirmed `Running`; `stop_instance` succeeded and a fresh
  `detect_local_hosts` confirmed `Stopped` again. The `default` profile (in active use) was never
  touched by this test.

## Remaining

1. Open a follow-up issue for D7's deferred items (progress/cancellation) if Start's UX proves
   insufficient in practice for slow VM boots.
2. Vagrant lifecycle actions, if wanted, need their own decision — this ADR does not block that,
   it simply does not build it.
