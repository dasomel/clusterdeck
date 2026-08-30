# ADR-0002: ClusterDeck-managed kubeconfigs stay isolated from the user's `~/.kube/config`

- Status: Accepted
- Date: 2026-08-30

## Context

Issue #3's original completion checklist included backing up the user's existing
`~/.kube/config` and merging each profile's fetched kubeconfig into it, with a strategy
for cleaning up stale contexts/clusters/users. The implementation that shipped in PR #12
does neither: `services/kubeconfig.rs::fetch_and_store` only ever reads/writes files under
the ClusterDeck-owned `~/.clusterdeck/kubeconfigs/<profile>.yaml` path, and never touches
`~/.kube/config`.

This was an implementation-time judgment call, not an oversight, but it was never written
down, so the issue #3 checklist reads as an unaddressed gap rather than a decision.

## Decision

ClusterDeck will not read, back up, or write to the user's `~/.kube/config`. Each profile's
kubeconfig lives only under `~/.clusterdeck/kubeconfigs/<profile>.yaml` (`chmod 0600`),
normalized so its cluster/context/user names equal the profile id. Users who want to use
`kubectl` against a profile point `--kubeconfig` (or `KUBECONFIG`) at that file directly, or
merge it in themselves if they choose to.

This matches `AGENTS.md`'s security rule ("Never overwrite a user's entire `~/.kube/config`
... without an explicit design decision") — this ADR is that explicit decision — and avoids
an entire class of bugs that a merge/backup/stale-entry-cleanup strategy would introduce:
partial-write corruption of a file ClusterDeck doesn't fully own, name collisions with the
user's own contexts, and an implicit obligation to keep a backup/restore path correct
forever.

`services/kube_import.rs`'s read-only import of existing `~/.kube/config` context metadata
(added later, also in PR #12) is consistent with this: it only *reads* the user's file to
help populate a new profile, and never writes to it.

## Consequences

Positive:

- One clear owner per file: ClusterDeck never risks corrupting or losing entries in a file
  it doesn't fully control.
- No backup/restore or stale-entry-GC logic to design, implement, or keep correct.
- No name-collision handling needed between ClusterDeck-managed and user-managed contexts.

Trade-offs:

- Users must explicitly select ClusterDeck's kubeconfig file (via `--kubeconfig`/`KUBECONFIG`
  or a shell alias/function) rather than having `kubectl` pick it up automatically after
  running `kubectl config use-context`.
- Issue #3's original checklist items for backup/merge/stale-entry cleanup are superseded by
  this decision rather than implemented; they should be closed as "won't do" with a link to
  this ADR, not left open.

## Non-goals

This decision does not preclude a future opt-in feature (e.g. "Copy path to clipboard" or an
explicit "merge this profile into ~/.kube/config" button the user triggers deliberately) —
it only rules out ClusterDeck doing so automatically or by default.
