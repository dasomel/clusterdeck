#!/usr/bin/env bash
# Guided real-Mac evidence for #95. Only the UI performs privileged changes.
set -euo pipefail
umask 077
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PASS=0 FAIL=0 SQUATTER_PID='' SQUATTER_TOKEN='' WORK='' CURRENT=''
AGENT="gui/$(id -u)/com.clusterdeck.dns"

usage() {
  cat <<'HELP'
Usage: verify-local-dns-lifecycle.sh <command> [snapshot-dir]
  snapshot                  Read-only capture into a new directory under ${TMPDIR}
  restore-check <dir>       Compare a snapshot; check agent, records and resolution
  item1-before              Capture pre-logout state; keep the printed directory
  item1-after <dir>         After login, compare PID/generation and check resolution
  item3                     With explicit y: bootout agent, run UDP+TCP squatter;
                            operator clicks Sync, approves cleanup, then restores
  item4                     Operator approves a new suggested zone, CANCELS prompt
  item5                     Operator leaves ONE LocalDns profile, then removes it
  safari                    Probe every published zone with the existing Swift check
  --help                    Show help
Interactive commands require a terminal. Prompts are Korean + English.
Never runs sudo or writes /etc, ~/.clusterdeck or ~/Library. All script state
is private under ${TMPDIR}; only its own child python squatter may be killed.
PASS means observed checks passed; any FAIL exits nonzero. Snapshots contain
private infrastructure data: keep them local. Snapshot PASS is capture, not health.
HELP
}
record() {
  if [ "$1" = PASS ]; then PASS=$((PASS + 1)); else FAIL=$((FAIL + 1)); fi
  printf '[%s] %s\n' "$1" "$2"
}
check() {
  local label="$1"; shift
  if "$@"; then record PASS "$label"; else record FAIL "$label"; fi
}
pause() {
  printf '\n%s\n완료 후 Enter / Press Enter when finished: ' "$1"
  read -r REPLY || exit 2
}
yes_answer() {
  printf '\n%s [y/N]: ' "$1"
  read -r REPLY || return 1
  [ "$REPLY" = y ]
}
# D1: parent + command + unique token prevent PID-reuse kills; costs a ps check
# before every signal. If identity is uncertain, leave it alone and report failure.
own_child() {
  local parent command_line
  [ -n "$SQUATTER_PID" ] || return 1
  parent="$(ps -p "$SQUATTER_PID" -o ppid= 2>/dev/null)" || return 1
  [ "${parent//[[:space:]]/}" = "$$" ] || return 1
  command_line="$(ps -ww -p "$SQUATTER_PID" -o command= 2>/dev/null)" || return 1
  [[ "$command_line" == *[Pp]ython*' -I -c '*"$SQUATTER_TOKEN" ]]
}
cleanup() {
  local i
  if own_child; then
    kill -TERM "$SQUATTER_PID" || return 1
    for i in 1 2 3 4 5; do
      if ! own_child; then break; fi
      sleep 0.2
    done
    if own_child; then kill -KILL "$SQUATTER_PID" || return 1; fi
    wait "$SQUATTER_PID" 2>/dev/null || true
  elif [ -n "$SQUATTER_PID" ] && ps -p "$SQUATTER_PID" -o pid= >/dev/null 2>&1; then
    printf '[FAIL] Squatter identity uncertain; no signal sent / 프로세스 확인 불가\n' >&2
    return 1
  fi
  SQUATTER_PID=''
}
finish() {
  local rc=$?
  trap - EXIT
  if ! cleanup; then record FAIL 'squatter cleanup on exit'; rc=1; fi
  if [ "$rc" -ne 0 ] && [ "$FAIL" -eq 0 ]; then record FAIL 'command interrupted or inspection failed'; fi
  printf 'PASS=%s FAIL=%s; evidence: %s\n' "$PASS" "$FAIL" "${WORK:-none}"
  [ "$FAIL" -eq 0 ] || rc=1
  exit "$rc"
}
trap finish EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

