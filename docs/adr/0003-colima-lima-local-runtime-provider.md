# ADR-0003: Colima/Lima local runtimes are observed through a read-only provider service

- Status: Accepted
- Date: 2026-09-17
- Issue: #14
- Note: the as-built implementation diverges from this decision; see [ADR-0005](0005-local-host-detection-prefills-profiles.md) (Proposed).
- Note: the "Lifecycle actions (start/stop/restart)" item below, deferred to "its own issue", is now decided by [ADR-0007](0007-local-runtime-lifecycle-actions.md) (Accepted).

## Context

`AGENTS.md` fixes the product boundary at `Discovery → SSH Bootstrap → SSH/ProxyJump →
kubeconfig Fetch → Normalize → Verify`, and forbids turning ClusterDeck into a general
Kubernetes administration console "unless the product direction is explicitly changed through
an Architecture Decision Record". Issue #14 asks for exactly that change: local Colima/Lima
VMs should appear next to remote SSH/kubeconfig environments so a user sees one topology from
the local Mac runtime out to remote clusters.

This ADR is that explicit decision. It widens the boundary by exactly one axis — ClusterDeck
may *observe* local runtimes as environment sources — and does not widen it into container or
Kubernetes resource administration, which mature tools (ColimaUI, Portainer, Headlamp) already
cover.

### Observed CLI behavior

Design-time evidence, macOS arm64, `colima 0.10.3` / `limactl 2.2.0`:

- `colima list --json` and `limactl list --json` emit **NDJSON** (one JSON object per line),
  not a JSON array.
- `colima list --json` works with every instance stopped, and carries
  `name/status/arch/cpus/memory/disk/runtime`; `runtime` is `docker+k3s` when Kubernetes is
  enabled. Memory and disk are bytes.
- `colima status --json` exits fatal (`colima is not running`) for a stopped instance, so it
  cannot be the listing source.
- `limactl list --json` embeds a large per-instance `config` blob: image URLs, provisioning
  scripts, absolute host paths and SSH key paths.
- Colima's Lima instances live under a separate `LIMA_HOME` (`~/.colima/_lima`) and therefore
  **do not** appear in `limactl list`. The two listings are disjoint.
- `docker context ls --format json` is NDJSON; a Colima context's `DockerEndpoint` is
  `~/.colima/<profile>/docker.sock`. The context *name* is `colima` for the default profile
  and `colima-<profile>` otherwise — a naming convention, not a structural link.
- Colima writes Kubernetes contexts into the user's `~/.kube/config` under the same
  `colima` / `colima-<profile>` names.
- `colima ssh-config` prints a Lima-generated block declaring `StrictHostKeyChecking no` and
  `UserKnownHostsFile /dev/null`, and states that modifications are lost on restart.

## Decision

### D1 — A new read-only service `services/local_runtime.rs` behind a provider trait

Local runtime discovery does **not** extend `services/discovery.rs`. That module is pure
network probing (CIDR expansion plus TCP connect) with no process execution; local runtime
discovery is CLI shell-out plus JSON parsing. Keeping them separate preserves
`discovery.rs`'s dependency-free shape and keeps the new service unit-testable with
`FakeRunner`.

```rust
#[async_trait]
pub trait LocalRuntimeProvider: Send + Sync {
    fn id(&self) -> &'static str;                  // "colima" | "lima"
    async fn available(&self, runner: &dyn CommandRunner) -> bool;
    async fn list(&self, runner: &dyn CommandRunner) -> Result<Vec<LocalRuntime>, String>;
}
```

`ColimaProvider` and `LimaProvider` implement it; the core domain type `LocalRuntime` names no
vendor. All execution goes through `CommandRunner` per `AGENTS.md`.

Parsing rules that follow from the observed behavior:

- Parse NDJSON **line by line**, and skip a line that fails to deserialize rather than failing
  the whole listing. One future field-format change must degrade a single row, not the view.
- Deserialize into a **narrow DTO**. Serde ignores unknown fields by default, so `limactl`'s
  `config` blob never crosses the Tauri boundary. This is an information-disclosure control as
  much as a size one: it keeps host paths and key locations out of React, per `docs/SECURITY.md`
  and the Rust/native boundary rule.
- `colima list --json` is the authoritative listing source. `colima status --json` is optional
  enrichment, attempted only for instances already reported `Running`, and its failure yields
  "no enrichment", never a listing error.
- Union the two providers' output with no dedup logic, because the listings are disjoint. Do
  **not** point `LIMA_HOME` at `~/.colima/_lima` to enumerate Colima's instances through
  `limactl` — that would double-list them and bypass Colima's own view of its profiles.

### D2 — Correlation uses structural keys, and Kubernetes contexts are referenced, never copied

- Docker context ↔ runtime is correlated on the `DockerEndpoint` socket path
  (`~/.colima/<profile>/docker.sock`), which is derived from the instance directory — not on
  the `colima-<profile>` name convention, which can drift.
- Kubernetes context ↔ runtime is stored as a **reference string only**, read through the
  existing read-only `services/kube_import.rs`. A Colima cluster's kubeconfig is already
  user-owned and locally reachable, so the remote fetch/normalize/store pipeline buys nothing
  and would duplicate an entry the user already has. ClusterDeck writes nothing to
  `~/.kube/config`, consistent with [ADR-0002](0002-kubeconfig-stays-isolated-from-user-kube-config.md).
