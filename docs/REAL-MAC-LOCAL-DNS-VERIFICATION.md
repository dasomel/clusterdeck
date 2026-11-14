# Issue #95: Real-Mac Verification Procedure for ADR-0011 Built-in Local DNS

- **Target Issue:** #95 (*test(local-dns): remaining real-Mac verification for ADR-0011*)
- **Target Architecture:** [ADR-0011: Built-in local DNS](adr/0011-built-in-local-dns.md)
- **Relevant Implementations:**
  - Manager & Lifecycle: [`src/services/local_dns/manager.rs`](../src-tauri/src/services/local_dns/manager.rs)
  - LaunchAgent & Process Gate: [`src/services/local_dns/agent.rs`](../src-tauri/src/services/local_dns/agent.rs)
  - Resolver File Generator: [`src/services/resolver_files.rs`](../src-tauri/src/services/resolver_files.rs)
  - Tauri Local DNS Commands: [`src/commands/local_dns.rs`](../src-tauri/src/commands/local_dns.rs)
- **Author:** QA Auditor, ClusterDeck

---

## 1. Executive Summary & Verification Context

ADR-0011 establishes a built-in authoritative DNS responder on `127.0.0.1:15353` coupled with macOS `/etc/resolver/<zone>` entries. This architecture solves two fundamental macOS issues:
1. **Network.framework (Safari / `URLSession`) Resolution Divergence:** macOS `mDNSResponder` frequently bypasses `/etc/hosts` under modern macOS versions and security extensions, while consistently honoring `/etc/resolver/<zone>`.
2. **Zero-Prompt Record Updates:** Pod/ingress IP address modifications update `~/.clusterdeck/dns/zones.json` in user space without requiring root administrator prompts (`osascript with administrator privileges`).

While unit tests (`services::local_dns::*`, `services::resolver_files::*`, `commands::local_dns::*`) achieve thorough branch coverage using `FakeRunner` and mock filesystems, unit tests **cannot** prove real macOS kernel/daemon behaviors (`launchd` login sessions, AppleScript authentication prompts, `mDNSResponder` multi-path resolution, and the critical 30-second stall failure mode).

This document establishes the exact test matrix, separation between code guarantees and real-Mac requirements, step-by-step verification procedures, and executable verification scripts for all 7 items of Issue #95.

---

## 2. Code-Guaranteed vs. Real-Mac Verification Matrix

