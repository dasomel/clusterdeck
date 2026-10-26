# Issue #95: macOS runtime verification, 2026-10-18

Status: **Partial; UI verification blocked by macOS accessibility.**

## Target and method

Source HEAD: `3c8ed78`. Built with `cargo build --manifest-path src-tauri/Cargo.toml --bin clusterdeck` and launched the debug app with the Vite server on loopback. The installed `/Applications/ClusterDeck.app` reports version 0.4.1 and was not used to validate the current source.

Tests used real processes and macOS APIs. No infrastructure names, addresses, screenshots, credentials, or raw application state are included here. Temporary scripts/results are in `/tmp/clusterdeck-95-runtime/`.

## Observed passes

- Existing launchd agent loaded; status PID owns the UDP listener, status generation matches the zone file, and no responder error is reported.
- Direct DNS answers matched all 36 configured records before and after the application-start refresh.
- One representative record in each of four zones resolved to its expected IP through Python `socket.getaddrinfo`.
- Swift `NWConnection` using UDP reached ready with the expected resolved remote IP for each of those four records. This checks Network.framework name resolution; it does not prove Safari page rendering.
- Application startup replaced the copied responder binary with one whose SHA-256 equals the newly built executable and changed the responder PID. Zone-file and resolver-file hashes stayed unchanged. The previous binary was backed up to `/tmp/clusterdeck-95-runtime/previous-dns-binary`.
- A separate current-source responder on an ephemeral loopback port passed 13 checks: initial generation, UDP A, TCP A, AAAA no-data, unknown-name NXDOMAIN, out-of-zone REFUSED, generation reload, changed A answer, malformed-config error, preservation of the preceding answer, recovery after valid configuration, rejection of an occupied port, and continued service by the first process after that rejection. The test responder was terminated afterward.

## Failures and limitations

- Swift ephemeral `URLSession` HEAD to a configured HTTPS endpoint timed out with code `-1001`. A direct TCP connection to its IP on port 443 also timed out. DNS resolution passed separately; HTTPS/Safari service reachability is not verified.
- The current app has a visible ClusterDeck window, but Orca cannot read its accessibility tree: `permission_denied`, AX window reads blocked after retries. The permissions tool reports accessibility granted; a retry still fails. User action was requested to toggle Orca Computer Use accessibility off/on. No screenshots of infrastructure were captured.
- A port collision was tested only against the isolated responder. The manager's privileged D-DEAD cleanup of actual resolver files was **not** tested.

## Remaining issue #95 checks

- Logout/login persistence (KeepAlive and RunAtLoad are configured, but this is not runtime evidence of login persistence).
- Actual app move/update lifecycle beyond the observed startup binary refresh.
- Port-squatter D-DEAD resolver cleanup and its warning.
- Cancelled administrator prompt and disk/published-zone consistency.
- Last-profile removal ordering and agent/plist/binary teardown.
- Connect record refresh without a privileged prompt.
- Safari page loading after those scenarios.

The active resolver was not stopped or its zones removed to exercise destructive scenarios. No issue checkboxes were changed and no commit/push was made. The debug UI and Vite server remain available while awaiting the accessibility action; the active responder now uses the current-source binary after the verified startup refresh.

## Follow-up run, 2026-10-27

Target: installed `/Applications/ClusterDeck.app` 0.5.0, which matches the source version. The agent binary's SHA-256 equals the installed app executable, so the binary refreshed after the app update (item 2, observed state; the refresh itself was not watched). The Orca accessibility blocker from 2026-10-18 is gone, and UI reads and clicks work.

- **Read-only check** (`scripts/test-real-mac-local-dns.sh`, one record and one in-zone nonexistent name per zone): agent, `status.json`, port 15353 ownership, 4 managed resolver files and all `dig` checks PASS. For `.internal` zones the Safari-stack checks PASS, both the positive name (HTTP 200) and fast name-not-found.
- **Item 6 (Connect refresh), PASS.** Clicked Connect / Sync on a LocalDns profile. Generation went 19 → 20 and `zones.json` was rewritten. The responder reloaded with no error and the same PID. The resolver-file hashes did not change. A 90 s process watch saw no `osascript`, `SecurityAgent` or `authorizationhost` process, so there was no admin prompt. The Safari-stack re-check afterwards matched the run before.
- **New finding: #119.** Negative lookups in `.clusterdeck.local` zones stall for about 5 s on both `getaddrinfo` and Network.framework because of the mDNS fallback (`domain: local`, `timeout: 5`). Positive names resolve in under 10 ms. Because of this, `--expect-failure` and `--teardown` fail for `.local` zones until #119 is decided.
- **Probe limitation.** The Safari-stack probe's HTTP step times out (`-1001`) against SSH-only hosts even though DNS resolved in under 10 ms. Choose probe hosts that serve HTTP(S), or read the "RESOLVED" line. This is service reachability, not DNS.
- **Minor UI observation.** After Connect, the Endpoints tab still showed `gen 18`; `status.json` was at 19 before and 20 after. The label is not refreshed. This is not filed.

Still not run, by user decision because they are destructive or need interactive admin action: item 1 (logout/login), item 3 (port squatter, D-DEAD), item 4 (cancelled admin prompt), item 5 (last-profile teardown), and the Safari checks after those.
