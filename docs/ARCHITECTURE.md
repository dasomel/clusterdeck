# ClusterDeck Architecture

## 1. Purpose

ClusterDeck is a lightweight macOS desktop application for managing access to frequently recreated VM and Kubernetes environments.

The primary goal is not to manage Kubernetes resources. It is to make a remote environment easy to discover, bootstrap, connect, and verify from a local workstation.

Core flow:

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
Local kubeconfig Profile
        ↓
Kubernetes Connectivity Check
```

## 2. Initial Platform

ClusterDeck initially targets macOS.

The first implementation should prioritize native macOS behavior, Keychain integration, filesystem permissions, and a lightweight menu-bar/desktop experience. Cross-platform support is intentionally deferred until the core workflow is stable.

## 3. Technology Stack

| Layer | Choice | Purpose |
| --- | --- | --- |
| Desktop | Tauri 2 | Lightweight macOS application shell |
| Backend | Rust | SSH orchestration, discovery, configuration, process execution, filesystem operations |
| Frontend | React + TypeScript | Profile and connection UI |
| UI | shadcn/ui-style components | Simple, compact interface |
| Configuration | YAML | Human-readable ClusterDeck profile definitions |
| Local state | Files first | Keep MVP simple; evaluate SQLite later |
| SSH | Native `ssh` / `scp` / `ssh-copy-id` first | Reuse mature OpenSSH behavior during MVP |
| Kubernetes | `kubectl` + kubeconfig parsing | Reuse the standard Kubernetes client configuration model |

### Why Rust + Tauri

The product is primarily a local systems tool. Its important operations are filesystem access, SSH/SCP execution, configuration generation, kubeconfig processing, and local command execution. Tauri keeps the UI lightweight while Rust provides a strong backend boundary for privileged and security-sensitive operations.

The MVP should avoid implementing a complete SSH client unless there is a concrete requirement. Existing OpenSSH commands provide mature support for keys, ProxyJump, known-host behavior, and enterprise SSH configurations.

## 4. Application Layers

```text
┌──────────────────────────────────────────────┐
│ Tauri UI                                     │
│ React / TypeScript                            │
│                                              │
│ Profiles · Hosts · Connect · Status          │
└───────────────────────┬──────────────────────┘
                        │ Tauri Commands
┌───────────────────────▼──────────────────────┐
│ Rust Application Core                         │
│                                              │
│ Profile Service                              │
│ Discovery Service                            │
│ SSH Service                                  │
│ Bastion / Relay Service                      │
│ Kubeconfig Service                           │
│ Cluster Health Service                       │
│ Local Configuration Service                   │
└───────────────┬───────────────┬──────────────┘
                │               │
        ┌───────▼──────┐  ┌────▼──────────┐
        │ OpenSSH      │  │ kubectl       │
        │ ssh/scp/etc. │  │ kubeconfig    │
        └──────────────┘  └───────────────┘
```

## 5. Profile Model

A Profile is the primary unit of user interaction. Users should think in terms of environments rather than IP addresses.

Example:

```yaml
profiles:
  cka-lab:
    name: CKA Lab
    hosts:
      - name: cka-m1
        address: 192.168.56.10
        user: root
        port: 22
        identity_file: ~/.ssh/cka
      - name: cka-w1
        address: 192.168.56.11
        user: root
        port: 22
        identity_file: ~/.ssh/cka
    kubeconfig:
      remote: /etc/kubernetes/admin.conf
      local: ~/.clusterdeck/kubeconfigs/cka-lab.yaml
      context: cka-lab
```

The actual implementation must not require these example values. Public repository documentation must use placeholder addresses and credentials only.

## 6. Multi-Host SSH Bootstrap

ClusterDeck should generalize the existing multi-VM SSH automation pattern.

Capabilities:

- Discover hosts using CIDR or explicitly supplied IP addresses.
- Map stable host/profile names to current IP addresses.
- Probe SSH connectivity before making changes.
- Optionally use an initial password only for bootstrap.
- Deploy the local public key using `ssh-copy-id` or an equivalent mechanism.
- Verify key-based authentication using non-interactive SSH.
- Support configurable retries and delay between retries.
- Retry selected hosts without rerunning successful hosts.
- Provide dry-run and connection-test modes.
- Report per-host success/failure.
- Generate or update ClusterDeck-owned SSH aliases.

The initial password is bootstrap-only and must never be stored in the repository or included in diagnostic logs.

## 7. SSH Configuration Ownership

ClusterDeck must not rewrite an entire user-managed `~/.ssh/config` file.

Preferred model:

```text
~/.ssh/config
    ↓
