# Skill replay evidence: `clusterdeck-connection-workflow` (2026-09-22)

Closes: `dasomel/clusterdeck#22` ("Re-verify clusterdeck-connection-workflow skill before
re-promoting to verified"), which itself exists to satisfy the bar set by
`dasomel/openforge#54` ("Verify draft Agent Skills with fresh-session replay"). File named to
follow the cross-project precedent cited in #22 (LDAPium's
`research/issue-140-directory-change-replay-*.md`).

## Context

`.agents/skills/clusterdeck-connection-workflow/SKILL.md` was reverted from an invalid
`verified` claim to `openforge-maturity: draft` (commit `cdf8e04`) because it had stale
References paths and no replay evidence file on record. This record is a genuine fresh-session
replay of the skill's 9-step Workflow, run by an agent with no prior context on this
repository, against a real task: verifying issue #23's SSH-exec kubeconfig fetch
(`services/kubeconfig.rs`, commit `73e2312`, merged to `main`) end-to-end against a real
SSH/Kubernetes target, per the skill's own Verification section warning that `FakeRunner`
proves the code path but not real SSH/known_hosts/filesystem/kubectl behavior.

- `repository`: `dasomel/clusterdeck`
- `revision`: `c3eb68289d76a6d5b76c7a7c3ab51bb0a106cd79`
- `environment`: `darwin/arm64` (macOS 27.0)
- `date`: 2026-09-22

## Real target

