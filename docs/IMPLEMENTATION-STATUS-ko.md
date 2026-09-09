# 구현 상태

Last verified: 2026-09-09 against `main`

이 문서는 미래 product direction이 아니라 현재 default branch의 동작을 기록합니다.

## 구현됨

- React/TypeScript UI와 Rust가 process/filesystem/network-sensitive operation을 소유하는 macOS-first Tauri 2 desktop application foundation.
- 자주 재생성되는 VM/Kubernetes 환경을 위한 Profile 중심 discovery/connection workflow.
- SSH key/bootstrap/alias, Bastion/ProxyJump, remote kubeconfig retrieval/normalization, Kubernetes connectivity verification 경로.
- Process execution을 위한 중앙 `CommandRunner` abstraction과 profile/SSH sink의 defensive validation.
- 사용자 파일 전체를 덮어쓰지 않는 SSH config, kubeconfig state, optional `/etc/hosts` managed-file boundary.
- `sshpass -e`/environment 처리와 private-key content를 frontend에 노출하지 않는 credential rule.
- 향후 autonomous mutation surface에 exact resolved-operation approval을 요구하는 OpenForge reduced local-tool security profile.

## 부분적 / 환경 의존

- Unit/FakeRunner test는 application control flow를 증명하지만 실제 OpenSSH, kubectl, native filesystem, target cluster 동작을 증명하지 못합니다. Critical path 변경은 가능한 경우 real-binary/runtime evidence가 필요합니다.
- 현재 application은 macOS-first입니다. Rust/React의 portability만으로 다른 platform 지원을 주장하지 않습니다.

## 주장하지 않음

- General Kubernetes administration console이 아닙니다.
- OpenForge security profile 문서가 autonomous agent-driven mutation을 활성화하지 않습니다.

## Evidence

- `README.md`
- `AGENTS.md`
- `docs/ARCHITECTURE.md`
- `docs/03-mvp-design.md`
- `src-tauri/`
- repository `make verify` / CI
- PR #16 (`e7daf5bcf64786c3d253674f6f0486882d040d70`)
