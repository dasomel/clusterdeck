#!/usr/bin/env bash
# Runs the make verify stages, times each one, and appends sanitized JSONL
# records per docs/research-evidence.md (OpenForge Research Evidence
# Collection Standard). Never captures stdout/stderr content, only exit
# codes and durations, to avoid leaking local paths/identifiers.
set -uo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"

out_dir="research/evidence"
mkdir -p "$out_dir"
out_file="$out_dir/$(date -u +%Y-%m).jsonl"

revision="$(git rev-parse HEAD 2>/dev/null || echo unknown)"
os="$(uname -s | tr '[:upper:]' '[:lower:]')"
arch="$(uname -m)"

record() {
  local target="$1" result="$2" duration_ms="$3"
  printf '{"schema_version":"1.0","timestamp":"%s","repository":"dasomel/clusterdeck","revision":"%s","event_type":"build","task_or_test":"%s","result":"%s","duration_ms":%s,"environment":"%s/%s","attempt":1,"human_interventions":null,"review_corrections":null,"ci_retries":null,"metadata":{"os":"%s","arch":"%s","make_target":"%s"}}\n' \
    "$(date -u +%Y-%m-%dT%H:%M:%SZ)" "$revision" "$target" "$result" "$duration_ms" "$os" "$arch" "$os" "$arch" "$target" \
    >> "$out_file"
}

now_ms() {
  # macOS `date` has no %N; perl is present on both macOS and Linux runners.
  perl -MTime::HiRes=time -e 'printf "%.0f", time()*1000'
}

run_stage() {
  local target="$1"
  local start end duration_ms result
  start=$(now_ms)
  if make "$target" >/dev/null 2>&1; then
    result="pass"
  else
    result="fail"
  fi
  end=$(now_ms)
  duration_ms=$((end - start))
  record "$target" "$result" "$duration_ms"
  echo "[$target] $result (${duration_ms}ms)"
  [ "$result" = "pass" ]
}

overall=0
for stage in format lint test build; do
  run_stage "$stage" || overall=1
done

echo "Evidence appended to $out_file"
exit "$overall"
