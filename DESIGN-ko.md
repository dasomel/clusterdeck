# DESIGN-ko.md

[English](DESIGN.md) | 한국어

## 제품 아키타입 (Product archetype)

`archetype: Operations Dashboard`

ClusterDeck은 Kubernetes 및 클러스터 노드 운영자를 위한 데스크톱 대시보드로, 통합 플릿 관리 및 노드 셸 자동화를 제공합니다.

## 제품 성격 (Personality)

- **밀도 (Density):** 높음 (High — 노드 목록, 터미널 세션, 리소스 상태를 위한 컴팩트 레이아웃)
- **시각적 비중:** 다크 데스크톱 네이티브 테마 및 고대비 시스템 배지
- **강조 색상:** 일렉트릭 블루 (`#3b82f6`) 및 운영 상태 지표 (실행 중, 경고, 오프라인)

## 시맨틱 토큰 매핑 (Token mapping)

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
