# ClusterDeck MVP Design

## 1. Direction

ClusterDeck follows the proven local-desktop pattern used by KubeMetal: Tauri v2 provides the macOS application shell, Rust owns native/system operations, and React/TypeScript owns the interface.

ClusterDeck is intentionally much smaller. The initial app is a connection switcher, not a Kubernetes management console.

## 2. MVP workflow

```text
Profile
  ↓
Host discovery / host selection
  ↓
SSH probe
  ↓
Optional password bootstrap
  ↓
SSH public-key verification
  ↓
SSH alias / ProxyJump configuration
  ↓
Control-plane selection
  ↓
Remote kubeconfig fetch
  ↓
Endpoint + context normalization
  ↓
Local kubeconfig profile
  ↓
kubectl connectivity check
```

## 3. Directory structure

```text
clusterdeck/
├── src/
│   ├── App.tsx
│   ├── main.tsx
│   └── styles.css
├── src-tauri/
│   ├── src/
│   │   ├── main.rs
│   │   ├── lib.rs
│   │   ├── commands/
│   │   │   ├── mod.rs
│   │   │   ├── app.rs
│   │   │   ├── profiles.rs
│   │   │   └── connection.rs
│   │   └── services/
│   │       ├── mod.rs
│   │       ├── config.rs
│   │       └── process.rs
│   ├── capabilities/
│   │   └── default.json
│   ├── Cargo.toml
│   └── tauri.conf.json
├── docs/
│   ├── ARCHITECTURE.md
│   ├── SECURITY.md
│   └── 03-mvp-design.md
├── index.html
├── package.json
├── tsconfig.json
└── vite.config.ts
```

## 4. Frontend structure

The first UI deliberately follows a compact macOS utility pattern rather than a large platform console.

```text
App
├── Sidebar
│   └── Profile list
├── Header
│   └── selected Profile + refresh
├── Connect card
├── Hosts card
├── Kubernetes card
└── Connection flow card
```

Later these components should move into feature directories following the KubeMetal organization style:

```text
src/components/
├── profiles/
├── hosts/
├── connection/
├── kubeconfig/
├── status/
└── common/
```

## 5. Rust backend structure

Rust is the system boundary. Frontend code must not execute SSH, SCP, kubectl, filesystem writes, or credential handling directly.

```text
commands/
  app.rs             app metadata
  profiles.rs        profile CRUD/load/save
  connection.rs      high-level connection workflow

services/
  process.rs         executable discovery + async command runner
  config.rs          Profile/Host/Bastion/kubeconfig domain types
```

The existing process helper intentionally searches common macOS paths because bundled `.app` processes do not necessarily inherit the user's login-shell PATH. This follows the same defensive pattern used by KubeMetal.

## 6. Process execution policy

For MVP, prefer mature system tools:

- `ssh`
- `scp`
- `ssh-copy-id` when available
- `kubectl`

Do not implement a complete SSH protocol stack in the first release.

The Rust backend should:

- resolve executables using known macOS paths;
- use asynchronous process execution;
- capture stdout/stderr separately;
- never print passwords/private keys/kubeconfig payloads;
- return structured results to the UI.

## 7. Profile domain

A Profile is the user-facing environment identity. IP addresses are mutable attributes.

```yaml
profiles:
  cka-lab:
    name: CKA Lab
    hosts:
      - name: cka-m1
        address: 192.0.2.10
        user: root
        port: 22
        identity_file: ~/.ssh/cka
      - name: cka-w1
        address: 192.0.2.11
        user: root
        port: 22
        identity_file: ~/.ssh/cka
    bastion: null
    kubeconfig:
      remote_path: /etc/kubernetes/admin.conf
      control_plane: cka-m1
      local_path: ~/.clusterdeck/kubeconfigs/cka-lab.yaml
      context: cka-lab
```

Example addresses are documentation-only placeholders.

## 8. Local state

The MVP uses files instead of a database:

```text
~/.clusterdeck/
├── profiles.yaml
├── ssh/
│   └── <profile>.conf
└── kubeconfigs/
    └── <profile>.yaml
```

The application should never require a repository checkout to operate.

## 9. UI state machine

A Profile should expose explicit stages:

```text
Discovered
   ↓
SSH Ready
   ↓
kubeconfig Synced
   ↓
Kubernetes Verified
```

Failures should identify the stage rather than showing one generic connection error.

## 10. KubeMetal-derived implementation conventions

The KubeMetal project provides useful conventions to reuse conceptually:

- Tauri v2 + Rust + React/TypeScript.
- Thin `main.rs`, application setup in the Rust library entrypoint.
- `commands/` for Tauri IPC commands.
- `services/` for native process/system integration.
- Explicit CLI path resolution for packaged macOS apps.
- Feature-oriented React components as the UI grows.
- Documentation separated into proposal, requirements, MVP, architecture, and security concerns.

ClusterDeck should not copy KubeMetal-specific infrastructure logic, commands, or application features. The reuse is structural and architectural only.

## 11. First implementation sequence

1. Make the Tauri app build and launch on Apple Silicon macOS. — implemented
2. Replace sample profile data with `~/.clusterdeck/profiles.yaml`. — implemented
3. Implement SSH host probe and retry. — implemented
4. Implement optional password bootstrap and key verification. — implemented
5. Generate ClusterDeck-owned SSH aliases. — implemented
6. Implement control-plane kubeconfig fetch and normalization. — implemented
7. Implement local kubeconfig storage. — implemented
8. Implement Kubernetes verification. — implemented
9. Add Bastion/ProxyJump workflow. — implemented
10. Add IP discovery and richer status refresh.

