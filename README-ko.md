# ClusterDeck

> VM 및 Kubernetes 환경을 빠르게 발견하고 SSH/kubeconfig를 자동 연결하는 macOS 중심 데스크톱 도구

[English](README.md) | **한국어**

ClusterDeck은 VM과 Kubernetes 환경을 자주 생성·삭제·재생성하면서 IP가 변경되는 환경을 대상으로 한다. 사용자는 IP 대신 사람이 기억하기 쉬운 Profile 이름을 유지하고, ClusterDeck이 SSH 접속, 선택적 SSH 키 bootstrap, Bastion/ProxyJump, 원격 kubeconfig 가져오기, Kubernetes 연결 확인을 자동화한다.

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

자세한 내용:

- [Architecture](docs/ARCHITECTURE.md)
- [MVP Design](docs/03-mvp-design.md)
- [Contributing](CONTRIBUTING.md)
- [Security](SECURITY.md)
- [Repository Engineering Rules](AGENTS.md)

## License

Apache License 2.0. [LICENSE](LICENSE) 참조.
