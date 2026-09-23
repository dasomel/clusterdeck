# Changelog

All notable changes to ClusterDeck will be documented here.

The format follows the principles of Keep a Changelog and uses semantic versioning where releases are applicable.

## [Unreleased]

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
