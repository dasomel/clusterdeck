# 호환성 및 코드 서명 정책 (Compatibility & Code Signing Policy)

본 문서는 ClusterDeck의 버전 업그레이드 시 설정, 상태 파일, 런타임 호환성 보장 기준과 다운그레이드 동작, 1.0.0 릴리스를 위한 코드 서명 및 배포 정책을 정의합니다.

---

## 1. 상태 및 설정 호환성 보장 (Compatibility Guarantees)

ClusterDeck은 애플리케이션 업그레이드 시 이전 설정과 사용자 데이터를 파괴하지 않는 무손실(lossless) 및 상위 호환성 정책을 준수합니다.

### 1.1 `profiles.yaml` (무손실 프로필 저장소)
- **무손실 보존(Lossless Persistence):** 0.5.0 이상 버전부터는 파싱되지 않은 알 수 없는 YAML 키, 사용자 주석, 미래 스키마 속성을 프로필 수정/저장/삭제 시에도 그대로 보존합니다.
- **무효 프로필 보호(Invalid Profile Protection):** 문법 오류나 유효성 검증에 실패한 프로필이 존재하더라도, 다른 정상 프로필을 저장할 때 무효 프로필이 영구 삭제(drop)되지 않습니다.
- **낙관적 락(Optimistic Locking):** 저장 시 `base_fingerprint`를 검증하여 동시 편집으로 인한 덮어쓰기 충돌을 방지합니다.
- **원자적 쓰기(Atomic Writes):** `profiles.yaml`에 대한 모든 저장은 임시 파일을 거쳐 atomic rename 방식으로 수행되어 파일 손상을 방지합니다.

### 1.2 사용자 상태 디렉토리 (`~/.clusterdeck/`)
ClusterDeck의 모든 런타임 상태 파일은 오직 `~/.clusterdeck/` 아래에 저장되며, 일반 사용자 권한으로 소유됩니다:

```text
~/.clusterdeck/
├── profiles.yaml          # 프로필 원본 저장소
├── state.json             # 연결 상태 캐시 및 API 터널 대상 정보
├── ssh/
│   └── <id>.conf          # 프로필별 생성된 OpenSSH 설정
├── kubeconfigs/
│   └── <id>.yaml          # 프로필별 격리/정규화된 kubeconfig
├── tunnels/
│   └── <id>.json          # 활성 터널 PID 및 메타데이터 (시작 시 오펀 자동 리핑)
├── bin/
│   └── clusterdeck-dns    # 독립 로컬 DNS 응답기 바이너리 (0755 모드)
└── dns/
    ├── zones.json         # 로컬 DNS 존 정의 및 A 레코드
    ├── status.json        # 응답기 런타임 상태 및 세대(generation)
    ├── generation         # 단조 증가 세대 카운터
    └── endpoints.json     # 프로필별 검색된 엔드포인트 캐시
```

- 앱 버전 업데이트 시 `~/.clusterdeck/` 디렉토리를 초기화하거나 삭제하지 않습니다.
- 새 디렉토리나 스키마 필드가 추가될 때는 기본값(`#[serde(default)]`)을 제공하여 기존 파일이 사용자 개입 없이 유지됩니다.

### 1.3 시스템 연동 파일 (`/etc/resolver` 및 `/etc/hosts`)
- **명시적 소유권 마커:**
  - `/etc/resolver/<zone>`: `# managed by ClusterDeck (profile: <id>)` 마커 사용.
  - `/etc/hosts`: `# >>> ClusterDeck BEGIN (profile: <id>) >>>` / `END` 블록 사용.
- ClusterDeck이 관리하지 않는 외부 설정 라인이나 제3자 resolver 파일은 절대로 변경하거나 삭제하지 않습니다.
- 무효 프로필이나 파일 손상이 감지될 때는 resolver 보호 모드(`protect_resolver`)가 활성화되어 전역 정리나 미참조 존 자동 삭제를 차단합니다.

---

## 2. 다운그레이드 정책 및 동작 (Downgrade Behavior)

하위 버전으로 롤백(다운그레이드)할 경우의 동작 및 복구 방법은 다음과 같습니다:

### 2.1 0.5.0 미만 버전으로 다운그레이드
- **배경:** 0.5.0 버전에서 기존 불리언 `manage_hosts_file: bool`이 열거형 `host_mapping: 'hosts' | 'local_dns' | 'off'`로 확장 마이그레이션되었습니다.
- **다운그레이드 시 동작:** 0.5.0 미만 구버전은 `host_mapping` 키를 인식하지 못하므로, 기존 `manage_hosts_file`이 없으면 이름 해석 매핑이 비활성화(`false`)됩니다 (ADR-0011 R9).
- **자동 백업 파일:** 0.5.0 마이그레이션 실행 시 다음 백업 파일이 자동 생성됩니다:
  ```text
  ~/.clusterdeck/profiles.yaml.pre-local-dns-backup
  ```
- **복구 방법:** 0.5.0 미만으로 다운그레이드 시 해당 백업 파일을 원본으로 복사합니다:
  ```bash
  cp ~/.clusterdeck/profiles.yaml.pre-local-dns-backup ~/.clusterdeck/profiles.yaml
  ```

### 2.2 0.4.0 미만 버전으로 다운그레이드
- 0.4.0 미만 버전은 `~/.clusterdeck/kubeconfigs/` 아래의 격리 파일을 사용하지 않으므로, 다운그레이드 후 클러스터에 다시 **Connect**하여 설정을 재동기화해야 합니다.

---

## 3. 코드 서명 및 배포 정책 (Code Signing & Distribution Policy)

### 3.1 현재 상태 (v0.5.x / 1.0.0-rc)
- ClusterDeck은 현재 **ad-hoc 코드 서명**(`tauri.conf.json`의 `signingIdentity: "-"`)으로 빌드됩니다.
- 현재 Apple Developer ID 인증서 없이 오픈 소스로 배포되므로:
  1. 최초 다운로드 후 실행 시 macOS Gatekeeper 경고("확인되지 않은 개발자")가 발생할 수 있습니다.
  2. 사용자는 `/Applications`에 복사한 후 격리(quarantine) 속성을 해제해야 합니다:
     ```bash
     xattr -d com.apple.quarantine /Applications/ClusterDeck.app
     ```
  3. `~/.clusterdeck/bin/clusterdeck-dns`로 복사되는 로컬 DNS 응답기 바이너리 역시 원본 앱 번들의 ad-hoc 서명을 그대로 유지하며, 커널/시스템 확장이 아닌 일반 사용자 LaunchAgent로 안전하게 실행됩니다.

### 3.2 1.0.0 릴리스 전략
- **초기 1.0.0 배포:** 명확한 설치 및 Gatekeeper 우회 안내 문서를 동반한 자체 빌드/오픈소스 개발자 도구 형태로 릴리스합니다.
- **향후 Developer ID 서명 도입:** Apple Developer Program 등록, 공식 Developer ID Application 인증서 서명 및 `.github/workflows/release.yml` 내 공증(Notarization) 자동화는 프로젝트 스폰서십 및 인증서 발급이 완료되는 포스트 1.0 마일스톤으로 진행합니다.