Include ~/.clusterdeck/ssh/*.conf
```

ClusterDeck owns only its generated configuration files under `~/.clusterdeck/ssh/`.

This allows the user to keep unrelated SSH settings untouched.

## 8. Bastion / Relay

Profiles may define a Bastion host when targets are not directly reachable.

```text
Local
  │
  └── SSH / ProxyJump
       ↓
   Bastion
       ├── Control Plane
       ├── Worker 01
       └── Worker 02
```

The model should support:

- Bastion host, user, port, and identity file.
- Target host definitions.
- Automatic `ProxyJump` generation.
- Target SSH verification through the Bastion.
- Multi-host bootstrap through the Bastion.
- kubeconfig fetch through the Bastion.
- Direct and Bastion access modes within the same Profile.
- Multiple ProxyJump hops as a later extension.

## 9. Remote kubeconfig Flow

ClusterDeck should retrieve a kubeconfig from a selected control-plane host instead of assuming a fixed cluster type.

```text
Profile
  ↓
Control-plane candidate
  ↓
SSH / SCP
  ↓
Remote kubeconfig
  ↓
Parse
  ↓
Normalize endpoint + names
  ↓
Store local Profile kubeconfig
```

The source may be a conventional path such as `/etc/kubernetes/admin.conf`, or a user-configured remote path.

The implementation should support:

- Selecting the kubeconfig source host.
- Fetching kubeconfig over SSH/SCP.
- Parsing and validating kubeconfig data.
- Embedding certificate/key material when necessary.
- Replacing loopback or internal API endpoints with a reachable endpoint when the Profile provides one.
- Stable Profile-based cluster/user/context naming.
- Backing up the existing local kubeconfig before a destructive merge.
- Avoiding accidental deletion of unrelated contexts.
- Restrictive local file permissions.

## 10. Local kubeconfig Management

ClusterDeck should keep its generated kubeconfigs separate from unrelated user-managed files during the MVP.

Recommended layout:

```text
~/.clusterdeck/
├── profiles.yaml
├── ssh/
│   ├── cka-lab.conf
│   └── dev.conf
└── kubeconfigs/
    ├── cka-lab.yaml
    └── dev.yaml
```

The application may later provide optional integration with the user's main `~/.kube/config`, but the generated source-of-truth should remain under `~/.clusterdeck/`.

## 11. Cluster Verification

After SSH and kubeconfig setup, ClusterDeck verifies the environment.

Minimum verification:

```bash
kubectl --kubeconfig <profile-kubeconfig> get nodes
```

The UI should distinguish:

```text
SSH             ✓
Kubeconfig      ✓
Kubernetes API  ✓
```

Optional metadata:

- Kubernetes version.
- Node count.
- API endpoint.
- Connection latency.
- Last successful verification time.

## 12. UI Direction

The UI should feel closer to SwitchHosts than to a traditional infrastructure management console.

Primary interaction:

```text
┌─────────────────────────────────────┐
│ ClusterDeck                    ●    │
├─────────────────────────────────────┤
│ ● CKA Lab                       ✓   │
│   3 hosts · Kubernetes ✓             │
│                                     │
│ ○ Dev Cluster                    ✓  │
│   5 hosts · Bastion                  │
│                                     │
│ ○ Test Cluster                   !  │
│   2 hosts · SSH failed               │
├─────────────────────────────────────┤
│          [ Connect / Sync ]          │
└─────────────────────────────────────┘
```

The main interaction should require as few clicks as possible. Detailed configuration can be secondary.

## 13. Security Principles

- Never commit passwords, private keys, kubeconfigs, bearer tokens, or real infrastructure addresses.
- Use macOS Keychain or another secure local secret store for credentials that must persist.
- Do not print secrets in logs or error messages.
- Use restrictive permissions for generated configuration and kubeconfig files.
- Treat initial password authentication as a one-time bootstrap mechanism.
- Avoid modifying user-managed SSH/Kubernetes configuration outside the ClusterDeck-owned area.
- Make destructive actions explicit and reversible where possible.
- Keep all network and filesystem operations in the Rust backend rather than the frontend.

## 14. MVP Boundaries

The first implementation should focus on:

1. Profile CRUD.
2. Multi-host IP discovery and SSH bootstrap.
3. SSH alias generation.
4. Bastion/ProxyJump support.
5. Remote kubeconfig fetch and normalization.
6. Local Profile kubeconfig storage.
7. Kubernetes connectivity verification.
8. Minimal macOS UI for selecting and connecting to a Profile.

VM-provider-specific IP discovery, advanced SSH chaining, automatic kubeconfig discovery, and cross-platform support are later phases.
