# Implementation Status

Last verified: 2026-09-21 against `main`

This file records current default-branch behavior, not future product direction.

## Implemented

- macOS-first Tauri 2 desktop application foundation with React/TypeScript UI and Rust-owned process/filesystem/network-sensitive operations.
- Profile-oriented discovery and connection workflow for frequently recreated VM/Kubernetes environments.
- SSH key/bootstrap and alias handling, Bastion/ProxyJump support, remote kubeconfig retrieval/normalization, and Kubernetes connectivity verification paths.
- Central `CommandRunner` abstraction for process execution and defensive validation for profile identifiers/SSH sinks.
- Managed-file boundaries for SSH configuration, kubeconfig-related state, and optional `/etc/hosts` edits rather than overwriting user-owned files wholesale.
- Credential rules including `sshpass -e`/environment handling and no private-key contents exposed to the frontend.
- OpenForge reduced local-tool execution-security profile requiring exact resolved-operation approval before any future autonomous mutation surface.
- On-demand local VM detection (Colima/Lima/Vagrant) that prefills the Profile editor form; detected state is held in editor component state only and never persisted unless the user saves the profile (see ADR-0005).
- Kubernetes API endpoint discovery (APISIX/Ingress/Istio/Gateway API/Service) used to normalize a fetched kubeconfig's server endpoint, plus a status banner UI surfaced on the default, kubeconfig-manager, and profile-editor views.
- Private cluster CA discovery (Ingress/ApisixTls → Secret `ca.crt`) with per-CA New/Trusted/Rotated status, trust/replace/remove actions against the macOS login keychain via `security(1)`, and a cross-profile Trusted CAs list in Settings (see ADR-0006).

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
- commit `d4d143a` (local runtime provider, kubeconfig endpoint fetch, status banner UI)
- `docs/adr/0005-local-host-detection-prefills-profiles.md`
- `docs/adr/0006-private-ca-local-trust.md`
- commit `cee7aea` (private CA local trust merge) and commit `066f720` (CA remove/manage fast-follow)
