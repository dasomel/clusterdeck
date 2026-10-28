# Built-in Local DNS User Guide

ClusterDeck provides an integrated local DNS responder (`host_mapping: local_dns`) that routes private cluster hostnames on macOS through `/etc/resolver` rather than editing `/etc/hosts`.

For architecture background and design rationale, see [ADR-0011: Built-in local DNS](adr/0011-built-in-local-dns.md) and [ARCHITECTURE.md §7.1](ARCHITECTURE.md#7-hosts-file-management).

---

## 1. Why Built-in Local DNS Exists

On macOS, system hostname resolution is split between two separate subsystems:

| Path | Components / Consumers | Honors `/etc/hosts` | Honors `/etc/resolver/<zone>` |
|---|---|---|---|
| **libinfo (`getaddrinfo`)** | Terminal tools, `curl`, `kubectl`, Chrome, Firefox, `dscacheutil` | Yes | **Yes** |
| **Network.framework / mDNSResponder** | Safari, `URLSession`, native macOS system services | **Inconsistent / Ignored** (under certain system extensions and configs) | **Yes** |

On setups where macOS ignores `/etc/hosts` for Network.framework applications, Safari fails to load cluster services even though `curl` works in the terminal.

Furthermore, `/etc/hosts` requires elevated root permissions (`sudo` / administrator password prompt) on **every IP or endpoint change**. Whenever pods or service load balancers are recreated with new addresses, users are prompted again.

ClusterDeck's built-in local DNS solves both issues:
1. **Universal Resolution:** Both libinfo and Network.framework reliably query per-domain `/etc/resolver/<zone>` entries.
2. **Zero-Prompt Record Updates:** `/etc/resolver` files point to a local loopback responder (`127.0.0.1:15353`). Zone records live in a user-owned JSON file (`~/.clusterdeck/dns/zones.json`). Connecting to the cluster (`Connect`) refreshes local DNS automatically, and endpoint/IP changes update atomically without administrator privileges.

---

## 2. Host Mapping Modes

ClusterDeck profiles support three name resolution modes via the `host_mapping` configuration key:

| Mode | Key (`host_mapping`) | Mechanism | Admin Prompt Frequency | Safari Compatibility |
|---|---|---|---|---|
| **Off** | `off` | No local resolution mapping | None | None |
| **Hosts** | `hosts` | Managed `# >>> ClusterDeck BEGIN` block in `/etc/hosts` | Every IP or endpoint change | Dependent on macOS environment |
| **Local DNS** | `local_dns` | User LaunchAgent on `127.0.0.1:15353` + `/etc/resolver/<zone>` | Only when the zone set changes | **Full** |

### Switching Modes

- **In the UI:** Open cluster profile settings and set **Host name mapping** to **Local DNS (zones approved separately)**. Connecting to the cluster (**Connect**) automatically refreshes local DNS, or click **Sync to local DNS** in the **Endpoints** tab.
- **In Configuration:** Edit `~/.clusterdeck/profiles.yaml`:
  ```yaml
  profiles:
    - id: lab
      name: Lab Cluster
      host_mapping: local_dns
      approved_dns_zones:
        - lab.example.internal
  ```
- **Seamless Make-Before-Break (MBB) Transition:** When switching between `hosts` and `local_dns`, ClusterDeck executes the `/etc/hosts` and `/etc/resolver` updates in a single privileged script call. There is no intermediate window where neither mapping is active.

---

## 3. Zones Architecture

A **zone** in ClusterDeck is an authoritative domain suffix managed by the loopback responder.

### 3.1 Node-Alias Zone (`<label>.clusterdeck.internal`)
Each profile with `host_mapping: local_dns` automatically receives an alias zone:
```text
<label>.clusterdeck.internal
```
where `<label>` is derived from the profile ID (lowercased, underscores replaced with hyphens, up to 63 characters, e.g. profile `lab` becomes `lab.clusterdeck.internal`).

- Node aliases take the form `<node-name>.<label>.clusterdeck.internal` (e.g. `m1.lab.clusterdeck.internal` pointing to `192.0.2.10`).
- If configured, bastion hosts take the form `<bastion-name>.<label>.clusterdeck.internal`.
- This zone requires **no manual approval** because ClusterDeck reservedly owns the `.clusterdeck.internal` namespace.

### 3.2 Approving an Ingress Zone
Discovered endpoints from Kubernetes (Ingress, APISIX, Gateway API) often use private custom domains (e.g. `api.lab.example.internal`).

- **Discovery:** Connecting to the cluster (**Connect**) or clicking **Scan Endpoints** discovers Kubernetes endpoints and immediately evaluates candidate zones via the backend `suggest_dns_zones` command. If endpoint discovery fails (for example, if the SSH tunnel or cluster connection is down, or kubeconfig is missing), ClusterDeck preserves the last known records from cache (`~/.clusterdeck/dns/endpoints.json`) and warns the user rather than clearing the DNS records.
- **Approval:** Candidate zones are immediately presented without requiring a prior sync:
  - A top-level **Suggested zones:** strip displays a single **Approve <zone>** action per suggested domain (e.g. `Approve lab.example.internal`).
  - An inline **Approve <zone>** button appears directly on each unserved endpoint row.
  - The skipped-hosts warning banner summarizes unserved endpoints and reasons, but contains no action buttons.
- **Activation:** Clicking **Approve <zone>** invokes `approve_dns_zone`, prompting once for administrator authorization to install `/etc/resolver/<zone>`. Once approved, all present and future endpoints under that zone resolve immediately.

### Discovery failures and concurrent changes

Discovery tracks Services, Ingress, APISIX, Istio, and Gateway API separately. A failed kind keeps its last known records. If Services fails, nonempty dependent resource lists are treated as failed even when their IP addresses cannot be resolved; genuinely empty lists still remove obsolete records. **Scan Endpoints** reports partial discovery errors alongside cache-write errors.

In Hosts mode, existing records whose resource kind is unknown survive repeated partial failures and are cached. They may remain stale until a fully successful Hosts sync replaces them. Sync rechecks the saved profile under the same lock as profile changes: if the profile was deleted or its mapping changed during discovery, the stale operation stops before writing. With mapping **Off**, select **Hosts** or **Local DNS** before syncing.

### 3.3 Private-TLD Rule
To prevent intercepting public Internet domains or tenant-controlled hostnames (e.g. an ingress with `github.com`), a zone must pass strict validation (`is_allowed_private_zone`):
1. **Lowercase ASCII only.**
2. **At least 3 labels** (e.g. `lab.example.internal` has 3 labels; `example.internal` has 2 and is rejected).
3. **Restricted to private TLDs:** The zone must end in one of:
   - `.internal`
   - `.test`
   - `.example`
   - `.local` (note: `clusterdeck.internal`, `clusterdeck.local` (legacy) and everything under them are reserved)
   - `.home.arpa`
   - `.localhost`
   - `.invalid`

### 3.4 Single Owner Per Zone and Ownership Switching
A zone can have only **one** active owner profile at a time:
- If profile `lab` owns `lab.example.internal`, another profile cannot claim it simultaneously.
- When multiple profiles define the same zone, the zone enters a conflict state (`owned_by_other_profile` or `claimed_by_multiple`) and will not be served until resolved.
- To transfer a zone between profiles, remove it from the existing profile and approve it in the target profile (or switch ownership in profile configuration). When switching, ClusterDeck checks the existing marker in `/etc/resolver/<zone>` and atomically updates the resolver file and ownership record.

---

## 4. What Gets Installed

ClusterDeck installs lightweight, isolated components on your Mac. All background state is stored under user ownership:

```text
~/
├── Library/LaunchAgents/
│   └── com.clusterdeck.dns.plist
└── .clusterdeck/
    ├── bin/
    │   └── clusterdeck-dns            # Standalone responder binary (mode 0755)
    └── dns/
        ├── zones.json                 # Authoritative zone definitions & A records
        ├── status.json                # Responder runtime status & loaded generation
        ├── generation                 # Monotonic generation counter
        ├── endpoints.json             # Discovered endpoints cache per profile
        └── agent.err.log              # LaunchAgent stderr log

/etc/
└── resolver/
    ├── lab.clusterdeck.internal          # Resolver entry for node aliases
    └── lab.example.internal           # Resolver entry for approved ingress zone
```

### 4.1 User LaunchAgent (`com.clusterdeck.dns`)
- Path: `~/Library/LaunchAgents/com.clusterdeck.dns.plist`
- Runs as your normal user account (`KeepAlive: true`, `RunAtLoad: true`).
- Does **not** run as root and requires no background system daemon.
- Executes:
  ```bash
  ~/.clusterdeck/bin/clusterdeck-dns --dns-responder \
    --zones ~/.clusterdeck/dns/zones.json \
    --status ~/.clusterdeck/dns/status.json \
    --port 15353
  ```

### 4.2 Resolver Files (`/etc/resolver/<zone>`)
For each active zone, an `/etc/resolver/<zone>` file is written with the ClusterDeck ownership marker:
```text
# managed by ClusterDeck (profile: lab)
nameserver 127.0.0.1
port 15353
```

---

## 5. When the Admin Password Is Asked

Because DNS records are loaded from user space (`~/.clusterdeck/dns/zones.json`), ClusterDeck minimizes privileged administrator prompts:

- **Prompt Required:**
  1. Adding, removing, or switching ownership of an `/etc/resolver/<zone>` file.
  2. Switching a profile between `hosts` and `local_dns` mode (single prompt to remove/add `/etc/hosts` and `/etc/resolver` entries together).
  3. **D-DEAD Recovery:** If the responder is stopped or the port is unowned, ClusterDeck prompts once to remove dangling resolver files so system lookups do not stall.
- **No Prompt Required (Zero-Prompt):**
  1. Connecting to the cluster (**Connect**), which automatically discovers endpoints and refreshes local DNS.
  2. Adding or updating endpoints when pods or services change IP addresses.
  3. Modifying node aliases or bastion targets within an existing profile.
  4. Periodic cluster sync or rescan (**Scan Endpoints** / **Sync to local DNS**).
- **Scoped Resolver Operations & Resolver Protection:**
  - When saving a profile, switching host mapping, syncing local DNS, or deleting a profile, `/etc/resolver` operations are strictly scoped to that modified profile. Pending, unapproved, or unapplied zone changes belonging to other profiles are never carried over or unintentionally applied during these operations.
  - **Lossless Profile Store & Protection Mode:** ClusterDeck preserves unparsed or validation-failing profiles and unrecognized YAML keys in `profiles.yaml` across all saves, edits, and deletions. When invalid profiles exist or the profile file format is partially unparseable, ClusterDeck engages a resolver protection mode: global resolver teardown or sweeps of unreferenced zones are blocked to prevent wiping out resolver entries that might belong to invalid or temporarily unparseable profiles.

---

## 6. Endpoints Tab Indicators

In the ClusterDeck desktop app, the **Endpoints** tab provides live visibility into local DNS operations:

- **Name Resolution Pill:**
  - `Local DNS` (green/ok) — Active mode (`profile.host_mapping === 'local_dns'`).
  - `Hosts (/etc/hosts)` — Legacy hosts file mode.
  - `Off` — No name resolution.
- **Responder Status Pill:**
  - `Responder running (pid <pid>) · gen <gen>` (green/ok) — The agent is healthy and serving generation `<gen>` (e.g. `Responder running (pid 12345) · gen 3`).
  - `Local DNS responder not running` (orange/warn) — Agent is unloaded, stopped, or not yet started.
- **Error Banner:** Displays red alert text if `status.json` reports a runtime error (`dnsStatus.last_error`, e.g. port conflict or invalid zones format).
- **Zone Table:** Lists configured zones (`<label>.clusterdeck.internal` and all `approved_dns_zones`) with:
  - State tag (`<Tag>`): `served`, `not served`, `responder not running`, `owned by <owner>`, `conflict (<kind>)`, or `unknown`.
  - Active record count (e.g. `2 record(s)`).
- **Suggested Zones Strip:**
  - When unserved endpoints are discovered, displays a `Suggested zones:` bar with a single `Approve <zone>` button for each candidate zone (e.g. `Approve lab.example.internal`), shown immediately without a prior sync.
- **Warning Banners:**
  - `Local DNS is not serving these zones.` — Highlights resolver conflicts with diagnostic reasons:
    - `"a /etc/resolver file for this zone already exists and is not managed by ClusterDeck."`
    - `"owned by profile <id>; switch the zone owner to use it here."`
    - `"claimed by <ids>; only one profile may own a zone."`
    - `"the /etc/resolver entry could not be read."`
    - `"not a valid private zone."`
  - Skipped Hosts Banner:
    - Title: `${outsideCount} endpoint${outsideCount === 1 ? ' is' : 's are'} not served: approve ${zonesStr} to publish ${outsideCount === 1 ? 'it' : 'them'}` (or `"Hosts skipped by local DNS. Approve a zone to publish them."`).
    - Lists unserved endpoints: `<host> (<reason>)` (e.g. `api.lab.example.internal (outside every approved zone)`).
    - Note: The banner displays diagnostic information only; action buttons are located in the Suggested Zones strip and endpoint rows.
- **Endpoint Status Indicators (Row Level):**
  - Node aliases and bastion aliases display target IP and status:
    - `Served by local DNS (<zone>)` (green)
    - `Pending: press Sync to local DNS` (gray)
    - `Local DNS responder not running` (gray)
    - `Conflict: <zone>` (orange)
  - Discovered Ingress / Gateway endpoints display target IP, resource name, and dynamic status driven by `local_dns_status`:
    - `Served by local DNS (<zone>)` — Zone is approved, responder is running, and records are published.
    - `Pending: press Sync to local DNS` — Zone is approved and responder is running, but records are waiting for synchronization.
    - `Served by another profile (<owner>)` — Endpoint hostname is covered by a zone owned by another profile.
    - `Not served (outside approved zones)` — Endpoint domain has not yet been approved.
    - `Conflict: <zone>` — Zone encounters a resolver conflict.
    - `Local DNS responder not running` — Responder process is not running.
    - `Not served (invalid DNS name)` — Hostname fails DNS syntax validation.
    - `Not served (invalid IPv4 address)` — Discovered IP is not a valid IPv4 address.
  - Row Actions:
    - `Approve <zone>` button: Displayed directly on unapproved endpoint rows for one-click approval.
    - Copy button (`Copy <host>`).
    - External link button (`Open https://<host> in browser`).
- **Action Buttons in Controls Strip:**
  - **Scan Endpoints:** "Scan cluster for Ingresses, ApisixRoutes, and Gateways"
  - **Sync to local DNS:** "Publish cluster endpoints and node aliases through local DNS (may require admin approval)"
  - **Remove from local DNS:** "Remove this profile's zones from local DNS"

---

## 7. Troubleshooting

If hostnames fail to resolve or behave unexpectedly, follow this structured diagnosis:

### Step 1: Check the LaunchAgent
Verify that the user LaunchAgent is loaded and running:
```bash
launchctl print gui/$(id -u)/com.clusterdeck.dns
```
Look for:
- `state = running`
- `pid = <number>`
- `last exit code = 0` (or a non-zero exit code such as `1` if the responder failed, e.g. due to a port bind collision or invalid argument)

If the job is not loaded or has failed, check the agent error log:
```bash
cat ~/.clusterdeck/dns/agent.err.log
```

### Step 2: Inspect Responder Status
Check the responder's runtime status file:
```bash
cat ~/.clusterdeck/dns/status.json
```
Example healthy output:
```json
{
  "generation": 3,
  "loaded_at": "2026-10-16T07:30:00Z",
  "error": null,
  "pid": 12345,
  "instance": "a1b2c3d4e5f60718"
}
```
If `"error"` contains a message, the responder failed validation or encountered a port conflict.

### Step 3: Query the Responder Directly
Bypass macOS mDNSResponder caching and test the loopback authoritative server directly with `dig`:
```bash
dig @127.0.0.1 -p 15353 api.lab.example.internal
```
- Expect an answer section containing the target IP (e.g. `192.0.2.10`) with `status: NOERROR` and `flags: qr aa rd`.
- An unknown name in an active zone returns `NXDOMAIN`.
- Names outside approved zones return `REFUSED`.

### Step 4: Test System Resolution (libinfo)
Test how standard macOS resolver libraries resolve the domain:
```bash
dscacheutil -q host -a name api.lab.example.internal
```
This queries through `/etc/resolver/lab.example.internal` and prints the IP address.

### Step 5: Test Network.framework / Safari Resolution
To test the Network.framework stack used by Safari and iOS/macOS system apps, run a quick Swift command in your terminal:
```bash
swift -e '
import Foundation
let url = URL(string: "http://api.lab.example.internal")!
let sem = DispatchSemaphore(value: 0)
URLSession.shared.dataTask(with: url) { _, resp, err in
    if let http = resp as? HTTPURLResponse {
        print("URLSession Status:", http.statusCode)
    } else if let err = err {
        print("URLSession Error:", err.localizedDescription)
    }
    sem.signal()
}.resume()
sem.wait()
'
```
Or open `http://api.lab.example.internal` directly in Safari.

> [!WARNING]
> **`dns-sd -G` is NOT a reliable oracle.**
> Do not use `dns-sd -G v4 <hostname>` to verify `/etc/resolver` status. During verification spikes on macOS, `dns-sd -G` frequently returned `No Such Record` even while Safari, `URLSession`, and `dscacheutil` successfully resolved and connected to the target through `/etc/resolver`. Rely on `dscacheutil`, `URLSession`, or `dig @127.0.0.1 -p 15353` instead.

---

## 8. Common Failures & Remedies

### Unowned `/etc/resolver/<zone>` File Conflict
- **Symptom:** UI displays a warning: `"a /etc/resolver file for this zone already exists and is not managed by ClusterDeck."`
- **Cause:** Another tool or manual configuration created `/etc/resolver/<zone>` without ClusterDeck's marker line (`# managed by ClusterDeck (profile: ...)`). To protect user files, ClusterDeck will **never** overwrite unmanaged files.
- **Remedy:** Inspect the file:
  ```bash
  cat /etc/resolver/lab.example.internal
  ```
  If it is obsolete, remove it manually (`sudo rm /etc/resolver/lab.example.internal`), then click **Sync to local DNS** in the app.

### Responder Down (D-DEAD State)
- **Symptom:** System lookups for cluster domains stall for 30 seconds before failing.
- **Cause:** A resolver file pointing to an inactive port causes macOS resolution to stall. ClusterDeck enforces a safety mechanism (**D-DEAD**): whenever the manager detects that the responder is unavailable or port ownership is lost, it initiates a cleanup prompt to remove dangling resolver files.
- **Remedy:** Inspect `~/.clusterdeck/dns/agent.err.log`. If port 15353 was occupied by another process, free the port or restart the LaunchAgent:
  ```bash
  launchctl kickstart -k gui/$(id -u)/com.clusterdeck.dns
  ```

### Failed Endpoint Discovery
- **Symptom:** Sync or Connect warns with a message depending on the cause:
  - `"Could not discover endpoints (Connect first); serving the last known records."`
  - `"Could not discover endpoints (Connect first); serving the last known records: <error>"`
  - `"Endpoint discovery failed: <error>; serving the last known records."`
  - Or on Connect: `"Could not discover endpoints; serving the last known records."`
- **Cause:** The Kubernetes API could not be reached (for example, the SSH tunnel or cluster connection is down, or kubeconfig is missing or expired).
- **Behavior:** Rather than clearing existing DNS records, ClusterDeck preserves the last known endpoints from cache (`~/.clusterdeck/dns/endpoints.json`) and continues serving them.
- **Remedy:** Ensure the cluster connection is active by clicking **Connect**, then click **Scan Endpoints** or **Sync to local DNS**.

### Zone Not Approved
- **Symptom:** The cluster is connected, but endpoints show `Not served (outside approved zones)` and are listed in the skipped hosts banner.
- **Cause:** Discovered hosts have a domain suffix that has not been approved yet.
- **Remedy:** Click the **Approve <zone>** button in the **Suggested zones:** strip or the inline **Approve <zone>** button directly on the endpoint row. No prior sync is required.

### Discovery Required
- **Symptom:** The alias zone works, but newly created cluster ingress services do not resolve.
- **Cause:** The cluster has not been scanned since the ingress resources were created.
- **Remedy:** Click **Connect** (which refreshes local DNS automatically) or click **Scan Endpoints** / **Sync to local DNS** while connected to the cluster.

---

## 9. Manual Uninstallation

To completely remove ClusterDeck's local DNS components manually, always remove resolver files and flush the DNS cache before stopping the responder; resolver files pointing to a stopped responder cause system lookups for those zones to stall for approximately 30 seconds (see ADR-0011 teardown sequence).

### 1. Safely Remove ClusterDeck Resolver Files
Use this safe one-liner to delete **only** files in `/etc/resolver` that carry ClusterDeck's ownership marker, skipping symlinks and leaving third-party resolver files intact:

```bash
for f in /etc/resolver/*; do
  if [ -f "$f" ] && [ ! -L "$f" ] && head -n 1 "$f" | grep -q "^# managed by ClusterDeck (profile: "; then
    echo "Removing $f"
    sudo rm "$f"
  fi
done
sudo dscacheutil -flushcache
sudo killall -HUP mDNSResponder
```

### 2. Unload the LaunchAgent
```bash
launchctl bootout gui/$(id -u)/com.clusterdeck.dns 2>/dev/null || true
```

### 3. Remove Agent, Plist, and State Files
```bash
rm -f ~/Library/LaunchAgents/com.clusterdeck.dns.plist
rm -f ~/.clusterdeck/bin/clusterdeck-dns
rm -rf ~/.clusterdeck/dns
```
