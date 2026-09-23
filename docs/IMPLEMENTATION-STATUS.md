# Implementation Status

Last verified: 2026-09-22 against `main`

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
- CA/leaf certificate health warnings surfaced per discovered CA: missing `serverAuth` EKU, leaf/CA expiry (14-day/30-day thresholds, with a distinct already-expired message), SAN not covering a discovered host, and structurally invalid CA (missing CA:TRUE/keyCertSign).
- Remote kubeconfig fetch reads over SSH exec (`sudo cat`, falling back to `cat`) instead of shelling out to `scp`, removing the local temp-file permission window (#23).
- TLS certificate verification on the curl Kubernetes API fallback path (used when `kubectl` itself fails, e.g. macOS Sequoia Local Network Privacy): verifies against the kubeconfig's CA data via `--cacert`, or curl's system trust store when no CA data is present — never `-k`/`--insecure` (#24).
- Local runtime discovery dashboard: architecture/CPU/memory/disk display and Docker context association for Colima/Lima instances, in a read-only "Local Runtime" section in Settings (#14 Phase 1).
- `clusterdeck-connection-workflow` Agent Skill re-verified via a fresh-session replay against a real target; `openforge-maturity` is `verified` again (#22).
- Explicit per-host SSH authentication mode (`key`, the default, or `password`): password mode uses `sshpass -e`/`SSHPASS` consistently across Connect, Test Connection, and kubeconfig fetch, omits `BatchMode=yes` (which would block the password prompt) while keeping `StrictHostKeyChecking=accept-new`, and keeps the password ephemeral in frontend state only. Profile YAML written before this field existed deserializes unchanged as `key` (#26).

## Partial / environment-dependent

- Unit/FakeRunner tests prove application control flow but cannot prove real OpenSSH, kubectl, native filesystem, or target-cluster behavior; critical-path changes require real-binary/runtime evidence where practical.
- Password-mode SSH authentication (#26) is covered by unit tests that assert on real argv/env construction (no `BatchMode=yes`, has `StrictHostKeyChecking=accept-new`, uses `sshpass -e` with `SSHPASS`), but has not been exercised against a real password-authenticating SSH target: no such target was available in the development environment (the local Colima VM used for other real-process checks only supports key-based auth).
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
- commit `a0045c0` (CA/leaf certificate health warnings) and commit `c3eb682` (false-positive expiry fix, issue #25)
- commit `73e2312` (kubeconfig fetch over SSH exec instead of scp, issue #23)
- commit `21458de` (TLS certificate verification on the curl Kubernetes API fallback path, issue #24)
- commit `5e617d3` (local runtime discovery dashboard, issue #14 Phase 1) and commit `e6ff486` (relocated to Settings)
- commit `268feab` (`clusterdeck-connection-workflow` skill re-verified, issue #22); evidence at `research/issue-22-connection-workflow-replay-2026-09-22.md`
- commit `cb85aff` and commit `24b231c` (release workflow: macOS `.dmg` draft GitHub Release on `v*` tag push, third-party Actions pinned to commit SHAs)
- branch `feat/26-password-ssh-auth` (password-based SSH authentication mode, issue #26)
