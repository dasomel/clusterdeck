# 내장 Local DNS 사용자 가이드

ClusterDeck은 macOS에서 `/etc/hosts` 파일을 수정하는 대신 `/etc/resolver`를 통해 프라이빗 클러스터 도메인을 확인하는 내장 로컬 DNS 응답기(`host_mapping: local_dns`)를 제공합니다.

아키텍처 배경 및 설계 결정에 대한 자세한 내용은 [ADR-0011: 내장 로컬 DNS](adr/0011-built-in-local-dns.md)와 [ARCHITECTURE.md §7.1](ARCHITECTURE.md#7-hosts-file-management)을 참조하십시오.

---

## 1. 내장 Local DNS가 필요한 이유

macOS에서 호스트 이름 확인(resolution)은 두 개의 독립된 하위 시스템으로 분리되어 동작합니다:

| 경로 | 구성요소 / 소비자 | `/etc/hosts` 준수 여부 | `/etc/resolver/<zone>` 준수 여부 |
|---|---|---|---|
| **libinfo (`getaddrinfo`)** | 터미널 도구, `curl`, `kubectl`, Chrome, Firefox, `dscacheutil` | 준수함 | **준수함** |
| **Network.framework / mDNSResponder** | Safari, `URLSession`, 네이티브 macOS 시스템 서비스 | **일관되지 않음 / 무시됨** (특정 시스템 확장 및 설정 환경) | **준수함** |

macOS가 Network.framework 기반 애플리케이션에서 `/etc/hosts`를 무시하는 환경에서는, 터미널에서 `curl`로는 서비스에 정상 접속되더라도 Safari에서는 클러스터 웹 UI를 열지 못하는 현상이 발생합니다.

또한 `/etc/hosts`를 편집하는 방식은 **IP나 엔드포인트가 바뀔 때마다** 매번 루트 권한(`sudo` / 관리자 암호 프롬프트)을 요구합니다. 파드나 서비스 로드밸런서가 재생성되어 주소가 변경될 때마다 사용자에게 암호 입력 창이 뜨게 됩니다.

ClusterDeck의 내장 Local DNS는 두 문제를 모두 해결합니다:
1. **일관된 이름 확인:** libinfo와 Network.framework 모두 도메인별 `/etc/resolver/<zone>` 설정을 완벽하게 준수합니다.
2. **권한 프롬프트 없는 레코드 갱신 (Zero-Prompt):** `/etc/resolver` 파일은 로컬 루프백 응답기(`127.0.0.1:15353`)를 가리킵니다. 실제 레코드 정보는 사용자 소유의 JSON 파일(`~/.clusterdeck/dns/zones.json`)에 저장됩니다. 클러스터에 연결(**Connect**)하면 로컬 DNS가 자동으로 갱신되며, 엔드포인트나 IP가 변경되어도 관리자 암호 입력 없이 원자적으로(atomically) 갱신됩니다.

---

## 2. 호스트 매핑 모드

ClusterDeck 프로필은 `host_mapping` 설정 키를 통해 세 가지 이름 확인 모드를 지원합니다:

| 모드 | 키 값 (`host_mapping`) | 동작 방식 | 관리자 프롬프트 빈도 | Safari 호환성 |
|---|---|---|---|---|
| **Off** | `off` | 로컬 이름 매핑을 수행하지 않음 | 없음 | 불가 |
| **Hosts** | `hosts` | `/etc/hosts`에 `# >>> ClusterDeck BEGIN` 블록 작성 | IP나 엔드포인트가 변경될 때마다 | macOS 환경에 따라 불안정 |
| **Local DNS** | `local_dns` | `127.0.0.1:15353` 사용자 LaunchAgent + `/etc/resolver/<zone>` | 존(Zone) 세트가 변경될 때만 | **완전 지원** |

### 모드 전환 방법

- **UI에서 전환:** 클러스터 프로필 설정에서 **Host name mapping**을 **Local DNS (zones approved separately)**로 변경합니다. 클러스터에 연결(**Connect**)하면 로컬 DNS가 자동으로 갱신되며, 필요 시 **Endpoints** 탭에서 **Sync to local DNS**를 직접 클릭할 수도 있습니다.
- **설정 파일에서 전환:** `~/.clusterdeck/profiles.yaml` 파일을 직접 편집합니다:
  ```yaml
  profiles:
    - id: lab
      name: Lab Cluster
      host_mapping: local_dns
      approved_dns_zones:
        - lab.example.internal
  ```
- **중단 없는 전환 (Make-Before-Break, MBB):** `hosts` 모드와 `local_dns` 모드 사이를 전환할 때, ClusterDeck은 단 한 번의 권한 승인 스크립트 실행으로 `/etc/hosts` 정리와 `/etc/resolver` 생성/삭제를 함께 처리합니다. 두 매핑 방식이 모두 끊어지는 공백 시간은 발생하지 않습니다.

---

## 3. 존(Zone) 아키텍처

ClusterDeck에서 **존(Zone)**은 루프백 응답기가 authoritative(권한 있는) 방식으로 직접 응답하는 전용 도메인 접미사(suffix)입니다.

### 3.1 노드 별칭 존 (`<label>.clusterdeck.internal`)
`host_mapping: local_dns`가 활성화된 각 프로필은 다음과 같은 별칭 존을 자동으로 할당받습니다:
```text
<label>.clusterdeck.internal
```
여기서 `<label>`은 프로필 ID로부터 유도됩니다(소문자 변환, 언더스코어를 하이픈으로 대체, 최대 63자 제한; 예: 프로필 `lab`은 `lab.clusterdeck.internal`).

- 노드 별칭은 `<node-name>.<label>.clusterdeck.internal` 형태를 가집니다(예: `m1.lab.clusterdeck.internal` → `192.0.2.10`).
- 베스천(Bastion)이 설정된 경우 `<bastion-name>.<label>.clusterdeck.internal` 형태를 가집니다.
- ClusterDeck이 `.clusterdeck.internal` 네임스페이스를 완전히 소유하고 있으므로, 이 존은 **별도의 사용자 승인 없이 자동으로 제공**됩니다.

### 3.2 Ingress 존 승인
Kubernetes에서 발견된 엔드포인트(Ingress, APISIX, Gateway API)는 전용 프라이빗 도메인(예: `api.lab.example.internal`)을 사용하는 경우가 많습니다.

- **발견(Discovery):** 클러스터에 연결(**Connect**)하거나 **Scan Endpoints**를 클릭하면 Kubernetes 엔드포인트를 감지하고 백엔드 `suggest_dns_zones` 명령을 통해 후보 존을 즉시 평가합니다. 엔드포인트 검색이 실패하더라도(예: 클러스터 연결 또는 SSH 터널 다운, kubeconfig 부재), ClusterDeck은 캐시 파일(`~/.clusterdeck/dns/endpoints.json`)에 저장된 마지막 정상 레코드를 유지하고 사용자에게 경고를 표시하며 레코드를 삭제하지 않습니다.
- **승인(Approval):** 후보 존은 사전 동기화(sync) 없이도 즉시 표시됩니다:
  - 상단 **Suggested zones:** 영역에 추천된 상위 도메인별로 단일 **Approve <zone>** 버튼(예: `Approve lab.example.internal`)이 표시됩니다.
  - 아직 서비스되지 않는 각 엔드포인트 행(row)에도 인라인 **Approve <zone>** 버튼이 함께 표시됩니다.
  - 미제공 호스트 경고 배너에는 제외된 호스트 및 원인 목록만 안내되며, 조작 버튼은 포함되지 않습니다.
  - **활성화(Activation):** **Approve <zone>** 버튼을 클릭하면 `approve_dns_zone`이 호출되어 관리자 권한 프롬프트가 한 번 표시되고 `/etc/resolver/<zone>` 파일이 생성됩니다. 생성 이후에는 해당 존 하위의 모든 현재 및 미래 엔드포인트가 즉시 정상 확인됩니다.

### 검색 실패와 동시 설정 변경

Services, Ingress, APISIX, Istio, Gateway API의 검색 성공 여부를 각각 확인하며, 실패한 종류의 기존 레코드는 유지합니다. Services 조회가 실패하면 IP를 결정할 수 없는 경우도 포함해 비어 있지 않은 의존 리소스 목록을 실패로 처리합니다. 실제로 빈 목록을 반환한 종류의 오래된 레코드는 제거합니다. **Scan Endpoints**는 부분 검색 실패와 캐시 저장 오류를 함께 표시합니다.

Hosts 모드에서는 출처를 모르는 기존 레코드도 캐시에 저장하여 연속된 부분 실패 동안 보존합니다. 이 레코드는 전체 검색에 성공한 Hosts 동기화가 대체할 때까지 오래된 값으로 남을 수 있습니다. Sync는 프로필 변경과 같은 잠금 안에서 저장된 설정을 다시 확인합니다. 검색 도중 프로필이 삭제되거나 mapping이 변경되면 쓰기 전에 중단합니다. **Off** 상태에서는 **Hosts** 또는 **Local DNS**를 선택한 뒤 동기화하십시오.

### 3.3 프라이빗 TLD 규칙
공용 인터넷 도메인이나 클러스터 테넌트가 임의로 지정한 도메인(예: Ingress에 지정된 `github.com`)이 가로채지는 것을 방지하기 위해, 존 이름은 엄격한 검증(`is_allowed_private_zone`)을 통과해야 합니다:
1. **소문자 ASCII 문자열만 허용.**
2. **최소 3개 이상의 레이블 필요** (예: `lab.example.internal`은 레이블이 3개이므로 허용되지만, `example.internal`은 2개이므로 거부됨).
3. **지정된 프라이빗 TLD로만 제한:** 존은 반드시 다음 TLD 중 하나로 끝나야 합니다:
   - `.internal`
   - `.test`
   - `.example`
   - `.local` (단, `clusterdeck.internal`, `clusterdeck.local`(레거시) 및 그 하위 도메인은 노드 별칭용 예약 도메인임)
   - `.home.arpa`
   - `.localhost`
   - `.invalid`

### 3.4 존당 단일 소유자 원칙 및 소유권 전환
하나의 존은 한 번에 오직 **하나의 프로필만** 소유할 수 있습니다:
- 프로필 `lab`이 `lab.example.internal`을 소유하고 있다면 다른 프로필이 이를 동시에 소유할 수 없습니다.
- 여러 프로필이 동일한 존을 등록하려고 하면 충돌 상태(`owned_by_other_profile` 또는 `claimed_by_multiple`)가 되며 충돌이 해결되기 전까지 해당 존은 서비스되지 않습니다.
- 프로필 간에 존을 이전하려면 기존 프로필에서 존을 제거하고 대상 프로필에서 승인하거나(또는 프로필 설정에서 소유권을 전환), 존 소유권을 변경합니다. 전환 시 ClusterDeck은 `/etc/resolver/<zone>`에 기록된 기존 마커를 확인한 후 resolver 파일과 소유권 레코드를 원자적으로 갱신합니다.

---

## 4. 설치되는 구성 요소

ClusterDeck은 시스템에 불필요한 데몬을 상주시키지 않으며, 모든 백그라운드 상태를 사용자 계정 디렉터리 내에 격리하여 보관합니다:

```text
~/
├── Library/LaunchAgents/
│   └── com.clusterdeck.dns.plist
└── .clusterdeck/
    ├── bin/
    │   └── clusterdeck-dns            # 독립 실행형 응답기 바이너리 (권한 0755)
    └── dns/
        ├── zones.json                 # 권한 있는 존 정의 및 A 레코드 목록
        ├── status.json                # 응답기 런타임 상태 및 로드된 generation
        ├── generation                 # 단조 증가하는 generation 카운터
        ├── endpoints.json             # 프로필별 발견된 엔드포인트 캐시
        └── agent.err.log              # LaunchAgent 표준 에러(stderr) 로그

/etc/
└── resolver/
    ├── lab.clusterdeck.internal          # 노드 별칭용 resolver 파일
    └── lab.example.internal           # 승인된 ingress 존용 resolver 파일
```

### 4.1 사용자 LaunchAgent (`com.clusterdeck.dns`)
- 경로: `~/Library/LaunchAgents/com.clusterdeck.dns.plist`
- 일반 사용자 계정 권한으로 실행됩니다(`KeepAlive: true`, `RunAtLoad: true`).
- 루트(root) 권한으로 실행되지 않으며, 시스템 전역 백그라운드 서비스를 요구하지 않습니다.
- 실행 명령:
  ```bash
  ~/.clusterdeck/bin/clusterdeck-dns --dns-responder \
    --zones ~/.clusterdeck/dns/zones.json \
    --status ~/.clusterdeck/dns/status.json \
    --port 15353
  ```

### 4.2 Resolver 파일 (`/etc/resolver/<zone>`)
활성화된 각 존마다 ClusterDeck 소유권 마커가 포함된 `/etc/resolver/<zone>` 파일이 작성됩니다:
```text
# managed by ClusterDeck (profile: lab)
nameserver 127.0.0.1
port 15353
```

---

## 5. 관리자 암호를 요구하는 시점

실제 DNS 레코드는 사용자 공간의 파일(`~/.clusterdeck/dns/zones.json`)에서 읽어오므로, 관리자 권한 승인 프롬프트 빈도가 최소화됩니다:

- **관리자 암호가 필요한 경우:**
  1. `/etc/resolver/<zone>` 파일을 추가, 삭제하거나 소유권을 다른 프로필로 전환할 때.
  2. 프로필의 모드를 `hosts`와 `local_dns` 간에 전환할 때 (한 번의 프롬프트로 `/etc/hosts`와 `/etc/resolver` 동시 처리).
  3. **D-DEAD 복구 시:** 응답기 프로세스가 종료되었거나 포트 점유권이 상실된 경우, 시스템의 DNS 조회가 지연되지 않도록 고아(dangling) resolver 파일을 제거하기 위해 한 번 프롬프트가 뜹니다.
- **관리자 암호가 필요 없는 경우 (Zero-Prompt):**
  1. 클러스터에 연결(**Connect**)하여 엔드포인트를 자동 검색하고 로컬 DNS를 갱신할 때.
  2. 파드나 서비스가 재생성되어 IP 주소가 변경되었을 때.
  3. 기존 프로필 내에서 노드 별칭이나 베스천 대상 IP가 변경되었을 때.
  4. 일상적인 클러스터 엔드포인트 검색(**Scan Endpoints**) 및 수동 동기화(**Sync to local DNS**) 시.
- **프로필 범위로 한정된 Resolver 작업 및 Resolver 보호 (Scoped Operations & Protection):**
  - 특정 프로필을 저장하거나 매핑 방식을 변경할 때, 또는 로컬 DNS를 동기화하거나 프로필을 삭제할 때 `/etc/resolver` 변경 사항은 오직 해당 프로필로만 엄격하게 한정됩니다. 다른 프로필에 대기 중이거나 아직 승인되지 않은 존 변경 사항이 함께 적용되거나 누출되지 않습니다.
  - **무손실 프로필 저장소(Lossless Store) 및 Resolver 보호 모드:** ClusterDeck은 프로필 저장, 수정, 삭제 시 `profiles.yaml` 내의 미인식 YAML 키 및 유효성 검증에 실패한 invalid 프로필을 유실 없이 그대로 보존합니다. 파싱되지 않은 프로필이나 유효하지 않은 프로필이 남아있는 경우 resolver 보호 모드가 활성화되어, 전역 resolver 정리(teardown)나 미참조 존에 대한 자동 정리를 차단함으로써 일시적으로 유효하지 않은 프로필의 resolver 항목이 실수로 삭제되는 것을 방지합니다.

---

## 6. Endpoints 탭 상태 표시

ClusterDeck 데스크톱 앱의 **Endpoints** 탭에서는 로컬 DNS의 동작 상태를 실시간으로 확인할 수 있습니다:

- **이름 확인(Name Resolution) 상태 뱃지:**
  - `Local DNS` (녹색/ok) — 로컬 DNS 모드 활성화됨 (`profile.host_mapping === 'local_dns'`).
  - `Hosts (/etc/hosts)` — 기존 hosts 파일 모드.
  - `Off` — 이름 확인 매핑 비활성화됨.
- **응답기(Responder) 상태 뱃지:**
  - `Responder running (pid <pid>) · gen <gen>` (녹색/ok) — 응답기가 정상 실행 중이며 프로세스 ID 및 로드된 세대(generation) 번호를 제공함 (예: `Responder running (pid 12345) · gen 3`).
  - `Local DNS responder not running` (주황색/warn) — 응답기 에이전트가 언로드되었거나 정지됨 또는 초기화되지 않음.
- **에러 배너:** `status.json`에 런타임 에러(`dnsStatus.last_error`, 포트 충돌, 유효하지 않은 존 포맷 등)가 기록되면 붉은색 상태 배너가 표시됩니다.
- **존 목록 테이블:** 설정된 모든 존(`<label>.clusterdeck.internal` 및 승인된 존)을 다음 정보와 함께 표시합니다:
  - 상태 태그(`<Tag>`): `served`, `not served`, `responder not running`, `owned by <owner>`, `conflict (<kind>)`, 또는 `unknown`.
  - 등록된 레코드 수 (예: `2 record(s)`).
- **추천 존(Suggested zones) 스트립:**
  - 아직 승인되지 않은 엔드포인트가 발견되면 `Suggested zones:` 텍스트와 함께 후보 존별 단일 `Approve <zone>` 버튼(예: `Approve lab.example.internal`)이 표시됩니다. 사전 동기화 없이 즉시 표시됩니다.
- **경고 배너 (Warning Banners):**
  - `Local DNS is not serving these zones.` — 충돌이 발생한 존과 상세 원인을 안내합니다:
    - `"a /etc/resolver file for this zone already exists and is not managed by ClusterDeck."`
    - `"owned by profile <id>; switch the zone owner to use it here."`
    - `"claimed by <ids>; only one profile may own a zone."`
    - `"the /etc/resolver entry could not be read."`
    - `"not a valid private zone."`
  - 미제공 호스트(Skipped Hosts) 배너:
    - 제목: `${outsideCount} endpoint${outsideCount === 1 ? ' is' : 's are'} not served: approve ${zonesStr} to publish ${outsideCount === 1 ? 'it' : 'them'}` (또는 `"Hosts skipped by local DNS. Approve a zone to publish them."`).
    - 미제공 엔드포인트 및 원인 목록: `<host> (<reason>)` (예: `api.lab.example.internal (outside every approved zone)`).
    - 주의: 이 배너는 안내 정보만 표시하며 버튼을 포함하지 않습니다.
- **엔드포인트 상태 표시 (행 단위):**
  - 노드 별칭 및 베스천 별칭: 대상 IP 및 이름 확인 상태를 표시합니다:
    - `Served by local DNS (<zone>)` (녹색)
    - `Pending: press Sync to local DNS` (회색)
    - `Local DNS responder not running` (회색)
    - `Conflict: <zone>` (주황색)
  - 발견된 Ingress / Gateway 엔드포인트: 대상 IP, 리소스 이름 및 `local_dns_status` 기반 동적 상태를 표시합니다:
    - `Served by local DNS (<zone>)` — 존이 승인되었고 응답기가 실행 중이며 레코드가 정상 제공됨.
    - `Pending: press Sync to local DNS` — 존은 승인되었고 응답기도 실행 중이나 동기화 대기 중임.
    - `Served by another profile (<owner>)` — 호스트 이름이 다른 프로필이 소유한 존에 포함되어 있음.
    - `Not served (outside approved zones)` — 호스트 이름의 상위 도메인이 아직 승인되지 않음.
    - `Conflict: <zone>` — 존에 resolver 충돌이 발생함.
    - `Local DNS responder not running` — 응답기 프로세스가 실행되지 않음.
    - `Not served (invalid DNS name)` — DNS 이름 규칙에 위배됨.
    - `Not served (invalid IPv4 address)` — 유효한 IPv4 주소가 아님.
  - 행 단위 액션:
    - `Approve <zone>` 버튼: 승인되지 않은 엔드포인트 행에 바로 표시되어 한 번의 클릭으로 승인 가능.
    - 복사 버튼 (`Copy <host>`).
    - 브라우저 열기 버튼 (`Open https://<host> in browser`).
- **조작 버튼:**
  - **Scan Endpoints:** "Scan cluster for Ingresses, ApisixRoutes, and Gateways"
  - **Sync to local DNS:** "Publish cluster endpoints and node aliases through local DNS (may require admin approval)"
  - **Remove from local DNS:** "Remove this profile's zones from local DNS"

---

## 7. 문제 해결 (Troubleshooting)

도메인 이름이 정상적으로 확인되지 않거나 예상과 다르게 동작할 경우, 다음 절차에 따라 진단하십시오:

### 1단계: LaunchAgent 실행 상태 확인
사용자 LaunchAgent가 로드되어 실행 중인지 확인합니다:
```bash
launchctl print gui/$(id -u)/com.clusterdeck.dns
```
다음 항목을 확인합니다:
- `state = running`
- `pid = <숫자>`
- `last exit code = 0` (포트 바인딩 충돌이나 인자 오류 등으로 실행 실패 시 1 등의 0이 아닌 종료 코드가 표시됨)

만약 작업이 로드되지 않았거나 비정상 종료되었다면 에이전트 에러 로그를 확인합니다:
```bash
cat ~/.clusterdeck/dns/agent.err.log
```

### 2단계: 응답기 상태 파일 확인
응답기의 런타임 상태 파일을 검사합니다:
```bash
cat ~/.clusterdeck/dns/status.json
```
정상적인 출력 예시:
```json
{
  "generation": 3,
  "loaded_at": "2026-10-16T07:30:00Z",
  "error": null,
  "pid": 12345,
  "instance": "a1b2c3d4e5f60718"
}
```
`"error"` 항목에 메시지가 있다면, 유효성 검사에 실패했거나 포트 충돌이 발생한 것입니다.

### 3단계: 응답기 직접 쿼리
macOS mDNSResponder 캐시를 우회하여 루프백 DNS 응답기(`127.0.0.1:15353`)로 직접 `dig` 쿼리를 실행해 봅니다:
```bash
dig @127.0.0.1 -p 15353 api.lab.example.internal
```
- 대상 IP(예: `192.0.2.10`)가 포함된 응답 섹션과 함께 `status: NOERROR`, `flags: qr aa rd`가 반환되어야 합니다.
- 활성 존 내에 없는 알 수 없는 이름은 `NXDOMAIN`을 반환합니다.
- 승인되지 않은 존의 이름은 `REFUSED`를 반환합니다.

### 4단계: 시스템 해석기(libinfo) 확인
표준 macOS 시스템 확인 방식을 통해 도메인이 올바르게 확인되는지 테스트합니다:
```bash
dscacheutil -q host -a name api.lab.example.internal
```
`/etc/resolver/lab.example.internal` 설정을 참조하여 대상 IP 주소가 출력되어야 합니다.

### 5단계: Network.framework / Safari 해석 경로 확인
Safari 및 iOS/macOS 시스템 앱에서 사용하는 Network.framework 스택을 터미널에서 즉시 검증하려면 다음 Swift 명령어를 실행합니다:
```bash
swift -e '
import Foundation
let url = URL(string: "http://api.lab.example.internal")!
let sem = DispatchSemaphore(value: 0)
URLSession.shared.dataTask(with: url) { _, resp, err in
    if let http = resp as? HTTPURLResponse {
        print("URLSession Status:", http.statusCode)
    } else if let err = err {
        print("URLSession Error:", err.localizedDescription)
    }
    sem.signal()
}.resume()
sem.wait()
'
```
또는 Safari 주소창에 `http://api.lab.example.internal`을 직접 입력하여 접속해 볼 수 있습니다.

> [!WARNING]
> **`dns-sd -G`는 신뢰할 수 있는 검증 도구가 아닙니다.**
> `/etc/resolver` 설정 상태를 확인할 때 `dns-sd -G v4 <hostname>`을 사용하지 마십시오. macOS 검증 스파이크(ADR-0011) 결과, Safari나 `URLSession`, `dscacheutil`이 `/etc/resolver`를 통해 완벽히 대상을 찾아서 접속하는 상황에서도 `dns-sd -G`는 종종 `No Such Record`를 반환하는 결함이 관찰되었습니다. 상태 검증에는 항상 `dscacheutil`, `URLSession`, 또는 `dig @127.0.0.1 -p 15353`을 사용하십시오.

---

## 8. 흔히 발생하는 오류 및 해결 방법

### 미소유 `/etc/resolver/<zone>` 파일 충돌
- **증상:** UI에 다음과 같은 경고가 표시됨: `"a /etc/resolver file for this zone already exists and is not managed by ClusterDeck."`
- **원인:** 다른 도구나 수동 설정으로 생성된 `/etc/resolver/<zone>` 파일에 ClusterDeck 마커(`# managed by ClusterDeck (profile: ...)`)가 없습니다. 사용자의 파일을 보호하기 위해 ClusterDeck은 소유하지 않은 파일을 절대 덮어쓰지 않습니다.
- **해결책:** 해당 파일의 내용을 확인합니다:
  ```bash
  cat /etc/resolver/lab.example.internal
  ```
  더 이상 필요 없는 파일이라면 수동으로 삭제(`sudo rm /etc/resolver/lab.example.internal`)한 뒤 앱에서 **Sync to local DNS**를 누릅니다.

### 응답기 다운 (D-DEAD 상태)
- **증상:** 클러스터 도메인 조회가 30초 동안 멈춘(stall) 후 실패함.
- **원인:** 응답기가 켜져 있지 않은 상태에서 resolver 파일만 남아있으면 macOS의 DNS 조회가 30초 타임아웃에 걸립니다. ClusterDeck은 이를 방지하기 위한 안전장치(**D-DEAD**)를 갖추고 있습니다. 매니저가 응답기 다운 또는 포트 점유권 상실을 감지하면 자동으로 관리자 권한을 요청하여 dangling resolver 파일을 제거합니다.
- **해결책:** `~/.clusterdeck/dns/agent.err.log`를 확인합니다. 다른 프로세스가 15353 포트를 점유하고 있다면 포트를 확보하거나 LaunchAgent를 다시 시작합니다:
  ```bash
  launchctl kickstart -k gui/$(id -u)/com.clusterdeck.dns
  ```

### 엔드포인트 검색 실패 (Failed Endpoint Discovery)
- **증상:** 동기화(Sync) 또는 연결(Connect) 시 원인에 따라 다음과 같은 경고가 발생함:
  - `"Could not discover endpoints (Connect first); serving the last known records."`
  - `"Could not discover endpoints (Connect first); serving the last known records: <error>"`
  - `"Endpoint discovery failed: <error>; serving the last known records."`
  - 또는 Connect 시: `"Could not discover endpoints; serving the last known records."`
- **원인:** Kubernetes API에 연결할 수 없음 (예: SSH 터널 또는 클러스터 연결 다운, kubeconfig 부재 또는 만료).
- **동작 방식:** 기존 DNS 레코드를 지우지 않고 캐시 파일(`~/.clusterdeck/dns/endpoints.json`)에 저장된 마지막 정상 엔드포인트 목록을 유지하여 로컬 DNS 조회가 끊기지 않도록 보호합니다.
- **해결책:** **Connect**를 실행하여 클러스터 연결 및 터널을 복구한 후 **Scan Endpoints** 또는 **Sync to local DNS**를 실행합니다.

### 존이 승인되지 않음
- **증상:** 클러스터 연결은 성공했으나 엔드포인트에 `Not served (outside approved zones)`가 표시되고 미제공 호스트 배너에 나열됨.
- **원인:** 발견된 호스트 이름의 상위 도메인이 아직 승인되지 않았습니다.
- **해결책:** **Suggested zones:** 영역의 **Approve <zone>** 버튼 또는 엔드포인트 행의 인라인 **Approve <zone>** 버튼을 클릭하여 승인합니다. 사전 동기화는 필요하지 않습니다.

### 클러스터 검색(Discovery) 필요
- **증상:** 노드 별칭은 연결되지만 새로 생성한 Ingress 서비스 이름이 풀리지 않음.
- **원인:** Ingress 리소스가 생성된 후 클러스터 엔드포인트 스캔이 아직 수행되지 않았습니다.
- **해결책:** 클러스터에 연결(**Connect**, 로컬 DNS 자동 갱신)하거나 **Scan Endpoints** / **Sync to local DNS**를 실행합니다.

---

## 9. 수동 삭제 (Uninstall) 방법

ClusterDeck의 로컬 DNS 관련 구성 요소를 수동으로 완전히 삭제할 때는 응답기(responder)를 중지하기 전에 반드시 resolver 파일을 먼저 삭제하고 DNS 캐시를 플러시해야 합니다. 정지된 응답기(127.0.0.1:15353)를 가리키는 resolver 파일이 남아 있으면 시스템의 도메인 조회가 약 30초간 중단(stall)되기 때문입니다 (ADR-0011 teardown 절차 참조).

### 1. ClusterDeck이 생성한 Resolver 파일만 안전하게 삭제
심볼릭 링크를 건너뛰고 다른 도구가 생성한 resolver 파일을 건드리지 않은 채, ClusterDeck 마커가 있는 파일만 선별하여 안전하게 삭제한 뒤 캐시를 플러시합니다:

```bash
for f in /etc/resolver/*; do
  if [ -f "$f" ] && [ ! -L "$f" ] && head -n 1 "$f" | grep -q "^# managed by ClusterDeck (profile: "; then
    echo "Removing $f"
    sudo rm "$f"
  fi
done
sudo dscacheutil -flushcache
sudo killall -HUP mDNSResponder
```

### 2. LaunchAgent 언로드
```bash
launchctl bootout gui/$(id -u)/com.clusterdeck.dns 2>/dev/null || true
```

### 3. 에이전트, Plist 및 상태 파일 삭제
```bash
rm -f ~/Library/LaunchAgents/com.clusterdeck.dns.plist
rm -f ~/.clusterdeck/bin/clusterdeck-dns
rm -rf ~/.clusterdeck/dns
```
