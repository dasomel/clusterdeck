# Implementation Status

Last verified: 2026-09-09 against `main`

This file records current default-branch behavior, not future product direction.

## Implemented

- macOS-first Tauri 2 desktop application foundation with React/TypeScript UI and Rust-owned process/filesystem/network-sensitive operations.
- Profile-oriented discovery and connection workflow for frequently recreated VM/Kubernetes environments.
- SSH key/bootstrap and alias handling, Bastion/ProxyJump support, remote kubeconfig retrieval/normalization, and Kubernetes connectivity verification paths.
- Central `CommandRunner` abstraction for process execution and defensive validation for profile identifiers/SSH sinks.
- Managed-file boundaries for SSH configuration, kubeconfig-related state, and optional `/etc/hosts` edits rather than overwriting user-owned files wholesale.
- Credential rules including `sshpass -e`/environment handling and no private-key contents exposed to the frontend.
- OpenForge reduced local-tool execution-security profile requiring exact resolved-operation approval before any future autonomous mutation surface.

## Partial / environment-dependent

- Unit/FakeRunner tests prove application control flow but cannot prove real OpenSSH, kubectl, native filesystem, or target-cluster behavior; critical-path changes require real-binary/runtime evidence where practical.
- The application remains macOS-first; other platform support should not be inferred from portable Rust/React code alone.

## Not claimed

- ClusterDeck is not a general Kubernetes administration console.
- No autonomous agent-driven local/remote mutation surface is enabled by the OpenForge security profile documentation.

## Evidence

- `README.md`
- `AGENTS.md`
- `docs/ARCHITECTURE.md`
- `docs/03-mvp-design.md`
- `src-tauri/`
- repository `make verify` / CI
- PR #16 (`e7daf5bcf64786c3d253674f6f0486882d040d70`)
