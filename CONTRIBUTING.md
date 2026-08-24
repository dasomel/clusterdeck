# Contributing to ClusterDeck

Thank you for contributing to ClusterDeck.

## Project Principles

- ClusterDeck is an open-source macOS-first desktop application for connecting to frequently recreated VM and Kubernetes environments.
- User-facing functionality should remain simple and safe: discover → connect → sync → verify.
- Keep the application focused on local environment access. ClusterDeck is not a general Kubernetes resource management console.
- Prefer small, testable, reviewable changes.
- Security-sensitive operations belong in the Rust/Tauri backend, not in the frontend.
- Never commit real infrastructure addresses, credentials, private keys, bearer tokens, or kubeconfigs.

## Before Making a Change

1. Check existing Issues and Architecture Decision Records.
2. Use or create a GitHub Issue for non-trivial work.
3. Define the affected boundary: discovery, SSH, Bastion/Relay, kubeconfig, verification, or UI.
4. Keep unrelated refactoring out of the same change.

## Development Stack

- macOS-first
- Tauri 2
- Rust
- React + TypeScript
- Vite
- Tailwind CSS
- OpenSSH / `kubectl` integration for the MVP

See `docs/ARCHITECTURE.md` and `docs/03-mvp-design.md` for the current design.

## Pull Requests

PRs should:

- reference the related Issue
- explain the problem and implementation
- describe verification performed
- update documentation when behavior or design changes
- avoid unrelated changes

Prefer focused PRs with a single objective.

## Commit Convention

Use Conventional Commits where practical:

```text
feat: add SSH host discovery
fix: handle unreachable bastion
refactor: split kubeconfig service
chore: update Tauri dependencies
docs: document profile model
```

## Testing and Verification

At minimum, run the checks relevant to the change:

```bash
pnpm install
pnpm build
cargo check --manifest-path src-tauri/Cargo.toml
```

For functionality involving SSH, kubeconfig, or Kubernetes connectivity, add or perform a reproducible integration check when the environment allows it.

Do not use real production credentials or infrastructure in tests committed to the repository.

## AI-Assisted Development

AI-generated code is treated the same as human-authored code. Contributors remain responsible for:

- correctness
- security
- licenses
- tests
- dependency changes
- documentation

Repository instructions are authoritative over generic AI suggestions.
