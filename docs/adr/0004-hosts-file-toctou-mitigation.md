# ADR-0004: `/etc/hosts` TOCTOU mitigation scope, and kubeconfig fetch permissions

- Status: Accepted
- Date: 2026-09-17

## Context

Issue #13 identified two related TOCTOU (time-of-check-to-time-of-use) findings:

1. `services/hosts_file.rs::upsert_hosts_block` / `remove_hosts_block` read `/etc/hosts`
   synchronously, compute the new managed-block content, then call `write_hosts_file`, which
   shells out to `osascript ... with administrator privileges` to copy a temp file over
   `/etc/hosts`. The macOS admin-approval prompt can block for an arbitrary amount of time.
   If another process or user edits lines outside ClusterDeck's managed block while that
   prompt is pending, ClusterDeck's stale snapshot silently overwrites the concurrent edit
   when the privileged copy finally runs.
2. `services/kubeconfig.rs`'s `scp`-based fetch writes to a local temp file before moving it
   into place. The destination kubeconfig is already created/chmod'd `0600` (commit
   `7e8af41`), but `scp` itself can reset the temp file's mode based on the remote file's
   permissions at transfer completion, so a brief window where the temp file is
   more-permissive than `0600` cannot be fully ruled out while `scp` is shelled out to
   directly.

Both issues require a local, already-somewhat-privileged actor (someone who can edit
`/etc/hosts` or read another local user's temp files) to exploit; neither is reachable by a
remote network attacker.

## Decision

**Problem 1 (mitigated now):** `upsert_hosts_block` / `remove_hosts_block` re-read
`/etc/hosts` immediately before invoking the privileged write and compare against the
snapshot the new content was computed from (`recheck_snapshot` in
`services/hosts_file.rs`). On a mismatch, the content is recomputed once against the fresh
read and retried; a second mismatch aborts the write entirely rather than silently
overwriting the concurrent edit. Existing `services/validate.rs` checks
(`is_safe_profile_id`, `is_safe_ssh_identifier`) are unchanged and still run during
`render_hosts_block` before any of this — the mitigation only changes when the write
happens, not what content is considered safe to write.

This narrows the race window (the reads happen back-to-back, right before the admin prompt
is shown) but does not eliminate it — a change landing in the instant between the recheck
read and the actual `cp` inside `osascript` is still possible. Closing it fully would
require moving the read-merge-write into the privileged shell script itself, which the issue
allows as an alternative but which materially increases the risk of reintroducing shell
injection into a script string already built from user-influenced profile data; that
trade-off was rejected in favor of the safer, partial mitigation.

**Problem 2 (completed in issue #23):** Kubeconfigs are now fetched
via SSH exec + local write instead of shelling out to `scp`, so ClusterDeck controls the
destination file's permissions directly and no longer relies on `scp`'s behavior.

## Consequences

Positive:

- Closes the most likely trigger for problem 1 (a slow/ignored admin prompt) with a small,
  contained change to `services/hosts_file.rs` only.
- No change to the SSH/hosts argv construction or validation logic, so no new injection
  surface.

Trade-offs:

- The window is narrowed, not closed, for problem 1. A determined local attacker with tight
  timing could still race the final privileged copy.
- Problem 2 is closed by issue #23's SSH-exec-based kubeconfig fetch rewrite.

## Non-goals

The SSH-exec-based kubeconfig fetch is tracked by issue #23; this ADR records its completion
because it closes the deferred permissions concern described above.
