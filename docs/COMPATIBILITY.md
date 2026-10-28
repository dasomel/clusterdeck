# Compatibility & Code Signing Policy

This document defines the configuration, state, and runtime compatibility guarantees for ClusterDeck, along with code signing and distribution policy leading to the 1.0.0 release.

---

## 1. State & Configuration Compatibility Guarantees

ClusterDeck commits to forward-compatible, non-destructive state management across application upgrades.

### 1.1 `profiles.yaml` (Lossless Store)
- **Lossless Persistence:** Starting with version 0.5.0+, the store preserves unrecognized YAML keys, custom comments, and future schema attributes across all edit, save, and deletion operations.
- **Invalid Profile Protection:** If a profile fails validation (e.g., syntactically broken or unparseable), it is never silently dropped or deleted when saving other profiles.
- **Optimistic Locking:** Saves include `base_fingerprint` validation to prevent concurrent edits from overwriting each other.
- **Atomic Writes:** All updates to `profiles.yaml` are written through temporary sibling files and atomically renamed.

### 1.2 User State Directory (`~/.clusterdeck/`)
All ClusterDeck runtime state resides exclusively under `~/.clusterdeck/` and is owned by the local user account:

```text
~/.clusterdeck/
├── profiles.yaml          # Authoritative profiles store
├── state.json             # Verification cache and API tunnel targets
├── ssh/
│   └── <id>.conf          # Generated OpenSSH configuration per profile
├── kubeconfigs/
│   └── <id>.yaml          # Normalized, isolated kubeconfig per profile
├── tunnels/
│   └── <id>.json          # Active tunnel PID and metadata (auto-reaped on startup)
├── bin/
│   └── clusterdeck-dns    # Standalone DNS responder binary (mode 0755)
└── dns/
    ├── zones.json         # Authoritative zone definitions & A records
    ├── status.json        # Responder runtime status & loaded generation
    ├── generation         # Monotonic generation counter
    └── endpoints.json     # Discovered endpoints cache per profile
```

- Upgrades will never wipe `~/.clusterdeck/`.
- New directory entries or state schemas must provide safe defaults (`#[serde(default)]`) so that existing state files remain valid without manual intervention.

### 1.3 System Integration Files (`/etc/resolver` & `/etc/hosts`)
- **Strict Scoping & Ownership Markers:**
  - `/etc/resolver/<zone>` entries require `# managed by ClusterDeck (profile: <id>)`.
  - `/etc/hosts` uses `# >>> ClusterDeck BEGIN (profile: <id>) >>>` / `END` marker pairs.
- ClusterDeck will **never** modify or delete unmanaged lines or third-party resolver files.
- When invalid profiles exist or the YAML document shape is corrupted, ClusterDeck engages resolver protection mode (`protect_resolver`), blocking global resolver sweeps or teardowns.
- **Node alias domain rename (Unreleased, BREAKING, #119):** node aliases and per-profile alias zones moved from `*.clusterdeck.local` to `*.clusterdeck.internal`, because negative lookups under mDNS `.local` stall about 5 s on macOS. On the next Sync/apply ClusterDeck removes its own old `/etc/resolver/<label>.clusterdeck.local` files (marker-owned only) and drops legacy `.clusterdeck.local` lines from its hosts block; this needs one admin prompt. Bookmarks, SSH configs or scripts that use the old names must be updated. `clusterdeck.local` stays reserved and cannot be approved as a zone. Downgrading to a version before this change recreates the `.local` zones and aliases.

---

## 2. Downgrade Policy & Behavior

ClusterDeck designs state to be backward-tolerable, but downgrades below schema boundary versions require attention:

### 2.1 Downgrading Below 0.5.0
- **Context:** Version 0.5.0 transitioned profile name resolution from legacy boolean `manage_hosts_file: bool` to the extensible enum `host_mapping: 'hosts' | 'local_dns' | 'off'`.
- **Behavior on Downgrade:** Versions prior to 0.5.0 will not recognize `host_mapping`. They will fall back to reading `manage_hosts_file` (if present) or default to `false`.
- **Automatic Backup:** During the 0.5.0 upgrade migration, ClusterDeck creates an automatic backup file at:
  ```text
  ~/.clusterdeck/profiles.yaml.pre-local-dns-backup
  ```
- **Recovery:** If rolling back to pre-0.5.0, restore the pre-migration backup:
  ```bash
  cp ~/.clusterdeck/profiles.yaml.pre-local-dns-backup ~/.clusterdeck/profiles.yaml
  ```

### 2.2 Downgrading Below 0.4.0
- Versions prior to 0.4.0 do not use isolated kubeconfigs under `~/.clusterdeck/kubeconfigs/`. Existing cluster connections may require clicking **Connect** again to re-fetch kubeconfigs.

---

## 3. Code Signing & Gatekeeper Policy

### 3.1 Current Status (v0.5.x / 1.0.0-rc)
- ClusterDeck is currently built with **ad-hoc code signing** (`signingIdentity: "-"` in `tauri.conf.json`).
- Because ClusterDeck is distributed as an open-source tool without an active Apple Developer ID certificate:
  1. macOS Gatekeeper may flag the `.app` or `.dmg` on first download ("unidentified developer").
  2. Users must clear the quarantine attribute after copying to `/Applications`:
     ```bash
     xattr -d com.apple.quarantine /Applications/ClusterDeck.app
     ```
  3. The internal DNS responder copied to `~/.clusterdeck/bin/clusterdeck-dns` inherits the ad-hoc signature of the source bundle. It runs as a user LaunchAgent without kernel/system extension entitlements.

### 3.2 1.0.0 Release Strategy
- **Initial 1.0.0 Release:** Released as a self-built / open-source developer tool with detailed installation and Gatekeeper verification guidance.
- **Future Developer ID Signing:** Setting up Apple Developer Program enrollment, Developer ID Application certificates, and automated Apple Notarization in `.github/workflows/release.yml` is scheduled as a post-1.0 milestone once project sponsorship/credentials are provisioned.
