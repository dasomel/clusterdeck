# ClusterDeck Repository Guidance

ClusterDeck is a macOS-first desktop app for connecting local users to frequently recreated VM/Kubernetes environments. Core workflow: Discovery → SSH Bootstrap → SSH/ProxyJump → kubeconfig Fetch → Normalize → Verify. Do not turn it into a general Kubernetes administration console without an ADR (`docs/adr/`).

Before substantial work: find the relevant GitHub Issue, read `docs/ARCHITECTURE.md` and `docs/03-mvp-design.md`, and check for an existing ADR. Change management and agent engineering follow https://github.com/dasomel/openforge/blob/main/docs/agent-engineering.md.

## Architecture

- Rust owns filesystem, process, SSH, kubeconfig, and network-sensitive operations; React/TypeScript is presentation only.
- Treat Tauri command exposure, filesystem/process access, and public API widening as design changes.
- All process execution goes through the `CommandRunner` trait (`services/process.rs`), never `tokio::process::Command` directly — this is what makes services unit-testable with a `FakeRunner` instead of hitting real SSH/kubectl/osascript.
- Any `profile.id`, host/bastion name, or address that reaches a privileged sink (SSH argv, `~/.ssh/config`, `/etc/hosts`, a generated file path) must be validated via `services/validate.rs` (`is_safe_profile_id`, `is_safe_ssh_identifier`) first. `store::upsert_profile` enforces this at the persistence boundary; a new sink should still re-check rather than assume upstream validation covers it.
- Frontend styling uses the CSS custom-property tokens in `src/styles/tokens.css` (`--bg`, `--border`, `--text-primary`, `--accent`, `--font-mono`, light/dark via `prefers-color-scheme` + an explicit `data-theme` override). Reuse them; do not hardcode colors.

## Security

- Never place real credentials or infrastructure information in source, issues, tests, fixtures, screenshots, or documentation. Never log secrets.
- Do not expose private-key contents to the frontend. Do not store bootstrap passwords in repository files.
- Service login credentials (`services/service_login.rs`) are an explicit ADR-0014 exception: read on demand only on explicit user action, gated by the user's own Kubernetes RBAC (`kubectl auth can-i`, impersonation refused), held transiently in memory, never persisted or logged.
- Never overwrite a user's entire `~/.ssh/config`, `~/.kube/config`, `/etc/hosts`, or another tool's `/etc/resolver/*` file — own only clearly marked content (`Include ~/.clusterdeck/ssh/*.conf` for SSH; a `# >>> ClusterDeck BEGIN (profile: <id>) >>>` / `END` marker pair per profile for `/etc/hosts` when `Profile.host_mapping` is `hosts`; `/etc/resolver/<zone>` files carrying `# managed by ClusterDeck (profile: <id>)` when it is `local_dns`).
- An SSH bootstrap password goes through `CommandRunner::run_with_env` as the `SSHPASS` env var (`sshpass -e`), never as a `-p <password>` argv element — argv is visible to other local processes via `ps`.
- Every SSH invocation in `BatchMode=yes` must also set `StrictHostKeyChecking=accept-new` — without it, connecting to any host not already in `known_hosts` fails outright, which breaks the core scenario (recreated VMs are by definition new hosts). `FakeRunner` tests don't check argv content, so a missing option passes them; check each new SSH invocation site against a real SSH target.

## Validation

`make verify` is the local gate (`cargo fmt --check`, `cargo clippy --all-targets --all-features -- -D warnings`, `cargo test --all-targets --all-features`, `pnpm build`); `docs/CI.md` also lists the Tauri configuration check. `make evidence` runs the stages and appends sanitized exit-code/timing records to `research/evidence/YYYY-MM.jsonl` per `docs/research-evidence.md`.

A `FakeRunner` test proves the code path runs, not that the external command behaves as intended. When a change touches how an SSH argv, `/etc/hosts`/`~/.ssh/config` write, or kubeconfig normalization is actually built, also exercise the real binary once (a temporary `#[ignore]`d test against a real local target such as `colima`'s SSH-exposed VM, removed before committing). State which checks ran: frontend/unit evidence is not Tauri/native/SSH/Kubernetes runtime evidence.

## Documentation

English is canonical; Korean translations are `<name>-ko.md`. Architecture decisions go in `docs/adr/`.
