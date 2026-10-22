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
Usage: $(basename "$0") [--teardown] [--expect-failure <host>]... [<host>[=<expected_ipv4>]]...

Read-only checks: LaunchAgent running, status.json, port $PORT ownership,
ClusterDeck-managed /etc/resolver files, dig against 127.0.0.1:$PORT, then the
Safari/Network.framework probe (scripts/verify-safari-resolution.swift) per host.

  <host>[=<ip>]            must resolve (to <ip> if given)
  --expect-failure <host>  responder must answer NXDOMAIN and the Safari stack must
                           fail fast with a clean name-not-found (no stall)
  --teardown               post "Remove from local DNS" mode: assert the agent is not
                           loaded, port $PORT is unowned and no managed resolver files
                           remain; every host is then checked with the Safari stack
                           only (as --expect-failure) and dig is skipped

Exits non-zero if any check FAILs.
USAGE
}

TEARDOWN=0
HOSTS=() # entries: "resolve|host|ip" or "fail|host|"
while [ $# -gt 0 ]; do
  case "$1" in
    -h|--help) usage; exit 0 ;;
    --teardown) TEARDOWN=1; shift ;;
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

# 0. Required tools. Absence must be a FAIL, never mistaken for "nothing there" (esp. --teardown).
for tool in launchctl lsof head grep awk sed sort tr; do
  command -v "$tool" >/dev/null 2>&1 || record FAIL "required command '$tool' not found"
done

# 1. LaunchAgent state. Only launchd's explicit "Could not find service" counts as absent;
# any other failure (exec error, permission) is an error, not evidence of removal.
LC_RC=0
OUT="$(launchctl print "gui/$UID_NUM/$AGENT_LABEL" 2>&1)" || LC_RC=$?
if [ "$LC_RC" -eq 0 ]; then
  AGENT_PID="$(printf '%s\n' "$OUT" | awk '/^[[:space:]]*pid = /{print $3; exit}')"
  if [ "$TEARDOWN" -eq 1 ]; then
    record FAIL "launchd agent $AGENT_LABEL still loaded after teardown"
  elif [ -n "$AGENT_PID" ]; then
    record PASS "launchd agent $AGENT_LABEL running" "pid=$AGENT_PID"
  else
    record FAIL "launchd agent $AGENT_LABEL loaded but not running"
  fi
elif printf '%s\n' "$OUT" | grep -q 'Could not find service'; then
  if [ "$TEARDOWN" -eq 1 ]; then
    record PASS "launchd agent $AGENT_LABEL not loaded"
  else
    record FAIL "launchd agent $AGENT_LABEL not loaded in gui/$UID_NUM"
  fi
else
  record FAIL "launchctl print failed" "rc=$LC_RC: $(printf '%s' "$OUT" | head -c 200 | tr '\n' ' ')"
fi

# 2. status.json must agree with launchd and report no error (not applicable after teardown)
if [ "$TEARDOWN" -eq 1 ]; then
  :
elif ! command -v jq >/dev/null 2>&1; then
  record FAIL "status.json" "jq not installed"
elif [ -f "$DNS_DIR/status.json" ]; then
  # Conditionals keep a corrupt file from tripping `set -e` before the summary prints.
  if S_PID="$(jq -er '.pid' "$DNS_DIR/status.json" 2>/dev/null)" \
    && S_GEN="$(jq -er '.generation' "$DNS_DIR/status.json" 2>/dev/null)" \
    && S_ERR="$(jq -r '.error' "$DNS_DIR/status.json" 2>/dev/null)"; then
    if [ -n "$AGENT_PID" ] && [ "$S_PID" = "$AGENT_PID" ] && [ "$S_ERR" = null ]; then
      record PASS "status.json consistent" "pid=$S_PID generation=$S_GEN error=null"
    else
      record FAIL "status.json mismatch" "pid=$S_PID (agent=${AGENT_PID:-none}) error=$S_ERR"
    fi
  else
    record FAIL "status.json unreadable or malformed" "$DNS_DIR/status.json"
  fi
else
  record FAIL "status.json missing" "$DNS_DIR/status.json"
fi

# 3. Port ownership: UDP and TCP LISTEN must both belong solely to the agent
# lsof exits 1 with no output when nothing matches; any other outcome is an error.
LSOF_ERR=""
lsof_pids() { # args: <result var> <lsof selector>... ; stores space-separated PIDs
  local var="$1" out rc=0 pids=""
  shift
  out="$(lsof -nP "$@" -t 2>&1)" || rc=$?
  if [ "$rc" -eq 0 ] || { [ "$rc" -eq 1 ] && [ -z "$out" ]; }; then
    pids="$(printf '%s\n' "$out" | grep -E '^[0-9]+$' | sort -u | tr '\n' ' ' || true)"
  else
    LSOF_ERR="lsof rc=$rc"
  fi
  printf -v "$var" '%s' "$pids"
}
lsof_pids UDP_PIDS -iUDP:$PORT
lsof_pids TCP_PIDS -iTCP:$PORT -sTCP:LISTEN
if [ -n "$LSOF_ERR" ]; then
  record FAIL "port $PORT ownership could not be determined" "$LSOF_ERR"
