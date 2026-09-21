# Changelog

All notable changes to ClusterDeck will be documented here.

The format follows the principles of Keep a Changelog and uses semantic versioning where releases are applicable.

## [Unreleased]

- Initial OSS repository bootstrap
- Define macOS-first Tauri/Rust/React architecture
- Add multi-VM SSH bootstrap design
- Add remote kubeconfig management design
- Add Bastion/ProxyJump design
- Add Kubernetes connectivity verification design
- Add on-demand local VM detection (Colima/Lima/Vagrant) that prefills the Profile editor (ADR-0005)
- Add Kubernetes API endpoint discovery (APISIX/Ingress/Istio/Gateway API/Service) for kubeconfig endpoint normalization
- Add `StatusBanner`, `KubeconfigManager`, and `ConfirmModal` UI components
