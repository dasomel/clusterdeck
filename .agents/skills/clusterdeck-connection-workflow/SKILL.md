---
name: clusterdeck-connection-workflow
description: Implement or debug ClusterDeck Discovery → SSH bootstrap/ProxyJump → kubeconfig fetch/normalize/verify flows while preserving CommandRunner, input validation, user-config ownership, secret handling, and real SSH/kubectl evidence. Use for SSH, bastion, hosts-file, kubeconfig, discovery, or verification-path changes.
license: Apache-2.0
compatibility: Requires the ClusterDeck checkout and project Rust/Tauri/pnpm toolchain; real-path verification requires suitable SSH and Kubernetes targets.
metadata:
  openforge-scope: project
  openforge-owner: dasomel/clusterdeck
  openforge-maturity: verified
  openforge-version: "1"
---

# ClusterDeck Connection Workflow

## Use When

- Changing Discovery, SSH/bootstrap, bastion/ProxyJump, kubeconfig fetch/merge/normalization, host mapping, or connection verification.
- Fixing failures that cross Rust process execution and external OpenSSH/kubectl behavior.

## Do Not Use When

- Pure presentation changes that do not touch connection/domain contracts.
- General Kubernetes administration features outside ClusterDeck's connection-helper product boundary.

## Inputs

- Requested connection flow and affected profile fields.
- Relevant architecture/design/ADR and current service ownership.
- Whether a real SSH/Kubernetes target is available.

## Workflow

1. Read `AGENTS.md`, `docs/ARCHITECTURE.md`, `docs/03-mvp-design.md`, and the relevant issue/ADR.
2. Preserve the product flow: Discovery -> SSH Bootstrap -> SSH/ProxyJump -> kubeconfig Fetch -> Normalize -> Verify.
3. Route all external processes through `CommandRunner`; do not add direct `tokio::process::Command` sites.
4. Validate profile IDs, host/bastion identifiers, and addresses at privileged sinks before using them in argv, generated paths, SSH config, or `/etc/hosts` blocks.
5. Keep passwords out of argv and logs. Preserve `sshpass -e` / environment-based handling where password bootstrap is supported.
6. Preserve non-interactive SSH behavior: `BatchMode=yes` paths must retain the project's trust-on-first-use policy (`StrictHostKeyChecking=accept-new`) unless an approved security design changes it.
7. Modify only ClusterDeck-owned blocks/includes in user SSH, kubeconfig, or hosts configuration; never replace the user's entire file.
8. Add/update unit tests with `FakeRunner` for domain branching, but do not claim they prove real SSH argv/file behavior.
9. Run `make verify` (the authoritative local gate). For changes to critical external-command/file paths, also exercise the real binary/target once when feasible.

## Verification

Report static/build/unit evidence separately from real SSH/Tauri/Kubernetes evidence. `FakeRunner` proves the code path, not the behavior of an actual SSH server, known_hosts interaction, filesystem permission, or kubectl context.

## Stop / Escalate When

- The change would overwrite uncontrolled portions of user config.
- It exposes secrets/private keys to the frontend or process argv.
- It widens Tauri command/process/filesystem authority beyond the approved architecture.
- Real external-command behavior is central to the fix but cannot be exercised or otherwise evidenced.

## References

- `AGENTS.md`
- `docs/ARCHITECTURE.md`
- `docs/03-mvp-design.md`
- `services/process.rs`
- `services/validate.rs`
- `Makefile`
