# 보안 정책 (Security Policy)

[English](SECURITY.md) | 한국어

## 적용 범위 (Scope)

ClusterDeck은 SSH 구성, 개인 키 참조, 일회성 부트스트랩 비밀번호, Kubernetes kubeconfig 등 민감한 로컬 접근 자산을 처리합니다.

## 보안 규칙 (Rules)

- 개인 키, 비밀번호, 토큰, kubeconfig, 실제 인프라 엔드포인트를 저장소에 커밋하지 않습니다.
- 생성된 자격 증명 및 kubeconfig 파일은 저장소 외부에 보관합니다.
- 영구 저장이 필요한 시크릿은 macOS Keychain 또는 이에 상응하는 안전한 로컬 보안 메커니즘을 사용합니다.
- 로그에 비밀번호, 개인 키 내용, kubeconfig 인증 정보, 베어러 토큰을 절대 출력하지 않습니다.
- 생성된 SSH 및 kubeconfig 파일은 최소 권한 파일 권한을 적용합니다.
- 파괴적 작업은 명시적이어야 하며 안전한 복구 경로를 제공해야 합니다.

## 공개 저장소 준수 사항

본 저장소는 공개 OSS입니다. 예제, 테스트 픽스처, 스크린샷, 이슈 보고서, 문서에는 항상 플레이스홀더를 사용해야 합니다.

```text
192.0.2.10
cluster.example.invalid
user: example
```

## 취약점 보고 절차 (Reporting a Vulnerability)

공개 GitHub Issue에 미공개 보안 취약점을 등록하지 마십시오. GitHub Private Vulnerability Reporting(비공개 보안 권고)을 사용하거나 유지관리자에게 비공개로 보고해 주십시오. 48시간 이내에 접수 확인 및 대응 일정을 안내합니다.

참조: [OpenForge Security Standard](https://github.com/dasomel/openforge/blob/main/docs/security.md)
