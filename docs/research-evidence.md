# Research Evidence Collection

ClusterDeck adopts the [OpenForge Research Evidence Collection Standard](https://github.com/dasomel/openforge/blob/main/docs/research-evidence.md) for capturing structured, longitudinal evidence about verification runs, agent-assisted development, and (later) SSH/kubeconfig workflow timings. This document summarizes the standard and how it applies here; the upstream document is authoritative for anything not covered below.

## Why

`make verify` and CI already produce pass/fail/timing signals, but they are ephemeral (console output, discarded after the run). Capturing them as structured records lets us see trends (flaky tests, growing build time, agent-assisted task success rate) without changing what verification actually checks.

## Schema

Each record is one JSON object (JSONL, one per line) with these fields, per the upstream standard:

| Field | Notes |
|---|---|
| `schema_version` | Starts at `1.0`, bump on field changes |
| `timestamp` | UTC ISO-8601 |
| `repository` | Public slug (`dasomel/clusterdeck`) — never a private mirror path |
| `revision` | Git commit SHA (`git rev-parse HEAD`) |
| `event_type` | One of `build`, `test`, `install`, `runtime`, `agent_task` for this repo currently |
| `task_or_test` | The Makefile target or check name (`fmt`, `lint`, `test`, `build`) |
| `result` | `pass`, `fail`, `partial`, `cancelled`, `skipped` |
| `duration_ms` | Measured wall-clock only — never backfilled/estimated |
| `environment` | OS/arch only (e.g. `darwin/arm64`); no hostnames or paths |
| `attempt` | Retry counter, starts at 1 |
| `metadata` | Allowlisted scalars only (see below) |

`human_interventions`, `review_corrections`, and `ci_retries` are reserved fields for agent-assisted development records; `scripts/collect-evidence.sh` currently emits `null` for these since it only instruments local/CI verification runs, not agent task tracking.

## What is never captured

Per AGENTS.md's existing Security Rules and the upstream standard's exclusion list, evidence must never contain:

- Credentials, private keys, SSH passwords, kubeconfig contents, or bastion/bootstrap connection details
- Raw command stdout/stderr (may contain local paths, usernames, or infrastructure identifiers) — only counts, exit codes, and durations
- Any private-endpoint, customer, or employer-identifying data

`metadata` is an allowlist, not a free-form dump: only documented scalar keys (currently: `os`, `arch`, `make_target`) are permitted. Do not widen it to include environment variables or command output without updating this document.

## Storage

```
research/
  README.md              # this pointer + local notes
  evidence/
    YYYY-MM.jsonl         # append-only, one file per month
```

`research/evidence/*.jsonl` is local-only (gitignored) by default, since even sanitized local timing data is not worth committing per-run — CI can upload it as a build artifact instead if longitudinal tracking across machines becomes valuable. This mirrors the upstream guidance to prefer aggregated/sanitized data over raw evidence when the raw form cannot be proven safe to publish.

## Automation

`make evidence` runs `make verify` stage-by-stage (`fmt`, `lint`, `test`, `build`), times each stage, and appends one sanitized JSONL record per stage to `research/evidence/<current-month>.jsonl`. See `scripts/collect-evidence.sh`.
