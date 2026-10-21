#!/usr/bin/env bash
# Read-only real-Mac verification of the ClusterDeck local DNS responder (ADR-0011, #95).
# Never modifies system state: only launchctl print, lsof, dig, head and the Swift resolver probe.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
DNS_DIR="${CLUSTERDECK_DNS_DIR:-$HOME/.clusterdeck/dns}"
AGENT_LABEL="com.clusterdeck.dns"
PORT=15353

usage() {
  cat <<USAGE
Usage: $(basename "$0") [--expect-failure <host>]... [<host>[=<expected_ipv4>]]...

Read-only checks: LaunchAgent running, status.json, port $PORT ownership,
ClusterDeck-managed /etc/resolver files, dig against 127.0.0.1:$PORT, then the
Safari/Network.framework probe (scripts/verify-safari-resolution.swift) per host.

  <host>[=<ip>]            must resolve (to <ip> if given)
  --expect-failure <host>  must fail fast with a clean NXDOMAIN (no stall)

Exits non-zero if any check FAILs.
USAGE
}

HOSTS=() # entries: "resolve|host|ip" or "fail|host|"
while [ $# -gt 0 ]; do
  case "$1" in
    -h|--help) usage; exit 0 ;;
    --expect-failure)
      [ $# -ge 2 ] || { usage >&2; exit 2; }
      HOSTS+=("fail|$2|"); shift 2 ;;
    -*) usage >&2; exit 2 ;;
    *)
      if [[ "$1" == *=* ]]; then HOSTS+=("resolve|${1%%=*}|${1#*=}"); else HOSTS+=("resolve|$1|"); fi
      shift ;;
  esac
done

PASS=0
FAIL=0
SUMMARY=()
record() { # status label [detail]
  if [ "$1" = PASS ]; then PASS=$((PASS + 1)); else FAIL=$((FAIL + 1)); fi
  SUMMARY+=("$1  $2${3:+ - $3}")
  echo "[$1] $2${3:+ - $3}"
}

UID_NUM="$(id -u)"
AGENT_PID=""

# 1. LaunchAgent running
if OUT="$(launchctl print "gui/$UID_NUM/$AGENT_LABEL" 2>&1)"; then
  AGENT_PID="$(printf '%s\n' "$OUT" | awk '/^[[:space:]]*pid = /{print $3; exit}')"
  if [ -n "$AGENT_PID" ]; then
    record PASS "launchd agent $AGENT_LABEL running" "pid=$AGENT_PID"
  else
    record FAIL "launchd agent $AGENT_LABEL loaded but not running"
  fi
else
  record FAIL "launchd agent $AGENT_LABEL not loaded in gui/$UID_NUM"
fi

# 2. status.json must agree with launchd and report no error
if ! command -v jq >/dev/null 2>&1; then
  record FAIL "status.json" "jq not installed"
elif [ -f "$DNS_DIR/status.json" ]; then
  S_PID="$(jq -r '.pid' "$DNS_DIR/status.json")"
  S_GEN="$(jq -r '.generation' "$DNS_DIR/status.json")"
  S_ERR="$(jq -r '.error' "$DNS_DIR/status.json")"
  if [ -n "$AGENT_PID" ] && [ "$S_PID" = "$AGENT_PID" ] && [ "$S_ERR" = null ]; then
    record PASS "status.json consistent" "pid=$S_PID generation=$S_GEN error=null"
  else
    record FAIL "status.json mismatch" "pid=$S_PID (agent=${AGENT_PID:-none}) error=$S_ERR"
  fi
else
  record FAIL "status.json missing" "$DNS_DIR/status.json"
fi

# 3. Port ownership: UDP and TCP LISTEN must both belong solely to the agent
UDP_PIDS="$(lsof -nP -iUDP:$PORT -t 2>/dev/null | sort -u | tr '\n' ' ' || true)"
TCP_PIDS="$(lsof -nP -iTCP:$PORT -sTCP:LISTEN -t 2>/dev/null | sort -u | tr '\n' ' ' || true)"
if [ -n "$AGENT_PID" ] && [ "${UDP_PIDS% }" = "$AGENT_PID" ] && [ "${TCP_PIDS% }" = "$AGENT_PID" ]; then
  record PASS "port $PORT owned by agent" "pid=$AGENT_PID (UDP+TCP)"
else
  record FAIL "port $PORT ownership" "udp='${UDP_PIDS% }' tcp='${TCP_PIDS% }' expected='${AGENT_PID:-none}'"
fi

# 4. ClusterDeck-managed resolver files (regular files only, marker on line 1)
MANAGED=0
for f in /etc/resolver/*; do
  { [ -f "$f" ] && [ ! -L "$f" ]; } || continue
  if head -n 1 "$f" | grep -q '^# managed by ClusterDeck (profile: '; then
    MANAGED=$((MANAGED + 1))
    echo "   managed resolver file: $(basename "$f")"
  fi
done
if [ "$MANAGED" -gt 0 ]; then
  record PASS "managed /etc/resolver files" "count=$MANAGED"
else
  record FAIL "managed /etc/resolver files" "none found"
fi

# 5. Per-host: direct dig against the responder, then the Safari-stack probe
for entry in ${HOSTS[@]+"${HOSTS[@]}"}; do
  IFS='|' read -r MODE HOST IP <<<"$entry"
  ANSWER="$(dig @127.0.0.1 -p $PORT "$HOST" A +short +time=2 +tries=1 2>&1 || true)"
  ONELINE="$(printf '%s' "$ANSWER" | tr '\n' ' ')"
  if [ "$MODE" = fail ]; then
    if [ -z "$ANSWER" ]; then
      record PASS "dig $HOST" "no answer, as expected"
    else
      record FAIL "dig $HOST" "expected no answer, got: $ONELINE"
    fi
    SW_ARGS=("$HOST" --expect-failure)
  elif [ -n "$IP" ]; then
    if printf '%s\n' "$ANSWER" | grep -qxF "$IP"; then
      record PASS "dig $HOST" "$IP"
    else
      record FAIL "dig $HOST" "expected $IP, got: ${ONELINE:-<empty>}"
    fi
    SW_ARGS=("$HOST" --expect-ip "$IP")
  else
    if printf '%s\n' "$ANSWER" | grep -Eq '^[0-9]+\.[0-9]+\.[0-9]+\.[0-9]+$'; then
      record PASS "dig $HOST" "$ONELINE"
    else
      record FAIL "dig $HOST" "no A record: ${ONELINE:-<empty>}"
    fi
    SW_ARGS=("$HOST")
  fi
  RC=0
  swift "$SCRIPT_DIR/verify-safari-resolution.swift" "${SW_ARGS[@]}" || RC=$?
  if [ "$RC" -eq 0 ]; then
    record PASS "safari-stack $HOST"
  else
    record FAIL "safari-stack $HOST" "exit=$RC"
  fi
done

echo "=========================================================="
printf '%s\n' "${SUMMARY[@]}"
echo "PASS=$PASS FAIL=$FAIL"
[ "$FAIL" -eq 0 ]
