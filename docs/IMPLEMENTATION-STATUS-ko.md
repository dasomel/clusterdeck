# 구현 상태

Last verified: 2026-09-22 against `main`

이 문서는 미래 product direction이 아니라 현재 default branch의 동작을 기록합니다.

## 구현됨

- React/TypeScript UI와 Rust가 process/filesystem/network-sensitive operation을 소유하는 macOS-first Tauri 2 desktop application foundation.
- 자주 재생성되는 VM/Kubernetes 환경을 위한 Profile 중심 discovery/connection workflow.
- SSH key/bootstrap/alias, Bastion/ProxyJump, remote kubeconfig retrieval/normalization, Kubernetes connectivity verification 경로.
- Process execution을 위한 중앙 `CommandRunner` abstraction과 profile/SSH sink의 defensive validation.
- 사용자 파일 전체를 덮어쓰지 않는 SSH config, kubeconfig state, optional `/etc/hosts` managed-file boundary.
- `sshpass -e`/environment 처리와 private-key content를 frontend에 노출하지 않는 credential rule.
- 향후 autonomous mutation surface에 exact resolved-operation approval을 요구하는 OpenForge reduced local-tool security profile.
- Profile editor를 prefill하는 on-demand local VM detection(Colima/Lima/Vagrant). Detected 상태는 editor component state에만 유지되며 사용자가 profile을 저장하기 전까지 persist되지 않습니다 (ADR-0005 참조).
- 가져온 kubeconfig의 server endpoint를 정규화하기 위한 Kubernetes API endpoint discovery(APISIX/Ingress/Istio/Gateway API/Service), 그리고 기본/kubeconfig-manager/profile-editor 화면에 노출되는 status banner UI.
- Ingress/ApisixTls → Secret의 `ca.crt`를 discovery하여 CA별 New/Trusted/Rotated 상태를 계산하고, macOS login keychain에 대해 `security(1)`로 trust/replace/remove를 수행하며, Settings에 프로필 전체를 아우르는 Trusted CAs 목록을 제공합니다 (ADR-0006 참조).
- 발견된 CA/leaf 인증서마다 health warning을 표시합니다: `serverAuth` EKU 누락, leaf/CA 만료 임박(14일/30일 threshold, 이미 만료된 경우 별도 메시지), 발견된 host를 커버하지 않는 SAN, 구조적으로 올바르지 않은 CA(CA:TRUE/keyCertSign 누락).
- 원격 kubeconfig fetch가 `scp`를 shell-out하는 대신 SSH exec(`sudo cat` 실패 시 `cat`으로 fallback)로 읽도록 바뀌어 local temp-file permission window를 제거했습니다 (#23).
- curl Kubernetes API fallback 경로(예: macOS Sequoia Local Network Privacy로 `kubectl` 자체가 실패하는 경우)에서 TLS 인증서를 검증합니다: kubeconfig의 CA 데이터로 `--cacert` 검증하거나, CA 데이터가 없으면 curl의 system trust store를 사용하며, `-k`/`--insecure`는 사용하지 않습니다 (#24).
- Colima/Lima instance의 architecture/CPU/memory/disk 표시와 Docker context 연동을 포함한 local runtime discovery dashboard가 완성되어 Settings의 read-only "Local Runtime" 섹션으로 제공됩니다 (#14 Phase 1).
- 실제 대상에 대한 fresh-session replay로 `clusterdeck-connection-workflow` Agent Skill을 재검증했습니다; `openforge-maturity`는 다시 `verified`입니다 (#22).

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
- commit `d4d143a` (local runtime provider, kubeconfig endpoint fetch, status banner UI)
- `docs/adr/0005-local-host-detection-prefills-profiles.md`
- `docs/adr/0006-private-ca-local-trust.md`
- commit `cee7aea` (private CA local trust merge) and commit `066f720` (CA remove/manage fast-follow)
- commit `a0045c0` (CA/leaf certificate health warnings) and commit `c3eb682` (false-positive expiry fix, issue #25)
- commit `73e2312` (kubeconfig fetch over SSH exec instead of scp, issue #23)
- commit `21458de` (TLS certificate verification on the curl Kubernetes API fallback path, issue #24)
- commit `5e617d3` (local runtime discovery dashboard, issue #14 Phase 1) and commit `e6ff486` (relocated to Settings)
- commit `268feab` (`clusterdeck-connection-workflow` skill re-verified, issue #22); evidence at `research/issue-22-connection-workflow-replay-2026-09-22.md`
- commit `cb85aff` and commit `24b231c` (release workflow: macOS `.dmg` draft GitHub Release on `v*` tag push, third-party Actions pinned to commit SHAs)