# D2: one read-only collector is shared by all checks to avoid divergent oracles.
# Snapshots cost disk space; retain them for review rather than deleting evidence.
inspect() {
  python3 -I - "$@" <<'PY'
import hashlib, json, os, pathlib, re, subprocess, sys
mode, directory, *args = sys.argv[1:]
p = pathlib.Path(directory)
home = pathlib.Path.home()
def digest(data):
    return hashlib.sha256(data).hexdigest()
def run(argv, absent=False):
    r = subprocess.run(argv, capture_output=True, text=True, check=False)
    if r.returncode and not (absent and r.returncode == 1 and not r.stdout and not r.stderr):
        raise RuntimeError(f'{argv[0]} inspection failed ({r.returncode}): {r.stderr.strip()}')
    return r.stdout
try:
    if mode == 'snapshot':
        state = dict(files={}, resolvers={}, owned={}, pid=None, loaded=False)
        (p / 'resolver').mkdir()
        for name, path in {
            'zones.json': home / '.clusterdeck/dns/zones.json',
            'status.json': home / '.clusterdeck/dns/status.json',
            'generation': home / '.clusterdeck/dns/generation',
            'plist.xml': home / 'Library/LaunchAgents/com.clusterdeck.dns.plist',
            'binary': home / '.clusterdeck/bin/clusterdeck-dns',
        }.items():
            if path.exists():
                data = path.read_bytes()
                state['files'][name] = digest(data)
                if name != 'binary':
                    (p / name).write_bytes(data)
        resolver = pathlib.Path('/etc/resolver')
        for f in sorted(resolver.iterdir()) if resolver.exists() else []:
            if f.is_symlink() or not f.is_file():
                raise RuntimeError('resolver entry is not a regular file; ownership uncertain')
            data = f.read_bytes()
            state['resolvers'][f.name] = digest(data)
            marker = re.fullmatch(rb'# managed by ClusterDeck \(profile: ([A-Za-z0-9_-]{1,64})\)', data.split(b'\n')[0])
            labels = f.name.split('.')
            private = (len(f.name) <= 253 and len(labels) >= 3
                       and all(re.fullmatch(r'[a-z0-9](?:[a-z0-9-]{0,61}[a-z0-9])?', x) for x in labels)
                       and f.name.endswith(('.internal', '.test', '.example', '.local', '.home.arpa', '.localhost', '.invalid')))
            if marker and private:
                state['owned'][f.name] = marker[1].decode()
                (p / 'resolver' / f.name).write_bytes(data)
        r = subprocess.run(['launchctl', 'print', f'gui/{os.getuid()}/com.clusterdeck.dns'], capture_output=True, text=True)
        (p / 'launchctl.txt').write_text(r.stdout + r.stderr)
        if r.returncode == 0:
            state['loaded'] = True
            match = re.search(r'^\s*pid = (\d+)\s*$', r.stdout, re.M)
            state['pid'] = int(match[1]) if match else None
        elif 'Could not find service' not in r.stdout + r.stderr:
            raise RuntimeError('launchctl failed without explicit service-absent result')
        for kind, flags in [('udp', ['-iUDP:15353']), ('tcp', ['-iTCP:15353', '-sTCP:LISTEN'])]:
            out = run(['lsof', '-nP', *flags, '-t'], absent=True)
            (p / f'lsof-{kind}.txt').write_text(out)
            state[kind] = sorted({int(x) for x in out.split()})
        (p / 'state.json').write_text(json.dumps(state, indent=2) + '\n')
        print('Captured resolver contents/hashes, zones, status, generation, launchd, listeners, binary hash.')
        sys.exit(0)
    s = json.loads((p / 'state.json').read_text())
    def load(name):
        return json.loads((p / name).read_text())
    failures = []
    def verify(ok, label):
        print(f'[{"PASS" if ok else "FAIL"}] {label}')
        if not ok:
            failures.append(label)
    if mode == 'healthy':
        z, st = load('zones.json'), load('status.json')
        verify(bool(s['pid']) and s['loaded'], 'LaunchAgent running')
        verify(s['pid'] == st['pid'] and st['error'] is None, 'status PID/error agrees')
        verify(s['pid'] is not None and s['udp'] == s['tcp'] == [s['pid']], 'UDP+TCP owned solely by agent')
        verify(bool(z['zones']) and z['port'] == 15353, 'nonempty published zones on port 15353')
        verify(st['generation'] >= z['generation'], 'responder loaded published generation')
        verify(all(s['owned'].get(e['zone']) == e['profile_id'] for e in z['zones']), 'published (zone, owner) exists on disk')
    elif mode in ('restore', 'cancel', 'login', 'removed'):
        b = pathlib.Path(args[0]); old = json.loads((b / 'state.json').read_text())
        foreign = {k: v for k, v in old['resolvers'].items() if k not in old['owned']}
        verify(all(s['resolvers'].get(k) == v for k, v in foreign.items()), 'pre-existing foreign resolver files unchanged')
        if mode == 'removed':
            verify(not s['owned'], 'no ClusterDeck resolver files')
            if args[1] == 'teardown':
                verify(not s['loaded'] and not s['udp'] and not s['tcp'], 'agent unloaded and port free')
                verify(all(k not in s['files'] for k in ('zones.json', 'status.json', 'plist.xml', 'binary')), 'zones/status/plist/binary removed')
                verify('generation' in s['files'] and int((p / 'generation').read_text()) >= int((b / 'generation').read_text()), 'generation retained and monotonic')
        elif mode == 'cancel':
            verify(s['resolvers'] == old['resolvers'], 'Cancel left resolver disk contents unchanged')
            previous = json.loads((b / 'zones.json').read_text())['zones']
            current = load('zones.json')['zones']
            # reconcile_entries picks desired or previous only for an exact on-disk owner.
            expected = sorted((e['zone'], e['profile_id']) for e in previous if s['owned'].get(e['zone']) == e['profile_id'])
            verify(sorted((e['zone'], e['profile_id']) for e in current) == expected, 'Cancel reconciled known (zone, owner) pairs; no uninstalled candidate')
        elif mode == 'login':
            verify(bool(s['pid']) and s['pid'] != old['pid'], 'post-login PID changed (PID reuse requires manual review)')
            verify(load('status.json')['generation'] >= json.loads((b / 'status.json').read_text())['generation'], 'post-login generation monotonic')
        else:
            verify(s['resolvers'] == old['resolvers'], 'resolver names and SHA-256 match baseline')
            for key in ('binary', 'plist.xml'):
                verify(s['files'].get(key) == old['files'].get(key), f'{key} SHA-256 matches baseline')
            a, z = load('zones.json'), json.loads((b / 'zones.json').read_text())
            canonical = lambda x: sorted(json.dumps(e, sort_keys=True) for e in x['zones'])
            verify(canonical(a) == canonical(z), 'zone owners and records match baseline (generation may advance)')
            verify(a['generation'] >= z['generation'], 'published generation monotonic')
    elif mode == 'hosts':
        z = load('zones.json')['zones']
        if not z:
            raise RuntimeError('no zones to probe')
        for e in z:
            for r in e['records']:
                print(r['name'] + '\t' + r['ipv4'])
            if not e['records']:
                print('probe-nxdomain.' + e['zone'] + '\t-')
    else:
        raise RuntimeError('unknown inspection mode')
    sys.exit(bool(failures))
except (OSError, ValueError, KeyError, TypeError, RuntimeError) as e:
    print(f'[FAIL] {e}', file=sys.stderr)
    sys.exit(1)
PY
}
capture() {
  CURRENT="$(mktemp -d "$WORK/state-XXXXXX")"
  if inspect snapshot "$CURRENT"; then record PASS "snapshot: $CURRENT";
  else record FAIL "snapshot incomplete: $CURRENT"; return 1; fi
}
healthy() { check 'agent and publication health' inspect healthy "$CURRENT"; }
probes() {
  local source="$1" mode="${2:-positive}" host ip
  inspect hosts "$source" > "$WORK/hosts.tsv" || { record FAIL 'probe input'; return; }
  while IFS=$'\t' read -r host ip; do
    if [ "$mode" = negative ] || [ "$ip" = - ]; then
      check 'Safari fast name-not-found' swift -module-cache-path "$WORK/swift-cache" "$SCRIPT_DIR/verify-safari-resolution.swift" "$host" --expect-failure
    else
      check 'direct DNS record' dig_record "$host" "$ip"
      check 'Safari expected IP' swift -module-cache-path "$WORK/swift-cache" "$SCRIPT_DIR/verify-safari-resolution.swift" "$host" --expect-ip "$ip"
    fi
  done < "$WORK/hosts.tsv"
}
dig_record() {
  local answer
  answer="$(dig @127.0.0.1 -p 15353 "$1" A +short +time=2 +tries=1)" || return 1
  printf '%s\n' "$answer" | grep -qxF "$2"
}
restore() {
  pause '각 원래 Local DNS 프로필에서 Sync하고 관리자 승인을 완료하세요. / Sync each original Local DNS profile and approve admin prompts. Wait for completion.'
  capture
  check 'restored baseline' inspect restore "$CURRENT" "$1"
  healthy; probes "$CURRENT"
}
item3() {
  capture; healthy
  [ "$FAIL" -eq 0 ] || return 1
  local before="$CURRENT"
  yes_answer '에이전트를 중지하고 UDP+TCP 점유 테스트를 진행할까요? / Bootout our agent and start port-squatter test?' || exit 2
  launchctl bootout "$AGENT"
  SQUATTER_TOKEN="clusterdeck-squatter-$$-${RANDOM}"
  python3 -I -c '
import signal, socket, sys, time
udp = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
tcp = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
udp.bind(("127.0.0.1", 15353))
tcp.bind(("127.0.0.1", 15353)); tcp.listen(5)
signal.signal(signal.SIGTERM, lambda *_: sys.exit(0))
open(sys.argv[1], "x").close()
while True: time.sleep(1)
' "$WORK/ready" "$SQUATTER_TOKEN" > "$WORK/squatter.log" 2>&1 &
  SQUATTER_PID=$!
  local i
  for i in 1 2 3 4 5; do [ ! -f "$WORK/ready" ] || break; sleep 0.2; done
  if ! own_child || [ ! -f "$WORK/ready" ]; then record FAIL 'squatter bind/identity'; return 1; fi
  record PASS 'own child bound 127.0.0.1:15353 UDP+TCP'
  pause 'Endpoints에서 Sync to local DNS를 누르고 정리 관리자 프롬프트를 승인하세요. Connect는 정리하지 않습니다. / Click Sync to local DNS; approve cleanup. Wait for the unavailable warning (startup/status timeout can be the cause).'
  capture
  check 'D-DEAD removal' inspect removed "$CURRENT" "$before" cleanup
  if yes_answer 'responder unavailable 경고를 보셨나요? / Did the UI show local DNS responder unavailable?'; then record PASS 'operator observed warning'; else record FAIL 'warning not observed'; fi
  probes "$before" negative
  check 'squatter cleanup' cleanup
  restore "$before"
}
item4() {
  capture; healthy
  [ "$FAIL" -eq 0 ] || return 1
  local before="$CURRENT"
  pause '다른 변경 없이 Endpoints의 새 Suggested zone에서 Approve를 누르고 실제 관리자 창에서 취소하세요. 후보가 없으면 중단하세요. / With no other edits, Approve a NEW suggested zone whose resolver file is absent; CANCEL the actual admin dialog. Abort if no candidate/prompt. Approval remains saved as pending; do not Sync yet.'
  if yes_answer '실제 관리자 창 취소와 Zone Approval Incomplete 경고를 확인했나요? / Observed actual Cancel and Zone Approval Incomplete warning?'; then record PASS 'operator observed Cancel/warning'; else record FAIL 'Cancel/warning unconfirmed'; fi
  capture
  check 'cancel consistency' inspect cancel "$CURRENT" "$before"
  healthy; probes "$CURRENT"
  printf '%s\n' '미설치 후보는 pending 상태입니다. 나중에 Sync로 승인하거나 UI에서 승인을 취소하세요. / Candidate approval remains pending; later Sync to install, or revoke it in the UI.'
}
item5() {
  capture; healthy
  [ "$FAIL" -eq 0 ] || return 1
  local original="$CURRENT"
  pause '먼저 프로필 설정에서 테스트할 하나만 Local DNS로 남기고 나머지는 Off로 저장·승인하세요. 원래 설정을 기록하세요. / Record original mappings; in Settings save all OTHER profiles as Off and approve changes. Leave exactly ONE configured Local DNS profile. Repeated Remove alone does not change saved mappings.'
  capture; healthy
  [ "$FAIL" -eq 0 ] || return 1
  local before="$CURRENT"
  yes_answer '정확히 하나만 Local DNS로 저장되어 있나요? 일시 제거할까요? / Exactly ONE saved Local DNS profile remains; temporarily remove its setup?' || exit 2
  pause '남은 프로필 Endpoints에서 Remove from local DNS를 누르고 관리자 승인을 완료하세요. / On the sole remaining Local DNS profile click Remove from local DNS and approve; wait for completion.'
  capture
  check 'last-profile teardown' inspect removed "$CURRENT" "$before" teardown
  probes "$before" negative
  restore "$before"
  pause '다른 프로필의 원래 매핑을 Settings에서 복구하고 각 Sync를 실행하세요. / Restore other profiles original mappings in Settings and Sync each. Wait for completion.'
  capture
  check 'all original profiles restored' inspect restore "$CURRENT" "$original"
  healthy; probes "$CURRENT"
}

