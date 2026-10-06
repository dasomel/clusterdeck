# Figma Design Snapshot

This directory contains the repository-owned fallback for the external Figma reference.
The live source is intentionally retained in [`DESIGN.md`](../DESIGN.md), but implementation
work must remain possible when the Figma file is unavailable.

## Source and provenance

- Source: [OpenForge Design System](https://www.figma.com/design/Y1JpRSOwctAKSwPjDNbe1g)
- Snapshot date: 2026-09-22
- Snapshot status: repository baseline, derived from the token mapping already documented in
  `DESIGN.md` and the implemented aliases in `src/styles.css`
- Direct Figma extraction: unavailable in the current environment

This file is therefore a stable implementation reference, not a claim that every Figma frame
or component has been exported. When the source becomes available again, update this snapshot
and record the new date and changed sections.

## Product direction

ClusterDeck uses a compact macOS desktop operator interface for SSH and Kubernetes environment
access. The visual language is a high-contrast technical dashboard:

- dense information layout with compact cards and lists;
- slate-blue surfaces and clear borders;
- electric-blue primary actions and focus states;
- explicit success, warning, danger, and informational status colors;
- system UI typography with a monospace face for technical values.

## Semantic tokens

The canonical runtime implementation is in [`src/styles.css`](../src/styles.css). The aliases
below preserve the Figma/OpenForge naming while allowing the application to provide its own
light/dark values.

| Semantic role | CSS custom property | Fallback |
| --- | --- | --- |
| Canvas | `--of-color-bg-canvas` → `--bg` | `#0f141c` |
| Surface | `--of-color-bg-surface` → `--bg-elevated` | `#161f2c` |
| Sunken surface | `--of-color-bg-subtle` → `--bg-sunken` | `#0b0f16` |
| Raised surface | `--of-color-bg-surface-raised` | `#1e2b3e` |
| Primary text | `--of-color-text-primary` → `--text-primary` | `#f1f5f9` |
| Secondary text | `--of-color-text-secondary` → `--text-secondary` | `#cbd5e1` |
| Muted text | `--of-color-text-muted` → `--text-tertiary` | `#94a3b8` |
| Default border | `--of-color-border-default` → `--border` | `#28374d` |
| Strong border | `--of-color-border-strong` → `--border-strong` | `#3e5270` |
| Primary accent | `--of-color-accent-primary` → `--accent` | `#38bdf8` |
| Accent contrast | `--of-color-accent-contrast` → `--accent-contrast` | `#031525` |
| Focus ring | `--of-color-focus-ring` | `#38bdf8` |
| Success | `--of-color-status-success` → `--success` | `#22c55e` |
| Warning | `--of-color-status-warning` → `--warning` | `#f59e0b` |
| Danger | `--of-color-status-danger` → `--danger` | `#ef4444` |
| Informational | `--of-color-status-info` | `#38bdf8` |

## Implementation rules

1. Use semantic tokens rather than hardcoded colors in React components.
2. Keep technical identifiers, addresses, commands, and status details in the monospace face.
3. Preserve visible focus states for keyboard and desktop navigation.
4. Treat status colors as semantic signals; do not use them as decoration.
5. Add new tokens here and to `DESIGN.md` before introducing a new visual primitive.

## Update procedure

When Figma access is available:

1. Export or inspect the changed variables/components.
2. Update this snapshot and `design/design-tokens.json` together.
3. Update `DESIGN.md` and `DESIGN-ko.md` provenance dates.
4. Run the frontend build and visually inspect the affected screen.
