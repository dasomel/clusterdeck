# ClusterDeck UI redesign spec (lane B)

Status: spec only. Inputs: `.omc/plans/infradeck-merge.md` (D1-D10, frozen IPC), `AGENTS.md`, `DESIGN.md`, `src/styles.css`, `src/App.tsx` (1200 lines), `src/components/*`, `src/api/tauri.ts`, InfraDeck `public/app.js` + README.
Decision ids below are `U1..Un` (UI decisions), separate from the plan's D-ids.

## 0. Direction

Archetype stays "Operations Dashboard / Desktop Operator" (DESIGN.md), compact. The existing "Patch Panel" token system is the identity; we lean into the metaphor instead of fighting it.

- **The one memorable thing: the Patch Rail.** The workflow Discover, Bootstrap, SSH, Kubeconfig, Verify is drawn as five jacks joined by a cable. Each jack is lit by real state (ok / warn / fail / running / skipped). It appears on every cluster, at three sizes (full in the cluster header, mini in Overview rows and the profile list). Everything else stays quiet.
- Surfaces: flat, 1px borders, almost no shadow (shadow only on modal and menus). The rail keeps the blueprint grid texture (`--sidebar-grid`) as the one decorative element.
- Type: system UI face and system mono, both already in tokens (Tauri is offline-first; no webfonts). Personality comes from tabular figures, mono for every machine-readable value (ids, IPs, paths, FQDNs), sentence-case 600-weight labels. No tracked ALL-CAPS labels, no eyebrow above each heading, no hero banner.
- Domain check: this is a dev/ops tool, so the editorial default is wrong. The existing light theme (cream `#f7f4ee` + amber `#c9781a`) is a shipped token and is kept as-is, because the user's tokens are authoritative. We add no new hues. Dark theme (slate `#0f141c`, sky `#38bdf8`) is the primary operator look.

Decisions:
- U1 Five top-level sections, no Settings page: Overview, Infrastructure, Clusters, Kubeconfig. Theme toggle and version live in the rail footer (there is nothing else to configure; an empty Settings page is clutter).
- U2 No router library. Section is `useState` persisted to `localStorage` (`clusterdeck.section`). Inactive screens stay mounted (`hidden`) once visited, so in-flight state (lastResult, CA views) survives navigation exactly as the old single-page App did.
- U3 "Profile" is user-facing renamed **Cluster** in nav and copy; code/types keep `Profile`. Reason: operators think in clusters/environments; the Infrastructure axis already says "environment".
- U4 Environment to Connect is two explicit steps: the Infrastructure "Set up cluster" button calls `create_profile_from_environment`, navigates to Clusters with that profile selected, and focuses the Connect button. It never auto-runs Connect (Connect may prompt for passwords and write `/etc/hosts` with admin approval; side effects need an intentional click).
- U5 One global status dock (existing `StatusBanner`) under the screen header on every screen. Removes the old "show banner only if manager/editor open" workaround.

## 1. Information architecture

```
Rail (184px)            Screen
 Overview               fleet board: every cluster's Patch Rail + infra glance + needs-attention
 Infrastructure         VM inventory by environment (Colima / VirtualBox / VMware / Vagrant), providers, resource totals, local runtimes
 Clusters               cluster list | cluster detail (rail, Nodes, Endpoints, Trust, Kubeconfig) | editor
 Kubeconfig             ~/.kube/config: Contexts, Backups, Managed, Trusted CAs
 ---
 theme toggle, version
```

Re-homing of today's UI:

| Today (App.tsx / components) | New home |
|---|---|
| Sidebar profile list, Add profile | Clusters, `ProfileList` pane (also mini-rail per row) |
| Header: refresh, theme, kubeconfig-manager gear | Per-screen refresh in `ScreenHeader`; theme in rail footer; gear removed (Kubeconfig is a section) |
| hero-card (Connect / Test, SSH + bootstrap password fields) | Clusters, `ClusterHeader` actions + `PasswordPrompt` row (only when needed) |
| Hosts panel (per-host SSH pill, open SSH) | Clusters, Nodes tab |
| Kubernetes panel (SSH/Kubeconfig/Context/API/Version//etc/hosts/Endpoint rows) | Replaced by the Patch Rail (header) + a "Cluster facts" definition list at top of Nodes tab |
| ~/.kube/config Integration: Merge | Clusters, Kubeconfig tab (per profile) |
| ~/.kube/config Integration: Backup (move) | Kubeconfig section, Backups tab, "Move current config to ~/.kube/bak" |
| Hosts File (/etc/hosts) & Endpoints: sync/clear/scan, node + bastion FQDNs, endpoints | Clusters, Endpoints tab (hosts-file strip on top, then rows) |
| Discovered CA rows (Trust / Update Trust / Remove) | Clusters, Trust tab |
| KubeconfigManager: current config, contexts, set/delete context | Kubeconfig, Contexts tab |
| KubeconfigManager: backups (restore/delete/open in Finder) | Kubeconfig, Backups tab |
| KubeconfigManager: managed profile kubeconfigs | Kubeconfig, Managed tab |
| KubeconfigManager: trusted CAs across profiles (remove) | Kubeconfig, Trusted CAs tab |
| KubeconfigManager: Local runtime (Colima/Lima start/stop/restart/shell/context) | Infrastructure, `LocalRuntimePanel` (collapsed section under the table; ported 1:1, same `detectLocalHosts` data, so no machine-to-instance mapping risk) |
| ProfileEditor | Clusters, replaces the detail pane (breadcrumb "Clusters / name / Edit"); unchanged behavior, reachable also from Overview rows |
| InfraDeck dashboard (metrics, providers, grouped machines, demo, stale hint, warnings, "Connect with ClusterDeck") | Infrastructure (new) |

### Workflow made visible: the Patch Rail

Five steps, derived only from data already returned by existing commands (no new IPC). On profile select, call existing `get_profile_status` once so the rail is lit before any Connect (rail never starts blank for a previously verified cluster).

| Step | Icon | ok when | warn/fail | skipped |
|---|---|---|---|---|
| Discover | Radar | profile has >=1 host | no hosts (warn) | never |
| Bootstrap | KeyRound | all hosts reachable after a bootstrap run in this session or key already works | any host unreachable (warn) | `bootstrap.enabled === false` |
| SSH | Terminal | `verification.ssh` or every host `reachable` | some unreachable (warn), all unreachable (fail) | never |
| Kubeconfig | FileDown | `verification.kubeconfig` | profile has kubeconfig source but not synced (warn) | profile has no `kubeconfig` |
| Verify | ShieldCheck | `verification.kubernetes` (title attr: version + endpoint) | connect ran and k8s false (fail) | profile has no `kubeconfig` |

Step state type: `'idle' | 'running' | 'ok' | 'warn' | 'fail' | 'skipped'`. Derivation is a pure function `deriveRail(profile, lastResult, status, busy): RailStep[]` in `src/lib/rail.ts` (unit-testable with vitest if present; otherwise plain function). While Connect runs, jacks go `running` left-to-right by elapsed phase only if the backend reports phases; it does not, so all not-yet-known jacks pulse together (honest, no fake progress).

```
 Full rail (cluster header)
  (o)━━━━(o)━━━━(o)━━━━(o)━━━━(o)
  Discover Bootstrap SSH Kubeconfig Verify
  3 hosts   skipped   3/3   synced    v1.31.2
 (ok = filled success + check; warn = filled warning + !; fail = danger + x, cable to it dashed;
  idle = hollow border-strong; skipped = hollow, label struck-through tertiary; running = accent ring pulse)

 Mini rail (list/overview): 5 x 8px dots joined by 1px line, no labels, aria-label summarises.
```

## 2. Screens

Global frame (min window width 900 stays; body `min-width: 900px`):

```
+--------+---------------------------------------------------------------+
| [box]  | Screen title            [segmented/filters]   [Refresh] [..]  |  ScreenHeader 48px
| Click  |---------------------------------------------------------------|
|        | [ StatusBanner dock, only when a message exists ]             |
| Overv. |                                                               |
| Infra. |   screen content, padding 20px 24px, scroll inside content    |
| Clust. |                                                               |
| Kubec. |                                                               |
|        |                                                               |
| ------ |                                                               |
| theme  |                                                               |
| v0.4.1 |                                                               |
+--------+---------------------------------------------------------------+
```

Rail items: icon 16 + label 13px/500, height 32, active = `--accent-soft-bg` fill + 2px `--accent` left bar + `--text-primary`; hover `--hover-row-bg`. A numeric badge (mono 11px, pill.warn) on Infrastructure and Clusters when attention items exist (stale Vagrant, resource over host, failing rail). Rail background keeps `--bg-sunken` + grid texture.

### 2.1 Overview

Job: "is everything healthy, and what do I do next?" Not a marketing hero.

```
Overview                                                [Refresh]
+---------------------------------------------+ +-------------------+
| Clusters (4)                      [Add]     | | Infrastructure     |
|---------------------------------------------| | 6 running / 9 VMs  |
| narwhal-dev  (o)-(o)-(o)-(o)-(o)  3 hosts   | | CPU  12/16 [=====-]|
|   direct . v1.31.2        [Connect / Sync]  | | Mem  24/32 GiB [==]|
| narwhal-stg  (o)-(o)-(x)-(.)-(.)  ssh 2/3   | | Colima ok  VBox ok |
|   bastion . retry          [Connect / Sync] | | VMware n/a Vagrant |
+---------------------------------------------+ |        [Open >]    |
| Needs attention                              | +-------------------+
|  ! narwhal-stg: SSH unreachable on node-2   [Open cluster]
|  ! Vagrant "web-1" is a stale cached entry  [Open infrastructure]
|  ! Allocated memory 40 GiB exceeds host 32  [Open infrastructure]
+----------------------------------------------------------------+
```

- Cluster rows: name, meta (hosts count, Bastion/Direct), mini-rail, k8s version if known, `Connect / Sync` (secondary button, it navigates to Clusters and runs nothing; keeps side effects on the detail screen).
- Needs attention: derived list; each item has one "Open" action. Empty: "Nothing needs attention." plus last-checked time.
- Infra glance reads the shared `InventoryContext` (live mode, loaded once on app start, read-only, no polling). If never loaded or failed, the card shows its own error/Retry; it never blocks the cluster list.
- First run (0 clusters): replace the left card with two actions: "Detect local VMs" (goes to Infrastructure) and "Add cluster manually" (opens editor). Copy: "No clusters yet. Detect VMs running on this Mac, or add one by hand."
- Loading: cluster rows render immediately from `list_profiles`; infra card shows 3 skeleton bars.

### 2.2 Infrastructure (new)

```
Infrastructure           [Live | Demo]  [Filter machines  /]   [Refresh] updated 14:02:11
+-DEMO DATA (hatched band, only in demo)--------------------------------------------+
| Sample environments. No host commands are executed.                [Switch to live]|
+---------------------------------------------------------------------------------+
Providers:  (o) Colima available   (.) VirtualBox not installed   (o) VMware Fusion   (x) Vagrant error: ...
Totals:   Running 6 | CPU 12 of 16 [======----] | Memory 24 of 32 GiB [=======---] | Disks 410 GiB (3 unknown)
! Allocated memory 40 GiB exceeds host 32 GiB.        <- summary.warnings + computed over-host
---------------------------------------------------------------------------------
table: State  Machine            Resources            Disk        Address       Created   K8s
v narwhal-dev   2 machines . k8s detected                          [Set up cluster]
  . running  node-1  vagrant>virtualbox  2 CPU 4 GiB  12/40 GiB   192.168.56.11  2026-09-14  detected
  . running  node-2  vagrant>virtualbox  2 CPU 4 GiB  11/40 GiB   192.168.56.12  2026-09-14  detected
v colima (default)   1 machine
  . stopped  colima  colima>colima       4 CPU 8 GiB  --          --              2026-08-30  none
  ! stale   web-1    vagrant             --           --          --              --          --
     Cached Vagrant entry, no .vagrant data found.  vagrant global-status --prune  [Copy]
> Local runtimes (Colima / Lima)   collapsed; start / stop / restart / shell / use context
```

- Table is a real `<table>`; each environment is a `<tbody>` with a group header row (`<th scope="rowgroup" colSpan>`), collapsible (chevron button, `aria-expanded`). Row height 32px (compact), cell font 12px, numbers `tabular-nums`, IPs and ids mono.
- `Resources` = "N CPU  M GiB" with `--` for null (null never rendered as 0). `Disk` = `used/total GiB` when used known, else `total GiB`. `Created` = locale date, tooltip = full timestamp.
- "Set up cluster" button shows only when mode is live and the environment has an `orchestrator === 'vagrant'` machine or a `runtime === 'colima'` machine (same rule as InfraDeck). In demo it is replaced by disabled text "Demo data: nothing to connect". Click: `api.createProfileFromEnvironment(environment)`; success banner: "Cluster narwhal-dev created" or "Cluster narwhal-dev updated (node-2 address refreshed)" or "Cluster already up to date" (from `created` / `updated_hosts`); then navigate to Clusters, select `profile_id`, focus Connect. Error: error banner, button re-enabled. Per-environment busy state so two environments cannot race.
- Resource-over-host warning: if `summary.cpu > host.cpu` or `summary.memory_gib > host.memory_gib` (both non-null), the totals meter turns `--warning`, shows an overflow tick at 100%, and a warning line states the numbers. If `summary.unknown_resources > 0`: "N running machines have unknown resource allocations; totals are incomplete." All `summary.warnings[]` are listed verbatim under the totals.
- Stale Vagrant (`state === 'stale'`): row is `pill.warn "stale"` plus an inline hint row with the prune command in mono and a copy button. The message is hint-only; the app never runs prune (no lifecycle for Vagrant, D1).
- Provider strip: one chip per `providers[]` entry, status pill by semantics in section 3.4, `message` in tooltip and, for `error`, shown inline under the strip.
- Demo mode: segmented control Live | Demo, persisted `clusterdeck.inventoryDemo`; default Live. Demo is unmistakable: hatched warning band (token `--demo-hatch`) above the table and a diagonal hatch on the table body background. Switching refetches immediately.
- Empty (live, 0 machines): centered empty state: "No virtual machines found." + one line "ClusterDeck looks for Colima, VirtualBox, VMware Fusion and Vagrant on this Mac." + provider strip stays visible so the user sees which CLIs are missing + buttons [Try demo data] [Refresh].
- Empty (filter): "No machines match "xyz"." + [Clear filter].
- Loading: first load shows 2 environment skeleton groups (3 bars each, shimmer only if no `prefers-reduced-motion`). Refresh keeps old data visible with the Refresh icon spinning and `aria-busy="true"` on the table; never blank the table on refresh.
- Error (`discover_inventory` rejects): inline error panel replacing totals/table if no previous data, else a banner above stale data dimmed to 60% opacity: "Could not read inventory: <error>" + [Retry].
- No destructive actions on this screen except the existing Local runtime stop/restart (ConfirmModal, as today).

### 2.3 Clusters

```
Clusters
+-------------+-------------------------------------------------------------+
| [Add]       | narwhal-dev   direct . 3 hosts . id narwhal-dev    [Edit][Del]|
| filter      | (o)━(o)━(o)━(o)━(o)   Discover Bootstrap SSH Kubeconfig Verify|
| > narwhal-dev [Test connection]  [ Connect / Sync ]   SSH password [......] |
|   (o)-(o)-(o)-(o)-(o)  | Nodes | Endpoints | Trust | Kubeconfig |           |
|   narwhal-stg|--------------------------------------------------------------|
|   (o)-(o)-(x)-(.)-(.)  | tab content                                        |
+-------------+-------------------------------------------------------------+
```

List pane: 232px, `role="listbox"`, roving tabindex, Up/Down moves selection, Enter selects, row = name (13px/600), meta ("3 hosts . Bastion") 11px, mini-rail. No nth-child stagger animation (removed; generic motion).

`ClusterHeader`: name 20px/600, meta line mono id. Right side: `Test connection` (secondary), `Connect / Sync` (primary). Password fields are not permanent: `PasswordPrompt` row appears directly under the actions only when `needsSshPassword` (any host `auth === 'password'`) or `bootstrap.enabled`; two labeled inputs ("SSH password", "Bootstrap password"), `type=password`, `autoComplete="off"`, never persisted. A subtle hint "Not saved. Cleared after each attempt."

Tabs (`Tabs` primitive, `role=tablist`, arrow-key navigation, state persisted per session in the component only):

- **Nodes**: facts list (Context, API endpoint, Version, /etc/hosts state) as a two-column definition list, then host table: Host (name + address:port mono) / Auth / SSH status pill / actions (Open SSH session, Copy). Failure detail line (mono, danger) under unreachable host as today. Bastion shown as first row with tag "Bastion".
- **Endpoints**: strip: `/etc/hosts` pill (Synced / Not in /etc/hosts), Auto-sync On/Off pill, actions [Scan endpoints] [Sync to /etc/hosts] [Clear]. One-line explainer: "Maps *.<id>.clusterdeck.local node aliases and discovered Ingress/APISIX/Gateway hosts into /etc/hosts." Then a table of rows: kind tag (Node / Bastion / Endpoint), FQDN mono, actions Copy, Open (http for nodes, https for endpoints). Empty before scan: "No endpoints scanned yet." + [Scan endpoints].
- **Trust**: CA rows (CN or secret_ref mono, status pill Trusted / Changed / Untrusted, "N hosts, expires ...", warnings list), actions Trust CA / Update Trust (ConfirmModal, non-danger) and Remove (ConfirmModal, danger) exactly as today. Empty: "No certificate authorities found. Scan endpoints first." (CA discovery runs after scan/connect.)
- **Kubeconfig**: kubeconfig source summary (context name, file path mono + Open in Finder via `open_path_in_finder`), [Merge to ~/.kube/config] (backs up first, `backupFirst: true`, shows safety backup path), link "Manage ~/.kube/config" navigates to Kubeconfig section.

Editor (ProfileEditor) replaces the detail pane; unsaved-changes guard is out of scope (not present today). Delete uses ConfirmModal (danger) from list row menu, header, and editor.

Empty (no clusters) and error (`list_profiles` fail): the list pane shows the same copy as Overview first-run; load error is a banner "Could not load clusters: <err>" with [Retry]. Selected-but-vanished profile falls back to first remaining.

### 2.4 Kubeconfig

```
Kubeconfig        ~/.kube/config  (14 contexts, current: narwhal-dev)    [Refresh]
 Contexts | Backups | Managed | Trusted CAs
 Contexts: table Name (mono) / Cluster / User / [Use] [Delete]; current row has "current" pill
 Backups:  [Move current config to ~/.kube/bak]   table file / size / modified / [Restore][Open][Delete]
 Managed:  per-profile kubeconfigs under ClusterDeck dir, [Open in Finder]
 Trusted CAs: grouped by cluster, [Remove]
```

Same data/commands as `KubeconfigManager` (one `reload()` with `Promise.all`; it still calls `detectLocalHosts` only if the Local runtime port needs it, which moves to Infrastructure). Destructive ConfirmModals: Delete context (new, danger; today it deletes on click, now confirmed because it edits the user's real kubeconfig), Delete backup (new, danger), Restore backup (new, non-danger, "replaces ~/.kube/config; current file is backed up first" only if backend does so; otherwise danger), Move current config (new, non-danger), Remove CA trust (existing). These added confirmations are the only intentional behavior additions; each is listed in the PR description.

## 3. Tokens

### 3.1 Reuse (no change)
All existing variables: `--bg --bg-elevated --bg-sunken --border --border-strong --text-primary/secondary/tertiary --accent --accent-contrast --accent-soft-bg --success/warning/danger (+ -soft-bg) --pill-border-* --hover-row-bg --sidebar-grid --shadow-card --font-ui --font-mono` and the `--of-color-*` layer. Light, `prefers-color-scheme: dark` with `:not([data-theme="light"])`, and `[data-theme="dark"]` blocks stay; new tokens are added to all three.

### 3.2 NEW tokens (names, values, DESIGN.md mapping)

Color (must exist in all three theme blocks):

| Token | Light | Dark | DESIGN.md / of-color mapping |
|---|---|---|---|
| `--info` | `var(--of-color-status-info)` (#0284c7) | `var(--of-color-status-info)` (#38bdf8) | `statusInfo` (already mapped, just no short alias) |
| `--info-soft-bg` | `rgba(2,132,199,.10)` | `rgba(56,189,248,.15)` | derived from statusInfo |
| `--pill-border-info` | `rgba(2,132,199,.25)` | `rgba(56,189,248,.30)` | derived |
| `--demo-hatch` | `rgba(217,119,6,.08)` | `rgba(251,191,36,.08)` | derived from statusWarning; demo-mode texture only |
| `--scrim` | `rgba(20,16,8,.45)` | `rgba(0,0,0,.60)` | new row `overlayScrim`; replaces the hardcoded `rgba(0,0,0,.55)` in `ConfirmModal` |
| `--focus-ring` | `var(--accent)` | `var(--accent)` | `focusRing` (DESIGN.md already lists it; add the CSS var) |

Non-color (theme-independent, declared once on `:root`; document under a new "Spacing, radius, type" block in DESIGN.md):

```
--space-1: 4px;  --space-2: 8px;  --space-3: 12px; --space-4: 16px; --space-5: 20px; --space-6: 24px;
--radius-sm: 4px (pills, tags)  --radius-md: 6px (controls, rows)  --radius-lg: 10px (panels, modal)
--row-h: 32px (table/list rows)  --rail-w: 184px  --list-w: 232px
--fs-xs: 11px  --fs-sm: 12px  --fs-md: 13px  --fs-lg: 15px  --fs-xl: 20px
--lh-tight: 1.25  --lh-body: 1.45
```

Also update `design/design-tokens.json` (lane E): add `statusInfoSoft`, `demoHatch`, `scrim`, plus a `spacing`/`radius`/`fontSize` group. Rule: no hex/rgb literal in any new `.tsx` or screen CSS; only the token files may contain colors. The few existing literals (`ConfirmModal` scrim, hardcoded `font-size`/`padding` inline styles) are removed during migration step 8.

### 3.3 Typography and spacing scale
- Screen title 20/600 (`--fs-xl`), section title 15/600, body/controls 13/400-500, table cells 12, meta/captions 11 in `--text-tertiary`. Weights only 400/500/600 (drop 700). Labels are sentence case; the old `.eyebrow` and `.section-label` (letter-spaced caps) are deleted.
- Mono: ids, IPs, FQDNs, file paths, versions, command hints. `font-variant-numeric: tabular-nums` on every numeric column.
- Spacing: 4pt grid via `--space-*`; row height `--row-h`; card padding `--space-4`; between sections `--space-5`. No inline `style={{}}` for layout in new code; classes only.
- Radius hierarchy replaces the single 9-12px radius: 4 pills, 6 controls and rows, 10 panels and modal.

### 3.4 Status-pill semantics (never color alone: icon or dot plus text)

| Pill | Token set | Meaning / used for |
|---|---|---|
| ok | `--success`, `-soft-bg`, `--pill-border-success`, check icon | running VM, SSH reachable, Synced, CA Trusted, provider available, step ok |
| warn | `--warning` set, alert-triangle | stale Vagrant, Needs retry, CA Changed, over-host, provider demo, step warn |
| fail | `--danger` set (`--danger-soft-bg`, `--pill-border-danger`), x-circle | provider error, step fail, CA Untrusted is warn not fail |
| info | `--info` set, loader/dot | action in progress, "current" context, k8s detected |
| idle | `--text-tertiary` on `--bg-sunken`, `--border`, hollow dot | stopped VM, not installed, step idle, K8s none, Auto-sync Off |
| skipped | idle + strikethrough label | bootstrap disabled, no kubeconfig source |

Machine `state` mapping: `running` ok; `stale` warn; `stopped|poweroff|saved|paused` idle; anything unrecognized idle with the raw string (never guess ok). Provider `status`: `available` ok, `not-installed` idle, `error` fail, `demo` warn. Machine `kubernetes` is a string from the frozen contract: render raw; treat `none|no|unknown|""` as idle, anything else as info "detected" (implementer: confirm exact values in `services/inventory/model.rs`).

### 3.5 Icons (lucide-react, strokeWidth 1.75, sizes 14 inline / 16 nav / 18 empty states)
Nav: LayoutDashboard, Server, Network, FileCog. Brand stays Boxes. Steps: Radar, KeyRound, Terminal, FileDown, ShieldCheck. Actions: RefreshCw (spin class exists), Plus, Pencil, Trash2, Copy, ExternalLink, FolderOpen, Archive, FilePlus, ChevronDown/Right, CircleAlert, CheckCircle2, XCircle, Sun/Moon. Every icon-only button has `aria-label` and a `title`.

### 3.6 Motion (restraint)
- One orchestrated moment: after Connect/Test completes, rail jacks settle to their final state with an 80ms stagger (a 120ms fill transition each). This answers a user action.
- `running` jack: one pulse ring animation (1.4s). Refresh icon spins (existing `.spin`). Skeleton shimmer.
- Removed: per-card entrance stagger, hover lifts. Hover = background change only.
- `@media (prefers-reduced-motion: reduce)`: stagger, pulse, shimmer disabled; states change instantly.

## 4. Components, files, state

### 4.1 File layout (every file < 300 lines)

```
src/
  main.tsx                         unchanged
  App.tsx                          shell only (~120): providers, NavRail, section switch, global modals none
  styles.css                       @import tailwindcss + @import "./styles/*.css" (order: tokens, base, shell, controls, tables, rail, screens)
  styles/
    tokens.css                     the three theme blocks + new tokens (moved verbatim from styles.css, then additions)
    base.css                       reset, scrollbar, body, focus ring, .mono, .spin, reduced-motion
    shell.css                      .app-shell, .nav-rail, .screen, .screen-header
    controls.css                   buttons, inputs, tabs, segmented, pill, tag, modal, status-banner, form-*
    tables.css                     .data-table, group rows, skeleton, empty-state
    rail.css                       .patch-rail, .patch-rail--mini, jack states
    screens.css                    overview/infra/clusters/kubeconfig specific
  api/
    tauri.ts                       + types Machine, Inventory, ProviderStatus, InventorySummary, EnvironmentProfileResult
                                   + api.discoverInventory(demo), api.createProfileFromEnvironment(environment)
  lib/
    format.ts                      gib(), count(), dateShort(), relativeTime(), target(host)  ("name (addr:port)")
    rail.ts                        deriveRail(...)
    status.ts                      machineStatePill(), providerPill(), k8sPill()
    attention.ts                   deriveAttention(profiles, statuses, inventory) -> AttentionItem[]
    messages.ts                    pure builders: connectMessage(result, profile), testMessage(...)  (moved out of App.tsx verbatim)
  state/
    StatusContext.tsx              status message + push helpers (success/warning/error + time stamp)
    ProfilesContext.tsx            profiles, loadError, selectedId, reload(), select(), remove(), trustVersion map + bumpTrust(id)
    InventoryContext.tsx           inventory, loading, error, demo, refresh(), createFromEnvironment()
  hooks/
    useTheme.ts                    exact old theme logic (clusterdeck-theme key, null = follow system)
    useLocalStorage.ts             try/catch wrapper
    useSection.ts                  section state + persistence + shortcuts
    useClusterSession.ts           connect/test/status, lastResult, passwords, busy flags (~220)
    useEndpoints.ts                scan, hosts-file status/sync/clear, CA views and trust actions (~220)
    useBusy.ts                     busy Set<string> helper: run(key, fn)
  components/
    ConfirmModal.tsx               keep API; swap inline styles/scrim literal to classes + --scrim; add role="dialog" aria-modal, focus trap, Esc
    StatusBanner.tsx               keep; add role=status|alert
    ProfileEditor.tsx              keep for now; optional split in step 10
    ui/
      Pill.tsx  Tag.tsx  Tabs.tsx  Segmented.tsx  IconButton.tsx  CopyButton.tsx
      Meter.tsx  EmptyState.tsx  Skeleton.tsx  Banner.tsx(optional: inline notice)  DefinitionList.tsx
    layout/
      NavRail.tsx  ScreenHeader.tsx
    rail/
      PatchRail.tsx                full + mini variants
  screens/
    overview/   OverviewScreen.tsx  FleetRow.tsx  InfraGlance.tsx  AttentionList.tsx
    infrastructure/
                InfrastructureScreen.tsx  ProviderStrip.tsx  ResourceSummary.tsx
                EnvironmentGroup.tsx  MachineRow.tsx  StaleHint.tsx  DemoBand.tsx  LocalRuntimePanel.tsx
    clusters/   ClustersScreen.tsx  ProfileList.tsx  ClusterDetail.tsx  ClusterHeader.tsx
                PasswordPrompt.tsx  NodesTab.tsx  EndpointsTab.tsx  TrustTab.tsx  KubeconfigTab.tsx
    kubeconfig/ KubeconfigScreen.tsx  ContextsTab.tsx  BackupsTab.tsx  ManagedTab.tsx  TrustedCasTab.tsx
```

### 4.2 State ownership

- `App.tsx`: `<ThemeProvider-less>` (`useTheme` returns `{effective, toggle}` passed to NavRail), `StatusProvider > ProfilesProvider > InventoryProvider`, `section` via `useSection`. Renders visited screens, inactive ones `hidden`.
- `StatusContext`: single `StatusMessage | null` (type unchanged), `push(type, title, details?)` stamps `new Date().toLocaleTimeString()`. Dock rendered by `ScreenHeader` slot.
- `ProfilesContext`: authoritative profile list and selection. `select(id)` only changes id; per-cluster ephemeral state is reset by giving `ClusterDetail` `key={selectedId}` (this reproduces the old manual resets of lastResult, caViews, modal targets, status message, passwords in one move; `select` also clears the status message). `remove(profile)` reproduces old delete logic. `trustVersion[id]` increments from `bumpTrust` (called by Kubeconfig Trusted CAs tab after removeCa) and `useEndpoints` refetches CA views when it changes (replaces the old `onCaRemoved` prop).
- `InventoryContext`: shared by Overview and Infrastructure. `refresh()` calls `discover_inventory(demo)`; keeps previous data on failure (`error` set, `inventory` retained); drops an out-of-order response using a request counter. Loaded once at app start in live mode.
- `useClusterSession(profile)`: owns `lastResult`, `status` (from `get_profile_status`), `sshPassword`, `bootstrapPassword`, `busy`. Passwords clear in `finally` of connect and test, as today.
- `useEndpoints(profile, lastResult, setLastResult)`: endpoints, `hostsStatus`, `caViews`, CA action targets and busy.

### 4.3 Component props and Tauri commands

| Component | Props | Commands (existing unless noted) |
|---|---|---|
| `NavRail` | `section, onSelect, badges: Partial<Record<Section,number>>, theme:'light'\|'dark', onToggleTheme, version` | none |
| `ScreenHeader` | `title, actions?: ReactNode, meta?: ReactNode` (renders the global `StatusBanner` below) | none |
| `PatchRail` | `steps: RailStep[], variant:'full'\|'mini'` | none |
| `OverviewScreen` | `onOpenCluster(id), onNavigate(section), onAddCluster()` | `list_profiles` and `get_profile_status` per profile via `ProfilesContext` (status cache map), `discover_inventory` via `InventoryContext` |
| `FleetRow` | `profile, steps, onOpen` | none |
| `InfraGlance` | `inventory, loading, error, onRetry, onOpen` | none |
| `InfrastructureScreen` | `onClusterReady(profileId)` | `discover_inventory(demo)`, `create_profile_from_environment(environment)` |
| `ProviderStrip` | `providers` | none |
| `ResourceSummary` | `summary, host, mode` | none |
| `EnvironmentGroup` | `environment, machines, mode, expanded, onToggle, onSetUp, busy` | none |
| `MachineRow` | `machine` | none |
| `StaleHint` | `machine` | none (copy via clipboard, as existing CopyButton) |
| `LocalRuntimePanel` | none (self-contained, ported from KubeconfigManager) | `detect_local_hosts`, `start_local_runtime`, `stop_local_runtime`, `restart_local_runtime`, `open_local_runtime_shell`, `open_local_runtime_context`; stop/restart through ConfirmModal as today |
| `ClustersScreen` | `focusConnectFor?: string` (set by U4) | via contexts |
| `ProfileList` | `profiles, selectedId, steps: Record<id,RailStep[]>, onSelect, onAdd, onEdit, onDelete` | none |
| `ClusterDetail` | `profile, onEdit, onDelete, autoFocusConnect` | `connect_profile`, `probe_profile_hosts`, `get_profile_status`, `get_hosts_file_status` |
| `ClusterHeader` | `profile, steps, busy, passwords+setters, onConnect, onTest, onEdit, onDelete` | none |
| `PasswordPrompt` | `needsSsh, needsBootstrap, sshPassword, bootstrapPassword, onChange*` | none |
| `NodesTab` | `profile, lastResult, onOpenSsh(host)` | `open_ssh_session` |
| `EndpointsTab` | `profile, endpoints, hostsStatus, busy flags, onScan, onSync, onClear, onOpenUrl` | `discover_cluster_endpoints_cmd`, `sync_hosts_file_cmd`, `remove_hosts_file_cmd`, `get_hosts_file_status`, `open_url_in_browser` |
| `TrustTab` | `caViews, busy, onTrust(ca), onRemove(ca)` (modals owned here) | `discover_cluster_cas_cmd`, `trust_ca_cmd`, `replace_ca_cmd`, `remove_ca_cmd` |
| `KubeconfigTab` | `profile, merging, onMerge, onManage()` | `merge_kubeconfig_to_system`, `get_profile_status`, `open_path_in_finder` |
| `KubeconfigScreen` | `onCaRemoved(profileId)` | `get_user_kubeconfig_details`, `list_kubeconfig_backups`, `list_managed_profile_kubeconfigs`, `list_profiles` |
| `ContextsTab` | `userConfig, onUse, onDelete` | `set_current_context`, `delete_user_kube_context` |
| `BackupsTab` | `backups, onMove, onRestore, onDelete, onOpen` | `backup_kubeconfig(true)`, `restore_kubeconfig_backup`, `delete_kubeconfig_backup`, `open_path_in_finder` |
| `ManagedTab` | `managed, onOpen` | `open_path_in_finder` |
| `TrustedCasTab` | `managed/profiles data, onRemove` | `remove_ca_cmd` |
| `ProfileEditor` | unchanged | unchanged (`save_profile`, `detect_local_hosts`, `list_local_kube_contexts_cmd`, `discover_hosts`) |

Only `discover_inventory` and `create_profile_from_environment` are new commands; both match the frozen contract (snake_case, D8). Frontend types: `Machine`, `Inventory`, `ProviderStatus`, `InventorySummary` exactly as in the plan; `create_profile_from_environment` returns `{ profile_id: string; created: boolean; updated_hosts: string[] }`.

Data rules: the UI never sends machine data to the backend (D6); it sends only the environment name string. Environment names are displayed through React text nodes only (no `dangerouslySetInnerHTML`). Command hints are static strings.

## 5. Migration order (each step ends with `pnpm build` green and the app rendering; steps 1-3 change no behavior)

1. Split `styles.css` into `src/styles/*.css` verbatim, add the new tokens to all three theme blocks, add `--focus-ring` focus rule. Verify: light, dark, `data-theme` override unchanged (screenshots).
2. Extract `lib/messages.ts`, `lib/format.ts`, contexts (`Status`, `Profiles`), `useTheme`, `useBusy`. `App.tsx` still renders the old layout using them. Verify: Connect/Test/Delete messages byte-identical.
3. Extract `useClusterSession` and `useEndpoints` from App.tsx, render old panels from them. Verify against section 6.
4. Add `NavRail`, `ScreenHeader`, `useSection`; move the old content into `ClustersScreen`; move KubeconfigManager into `KubeconfigScreen` as a single (still large) component. Dock the global StatusBanner. Verify: nothing lost, banner visible on every screen.
5. Build the Clusters detail: `ProfileList`, `ClusterHeader`, `PatchRail` (+ `deriveRail`), `PasswordPrompt`, tabs `Nodes/Endpoints/Trust/Kubeconfig`; delete the old hero-card and panels. Add `get_profile_status` on select.
6. Split KubeconfigManager into the four tabs; add the new destructive-action confirmations; move Local runtime into `LocalRuntimePanel` and mount it in Infrastructure (a placeholder screen is acceptable until step 7).
7. Add `api.discoverInventory`, `createProfileFromEnvironment`, `InventoryContext`, `InfrastructureScreen` and children (needs lane A commands; until then develop against a typed mock in `src/mocks/inventory.ts` gated by `import.meta.env.DEV` and `?mock=1`, removed before commit). Wire U4 navigation.
8. Overview, `deriveAttention`, nav badges. Remove remaining inline `style={{}}` layout and color/size literals, switch `ConfirmModal` to `--scrim` and classes.
9. A11y and keyboard pass (section 5.1), reduced-motion pass, browser verification light and dark at 900, 1100, 1440 widths with mocked IPC.
10. Optional, separate PR: split `ProfileEditor.tsx` (862 lines) into `components/editor/*` (Hosts, Bastion, Bootstrap, Kubeconfig import, local-detect). Not required for the redesign.

Verification per AGENTS.md: `make verify`; state which checks were frontend-only (browser + mocked IPC) versus real Tauri runtime.

### 5.1 Accessibility and keyboard checklist
- [ ] Global `:focus-visible { outline: 2px solid var(--focus-ring); outline-offset: 2px }` for every interactive element (buttons, rows with role, tabs, inputs); no `outline: none` without replacement.
- [ ] Nav rail: `<nav aria-label="Primary">`, buttons with `aria-current="page"`.
- [ ] Profile list: `role="listbox"`, options `role="option" aria-selected`, roving tabindex, Up/Down/Home/End. Edit/Delete icon buttons are not nested inside the option (siblings in a row container) to avoid nested interactive controls (current markup nests buttons in `role="button"`).
- [ ] Tabs: `role=tablist/tab/tabpanel`, `aria-selected`, `aria-controls`, Left/Right arrows.
- [ ] Tables: real `<table>`, `<caption class="sr-only">`, `<th scope="col|row|rowgroup">`, group toggle `aria-expanded`, `aria-busy` while refreshing.
- [ ] Status never color-only: every pill has icon/dot and text; mini rail has `aria-label="Discover ok, Bootstrap skipped, SSH failed, Kubeconfig not run, Verify not run"`; full rail is an `<ol>` with `aria-current="step"` on a running jack and visually-hidden state text.
- [ ] `StatusBanner`: `role="status"` (success/warning) or `role="alert"` (error), dismiss button `aria-label="Dismiss"`.
- [ ] ConfirmModal: `role="dialog" aria-modal="true" aria-labelledby`, initial focus on Cancel for danger actions, focus trap, Esc cancels, focus returns to the trigger.
- [ ] Forms: every input has a `<label htmlFor>`; password inputs keep `autoComplete="off"`.
- [ ] Contrast: all text uses text tokens on bg tokens (verify `--text-tertiary` on `--bg-sunken` >= 4.5:1 in both themes; if the light value `#8a8375` fails on `#ece7dd`, darken `--of-color-text-muted` light to `#6f6859` and record it in tokens.css).
- [ ] Shortcuts (displayed in button `title`s): Cmd+1..4 sections; Cmd+R refresh current screen; Cmd+Enter Connect/Sync when a cluster is selected and no field in a modal has focus; `/` focus the Infrastructure filter; Cmd+N add cluster; Esc closes modal, then editor. None conflict with the macOS menu defaults; Cmd+R must `preventDefault` (webview reload).
- [ ] Reduced motion honored; no information conveyed by animation alone.
- [ ] Min width 900 verified: Clusters list 232px + detail 668px; tables scroll horizontally inside their container, never the page.

## 6. Behaviors that must NOT regress (checklist from App.tsx, KubeconfigManager, ProfileEditor)

Profile selection and state
1. First load selects the first profile if none selected; `loadError` is shown when `list_profiles` fails.
2. Switching profile resets lastResult, caViews, CA trust/remove targets, status message, closes the editor, clears both passwords (ephemeral, never carried across profiles).
3. Delete: ConfirmModal (danger, "Delete Profile"); after delete select the first remaining profile or none, clear per-profile state, close the editor if it was editing that profile; works from list, header and from inside the editor (`onDeleteRequest`).

Connect / Test
4. `connect_profile(id, bootstrapPassword||undefined, sshPassword||undefined)`; afterwards reload hosts status and run CA discovery with `result.endpoints`; CA failure is swallowed (console.error) and does not turn success into an error.
5. Message rules: success only when no failed hosts, no `errors`, and (`kubernetes` verified or no kubeconfig source); else warning, titled "Connect & Sync failed" when every host failed and k8s not verified, otherwise "completed with warnings"; host details formatted `name (address:port): detail`; success details mention host count, k8s version + endpoint, endpoint count, and hosts-file sync when `manage_hosts_file`; thrown error gives an error banner "Connect & Sync error".
6. Test Connection: `probe_profile_hosts(id, sshPassword)`, merges hosts into lastResult keeping aliases/kubeconfig/verification/endpoints/errors (defaults from `EMPTY_VERIFICATION`); messages for all-ok / N of M unreachable / error.
7. Both buttons disabled while either action runs or no profile; passwords cleared in `finally` for both actions.
8. SSH password field only if some host `auth === 'password'`; bootstrap password field only if `bootstrap.enabled`; `type=password`, `autoComplete="off"`, held in React state only.

Hosts, endpoints, hosts file
9. Per-host reachable pill ("SSH reachable" / "Needs retry") and failure detail line; "Open SSH session" per host (`open_ssh_session`), error banner on failure.
10. Scan endpoints: if no lastResult, create a stub with `verification.kubernetes: true`; CA overlay best-effort; success message "Found N external cluster endpoint(s)".
11. Sync to /etc/hosts (admin approval prompt is native), merges returned `endpoints` into lastResult only when a lastResult exists, reloads hosts status; Clear removes this profile's block only; synced pill and Auto-sync On/Off pill reflect `hostsStatus.is_synced` and `manage_hosts_file`.
12. FQDN patterns `<host>.<id>.clusterdeck.local` for nodes and bastion; Copy per row; Open uses `http://` for node/bastion FQDNs and `https://` for discovered endpoints via `open_url_in_browser`.

Trust (CAs)
13. Status labels: trusted / changed (rotated) / untrusted (new); button "Trust CA" or "Update Trust" (rotated uses `replace_ca_cmd`, else `trust_ca_cmd`); trusted rows offer Remove only; both flows behind ConfirmModal; Remove is danger; success texts keep the Safari/Chrome and "re-trust if the cluster is rebuilt" guidance; CA list refreshed best-effort after each action; per-CA warnings listed; expiry shown ("unknown" fallback).

Kubeconfig
14. Merge uses `backupFirst: true`, shows the safety-backup path when present, then refreshes verification via `get_profile_status` (creating a stub lastResult from host list when none).
15. Backup uses `moveFile: true`; `backed_up === false` yields the warning "No ~/.kube/config to backup".
16. KubeconfigManager capabilities all remain: set current context, delete context, restore/delete backup, open in Finder, managed per-profile configs, trusted CA removal (ConfirmModal), Local runtime start/stop/restart/shell/use-context with stop/restart confirmation and per-instance busy keys (a Set, so concurrent instance actions don't clear each other).
17. CA removal in Kubeconfig must refresh a selected cluster's Trust tab (now via `trustVersion`).

Editor
18. Create/edit profile, local-host detection prefill (ADR-0005), import from local kubeconfig, save via `save_profile`, `onSaved` reloads profiles. Do not change validation behavior.

Shell
19. Theme: `clusterdeck-theme` localStorage key; `null` follows the system; explicit choice sets `data-theme` on `<html>`; toggle flips effective theme; storage errors are swallowed.
20. Refresh clears the status message, reloads profiles and the selected profile's hosts status (now per-screen Refresh does the equivalent for its own data).
21. `__APP_VERSION__` shown in the shell; `min-width: 900px` kept; ConfirmModal API (`title, message, confirmLabel, cancelLabel, isDanger, busy, onConfirm, onCancel`) unchanged.
22. Security rules from AGENTS.md hold: no secrets in logs/state beyond ephemeral password fields; no new Tauri commands beyond the two in the contract; no direct filesystem or process access from React.