elif [ "$TEARDOWN" -eq 1 ]; then
  if [ -z "${UDP_PIDS% }" ] && [ -z "${TCP_PIDS% }" ]; then
    record PASS "port $PORT unowned after teardown"
  else
    record FAIL "port $PORT still in use after teardown" "udp='${UDP_PIDS% }' tcp='${TCP_PIDS% }'"
  fi
elif [ -n "$AGENT_PID" ] && [ "${UDP_PIDS% }" = "$AGENT_PID" ] && [ "${TCP_PIDS% }" = "$AGENT_PID" ]; then
  record PASS "port $PORT owned by agent" "pid=$AGENT_PID (UDP+TCP)"
else
  record FAIL "port $PORT ownership" "udp='${UDP_PIDS% }' tcp='${TCP_PIDS% }' expected='${AGENT_PID:-none}'"
fi

# 4. ClusterDeck-managed resolver files (regular files only, marker on line 1)
MANAGED=0
RESOLVER_ERRORS=0
for f in /etc/resolver/*; do
  { [ -f "$f" ] && [ ! -L "$f" ]; } || continue
  # An unreadable file might be ClusterDeck's; never count it as absent.
  if ! FIRST_LINE="$(head -n 1 "$f" 2>/dev/null)"; then
    RESOLVER_ERRORS=$((RESOLVER_ERRORS + 1))
    echo "   unreadable resolver file: $(basename "$f")"
  elif [[ "$FIRST_LINE" == "# managed by ClusterDeck (profile: "* ]]; then
    MANAGED=$((MANAGED + 1))
    echo "   managed resolver file: $(basename "$f")"
  fi
done
if [ "$RESOLVER_ERRORS" -gt 0 ]; then
  record FAIL "/etc/resolver files could not be read" "unreadable=$RESOLVER_ERRORS"
elif [ "$TEARDOWN" -eq 1 ]; then
  if [ "$MANAGED" -eq 0 ]; then
    record PASS "no managed /etc/resolver files after teardown"
  else
    record FAIL "managed /etc/resolver files remain after teardown" "count=$MANAGED"
  fi
elif [ "$MANAGED" -gt 0 ]; then
  record PASS "managed /etc/resolver files" "count=$MANAGED"
else
  record FAIL "managed /etc/resolver files" "none found"
fi

# 5. Per-host: direct dig against the responder, then the Safari-stack probe
for entry in ${HOSTS[@]+"${HOSTS[@]}"}; do
  IFS='|' read -r MODE HOST IP <<<"$entry"
  if [ "$TEARDOWN" -eq 1 ]; then
    MODE=fail
  else
    # Full output, not +short: an empty +short also covers SERVFAIL/REFUSED/NODATA.
    DIG_RC=0
    DIG_OUT="$(dig @127.0.0.1 -p $PORT "$HOST" A +time=2 +tries=1 2>&1)" || DIG_RC=$?
    DIG_STATUS="$(printf '%s\n' "$DIG_OUT" | sed -n 's/.*status: \([A-Z]*\),.*/\1/p' | head -n 1)"
    ANSWER="$(printf '%s\n' "$DIG_OUT" | awk '$3 == "IN" && $4 == "A" {print $5}')"
    ONELINE="$(printf '%s' "$ANSWER" | tr '\n' ' ')"
    if [ "$DIG_RC" -ne 0 ] || [ -z "$DIG_STATUS" ]; then
      record FAIL "dig $HOST" "dig failed (rc=$DIG_RC)"
    elif [ "$MODE" = fail ]; then
      if [ "$DIG_STATUS" = NXDOMAIN ]; then
        record PASS "dig $HOST" "NXDOMAIN, as expected"
      else
        record FAIL "dig $HOST" "expected NXDOMAIN, got status=$DIG_STATUS ${ONELINE}"
      fi
    elif [ "$DIG_STATUS" != NOERROR ]; then
      record FAIL "dig $HOST" "status=$DIG_STATUS"
    elif [ -n "$IP" ]; then
      if printf '%s\n' "$ANSWER" | grep -qxF "$IP"; then
        record PASS "dig $HOST" "$IP"
      else
        record FAIL "dig $HOST" "expected $IP, got: ${ONELINE:-<empty>}"
      fi
    elif [ -n "$ANSWER" ]; then
      record PASS "dig $HOST" "$ONELINE"
    else
      record FAIL "dig $HOST" "NOERROR but no A record"
    fi
  fi
  if [ "$MODE" = fail ]; then
    SW_ARGS=("$HOST" --expect-failure)
  elif [ -n "$IP" ]; then
    SW_ARGS=("$HOST" --expect-ip "$IP")
  else
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
