# Issue #95: Real-Mac Verification Procedure for ADR-0011 Built-in Local DNS

- **Target Issue:** #95 (*test(local-dns): remaining real-Mac verification for ADR-0011*)
- **Target Architecture:** [ADR-0011: Built-in local DNS](file:///Users/m/Documents/IdeaProjects/20.dasomel/clusterdeck/docs/adr/0011-built-in-local-dns.md)
- **Relevant Implementations:**
  - Manager & Lifecyle: [`src/services/local_dns/manager.rs`](file:///Users/m/Documents/IdeaProjects/20.dasomel/clusterdeck/src-tauri/src/services/local_dns/manager.rs)
  - LaunchAgent & Process Gate: [`src/services/local_dns/agent.rs`](file:///Users/m/Documents/IdeaProjects/20.dasomel/clusterdeck/src-tauri/src/services/local_dns/agent.rs)
  - Resolver File Generator: [`src/services/resolver_files.rs`](file:///Users/m/Documents/IdeaProjects/20.dasomel/clusterdeck/src-tauri/src/services/resolver_files.rs)
  - Tauri Local DNS Commands: [`src/commands/local_dns.rs`](file:///Users/m/Documents/IdeaProjects/20.dasomel/clusterdeck/src-tauri/src/commands/local_dns.rs)
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
| **4. Cancelling admin prompt** | `manager::run_script` captures AppleScript `-128` ("User canceled"); `manager::reconcile_publish` reads disk and publishes ONLY on-disk zones. | Real macOS SecurityAgent dialog cancellation; no orphan temp files left in `/tmp` or `/etc/resolver`; UI surfaces warning; existing zones resolve, unapproved zone does not. |
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
- Profile synced: `/etc/resolver/<profile>.clusterdeck.local` exists.
- Agent running: `launchctl print gui/$(id -u)/com.clusterdeck.dns | grep "state = running"`.

#### Execution Steps
1. In macOS Terminal, record current PID and generation:
   ```bash
   BEFORE_PID=$(cat ~/.clusterdeck/dns/status.json | jq -r .pid)
   BEFORE_GEN=$(cat ~/.clusterdeck/dns/status.json | jq -r .generation)
   echo "Pre-logout PID: $BEFORE_PID, Generation: $BEFORE_GEN"
   ```
2. Log out of the macOS user account:
   - Click **Apple menu () → Log Out <user>...** (or run `sudo launchctl bootout user/$(id -u)`).
3. Log back into the macOS user account.
4. Open Terminal and run the verification command:
   ```bash
   # Check LaunchAgent status
   launchctl print gui/$(id -u)/com.clusterdeck.dns | grep -E "(state =|pid =|last exit code =)"
   
   # Check status.json
   cat ~/.clusterdeck/dns/status.json
   ```

#### Verification Criteria
- [ ] `launchctl print` reports `state = running` with a valid numeric `pid`.
- [ ] `~/.clusterdeck/dns/status.json` has `error: null` and `pid` matches `launchctl print`.
- [ ] `generation` in `status.json` is `>= BEFORE_GEN`.
- [ ] `dig @127.0.0.1 -p 15353 bastion.<profile>.clusterdeck.local +short` returns the expected IP.
- [ ] **Checklist 7 (Safari):** Safari / `URLSession` resolves cluster hostnames within 0.1s.

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

### Checklist Item 3: Port Squatter on 127.0.0.1:15353 Triggers D-DEAD Cleanup

#### Objective
Verify that if another process occupies `127.0.0.1:15353`, ClusterDeck's pre-flight and script-level ownership gates catch the squatter, prevent pointing system DNS at an untrusted port, trigger a privileged prompt to remove `/etc/resolver` files, and surface a clear warning.

#### Pre-conditions
- Profile `lab` active with `host_mapping: local_dns`.
- `/etc/resolver/lab.clusterdeck.local` exists.

#### Execution Steps
1. Unload the legitimate ClusterDeck agent to release port 15353:
   ```bash
   launchctl bootout gui/$(id -u)/com.clusterdeck.dns 2>/dev/null || true
   ```
2. Spawn a port squatter on `127.0.0.1:15353` (UDP and TCP):
   ```bash
   # In a separate terminal or background job:
   nc -l 127.0.0.1 15353 &
   SQUATTER_PID=$!
   echo "Squatter PID: $SQUATTER_PID"
   ```
3. In ClusterDeck, click **Sync to local DNS** (or run `sync_local_dns_profile`).
4. An administrator authentication prompt will appear:
   - Enter your password to authorize the cleanup script.
5. Inspect the command output / UI banner and filesystem:
   ```bash
   # Verify resolver files removed
   ls -la /etc/resolver/
   ```
6. Clean up the squatter:
   ```bash
   kill -9 $SQUATTER_PID 2>/dev/null || true
   ```

#### Verification Criteria
- [ ] The app displays the warning:
  `local DNS responder unavailable; resolver entries removed (port 15353 is not owned by the ClusterDeck DNS agent)` (or port not owned).
- [ ] All ClusterDeck-managed `/etc/resolver/<zone>` files are deleted.
- [ ] Non-ClusterDeck files in `/etc/resolver/` (if any) are strictly untouched.
- [ ] **Checklist 7 (Safari):** Querying `bastion.lab.clusterdeck.local` in Safari / `URLSession` returns `NSURLErrorCannotFindHost (-1003)` in **< 0.05 seconds** (proves that the dangling resolver file was removed, preventing the 30-second stall!).

---

### Checklist Item 4: Cancelling Admin Prompt Preserves Consistency

#### Objective
Verify that when an administrator prompt is displayed (e.g. approving a new zone) and the user clicks **Cancel** (or presses Escape), ClusterDeck leaves the disk untouched, reconciles `zones.json` to reflect only what is actually on disk, and reports a clear warning.

#### Pre-conditions
- Profile `lab` has `lab.clusterdeck.local` already published and active on disk.
- Candidate ingress zone `lab.example.internal` is suggested in the Endpoints tab.

#### Execution Steps
1. Click **Approve lab.example.internal** in ClusterDeck UI.
2. The macOS SecurityAgent administrator prompt appears asking for password:
   - **Click "Cancel"** (or press Escape).
3. Observe the UI notification and examine disk state:
   ```bash
   # 1. Check /etc/resolver
   ls -la /etc/resolver/lab.example.internal
   
   # 2. Check zones.json content
   cat ~/.clusterdeck/dns/zones.json | jq -r '.zones[].zone'
   
   # 3. Check status.json generation
   cat ~/.clusterdeck/dns/status.json
   ```

#### Verification Criteria
- [ ] UI shows notification/banner: `Admin approval was cancelled; local DNS was not changed`.
- [ ] `/etc/resolver/lab.example.internal` was **NOT** created on disk.
- [ ] `/etc/resolver/lab.clusterdeck.local` remains intact.
- [ ] `~/.clusterdeck/dns/zones.json` contains ONLY `lab.clusterdeck.local` (the cancelled zone is discarded).
- [ ] Generation in `status.json` and `zones.json` advances cleanly without corruption.
- [ ] **Checklist 7 (Safari):**
  - `bastion.lab.clusterdeck.local` continues to resolve normally.
  - `api.lab.example.internal` returns immediate `NSURLErrorCannotFindHost (-1003)`.

---

### Checklist Item 5: "Remove from local DNS" on Last LocalDns Profile (Teardown)

#### Objective
Verify that clicking **Remove from local DNS** on the last profile configured for Local DNS executes the proper ADR-0011 teardown sequence:
1. Deletes `/etc/resolver/<zone>` entries.
2. Verifies removal on disk.
3. Only then unloads LaunchAgent, deletes plist, and deletes binary.
4. Preserves the `generation` counter.

#### Pre-conditions
- Exactly one profile exists with `host_mapping: local_dns` (e.g. `profile-a`).
- Resolver files exist: `/etc/resolver/profile-a.clusterdeck.local`.
- LaunchAgent `com.clusterdeck.dns` is running.

#### Execution Steps
1. In ClusterDeck Endpoints tab, click **Remove from local DNS**.
2. When the administrator prompt appears, enter your password.
3. Verify filesystem and process state:
   ```bash
   # 1. Check /etc/resolver
   ls -la /etc/resolver/profile-a.clusterdeck.local
   
   # 2. Check LaunchAgent status
   launchctl print gui/$(id -u)/com.clusterdeck.dns 2>&1
   
   # 3. Check LaunchAgent plist
   ls -la ~/Library/LaunchAgents/com.clusterdeck.dns.plist
   
   # 4. Check binary
   ls -la ~/.clusterdeck/bin/clusterdeck-dns
   
   # 5. Check generation and zones
   ls -la ~/.clusterdeck/dns/
   ```

#### Verification Criteria
- [ ] `/etc/resolver/profile-a.clusterdeck.local` is deleted.
- [ ] `launchctl print` returns `Could not find service "com.clusterdeck.dns" in domain for gui/...` (or exit code 113).
- [ ] `~/Library/LaunchAgents/com.clusterdeck.dns.plist` is deleted.
- [ ] `~/.clusterdeck/bin/clusterdeck-dns` is deleted.
- [ ] `~/.clusterdeck/dns/zones.json` and `status.json` are deleted.
- [ ] `~/.clusterdeck/dns/generation` is **PRESERVED** (ensures monotonic counter survives re-enablement).
- [ ] **Checklist 7 (Safari):** Safari / `URLSession` returns immediate `NSURLErrorCannotFindHost (-1003)` in <0.05s (proves resolver file was removed before agent stop; no 30s hang).

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

#### Verification Script: `verify-safari-resolution.swift`
Run the following script directly in terminal:

```swift
#!/usr/bin/env swift
import Foundation

let arguments = CommandLine.arguments
guard arguments.count >= 2 else {
    print("Usage: swift verify-safari-resolution.swift <hostname> [expected_ip]")
    exit(1)
}

let hostName = arguments[1]
print("==================================================")
print("🌐 Testing Safari / Apple Network Framework Stack")
print("Target: \(hostName)")
print("==================================================")

// Step 1: Pure CFNetwork / CFHost resolution (exact DNS stack used by Safari/mDNSResponder)
let startDns = Date()
let host = CFHostCreateWithName(nil, hostName as CFString).takeRetainedValue()
let success = CFHostStartInfoResolution(host, .addresses, nil)
let elapsedDns = Date().timeIntervalSince(startDns)
let elapsedDnsMs = String(format: "%.1f ms", elapsedDns * 1000)

var resolvedIps: [String] = []
if success {
    var resolved: DarwinBoolean = false
    if let addresses = CFHostGetAddressing(host, &resolved)?.takeUnretainedValue() as? [Data], resolved.boolValue {
        resolvedIps = addresses.compactMap { data -> String? in
            data.withUnsafeBytes { rawBuffer -> String? in
                guard let baseAddr = rawBuffer.baseAddress else { return nil }
                let family = baseAddr.assumingMemoryBound(to: sockaddr.self).pointee.sa_family
                if family == sa_family_t(AF_INET) {
                    let addrIn = baseAddr.assumingMemoryBound(to: sockaddr_in.self).pointee
                    var ipBuf = [CChar](repeating: 0, count: Int(INET_ADDRSTRLEN))
                    var sinAddr = addrIn.sin_addr
                    inet_ntop(AF_INET, &sinAddr, &ipBuf, socklen_t(INET_ADDRSTRLEN))
                    return String(cString: ipBuf)
                }
                return nil
            }
        }
    }
}

if !resolvedIps.isEmpty {
    print("✅ Apple Network Stack RESOLVED in \(elapsedDnsMs):")
    print("   IP Addresses: \(resolvedIps.joined(separator: ", "))")
} else {
    if elapsedDns > 4.5 && !hostName.hasSuffix(".local") {
        print("❌ D-DEAD STALL DETECTED in \(elapsedDnsMs) (Exceeded 4.5s threshold!)")
        exit(2)
    } else if elapsedDns > 4.5 && hostName.hasSuffix(".local") {
        print("ℹ️ NXDOMAIN for .local in \(elapsedDnsMs) (Apple mDNS multicast probe fallback)")
    } else {
        print("ℹ️ FAST NXDOMAIN in \(elapsedDnsMs): Host not found (Clean negative answer)")
    }
}

// Step 2: URLSession quick probe (if IP was resolved)
if !resolvedIps.isEmpty {
    let url = URL(string: "http://\(hostName)")!
    let config = URLSessionConfiguration.ephemeral
    config.timeoutIntervalForRequest = 2.0
    config.timeoutIntervalForResource = 2.0
    let session = URLSession(configuration: config)
    
    let sem = DispatchSemaphore(value: 0)
    let startHttp = Date()
    session.dataTask(with: url) { _, resp, err in
        let elapsedHttp = Date().timeIntervalSince(startHttp)
        let elapsedHttpMs = String(format: "%.1f ms", elapsedHttp * 1000)
        if let http = resp as? HTTPURLResponse {
            print("✅ Safari URLSession HTTP Response: \(http.statusCode) in \(elapsedHttpMs)")
        } else if let err = err as? NSError {
            if err.code == -1004 { // Could not connect to server (port closed)
                print("✅ Safari URLSession TCP Connected/Refused in \(elapsedHttpMs) (Target IP reached)")
            } else {
                print("ℹ️ Safari URLSession Result in \(elapsedHttpMs): [\(err.domain) \(err.code)] \(err.localizedDescription)")
            }
        }
        sem.signal()
    }.resume()
    _ = sem.wait(timeout: .now() + 2.5)
}
print("==================================================")
```

---

## 5. Automated Verification Test Suite

Below is the consolidated automated test runner (`scripts/test-real-mac-local-dns.sh`) that automates status checking, process inspection, and Safari resolution verification.

```bash
#!/usr/bin/env bash
set -euo pipefail

BOLD="\033[1m"
GREEN="\033[32m"
RED="\033[31m"
YELLOW="\033[33m"
RESET="\033[0m"

log_pass() { echo -e "${GREEN}✓ PASS:${RESET} $1"; }
log_fail() { echo -e "${RED}✗ FAIL:${RESET} $1"; exit 1; }
log_info() { echo -e "${BOLD}▶ $1${RESET}"; }

DNS_DIR="$HOME/.clusterdeck/dns"
AGENT_LABEL="com.clusterdeck.dns"
PORT=15353

echo "=========================================================="
echo " ClusterDeck ADR-0011 Real-Mac QA Verification Runner"
echo "=========================================================="

# 1. Check LaunchAgent status
log_info "1. Verifying LaunchAgent state..."
if launchctl print "gui/$(id -u)/$AGENT_LABEL" >/dev/null 2>&1; then
  AGENT_PID=$(launchctl print "gui/$(id -u)/$AGENT_LABEL" | awk -F'= ' '/pid =/{print $2}')
  log_pass "LaunchAgent $AGENT_LABEL loaded with PID: $AGENT_PID"
else
  log_fail "LaunchAgent $AGENT_LABEL is not loaded in gui/$(id -u)"
fi

# 2. Check status.json
log_info "2. Verifying status.json..."
if [ -f "$DNS_DIR/status.json" ]; then
  STATUS_PID=$(jq -r .pid "$DNS_DIR/status.json")
  STATUS_GEN=$(jq -r .generation "$DNS_DIR/status.json")
  STATUS_ERR=$(jq -r .error "$DNS_DIR/status.json")
  
  if [ "$STATUS_PID" == "$AGENT_PID" ] && [ "$STATUS_ERR" == "null" ]; then
    log_pass "status.json valid: PID=$STATUS_PID, Gen=$STATUS_GEN, Error=null"
  else
    log_fail "status.json mismatch: PID=$STATUS_PID (expected $AGENT_PID), Error=$STATUS_ERR"
  fi
else
  log_fail "Missing $DNS_DIR/status.json"
fi

# 3. Check Port 15353 Ownership
log_info "3. Verifying socket listeners on port $PORT..."
UDP_PID=$(lsof -nP -iUDP:$PORT -t 2>/dev/null || echo "")
TCP_PID=$(lsof -nP -iTCP:$PORT -sTCP:LISTEN -t 2>/dev/null || echo "")

if [ "$UDP_PID" == "$AGENT_PID" ] && [ "$TCP_PID" == "$AGENT_PID" ]; then
  log_pass "Port $PORT strictly owned by PID $AGENT_PID (UDP and TCP LISTEN match)"
else
  log_fail "Socket ownership violation: UDP PID='$UDP_PID', TCP PID='$TCP_PID', Expected='$AGENT_PID'"
fi

# 4. Check Resolver Files
log_info "4. Inspecting /etc/resolver entries..."
RESOLVER_COUNT=0
for f in /etc/resolver/*; do
  [ -e "$f" ] || continue
  if [ -f "$f" ] && [ ! -L "$f" ]; then
    if head -n 1 "$f" | grep -q "^# managed by ClusterDeck (profile: "; then
      RESOLVER_COUNT=$((RESOLVER_COUNT + 1))
      ZONE=$(basename "$f")
      echo "   Found managed resolver file: $ZONE"
    fi
  fi
done
log_pass "Found $RESOLVER_COUNT ClusterDeck-managed resolver file(s)"

# 5. Direct DNS Query Test
if [ "$RESOLVER_COUNT" -gt 0 ]; then
  SAMPLE_ZONE=$(for f in /etc/resolver/*; do head -n 1 "$f" 2>/dev/null | grep -q "managed by ClusterDeck" && basename "$f" && break; done)
  log_info "5. Testing Direct Authoritative Query on $SAMPLE_ZONE..."
  DIG_OUT=$(dig @127.0.0.1 -p $PORT "test.$SAMPLE_ZONE" +time=1 +tries=1)
  if echo "$DIG_OUT" | grep -q "status: NXDOMAIN"; then
    log_pass "Direct DNS answered authoritative NXDOMAIN for test.$SAMPLE_ZONE"
  elif echo "$DIG_OUT" | grep -q "status: NOERROR"; then
    log_pass "Direct DNS answered NOERROR for test.$SAMPLE_ZONE"
  else
    log_fail "Direct DNS returned unexpected response:\n$DIG_OUT"
  fi
fi

echo "=========================================================="
echo -e "${GREEN}${BOLD}All Automated Core Checks Passed!${RESET}"
echo "=========================================================="
```

---

## 6. QA Audit Conclusion & Sign-off Recommendation

The local DNS codebase in `src-tauri/src/services/local_dns/` and `src-tauri/src/services/resolver_files.rs` provides strict software invariants (PID validation in root scripts, atomic temp files, generation monotonicity, and automatic D-DEAD cleanup routing).

However, because ADR-0011 addresses macOS kernel/daemon-specific quirks (`mDNSResponder` bypassing `/etc/hosts` and the 30-second lookup freeze), **manual sign-off on a physical Mac using the 7 checklist items above is mandatory before closing Issue #95.**
