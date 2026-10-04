# dasomel/openforge#54: clusterdeck-connection-workflow replay (2026-10-04)

Fresh-session replay (Claude Code subagent, `claude-sonnet-5-5`) of
`.agents/skills/clusterdeck-connection-workflow/SKILL.md` (version `"1"`) in a clean worktree of
`origin/main` (branch `chore/54-skill-verification-evidence`). Started from `AGENTS.md` / `CLAUDE.md` only.
All outputs below were observed in this session; nothing is estimated.

## Plan

1. Check activation from the frontmatter description, read the skill, follow its 9-step workflow.
2. Happy path: analysis of the SSH argv construction against the skill's rules, plus the real-binary `--ignored` tests.
3. Edge case: delete `StrictHostKeyChecking=accept-new` from the shared SSH options, confirm the repo's tests fail, revert, confirm they pass. Then run `make verify`.

## Step 1: Activation match

`ls .agents/skills` returns exactly one skill, `clusterdeck-connection-workflow`. Its frontmatter
description says: "Implement or debug ClusterDeck Discovery -> SSH bootstrap/ProxyJump -> kubeconfig
fetch/normalize/verify flows while preserving CommandRunner, input validation, user-config ownership,
secret handling, and real SSH/kubectl evidence. Use for SSH, bastion, hosts-file, kubeconfig,
discovery, or verification-path changes."

Representative task chosen from that description: "Audit that every SSH invocation site in the
bastion/ProxyJump path still sets `StrictHostKeyChecking=accept-new` and keeps passwords out of argv."

Result: the description matches (SSH, bastion, secret handling). `CLAUDE.md` also routes this class of
work to the skill. Caveat: with one skill in the repo there was no competing skill, so this shows the
description matches the task, not that it beats alternatives. I then read the skill and followed its Workflow.

## Step 2: Happy path (scratch-only)

Workflow steps 3-8 applied as an audit; no source change was needed, so nothing needed reverting.

- Step 3 (CommandRunner): `grep -rn "tokio::process" src-tauri/src` found `services/process.rs:6`
  (the runner itself) and one use in a test at `services/ssh.rs:926`. That test is the `#[ignore]`d
  real-argv stub test, not a production site.
- Steps 4-6, 8 (SSH options): `push_connection_options` (`ssh.rs:29`) sets `ConnectTimeout=5`,
  `StrictHostKeyChecking=accept-new`, `-p`, and `-i`. It is called at `ssh.rs:110` (after
  `BatchMode=yes`), `ssh.rs:337` and `ssh.rs:416`.
- The one BatchMode site outside `ssh.rs` is `kubeconfig.rs:331`. It passes `-F <generated conf>`,
  and `ssh_config.rs:49,65` render `StrictHostKeyChecking accept-new` into that config. This matches
  the comment at `kubeconfig.rs:317-320`, so it is not a missing-option gap.
- Real-binary evidence, run exactly as `AGENTS.md` Validation suggests:

```text
$ cd src-tauri && cargo test --all-features -- --ignored real_process_receives real_ssh_binary_parses
test services::ssh_config::tests::real_ssh_binary_parses_proxyjump_and_hostname_from_generated_config ... ok
test services::ssh::tests::real_process_receives_batchmode_stricthostkeychecking_and_proxyjump_argv ... ok
test result: ok. 2 passed; 0 failed; 0 ignored; 0 measured; 223 filtered out
exit=0
```

These two tests spawn the local `/usr/bin/ssh` binary against a stub or generated config. That is
real-binary evidence for argv and config parsing only. It is not a connection to a real VM or cluster.

## Step 3: Failure/edge case (real project-specific hazard)

Hazard: `AGENTS.md` Security Rules say every `BatchMode=yes` SSH invocation must also set
`StrictHostKeyChecking=accept-new`. Without it, a frequently recreated (never-seen) VM fails with
"Host key verification failed", which breaks the core scenario. `AGENTS.md` also warns that
`FakeRunner` tests do not check argv content, so I expected this one might slip through.

Mutation: deleted the two `args.push` lines for `StrictHostKeyChecking=accept-new` from
`push_connection_options` in `src-tauri/src/services/ssh.rs`. `git diff --stat` showed
`1 file changed, 2 deletions(-)`.

```text
$ cd src-tauri && cargo test --all-targets --all-features
test services::ssh::tests::build_ssh_target_args_accepts_new_host_keys_without_prompting ... FAILED
test services::ssh::tests::probe_auth_dispatches_password_hosts_to_sshpass_with_no_batchmode ... FAILED
test services::ssh::tests::shared_connection_options_present_on_all_three_ssh_argv_paths ... FAILED
assertion failed: args.contains(&"StrictHostKeyChecking=accept-new".to_string())
test result: FAILED. 219 passed; 3 failed; 3 ignored
exit=101
```

So the repository's own pre-existing unit tests catch this regression with a non-zero exit. The
`#[ignore]`d real-process test was not part of this run (it is ignored by default).

Revert: `git checkout src/services/ssh.rs`, after which `git status --short` was empty. Re-run:

```text
$ cargo test --all-targets --all-features
test result: ok. 222 passed; 0 failed; 3 ignored; 0 measured; 0 filtered out
exit=0
```

## Step 4: Repository-owned verification (`make verify`)

Frontend dependencies were installed inside the worktree with `pnpm install`, as `make install` does.
`pnpm install --frozen-lockfile` fails with `ERR_PNPM_NO_LOCKFILE` because `pnpm-lock.yaml` is
gitignored. `pnpm install` exited 0 in 3.1 s, and `node_modules/` is also gitignored.

```text
$ make verify          # cargo fmt --check, clippy --all-targets -D warnings, cargo test, pnpm build
exit=0   (run twice: once at the start, once after the revert; both exit 0)
test result: ok. 222 passed; 0 failed; 3 ignored
vite build: dist/index.html, 1 css and 1 js asset; "built in 359ms" (first run)
```

The `pnpm build` output contains three `[lightningcss minify] Unknown at rule: @theme` / `@tailwind`
warnings. They are non-fatal (exit 0), come from the Tailwind v4 CSS, and I left them alone.

## What was NOT verified

- No real SSH/k3s/Kubernetes target was used. The skill's own Verification section requires real
  SSH/kubectl evidence for connection-path changes. I made no connection-path change, so I gathered no live
  connect/bootstrap/kubeconfig-fetch evidence. Only the local `ssh` binary argv/config-parse tests ran.
- The `ca_trust` real-keychain `#[ignore]`d test was not run (macOS keychain mutation, out of scope).
- The edge case was proven against `FakeRunner`-level unit tests (which here do assert argv content).
  The ignored real-process test was not re-run under the mutation.
- Activation was assessed against a single-skill repository, so it did not show the skill winning against
  similar skills.

## Final state

`git status --short` shows only the two deliverables: this report and
`.agents/skill-evals/clusterdeck-connection-workflow.json`. `node_modules/`, `dist/`, `pnpm-lock.yaml` and
`src-tauri/target/` are gitignored. `SKILL.md`, source and CI were not modified.