Local `vagrant-beluga` profile (`~/.clusterdeck/profiles.yaml`, not repo-tracked): a Vagrant VM
running k3s, control-plane host `master-1` at `192.168.77.10` (RFC1918, retained per this
project's research/README.md public-data rule), real kubeconfig at
`/etc/rancher/k3s/k3s.yaml`. This is the user's real, already-working setup and was never
mutated by this replay (see Cleanup verification).

## What was exercised

Steps 1-9 of the skill's Workflow were followed in order: read `AGENTS.md`,
`docs/ARCHITECTURE.md`, `docs/03-mvp-design.md`, issue #23, and ADR-0004; read
`services/{kubeconfig,store,config,paths,validate,process,ssh,ssh_config}.rs` to establish how
`CommandRunner`, profile persistence, and SSH config generation fit together; confirmed
`fetch_and_store` routes only through `CommandRunner` and revalidates `profile.id` before use.

A temporary profile `skill-replay-test` was created (cloned in-memory from `vagrant-beluga`'s
real hosts/SSH details via `store::get_profile`, never hand-typed into source) with a
**deliberately wrong** `kubeconfig.remote_path` (`/etc/kubernetes/admin.conf`, a
kubeadm/vanilla-k8s convention a k3s host does not have). Because that path is already a member
of `CANDIDATE_KUBECONFIG_PATHS`, the real candidate list became:
`[/etc/kubernetes/admin.conf (configured, wrong), /etc/rancher/k3s/k3s.yaml (correct), ~/.kube/config, /var/lib/microk8s/credentials/client.config]`.

Pre-flight independent confirmation via a direct `ssh` probe (outside the Rust test, against
the real host):

```
admin.conf exists: no
k3s.yaml exists: yes
```

This proves the first candidate was a genuine failure on the real host, not a contrived mock,
and that the loop had a real, correct second candidate to fall through to.

A `#[cfg(unix)] #[ignore]`d `#[tokio::test]` (`fetch_and_store_falls_back_through_real_candidate_paths`,
temporarily added to `src-tauri/src/services/kubeconfig.rs`, removed after this replay -- see
Cleanup verification) then:

1. Persisted the temporary profile via `store::upsert_profile` (real validation path, same as
   the app itself).
2. Generated its SSH config via `ssh_config::write_profile_config` (so `BatchMode=yes` +
   `StrictHostKeyChecking=accept-new` were present exactly as the app would generate them, per
   AGENTS.md's Security Rules).
3. Called `fetch_and_store` with `SystemRunner` (the real command runner, not `FakeRunner`).
4. Asserted success and that the written kubeconfig file exists.
5. Cleaned up the temporary profile/ssh-conf/kubeconfig via a `Drop`-guard (runs even if the
   fetch assertion fails).

Result: **pass**, in 0.46s
(`cargo test --lib services::kubeconfig::tests::fetch_and_store_falls_back_through_real_candidate_paths -- --ignored --nocapture`).
This is the run's deliberate failure/edge-case regression: the real fetch had to fail on the
first candidate and fall through to the second for the test to pass at all.

## `make verify`

Run twice: once with the temporary test present (compiles + lints under
`--all-targets --all-features -- -D warnings`, does not execute since it's `#[ignore]`d), and
once after the temporary test was fully removed.

| Stage | Result |
|---|---|
| `cargo fmt --check` | pass (one diff surfaced on first run, from the newly-added code's own formatting; fixed with `cargo fmt --all`, clean on all subsequent runs) |
| `cargo clippy --all-targets --all-features -- -D warnings` | pass, 0 warnings |
| `cargo test --all-targets --all-features` | 141 passed, 0 failed, 3 ignored (baseline) / 4 ignored (temporary test present) |
| `pnpm build` | pass |

Final `make verify` after cleanup: exit 0, `141 passed; 0 failed; 3 ignored` -- identical to the
pre-replay baseline, confirming no regression from the round trip.

## Cleanup verification

After the test run, independently confirmed (not just trusting the `Drop` guard):

- `~/.clusterdeck/profiles.yaml` contains only `vagrant-beluga` (no `skill-replay-test` entry);
  `vagrant-beluga`'s `kubeconfig` block and `trusted_cas` count are byte-identical to
  pre-replay.
- `~/.clusterdeck/ssh/skill-replay-test.conf` and
  `~/.clusterdeck/kubeconfigs/skill-replay-test.yaml` do not exist.
- `~/.ssh/config` was never touched (the replay only called `ssh_config::write_profile_config`,
  never `ensure_ssh_include`, since `vagrant-beluga` already being real/working implies the
  `Include ~/.clusterdeck/ssh/*.conf` line already existed).
- `git diff -- src-tauri/src/services/kubeconfig.rs` is empty; `git status` matches the
  pre-replay baseline exactly (only pre-existing, unrelated `DESIGN.md`/`DESIGN-ko.md`
  modifications and untracked `.claude/`/`design/` from other in-progress work, none touched by
  this replay).
- No divergent copy of the skill's content exists outside `.agents/skills/clusterdeck-connection-workflow/SKILL.md`;
  `CLAUDE.md` only references it by path.

## Mapping to `dasomel/openforge#54`'s required evidence

1. **Fresh session, repo instructions + task only.** Yes -- this replay ran with no prior
   session context on this repository.
2. **Activation description is precise enough, with no manual hidden context.** Not blind-tested
   in this replay: the assigning agent explicitly pointed at the SKILL.md path rather than the
   skill being discovered purely via description-matching against an untagged task. Recorded
   here as a gap rather than papered over (see Not proven, below). Qualitative read: the
   frontmatter `description` field's keyword list (SSH, bastion, hosts-file, kubeconfig,
   discovery, verification-path) does cover this task's shape.
3. **Representative happy-path task, current sources of truth.** Yes -- the real
   `fetch_and_store` call succeeding end-to-end against the live k3s host is the happy path,
   using the current (post-#23) SSH-exec implementation.
4. **Failure/edge case distinguishing a real project-specific hazard.** Yes -- the k3s-vs-kubeadm
   default-path mismatch is a genuine, project-specific hazard class (exactly what
   `CANDIDATE_KUBECONFIG_PATHS` exists to paper over), exercised for real, not mocked.
5. **Repository-owned deterministic verification entrypoint(s).** Yes -- `make verify`, run
   before and after, both green.
6. **What was not proven -- see below.**
7. **Concise project-local trace/report, then flip maturity.** This file, then the frontmatter
   edit that follows it.

### Not proven by this replay

- Activation-by-description was not blind-tested (see point 2 above).
- The bastion/ProxyJump path (`profile.bastion`) was not exercised -- `vagrant-beluga` has no
  bastion configured, so `fetch_remote_kubeconfig_content`'s `-J` injection path is untouched by
  this replay.
- Whether the successful read actually needed `sudo cat` or fell through to the plain `cat` in
  `sudo cat '<path>' 2>/dev/null || cat '<path>'` was not independently distinguished; only that
  one of the two succeeded.
- The all-candidates-fail error path (`Err("kubeconfig fetch failed: ssh probe error: ...")`)
  was not exercised, by either real SSH or `FakeRunner` -- there is no existing unit test for it
  either.
- SSH bootstrap/password-based auth, `/etc/hosts` management, and the Discovery stage were out
  of scope for this replay (issue #23 only touched the kubeconfig-fetch transport).

## Findings on the skill document itself

1. **References gap.** `SKILL.md`'s References list (`AGENTS.md`, `docs/ARCHITECTURE.md`,
   `docs/03-mvp-design.md`, `services/process.rs`, `services/validate.rs`, `Makefile`) omits
   every service file that actually owns kubeconfig/SSH-config/profile-persistence domain logic
   (`services/kubeconfig.rs`, `services/store.rs`, `services/ssh.rs`, `services/ssh_config.rs`,
   `services/paths.rs`), despite the skill's own description centering on exactly this domain. A
   fresh-session agent has to rediscover these files by grep/read rather than being pointed at
   them directly.
2. **No pointer to the existing real-target test convention.** Workflow step 9 and the
   Verification section both say to exercise the real binary/target, but neither points at the
   fact that this repo already has three precedents (`services/ssh.rs` and
   `services/ssh_config.rs` each have one `#[ignore = "..."]` test; `services/ca_trust.rs` has a
   "Manual-only" one with the exact run command in its comment). A short pointer would have
   saved real search time; this replay initially missed all three due to a shell-quoting mistake
   in its own `grep --include=*.rs` invocation (zsh glob-expanded `*.rs` against the cwd instead
   of passing it through to grep) and only found them via `make verify`'s own test-name output.
   That was this replay's own tooling error, not the skill's, but a direct file pointer in the
   skill would have prevented the search from depending on grep working correctly at all.
3. **No guidance on safely exercising a real target that is user machine state, not repo
   state.** All three existing `#[ignore]`d precedents avoid needing live, already-provisioned
   infrastructure (a stub `ssh` script for argv capture, `ssh -G` for config parsing with no
   network connection, or a keychain test that is explicitly manual-only). This replay's task
   genuinely required a live reachable host (per AGENTS.md's own "e.g. `colima`'s SSH-exposed
   VM" example), which also meant mutating real state outside the repo
   (`~/.clusterdeck/profiles.yaml`, `~/.clusterdeck/ssh/`, `~/.clusterdeck/kubeconfigs/`) that
   the skill has zero guidance on handling safely -- no mention of "never touch the user's
   existing working profile, create a disposable one and guarantee its cleanup." This replay
   used a cloned temporary profile plus a `Drop`-guard cleanup pattern (never hand-typed real
   host/address/identity_file values into source); that pattern is not documented anywhere the
   skill points to, so the next agent attempting this would have to invent it again from first
   principles.

No blockers from the "Stop / Escalate When" section were hit.

## Conclusion

Real-target replay evidence and `make verify` evidence both collected and passing; the
deliberate first-candidate failure exercised the candidate-path fallback loop for real, not just
via `FakeRunner`. Measured against `dasomel/openforge#54`'s required-evidence list, all points
are satisfied except activation-description blind-testing, which is explicitly recorded above
rather than silently skipped. `openforge-maturity` updated `draft` -> `verified` in
`.agents/skills/clusterdeck-connection-workflow/SKILL.md`, referencing this file.
