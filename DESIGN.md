# DESIGN.md

English | [한국어](DESIGN-ko.md)

## Product archetype

`archetype: Operations Dashboard` (Desktop Operator)

ClusterDeck is a desktop application for Kubernetes and cluster node operators, providing unified cluster fleet management, SSH access, and Kubernetes connectivity.

- **Figma Reference:** [OpenForge Design System](https://www.figma.com/design/Y1JpRSOwctAKSwPjDNbe1g)
- **Reference Implementations:** OpenForge (`openforge/docs/design-system.md`), Dasomel Portal (`dasomel.github.io`)

## Product personality

- **Density:** High / Compact (tailored for macOS desktop operator workflow, tight node lists, terminal sessions, and resource status)
- **Visual weight:** High-contrast technical aesthetic with refined slate-blue dark mode surfaces and crisp borders
- **Accent:** Electric blue (`#38bdf8` / `#3b82f6`) with vivid semantic status indicators (running, warning, offline)

## Token mapping

Aligned with OpenForge & Figma Design System tokens:

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
