# ClusterDeck

> A lightweight macOS-first desktop app for discovering, bootstrapping, and connecting to VM and Kubernetes environments.

**English** | [한국어](README-ko.md)

ClusterDeck is designed for environments that are frequently created, deleted, or re-addressed. It keeps a human-friendly Profile name stable while automating SSH access, optional SSH key bootstrap, Bastion/ProxyJump access, remote kubeconfig retrieval, and Kubernetes connectivity verification.

## Current Status

ClusterDeck is an **early MVP / source-first project**. The repository currently documents and implements the workstation-access flow around Profiles, SSH, bastion/ProxyJump, kubeconfig retrieval/normalization, and Kubernetes connectivity checks.

The supported first-time path is development from source with Tauri. Do not treat packaged-app distribution, broad fleet management, or a general Kubernetes administration console as established product capabilities unless a release or repository documentation explicitly says so.

## Core Flow

```text
IP / Host Discovery
        ↓
SSH Connectivity
        ↓
SSH Bootstrap (optional)
        ↓
SSH Alias / ProxyJump
        ↓
Remote kubeconfig Fetch
        ↓
kubeconfig Normalization
        ↓
Local Profile
        ↓
Kubernetes Connectivity Check
```

## Initial Scope

- macOS-first desktop application
- Tauri 2 + Rust backend
- React + TypeScript frontend
- Multi-VM host Profiles
- SSH key bootstrap and alias management
- Bastion / ProxyJump support
- Remote kubeconfig fetch and normalization
- `kubectl` connectivity verification

ClusterDeck is not intended to become a general Kubernetes administration console.

## First Verified Success

For a new environment, the product outcome is not merely that the desktop app starts. A Profile has reached **first verified success** when the same workflow proves all three layers:

1. **SSH** — ClusterDeck can reach the selected host, directly or through the configured bastion.
2. **kubeconfig** — the remote kubeconfig is fetched and normalized into the local Profile without exposing credentials in logs or documentation.
3. **Kubernetes API** — the resulting context can make a real API call such as `kubectl get nodes`.

A useful manual cross-check while developing is:

```bash
ssh <profile-host> true
kubectl --context <normalized-context> get nodes
```

If SSH succeeds but the Kubernetes API fails, treat that as a partial connection rather than a successful Profile. See the architecture and MVP design documents for the boundary between discovery, SSH bootstrap, kubeconfig handling, and Kubernetes verification.

## Development

```bash
pnpm install
pnpm tauri dev
```

Validation:

```bash
pnpm build
cargo check --manifest-path src-tauri/Cargo.toml
```

## Documentation Map

- [Architecture](docs/ARCHITECTURE.md) — workstation access layers and component boundaries
- [MVP Design](docs/03-mvp-design.md) — intended MVP behavior and design detail
- [Contributing](CONTRIBUTING.md) — contribution workflow
- [Security](SECURITY.md) — vulnerability reporting and security policy
- [Repository Engineering Rules](AGENTS.md) — repository-local engineering contract

When an implementation detail and an older design note disagree, current source and explicitly verified behavior take precedence; update the design document in the same change when the architecture boundary changes.

## Public Repository Safety

This is a public repository. All examples, tests, screenshots, and documentation must use placeholder infrastructure data only. Never commit passwords, private keys, bearer tokens, kubeconfigs, certificates, or real internal addresses.

## Contributing / Feedback

External feedback is especially useful for environments with changing VM addresses, bastions, and remote kubeconfigs. Report reproducible failures through GitHub Issues and include only sanitized infrastructure details.

## License

Apache License 2.0. See [LICENSE](LICENSE).