case "${1:---help}" in --help|-h) trap - EXIT; usage; exit 0;; esac
CMD="$1"; shift
case "$CMD" in snapshot|item1-before|item3|item4|item5|safari) [ $# -eq 0 ] || { usage; exit 2; };;
  restore-check|item1-after) [ $# -eq 1 ] && [ -f "$1/state.json" ] || { usage; exit 2; };;
  *) usage; exit 2;; esac
case "$CMD" in item3|item4|item5|item1-before|item1-after)
  { [ -t 0 ] && [ -t 1 ]; } || { echo 'FAIL: 터미널이 필요합니다 / Interactive terminal required' >&2; exit 2; };; esac
for tool in python3 launchctl lsof; do
  command -v "$tool" >/dev/null || { record FAIL "missing tool: $tool"; exit 1; }
done
# D3: canonicalize TMPDIR before any write; refuse protected roots even via symlinks.
# Costs one Python call; operator can supply a normal macOS TMPDIR if misconfigured.
TEMP_ROOT="$(python3 -I - <<'PY'
import os, pathlib
p = pathlib.Path(os.environ.get('TMPDIR', '/tmp')).resolve(strict=True)
h = pathlib.Path.home().resolve()
assert p.is_dir() and p != pathlib.Path('/'), 'invalid TMPDIR'
for root in (pathlib.Path('/etc').resolve(), (h / '.clusterdeck').resolve(), (h / 'Library').resolve()):
    assert p != root and root not in p.parents, 'protected TMPDIR'
print(p)
PY
)"
WORK="$(mktemp -d "$TEMP_ROOT/clusterdeck-dns-$(date +%Y%m%d-%H%M%S)-XXXXXX")"
case "$CMD" in
  snapshot) capture;;
  item1-before)
    capture; healthy
    printf '로그아웃 후 앱을 열지 말고 실행 / Log out, log in, BEFORE opening ClusterDeck run:\n%s item1-after "%s"\n' "$0" "$CURRENT";;
  item1-after)
    capture; check 'login lifecycle' inspect login "$CURRENT" "$1"; healthy; probes "$CURRENT";;
  restore-check)
    capture; check 'snapshot comparison' inspect restore "$CURRENT" "$1"; healthy; probes "$CURRENT";;
  safari) capture; probes "$CURRENT";;
  item3) item3;; item4) item4;; item5) item5;;
esac
[ "$FAIL" -eq 0 ]
