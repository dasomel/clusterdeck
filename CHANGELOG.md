# Changelog

All notable changes to ClusterDeck will be documented here.

The format follows the principles of Keep a Changelog and uses semantic versioning where releases are applicable.

## [Unreleased]

- Absorb InfraDeck into ClusterDeck: observe local VM inventory across Colima, VirtualBox, VMware Fusion, and Vagrant with exact-identity reconciliation (`runtime:runtime_id`), resource capacity totals (CPU, RAM, disk in GiB), and evidence-based Kubernetes detection ([ADR-0008](docs/adr/0008-vm-inventory-environment-source.md))
- Add one-click "Set up cluster" action from observed multi-node environments, auto-saving a new profile via `store::upsert_profile` (superseding ADR-0005 D1 for this flow) and safely refreshing host address/port without overwriting custom profile settings
- Enforce bounded provider process execution via `CommandRunner::run_bounded` (10s timeout, 2 MiB stream caps, `LC_ALL=C`, stdin null, `kill_on_drop(true)`) and strict path-segment validation before any local file read
- Support built-in synthetic Demo mode (3-node Vagrant/VMware cluster) without running host commands; retire external InfraDeck repository and omit the standalone CLI binary to keep the Rust library API clean

## [0.4.1] - 2026-09-29

- Skip remote kubeconfig candidates that have no authentication settings or broken context references, then continue probing later paths. Report the rejected candidate and remote read-permission guidance if no usable file is found.
- Do not verify a stale or generated credential-free profile kubeconfig after Connect / Sync fails to fetch a fresh one, or merge that placeholder into `~/.kube/config`; show the fetch failure instead of a misleading Kubernetes `EOF`.
- Include both `kubectl` and `curl` errors when Kubernetes verification fails.

## [0.4.0] - 2026-09-29

- Re-resolve the SSH address, port, and identity of profiles created from a detected Colima/Lima instance before every SSH-using action (Test Connection, Connect/Sync, Bootstrap, kubeconfig fetch, Verify, endpoint discovery, SSH session); Colima/Lima forward a new SSH port on each VM restart, which previously left the saved port stale and failed with "Connection refused". Profiles record their origin in a new optional `local_runtime` field; existing profiles load unchanged (#14)
- Fix Colima detection when ClusterDeck is launched from Finder/Dock: child processes now receive the resolved tool search path, so `colima` can find its own `limactl`
- Match the Save and Cancel button size in modal footers

## [0.3.0] - 2026-09-24

- Add local runtime lifecycle actions for Colima/Lima instances: Start/Stop/Restart, open a VM shell, open a host-side shell scoped to the instance's Docker/kube context (no global `docker context use`/`kubectl config use-context` mutation), and copy a plain-text runtime summary; guarded by strict instance-name validation, a fresh-discovery re-check before every action, and a per-instance concurrency lock (#14 Phase 2, ADR-0007)
- Show the app version in the sidebar header (#33)
- Fix remote kubeconfig fetch for non-root SSH users: a remote `cat: ... Permission denied` no longer aborts the candidate-path loop as if it were an SSH auth failure, `~/.kube/config` now expands via `$HOME`, and `sudo -n` never waits on a password prompt (#31)
- Validate `KubeconfigSource.remote_path` against shell-quoting breakouts on save and at the fetch sink, without dropping legacy profiles on load (#39)
- Set `tls-server-name` to the original kubeconfig server host instead of the profile label, and align the curl verification fallback via `--connect-to` (#39)
- Add rke2 and k0s kubeconfig candidate paths; resolve CLI tools from absolute `$PATH` entries after the fixed directories (#39)
- Bump tauri 2.11.6, base64 0.23, sha1 0.11, sha2 0.11 (#38)

## [0.2.0] - 2026-09-23

- Add password-based SSH authentication as a per-host auth mode (`key` default | `password`): `sshpass -e` + `SSHPASS` env only, never argv/config/logs/YAML; consistent across Connect, Test Connection, and kubeconfig fetch; bounded by a 30s timeout; password auth through a bastion is refused explicitly (#26)
- Fix kubeconfig merge to upsert existing cluster/context/user entries by name instead of relying on `kubectl config view --flatten`, which kept the first (stale) occurrence on a name collision (#27)

## [0.1.0] - 2026-09-22

- Initial OSS repository bootstrap
- Define macOS-first Tauri/Rust/React architecture
- Add multi-VM SSH bootstrap design
- Add remote kubeconfig management design
- Add Bastion/ProxyJump design
- Add Kubernetes connectivity verification design
- Add on-demand local VM detection (Colima/Lima/Vagrant) that prefills the Profile editor (ADR-0005)
- Add Kubernetes API endpoint discovery (APISIX/Ingress/Istio/Gateway API/Service) for kubeconfig endpoint normalization
- Add `StatusBanner`, `KubeconfigManager`, and `ConfirmModal` UI components
- Add private cluster CA discovery and local trust via the macOS login keychain, with CA-rotation detection (ADR-0006)
- Add CA trust removal from the endpoints view and a cross-profile Trusted CAs list in Settings
- Add CA/leaf certificate health warnings (missing `serverAuth` EKU, expiry, SAN coverage, CA structural validity) surfaced per discovered CA
- Fix kubeconfig fetch to read over SSH exec instead of shelling out to `scp`, closing a temp-file permission window (#23)
- Finish local runtime discovery dashboard: architecture/CPU/memory/disk display and Docker context association for Colima/Lima instances, surfaced in Settings (#14 Phase 1)
- Re-verify the `clusterdeck-connection-workflow` Agent Skill via real-target fresh-session replay (#22)
- Fix a false-positive CA/leaf certificate expiry warning when `openssl` fails for a reason unrelated to genuine expiry (#25)
- Fix TLS certificate verification being disabled on the curl Kubernetes API fallback path (#24)
- Add a GitHub Actions release workflow that builds a macOS `.dmg` and creates a draft GitHub Release on `v*` tag push
