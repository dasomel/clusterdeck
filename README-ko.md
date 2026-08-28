# ClusterDeck

> VM 및 Kubernetes 환경을 빠르게 발견하고 SSH/kubeconfig를 자동 연결하는 macOS 중심 데스크톱 도구

[English](README.md) | **한국어**

ClusterDeck은 VM과 Kubernetes 환경을 자주 생성·삭제·재생성하면서 IP가 변경되는 환경을 대상으로 한다. 사용자는 IP 대신 사람이 기억하기 쉬운 Profile 이름을 유지하고, ClusterDeck이 SSH 접속, 선택적 SSH 키 bootstrap, Bastion/ProxyJump, 원격 kubeconfig 가져오기, Kubernetes 연결 확인을 자동화한다.

## 현재 상태

ClusterDeck은 현재 **초기 MVP / 소스 중심 프로젝트**다. 저장소에서 확인되는 핵심 범위는 Profile, SSH, Bastion/ProxyJump, kubeconfig 가져오기·정규화, Kubernetes 연결 확인으로 이어지는 Workstation Access 흐름이다.

현재 신규 사용자의 기본 경로는 Tauri 기반 소스 실행이다. 패키지 앱 배포, 대규모 Fleet 관리, 일반적인 Kubernetes 관리자 콘솔 기능은 Release나 저장소 문서에서 명시적으로 검증되기 전까지 구현 완료 기능으로 간주하지 않는다.

## 핵심 흐름

```text
IP / Host 검색
      ↓
SSH 연결 확인
      ↓
SSH Bootstrap (선택)
      ↓
SSH Alias / ProxyJump
      ↓
원격 kubeconfig 가져오기
      ↓
kubeconfig 정규화
      ↓
로컬 Profile
      ↓
Kubernetes 연결 확인
```

## 초기 범위

- macOS 중심 데스크톱 앱
- Tauri 2 + Rust 백엔드
- React + TypeScript 프론트엔드
- 멀티 VM Host Profile
- SSH 공개키 bootstrap 및 alias 관리
- Bastion / ProxyJump
- 원격 kubeconfig fetch 및 정규화
- `kubectl` 기반 연결 확인

ClusterDeck은 일반적인 Kubernetes 관리자 콘솔을 목표로 하지 않는다.

## 첫 성공 기준 (First Verified Success)

앱이 실행되는 것만으로는 성공으로 보지 않는다. 하나의 Profile이 아래 세 단계를 모두 통과했을 때 **첫 성공**으로 본다.

1. **SSH** — 직접 접속 또는 Bastion을 통해 선택한 Host에 실제로 도달한다.
2. **kubeconfig** — 원격 kubeconfig를 가져와 로컬 Profile용으로 정규화하며, credential을 로그나 공개 문서에 노출하지 않는다.
3. **Kubernetes API** — 정규화된 context로 `kubectl get nodes`와 같은 실제 API 호출에 성공한다.

개발 중 수동 교차 검증 예시는 다음과 같다.

```bash
ssh <profile-host> true
kubectl --context <normalized-context> get nodes
```

SSH는 성공했지만 Kubernetes API가 실패하면 완전한 Profile 성공이 아니라 부분 연결로 취급한다. Discovery, SSH bootstrap, kubeconfig 처리, Kubernetes 검증 사이의 책임 경계는 Architecture와 MVP Design 문서를 참고한다.

## 보안

공개 저장소이므로 실제 IP, hostname, 비밀번호, private key, kubeconfig, token, 인증서 등을 문서·이슈·테스트·스크린샷에 포함하지 않는다.

## 개발

```bash
pnpm install
pnpm tauri dev
```

검증:

```bash
pnpm build
cargo check --manifest-path src-tauri/Cargo.toml
```

## 문서 안내

- [Architecture](docs/ARCHITECTURE.md) — Workstation Access 계층과 책임 경계
- [MVP Design](docs/03-mvp-design.md) — MVP 동작과 설계 상세
- [Contributing](CONTRIBUTING.md) — 기여 절차
- [Security](SECURITY.md) — 보안 정책과 취약점 제보
- [Repository Engineering Rules](AGENTS.md) — 저장소 엔지니어링 계약

현재 코드와 오래된 설계 문서가 충돌하면 현재 소스와 명시적으로 검증된 동작을 우선한다. 아키텍처 경계가 바뀌면 관련 설계 문서도 같은 변경에서 갱신한다.

## 기여 / 피드백

IP가 자주 바뀌는 VM, Bastion, 원격 kubeconfig 환경에서의 외부 피드백이 특히 유용하다. 재현 가능한 실패는 GitHub Issues로 제보하되 실제 내부 인프라 정보는 제거한다.

## License

Apache License 2.0. [LICENSE](LICENSE) 참조.