- A missing `docker` or `kubectl` binary yields "unknown", not an error.

### D3 — A minimal additive Tauri surface; `Profile` is not extended

Two commands:

| Command | Shape |
| --- | --- |
| `list_local_runtimes` | `() -> Result<Vec<LocalRuntime>, String>` — aggregated, read-only |
| `open_local_runtime_shell` | `(instance: String) -> Result<(), String>` — reuses the existing `open_ssh_session` Terminal-launch pattern |

A local runtime is **discovered state, not user-authored config**, so it is not persisted into
`profiles.yaml` and `Profile` gains no fields. Persisting it would create a stale-cache
invalidation problem in an app whose premise is that these environments are recreated
constantly.

**Instance names from provider output are untrusted input.** They originate in external CLI
JSON, not in ClusterDeck's own store, and `open_local_runtime_shell` routes them into an
`osascript` AppleScript string and an `ssh`/`colima` argv. `services/validate.rs` therefore
gains a check applied at that sink before the name reaches any command — the same defensive
re-check `AGENTS.md` requires after two prior CRITICAL findings.

Frontend scope is one sidebar `LOCAL` section plus a runtime row rendered from the existing
Patch Panel tokens and `panel-card` pattern in `src/styles.css`. No new design system, and no
new view: a row's only navigation affordance selects the associated Kubernetes context in the
existing Kubernetes card.

## Rejected

- **Extending `services/discovery.rs`** — mixes a pure-network module with process execution
  and makes both harder to test.
- **A Colima-shaped domain model with Lima as a special case** — violates issue #14's
  provider principle and would need rewriting for Podman machine or Rancher Desktop.
- **Fetching and normalizing Colima's kubeconfig into `~/.clusterdeck/kubeconfigs/`** — the
  remote pipeline exists to reach hosts that are not locally reachable; a local socket is.
- **Ingesting `colima ssh-config` into a ClusterDeck-owned `~/.clusterdeck/ssh/*.conf`** —
  Colima owns and regenerates that file, and its `StrictHostKeyChecking no` /
  `UserKnownHostsFile /dev/null` settings contradict the repository's `accept-new` SSH rule.
  If this is ever bridged, it must be by referencing the file via `ssh -F`, not by copying
  lines into a file ClusterDeck claims to own.
- **A background polling daemon** — on-demand refresh matches the existing status model.

## Out of scope for this MVP

Deferred with a reason, not merely unlisted:

- **Lifecycle actions (start/stop/restart)** — mutating and long-running; the current command
  surface is fire-and-forget `Result<T, String>` with no cancellation or progress streaming.
  Needs its own issue. **Decided by [ADR-0007](0007-local-runtime-lifecycle-actions.md).**
- **Container listing and Docker socket access** (issue #14 Phase 3) — overlaps ColimaUI and
  Portainer directly, and opens a new privileged sink.
- **Phase 5 topology graph** — needs the provider data to exist first.
- **Colima configuration editing, VM image management, embedded Kubernetes, AI diagnostics** —
  already out of scope in issue #14.
- **Any per-runtime Kubernetes resource browser.** This is the guardrail on the boundary this
  ADR widens: a local runtime row may navigate to existing Kubernetes/SSH views and may not
  grow its own.

## Consequences

Positive:

- The product boundary moves by one explicit, documented axis instead of drifting.
- Read-only first means no new destructive path and no new file ClusterDeck must own.
- The provider trait keeps Colima out of the core domain, so Podman machine or Rancher Desktop
  is an added implementation rather than a refactor.
- Existing SSH/kubeconfig workflows are untouched; the feature is purely additive.

Trade-offs:

- Issue #14's MVP "Definition of Done" line for start/stop/restart is deliberately not met by
  this phase and moves to a follow-up issue.
- Depending on `colima`/`limactl`/`docker` CLI output is a stability dependency on tools
  ClusterDeck does not version.

## Risks

1. **CLI output drift.** Evidence is pinned to `colima 0.10.3` / `limactl 2.2.0`. Mitigated by
   the narrow DTO and tolerant per-line parse; a drifted field degrades one row to "detected,
   details unavailable".
2. **Unverified running-instance shape.** No instance was running at design time, so
   `colima status --json`'s payload and whether `colima list --json` carries an IP/address
   field while running are both **unconfirmed**. This blocks the address field specifically.
3. **Untrusted names at a privileged sink**, addressed by D3's validation requirement; a new
   sink added later must re-check rather than assume.
4. **Boundary erosion.** The unified view is the feature most likely to attract
   "just one more Kubernetes panel" requests. The D3 navigation-only rule is the test to apply.
5. **FakeRunner blind spot.** Unit tests prove the code path, not the real CLI's output, which
   is the same gap that hid the prior `BatchMode`/`accept-new` regression. Implementation must
   also exercise the real binaries once.

## Remaining

Follow-up work, in order:

1. Verify `colima status --json` and the running-instance IP field against a live Colima
   instance. Blocks the runtime address field.
2. Implement `services/local_runtime.rs` with both providers, using captured real NDJSON as
   test fixtures plus `FakeRunner` unit tests, and a one-off real-binary run as evidence.
3. Add the `validate.rs` instance-name check and the two Tauri commands.
4. Add the sidebar `LOCAL` section and the Kubernetes-context navigation affordance.
5. Open a follow-up issue for lifecycle actions (cancellation plus progress).
6. Open a follow-up issue for the Phase 5 topology view.
