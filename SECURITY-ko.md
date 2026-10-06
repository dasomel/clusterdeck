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

## 인프라 및 로컬 디스커버리 보안 (Infrastructure & Local Discovery Security)

- **제한된 명령 실행:** Provider CLI 디스커버리는 셸 없이 고정된 어댑터 명령만 실행하며, 10초 타임아웃 및 stdout/stderr 각각 2 MiB 버퍼 상한(`CommandRunner::run_bounded`)으로 제한됩니다. IPC가 임의의 실행 파일 이름이나 인자를 허용하도록 확장하지 않습니다.
- **신뢰할 수 있는 Provider 경계:** 설치된 provider CLI(`colima`, `VBoxManage`, `vagrant`, `vmrun`)와 로컬 VM 설정 파일은 신뢰할 수 있어야 합니다. 플러그인 초기화 및 provider 자식 프로세스 동작은 ClusterDeck의 프로세스 샌드박스 외부에서 수행됩니다.
- **경로 세그먼트 검증:** 파일시스템 경로(`.vagrant/machines/<name>`, `.vmx` 파일, Lima 설정 등)로 결합되는 동적 머신 이름, 런타임 ID, 프로젝트 디렉터리는 엄격한 경로 세그먼트 검증(`is_safe_path_segment`)을 통과해야 하며 경로 구분자, 디렉터리 순회(`..`), 선행 하이픈, NUL 바이트, 개행 문자를 금지합니다.
- **안전한 UI 렌더링:** 신뢰할 수 없는 메타데이터에서 조회된 VM 이름, 경로, 상태 문자열은 순수 텍스트로만 렌더링됩니다(HTML 또는 리치 텍스트 주입 방지).
- **실행 제한 및 Demo 안전성:** 임의의 셸 명령, Vagrantfile 코드 실행, 레지스트리 정리, 확인되지 않은 게스트 변경을 수행하지 않습니다. Demo 모드는 호스트 명령을 실행하지 않으며 로컬 VM 파일을 읽지 않습니다.

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
