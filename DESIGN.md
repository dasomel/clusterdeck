# DESIGN.md

English | [한국어](DESIGN-ko.md)

## Product archetype

`archetype: Operations Dashboard` (Desktop Operator)

ClusterDeck is a desktop application for Kubernetes and cluster node operators, providing unified cluster fleet management, SSH access, and Kubernetes connectivity.

- **Figma Reference:** [OpenForge Design System](https://www.figma.com/design/Y1JpRSOwctAKSwPjDNbe1g)
- **Repository snapshot:** [design/figma-snapshot.md](design/figma-snapshot.md) and [design/design-tokens.json](design/design-tokens.json)
- **Snapshot date:** 2026-09-22
- **Reference Implementations:** OpenForge (`openforge/docs/design-system.md`), Dasomel Portal (`dasomel.github.io`)

## Product personality

- **Density:** High / Compact (tailored for macOS desktop operator workflow, tight node lists, terminal sessions, and resource status)
- **Visual weight:** High-contrast technical aesthetic with refined slate-blue dark mode surfaces and crisp borders
- **Accent:** Electric blue (`#38bdf8` / `#3b82f6`) with vivid semantic status indicators (running, warning, offline)

## Token mapping

Aligned with OpenForge & Figma Design System tokens:

The repository snapshot is the fallback source when the Figma file cannot be accessed. Update
the snapshot and its provenance together with this mapping when the external design changes.

```yaml
tokens:
  # Surfaces & Canvas
  bgCanvas: var(--of-color-bg-canvas, var(--bg, #0f141c))
  bgSurface: var(--of-color-bg-surface, var(--bg-elevated, #161f2c))
  bgSurfaceSunken: var(--of-color-bg-subtle, var(--bg-sunken, #0b0f16))
  bgSurfaceRaised: var(--of-color-bg-surface-raised, #1e2b3e)

  # Text & Content
  textPrimary: var(--of-color-text-primary, var(--text-primary, #f1f5f9))
  textSecondary: var(--of-color-text-secondary, var(--text-secondary, #cbd5e1))
  textMuted: var(--of-color-text-muted, var(--text-tertiary, #94a3b8))

  # Borders & Dividers
  borderDefault: var(--of-color-border-default, var(--border, #28374d))
  borderStrong: var(--of-color-border-strong, var(--border-strong, #3e5270))

  # Action & Brand
  accentPrimary: var(--of-color-accent-primary, var(--accent, #38bdf8))
  accentContrast: var(--of-color-accent-contrast, var(--accent-contrast, #031525))
  focusRing: var(--of-color-focus-ring, #38bdf8)

  # Status Signals
  statusSuccess: var(--of-color-status-success, var(--success, #22c55e))
  statusWarning: var(--of-color-status-warning, #f59e0b)
  statusDanger: var(--of-color-status-danger, var(--danger, #ef4444))
  statusInfo: var(--of-color-status-info, #38bdf8)
```

## Architecture and Desktop UI Boundaries

- Tauri desktop host handles OS-level process management and SSH execution.
- React frontend communicates via strictly typed Tauri IPC invoke channels.
- UI state updates reactively without blocking terminal streams.

## Infrastructure Inventory

The Infrastructure inventory view shares the OpenForge design token system (`--of-color-*`, density, typography, and status indicators) with ClusterDeck's existing layout. The full UI redesign incorporating the infrastructure inventory alongside cluster profiles is specified in [design/ui-redesign.md](design/ui-redesign.md).
