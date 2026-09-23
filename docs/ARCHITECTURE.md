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
| SSH | Native `ssh` / `ssh-copy-id` first | Reuse mature OpenSSH behavior during MVP |
| Kubernetes | `kubectl` + kubeconfig parsing | Reuse the standard Kubernetes client configuration model |

### Why Rust + Tauri

The product is primarily a local systems tool. Its important operations are filesystem access, SSH execution, configuration generation, kubeconfig processing, and local command execution. Tauri keeps the UI lightweight while Rust provides a strong backend boundary for privileged and security-sensitive operations.

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
        │ ssh/etc.     │  │ kubeconfig    │
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

### 6a. Explicit SSH Authentication Mode

Each host also carries an explicit `auth` mode: `key` (default) or `password`. This is a
separate, ongoing setting from the bootstrap-only password above:

- `key` (default): the existing OpenSSH key-based flow. Profile YAML written before this field
  existed has no `auth` key and deserializes as `key`, so no migration is required.
- `password`: SSH connects using `sshpass -e` with the password carried through the `SSHPASS`
  environment variable, never as a `-p <password>` argv element or written to disk. Connect,
  Test Connection, and kubeconfig fetch all honor the selected mode. The password itself lives
  only in frontend React state for the duration of the action and is cleared afterward; it is
  never persisted to profile YAML, generated SSH config, or logs.
- Password mode applies to the target host only. When a profile uses a bastion, the bastion hop
  still authenticates with its own key (`Bastion.identity_file`); ClusterDeck does not support
  password authentication for the ProxyJump hop itself.
- `BatchMode=yes` (used everywhere else to avoid hanging on an interactive prompt) is omitted for
  password-mode connections, since it would block the password prompt `sshpass` answers.
  `StrictHostKeyChecking=accept-new` still applies in both modes.

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

### 7.1 Optional `/etc/hosts` Managed Block

For profiles that opt in via `manage_hosts_file: true`, ClusterDeck also maps each host and
bastion to a stable, namespaced hostname in `/etc/hosts`:

```text
<host-or-bastion-name>.<profile-id>.clusterdeck.local
```

The namespacing by profile id avoids collisions when two profiles happen to name a host the
same thing. As with SSH configuration, ClusterDeck owns only its own marker block per profile
and never touches any other line in the file:

```text
# >>> ClusterDeck BEGIN (profile: <profile-id>) >>>
<address> <name>.<profile-id>.clusterdeck.local
# <<< ClusterDeck END (profile: <profile-id>) <<<
```

Writing `/etc/hosts` requires root, so ClusterDeck performs a single
`osascript ... with administrator privileges` call per `connect_profile` invocation (never more
than one password prompt per action) rather than running the whole app elevated. The block is
written as part of `connect_profile` when the flag is set, and removed when the profile is
deleted. A cancelled password prompt is treated as a non-fatal stage failure, the same way a
failed kubeconfig fetch or alias write is: it is recorded in `ConnectionResult.errors` and does
not abort the rest of the pipeline. This flag defaults to `false` and currently has no UI
toggle — it is set by editing `profiles.yaml` directly, matching the deferred Profile-creation
UI elsewhere in this MVP.

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
SSH exec
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
- Fetching kubeconfig over SSH exec.
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

## 14. Local Runtime Detection (Profile Prefill + Settings Dashboard)

`services/local_runtime.rs` and `commands/local_runtime.rs` expose one Tauri command,
`detect_local_hosts`, that concurrently probes Colima, Lima, and Vagrant on the local machine
and returns a vendor-neutral `Vec<DiscoveredLocalHost>` (including `arch`, `cpus`,
`memory_bytes`, `disk_bytes`, and an associated `docker_context` where one can be resolved;
Vagrant has no equivalent metadata and stays `None`). It is not a standing observation surface
in the sense of a background poll, but it now has two triggers: the Profile editor calls it only
when the user clicks "Detect local VM", and detected hosts there are held in editor component
state only, never persisted, until the user applies a host to the in-progress form and presses
Save (`Profile` gains no schema for this; a saved host is an ordinary SSH host entry). Settings
(`KubeconfigManager`'s read-only "Local Runtime" section) additionally calls the same command
on load, purely for display — its results are never persisted or offered as prefill. See
[ADR-0005](adr/0005-local-host-detection-prefills-profiles.md) for the full prefill design and
its relationship to the earlier, differently-scoped
[ADR-0003](adr/0003-colima-lima-local-runtime-provider.md).

## 15. Kubernetes Endpoint Discovery

`services/k8s_endpoints.rs` queries a profile's cluster (via `kubectl --kubeconfig` or a `curl`
fallback, the latter needed when `kubectl` itself fails, e.g. macOS Sequoia Local Network
Privacy) to discover reachable API endpoints exposed through APISIX, Ingress, Istio, Gateway
API, or plain Services, for use when normalizing a fetched kubeconfig's server endpoint. TLS
client certificate/key material extracted during this process is written to owner-only (0600)
temporary files and unconditionally cleaned up, including on failure paths. The `curl` fallback
verifies the API server's TLS certificate using `--cacert` against the kubeconfig's
`certificate-authority-data`/`certificate-authority` when present, or curl's system trust store
otherwise — it never passes `-k`/`--insecure`.

## 16. Private CA Local Trust

`services/ca_trust.rs` and `commands/ca_trust.rs` resolve the Kubernetes `Secret` backing a
discovered Ingress/ApisixTls endpoint's TLS termination, fetch its `ca.crt` only (`tls.key` and
any other Secret field are discarded server-side and never cross the Tauri IPC boundary), and
fingerprint it natively (SHA-256/SHA-1). Each `Profile` persists a `trusted_cas` record keyed by
the originating Secret ref; on each discovery pass the freshly-fetched CA's fingerprint is
compared against that record to compute a `New` / `Trusted` / `Rotated` status per CA, since these
clusters are frequently torn down and recreated with a new self-signed bootstrap CA. Trusting,
replacing (untrust-then-trust, for a `Rotated` CA), and removing a CA all go through `security(1)`
against the macOS **login** keychain (never System), scoped to the `ssl` policy and to that CA's
SHA-1 fingerprint, so the user always confirms a macOS authorization prompt and ClusterDeck only
ever touches keychain trust entries it created itself. The endpoints view and the Settings
"Trusted CAs" list (`KubeconfigManager`, cross-profile) both surface trust/replace/remove actions.
`istio`/`gateway-api`-sourced endpoints are not yet resolved (v1 gap, not a design constraint —
see [ADR-0006](adr/0006-private-ca-local-trust.md) for the full design and security rationale).

Each discovered CA also carries a `warnings: Vec<String>` computed from `openssl x509 -noout
-text`/`-checkend` (macOS's LibreSSL has no `-ext` flag, hence text-parsing): missing `serverAuth`
EKU, leaf/CA expiry within 14/30 days respectively (with a distinct already-expired message), a
leaf SAN that does not cover a discovered host, and a CA that is structurally invalid (missing
CA:TRUE or keyCertSign). A genuine `-checkend` result is always silent on stdout/stderr — only
the exit code carries it — so a non-zero exit *with* stderr output (e.g. an unreadable temp file)
is treated as a tooling failure rather than expiry, avoiding a prior false-positive.

## 17. MVP Boundaries

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
