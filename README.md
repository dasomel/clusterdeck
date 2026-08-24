# ClusterDeck

> A lightweight macOS-first desktop app for discovering, bootstrapping, and connecting to VM and Kubernetes environments.

**English** | [한국어](README-ko.md)

ClusterDeck is designed for environments that are frequently created, deleted, or re-addressed. It keeps a human-friendly Profile name stable while automating SSH access, optional SSH key bootstrap, Bastion/ProxyJump access, remote kubeconfig retrieval, and Kubernetes connectivity verification.

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

See:

- [Architecture](docs/ARCHITECTURE.md)
- [MVP Design](docs/03-mvp-design.md)
- [Contributing](CONTRIBUTING.md)
- [Security](SECURITY.md)
- [Repository Engineering Rules](AGENTS.md)

## Public Repository Safety

This is a public repository. All examples, tests, screenshots, and documentation must use placeholder infrastructure data only. Never commit passwords, private keys, bearer tokens, kubeconfigs, certificates, or real internal addresses.

## License

Apache License 2.0. See [LICENSE](LICENSE).
