# DESIGN-ko.md

[English](DESIGN.md) | 한국어

## 제품 아키타입 (Product archetype)

`archetype: Operations Dashboard` (Desktop Operator)

ClusterDeck은 Kubernetes 및 클러스터 노드 운영자를 위한 데스크톱 대시보드로, 통합 플릿 관리, SSH 접속 및 Kubernetes 연결을 제공합니다.

- **Figma 디자인 시스템 원본:** [OpenForge Design System](https://www.figma.com/design/Y1JpRSOwctAKSwPjDNbe1g)
- **저장소 스냅샷:** [design/figma-snapshot.md](design/figma-snapshot.md) 및 [design/design-tokens.json](design/design-tokens.json)
- **스냅샷 기준일:** 2026-09-22
- **참조 디자인:** OpenForge (`openforge/docs/design-system-ko.md`), Dasomel Portal (`dasomel.github.io`)

## 제품 성격 (Personality)

- **밀도 (Density):** 높음 / 컴팩트 (macOS 데스크톱 오퍼레이터 워크플로에 최적화된 고밀도 노드 목록 및 상태 표시)
- **시각적 비중:** 슬레이트 블루 서피스와 또렷한 경계선 기반의 고대비 기술적 다크모드 미학
- **강조 색상:** 일렉트릭 블루 (`#38bdf8` / `#3b82f6`) 및 선명한 상태 지표 (실행 중, 경고, 오프라인)

## 시맨틱 토큰 매핑 (Token mapping)

OpenForge 및 Figma 디자인 시스템 토큰과 연동된 매핑:

Figma에 접근할 수 없을 때는 저장소 스냅샷을 기준으로 사용합니다. 외부 디자인이 변경되면
토큰 매핑과 provenance를 함께 갱신해야 합니다.

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

## 인프라 인벤토리 (Infrastructure Inventory)

인프라 인벤토리 화면은 OpenForge 디자인 토큰 체계(`--of-color-*`, 밀도, 타이포그래피, 상태 신호)를 ClusterDeck의 기존 레이아웃과 일관되게 공유합니다. 인프라 인벤토리를 클러스터 프로필과 결합하는 전체 UI 재설계 사양은 [design/ui-redesign.md](design/ui-redesign.md)를 참조하십시오.