| Checklist Item | Code Guarantee (Unit Test Covered) | What MUST Be Verified on Real Mac |
|---|---|---|
| **1. Agent survives logout & login** | `agent::render_plist` renders `RunAtLoad: true`, `KeepAlive: true`, valid XML escaping, 0644/0755 file modes. | Real `launchd` GUI domain (`gui/<uid>`) session lifecycle: Does `launchd` actually spawn `clusterdeck-dns` upon login without TCC/SIP/sandboxing blocks? Does it bind port 15353 cleanly? |
| **2. App move/update binary refresh** | `agent::ensure_binary` compares SHA-256; atomic copy with 0755; `manager::refresh_binary` triggers `launchctl bootout` + `bootstrap`. | Replacing a binary on a live macOS filesystem; `launchd` restarting the process with the new binary without port collision (`EADDRINUSE`); updating `status.json` with the new PID. |
| **3. Port squatter triggers D-DEAD** | Pre-flight ownership gate (`agent::ownership_ok`) checks launchctl PID, status PID, UDP listener, and TCP LISTEN listener; triggers `on_responder_unavailable`. Root script exit 7 gate. | Real macOS `lsof` detecting an external rogue process (e.g. `nc -l 15353`); triggering privileged prompt to remove `/etc/resolver/*`; verifying NO 30s stall occurs in system resolution. |
| **4. Cancelling admin prompt** | `manager::run_script` captures AppleScript `-128` ("User canceled"); `manager::reconcile_publish` reads disk and publishes ONLY on-disk zones. | Real macOS SecurityAgent dialog cancellation; resolver files unchanged; UI surfaces warning; existing zones resolve, cancelled candidate remains pending. |
| **5. "Remove from local DNS" (Teardown)** | `manager::teardown` removes resolver files first, verifies deletion on disk, unloads LaunchAgent, deletes plist & binary, preserves monotonic `generation`. | Privileged prompt executes cleanly; third-party resolver files are untouched; `launchctl` service completely unloaded; files removed; immediate NXDOMAIN in Safari (no 30s hang). |
| **6. Connect refresh (#94 zero-prompt)** | `commands::local_dns::refresh_local_dns_on_connect_detailed` detects unchanged zone set; calls `manager::apply_records_only`; no `build_privileged_script`. | Zero administrator prompt dialogs appear during Connect; `zones.json` updates; responder reloads new generation; Safari immediately connects to the new IP. |
| **7. Safari check after each item** | N/A (Unit tests cannot run WebKit or Network.framework). | Safari and `URLSession` resolve through `mDNSResponder` + `/etc/resolver`; verification that no test leaves Safari in a 30-second hanging state. |

---

## 3. Test Oracle & Safari Validation Methodology

> [!WARNING]
> **`dns-sd -G` is NOT a valid test oracle.**
> On macOS, `dns-sd -G v4 <host>` frequently reports `No Such Record` even while Safari, `URLSession`, and `dscacheutil` resolve successfully through `/etc/resolver`. Never rely on `dns-sd -G` for local DNS verification.

### Two-Path Resolution Model on macOS
1. **libinfo path (`getaddrinfo`):** Used by Terminal `curl`, `dscacheutil`, `kubectl`, Chrome, Firefox.
   - Command: `dscacheutil -q host -a name <host>`
2. **Network.framework path (`mDNSResponder`):** Used by Safari, `URLSession`, macOS system services.
   - Tested via: Headless Swift `URLSession` script with strict timeout and error classification.

### Resolution Outcome Classification in Network.framework (`URLSession`)

| Result | URLSession Code / Status | Meaning |
|---|---|---|
| **Resolved & Connected** | HTTP 200, 404, or `NSURLErrorCannotConnectToHost (-1004)` | **PASS:** DNS resolved successfully to target IP. (HTTP response or TCP connection refusal on port 80/443 proves IP was resolved). |
| **Fast NXDOMAIN** | `NSURLErrorCannotFindHost (-1003)` in **< 0.1s** | **PASS:** Host correctly identified as non-existent without hanging. |
| **Unhealthy Stall** | `NSURLErrorTimedOut (-1001)` or elapsed **> 5.0s** | **FAIL (D-DEAD violation):** Resolver file exists but responder is dead/unreachable, causing mDNSResponder 30s retry stall. |

---

## 4. Item-by-Item Real-Mac Verification Procedures

### Checklist Item 1: LaunchAgent Survives Logout and Login

#### Objective
Verify that `com.clusterdeck.dns` automatically starts upon user GUI login (`RunAtLoad: true`, `KeepAlive: true`) without user action, and resumes serving queries.

#### Pre-conditions
- ClusterDeck installed with at least one profile set to `host_mapping: local_dns`.
- Profile synced: `/etc/resolver/<profile>.clusterdeck.internal` exists.
- Agent running: `launchctl print gui/$(id -u)/com.clusterdeck.dns | grep "state = running"`.

#### Execution and criteria
Use `item1-before`, then log out via the Apple menu, log back in, and run
`item1-after <printed-snapshot-dir>` **before opening ClusterDeck** (see §6).
The script checks a changed launchd PID, matching status PID with `error: null`,
nondecreasing generation, exclusive UDP/TCP ownership, and direct/Safari resolution.
A reused PID needs manual review; a different PID alone does not prove login persistence.
Keep the temporary snapshot across logout; if the OS removes it, repeat the test.

---

### Checklist Item 2: Binary Refresh and Agent Reload on App Move/Update

#### Objective
Verify that when the ClusterDeck application is updated or launched from a new binary, `~/.clusterdeck/bin/clusterdeck-dns` is automatically refreshed to match the new executable's hash, and the LaunchAgent is reloaded via `bootout` + `bootstrap`.

#### Pre-conditions
- ClusterDeck LaunchAgent is currently loaded.
- Record the current binary hash:
  ```bash
  shasum -a 256 ~/.clusterdeck/bin/clusterdeck-dns
  OLD_PID=$(cat ~/.clusterdeck/dns/status.json | jq -r .pid)
  ```

#### Execution Steps
1. Simulate an application update or binary change:
   ```bash
   # Create a dummy modified binary or use a new build
   cp ~/.clusterdeck/bin/clusterdeck-dns /tmp/clusterdeck-dns-temp
   # Invalidate destination hash to trigger update logic
   echo "/* update simulation */" >> ~/.clusterdeck/bin/clusterdeck-dns
   ```
2. Start ClusterDeck (or trigger startup refresh):
   ```bash
   # ClusterDeck app startup triggers `startup_task` -> `refresh_binary`
   # Or run from debug build:
   target/debug/clusterdeck --version
   ```
3. Inspect the updated binary and process state:
   ```bash
   NEW_HASH=$(shasum -a 256 ~/.clusterdeck/bin/clusterdeck-dns | awk '{print $1}')
   NEW_PID=$(cat ~/.clusterdeck/dns/status.json | jq -r .pid)
   echo "Old PID: $OLD_PID, New PID: $NEW_PID"
   ```

#### Verification Criteria
- [ ] `~/.clusterdeck/bin/clusterdeck-dns` file permissions are `0755` (`-rwxr-xr-x`).
- [ ] `~/.clusterdeck/bin/clusterdeck-dns` hash matches the application binary, not the tampered one.
- [ ] LaunchAgent process reloaded: `NEW_PID != OLD_PID`.
- [ ] `~/.clusterdeck/dns/status.json` reflects `NEW_PID` with `error: null`.
- [ ] **Checklist 7 (Safari):** Safari / `URLSession` queries resolve immediately with no socket binding errors.

---

### Checklist Item 3: Port Squatter / D-DEAD

Run `item3` (§6). Only an explicit lowercase `y` authorizes agent bootout. The
script starts its own isolated Python child bound to `127.0.0.1:15353` on both
UDP and TCP. In the open app, click **Sync to local DNS** and approve cleanup.
`manager::apply` ensures the agent, waits for status and checks ownership; a
start/status failure also enters D-DEAD, so the warning need not mention only
"port not owned". Approve the removal prompt and observe the unavailable warning.

Startup also checks ownership and can sweep managed resolver files, but startup
results are not surfaced as the Sync warning. **Connect** uses records-only apply:
it reports unavailable and asks for Sync without privileged cleanup. Use Sync
for this guided test. PASS requires managed resolvers gone, pre-existing foreign
resolvers unchanged, fast negative Safari-stack results, and a successful restore.
Cached positive answers or another resolver may fail the negative check; record
that result rather than treating it as proof of a dangling resolver.

---

### Checklist Item 4: Cancel a New-Zone Approval

Run `item4` (§6) with a healthy existing setup and a new, nonconflicting suggested
zone whose resolver file is absent. Click **Approve <zone>**, then **Cancel** in
the actual macOS admin dialog. Do not use a mode change or a no-op Sync instead.
Observe **Zone Approval Incomplete**. Approval is saved in the profile before
apply; cancellation leaves it pending, rather than rolling back profile settings.

PASS requires all resolver bytes unchanged and published `(zone, profile_id)`
pairs reconciled from known previous entries whose exact owner marker remains
on disk. Arbitrary marked files unknown to desired/previous zones are not
necessarily published. The cancelled candidate must not be published. Generation
may advance; records can refresh. Existing records must still resolve. This does
not assert the absence of every temporary file created by the app. Afterwards,
Sync to install the pending approval or revoke it in the UI.

---

### Checklist Item 5: Remove the Sole Remaining LocalDns Profile

Run `item5` (§6). It first snapshots the full setup. Record original mappings,
then use **Settings** to save every other Local DNS profile as **Off**, approving
changes. Leave exactly one saved `local_dns` profile, then click its
**Remove from local DNS** and approve.

Remove excludes the selected profile for that apply; it does **not** persist a
mapping change. Repeatedly clicking Remove across several saved Local DNS profiles
can recreate other profiles' zones and does not guarantee teardown.

PASS requires owned resolvers removed before teardown, foreign resolver hashes
unchanged, agent explicitly absent from launchd, no port listeners, and these files
absent: `~/Library/LaunchAgents/com.clusterdeck.dns.plist`,
`~/.clusterdeck/bin/clusterdeck-dns`, `~/.clusterdeck/dns/zones.json` and
`status.json`. `generation` stays monotonic; the DNS directory, `endpoints.json`
and `agent.err.log` are not teardown targets. Final snapshots prove final state,
not deletion ordering; ordering is the code guarantee. The script checks fast
negative Safari resolution, guides Sync of the remaining profile, then restoration
of the other saved mappings and Sync, and compares against both baselines.

---

### Checklist Item 6: Connect Refresh (#94 Zero-Prompt Record Update)

#### Objective
Verify that for an active `LocalDns` profile, connecting to the cluster (`Connect`) updates discovered endpoints in `zones.json` with **ZERO administrator prompts** when the zone set is unchanged.

#### Pre-conditions
- Profile `lab` active with `host_mapping: local_dns`.
- Ingress zone `lab.example.internal` is already approved and installed in `/etc/resolver/`.
- Cluster connection credentials / mock tunnel available.

#### Execution Steps
1. Record pre-connect state:
   ```bash
   INITIAL_GEN=$(cat ~/.clusterdeck/dns/status.json | jq -r .generation)
   echo "Initial generation: $INITIAL_GEN"
   ```
2. Trigger an endpoint address change in Kubernetes (e.g. change service IP or mock discovery result).
3. Click **Connect** in ClusterDeck (or trigger `connect_profile`).
4. Observe prompt behavior and post-connect state:
   ```bash
   # Verify no osascript ran
   NEW_GEN=$(cat ~/.clusterdeck/dns/status.json | jq -r .generation)
   echo "New generation: $NEW_GEN"
   
   # Check updated record in zones.json
   cat ~/.clusterdeck/dns/zones.json | jq '.zones[] | select(.zone=="lab.example.internal")'
   
   # Query responder directly
   dig @127.0.0.1 -p 15353 api.lab.example.internal +short
   ```

#### Verification Criteria
- [ ] **ZERO administrator password prompts appear.**
- [ ] `status.json` generation increments: `NEW_GEN > INITIAL_GEN`.
- [ ] `~/.clusterdeck/dns/zones.json` contains the updated endpoint IP.
- [ ] `dig @127.0.0.1 -p 15353 api.lab.example.internal` immediately returns the new IP address.
- [ ] **Checklist 7 (Safari):** Safari / `URLSession` resolves the new IP address immediately.

---

### Checklist Item 7: Safari Resolution Check Across All Scenarios

#### Objective
Ensure Safari / Network.framework functions correctly and never experiences the 30-second lookup stall under each of the 6 test scenarios above.

#### Verification Script: `scripts/verify-safari-resolution.swift`
Resolves through CFHost (the Apple resolver stack Safari uses), then probes with `URLSession`. It exits non-zero on a wrong IP, an unexpected resolution result, a resolver stall (> 4.5 s) or a failed/timed-out probe.

```bash
# Must resolve to the expected cluster IP (exit 3 on mismatch)
swift scripts/verify-safari-resolution.swift api.lab.example.internal --expect-ip 192.0.2.10

# Must fail fast with a confirmed name-not-found (exit 4 if it resolves or fails for another reason)
swift scripts/verify-safari-resolution.swift api.lab.example.internal --expect-failure
```

Exit codes: `0` pass, `1` usage, `2` stall, `3` IP mismatch, `4` unexpected resolution result, `5` URLSession probe failed or timed out. A correct answer slower than 4.5 s also exits `2`. Under `--expect-failure`, only a genuine name-not-found passes; other resolver errors and IPv6-only answers exit `4`.

---

## 5. Automated Verification Test Suite

`scripts/test-real-mac-local-dns.sh` runs the read-only checks in one pass and never modifies system state: LaunchAgent running, `status.json` agrees with launchd and has no error, UDP/TCP port 15353 owned solely by the agent, ClusterDeck-managed `/etc/resolver` files present, `dig @127.0.0.1 -p 15353` per host, and the Safari-stack probe above per host. It prints a per-check PASS/FAIL summary and exits non-zero if any check fails. It requires `jq`.

```bash
scripts/test-real-mac-local-dns.sh api.lab.example.internal=192.0.2.10
scripts/test-real-mac-local-dns.sh --teardown api.lab.example.internal      # after "Remove from local DNS": agent, port and resolver files absent, Safari stack fails fast
scripts/test-real-mac-local-dns.sh --help
```

---

## 6. Guided Lifecycle Script (remaining #95 operator checklist)

Run from a macOS Terminal in the repository, with ClusterDeck open and a healthy,
synced Local DNS setup. Requirements: Bash (including macOS Bash 3.2), Python 3,
`launchctl`, `lsof`, `dig`, and Swift command-line tools. Interactive commands
require a terminal and stop on EOF. Prompts are Korean + English; only `y` counts
as confirmation. Do not run this through an unattended agent.

```bash
./scripts/verify-local-dns-lifecycle.sh --help
./scripts/verify-local-dns-lifecycle.sh snapshot
./scripts/verify-local-dns-lifecycle.sh item1-before
# Apple menu → Log Out; log in; do not open ClusterDeck yet:
./scripts/verify-local-dns-lifecycle.sh item1-after "<printed-state-directory>"
# Open ClusterDeck again for the remaining guided steps:
./scripts/verify-local-dns-lifecycle.sh item3
./scripts/verify-local-dns-lifecycle.sh item4
./scripts/verify-local-dns-lifecycle.sh item5
./scripts/verify-local-dns-lifecycle.sh safari
./scripts/verify-local-dns-lifecycle.sh restore-check "<earlier-state-directory>"
```

| Command | Operator action | PASS means |
|---|---|---|
| `snapshot` | None | State captured successfully, including explicit absence; **not** a health verdict. |
| `item1-before` | Keep printed state directory; log out through Apple menu. | Baseline captured and agent/publication healthy. |
| `item1-after <dir>` | After login, run before opening app. | PID changed, status/generation/ownership healthy, names resolve. |
| `item3` | Answer `y` to bootout; Sync and approve cleanup; attest warning; Sync all profiles after squatter stops. | D-DEAD removed owned files, foreign files unchanged, fast negatives, restored hashes/records/health. |
| `item4` | Approve a new suggested zone, Cancel actual admin prompt, attest warning. | Resolver bytes unchanged, exact known owner pairs reconciled, existing names resolve; saved approval remains pending. |
| `item5` | Save other mappings as Off; Remove on the sole remaining profile; restore mappings and Sync when prompted. | Teardown targets absent, generation retained, fast negatives, both baselines restored. |
| `safari` | None | Existing Swift CFHost/URLSession check passes for every record in every zone; empty zones use a generated negative name. Also checks direct DNS for positive records. |
| `restore-check <dir>` | Restore manually beforehand if needed. | Resolver hashes, binary/plist hashes, zone owners/records match baseline; generation may advance, PID may change; current health and resolution pass. Prints each comparison. |

Each check prints PASS/FAIL; the final summary counts top-level checks (nested
inspection checks are printed separately). Any FAIL, inspection error, interruption,
or declined required confirmation returns nonzero. Read errors, unrecognized
launchctl errors and resolver symlinks fail closed. Positive DNS caching can cause
negative checks to fail; a FAIL remains an unresolved result, not a waived check.
Swift's existing 4.5-second resolver threshold and URLSession result rules apply;
there is no invented sub-100-ms PASS threshold.

Snapshots are private timestamped directories beneath `${TMPDIR}` (fallback
`/tmp`), with a `state-XXXXXX` directory printed for comparisons. They hold owned
resolver contents, hashes of all resolver files, zones/status/generation, plist,
launchctl output/PID, UDP/TCP listener PIDs and binary SHA-256. They may contain
private infrastructure identifiers: keep them local, outside Git and issues.
The script never runs sudo, writes/deletes protected application/system files,
or restores them itself. The app performs every privileged change after human
approval. It signals only its own Python squatter after checking parent PID,
Python command and unique token; TERM precedes KILL, with EXIT/INT/TERM cleanup.
Interrupting does not restore the app configuration: after interruption, open
ClusterDeck and Sync original Local DNS profiles manually. Evidence is retained;
Swift module cache also lives in the temporary work directory.

This script supplies executable operator steps; completing syntax/help/snapshot
checks alone does **not** complete the real-Mac lifecycle checklist.

## 7. QA Audit Conclusion & Sign-off Recommendation

The local DNS codebase in `src-tauri/src/services/local_dns/` and `src-tauri/src/services/resolver_files.rs` provides strict software invariants (PID validation in root scripts, atomic temp files, generation monotonicity, and automatic D-DEAD cleanup routing).

However, because ADR-0011 addresses macOS kernel/daemon-specific quirks (`mDNSResponder` bypassing `/etc/hosts` and the 30-second lookup freeze), **manual sign-off on a physical Mac using the 7 checklist items above is mandatory before closing Issue #95.**
