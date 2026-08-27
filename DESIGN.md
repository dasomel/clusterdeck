# DESIGN.md

English | [한국어](DESIGN-ko.md)

## Product archetype

`archetype: Operations Dashboard`

ClusterDeck is a desktop application for Kubernetes and cluster node operators, providing unified cluster fleet management and node shell automation.

## Product personality

- **Density:** High (compact layout for cluster nodes, terminal sessions, and resource status)
- **Visual weight:** Dark desktop native aesthetic with high-contrast system badges
- **Accent:** Electric blue (`#3b82f6`) and status indicators (running, warning, offline)

## Token mapping

```yaml
tokens:
  bgCanvas: var(--of-color-bg-canvas, #090d16)
  bgSurface: var(--of-color-bg-surface, #131b2e)
  bgSurfaceRaised: var(--of-color-bg-surface-raised, #1e293b)
  textPrimary: var(--of-color-text-primary, #f8fafc)
  textSecondary: var(--of-color-text-secondary, #94a3b8)
  textMuted: var(--of-color-text-muted, #64748b)
  borderDefault: var(--of-color-border-default, #1e293b)
  accentPrimary: var(--of-color-accent-primary, #3b82f6)
  danger: var(--of-color-status-danger, #ef4444)
  success: var(--of-color-status-success, #22c55e)
```

## Architecture and Desktop UI Boundaries

- Tauri desktop host handles OS-level process management and SSH execution.
- React frontend communicates via strictly typed Tauri IPC invoke channels.
- UI state updates reactively without blocking terminal streams.
