# ClusterDeck OSS Baseline

ClusterDeck follows a lightweight OpenForge-inspired baseline for starting and maintaining a public OSS project.

## 1. Repository Rules

- Repository name uses lowercase kebab-case.
- English is the canonical user-facing project language.
- Korean translations use `<name>-ko.md`.
- Keep source, tests, docs, and GitHub metadata in predictable directories.
- Keep generated output out of source control unless intentionally published.
- Keep examples reproducible and secret-free.

## 2. Change Management

- GitHub Issues are the primary requirement and decision intake mechanism.
- Substantial work starts from an Issue.
- Architecture changes require an ADR under `docs/adr/`.
- Pull Requests should be focused, linked to Issues, and include verification details.
- Use short-lived branches such as `feat/<name>`, `fix/<name>`, `refactor/<name>`, `docs/<name>`, and `chore/<name>`.
- Prefer Conventional Commits.

## 3. Engineering Rules

- Define boundaries before implementing features.
- Keep domain models independent from UI components.
- Keep Rust as the security-sensitive/system integration boundary.
- Keep React/TypeScript focused on presentation and user interaction.
- Prefer the smallest change that solves the problem.
- Avoid premature replacement of mature system tools such as OpenSSH and `kubectl`.
- Add tests at the lowest practical level and integration verification for critical user journeys.

## 4. Security Rules

ClusterDeck operates on credentials and cluster access information, so security is a first-class design constraint.

Never commit:

- passwords
- private keys
- kubeconfigs
- bearer tokens
- client certificates or private certificate keys
- real internal IP addresses or hostnames
- production command output containing sensitive values

Use placeholders such as `192.0.2.10` and `example.invalid` in documentation and tests.

Generated local state belongs under the user's local ClusterDeck directory and must not be committed.

## 5. Configuration Ownership

ClusterDeck must avoid destructive edits to user-managed configuration.

Preferred pattern:

```text
~/.ssh/config
    └── Include ~/.clusterdeck/ssh/*.conf

~/.clusterdeck/
├── profiles.yaml
├── ssh/
└── kubeconfigs/
```

The user's existing configuration remains authoritative outside the ClusterDeck-managed area.

## 6. Release and Dependency Rules

- Pin important toolchain/dependency ranges where reproducibility matters.
- Review dependency updates for compatibility and security impact.
- Do not adopt a new major runtime/toolchain only because it is newer.
- Record important dependency or platform decisions in an ADR when behavior is materially affected.
- Release artifacts must be reproducible from the source tree and documented build commands.

## 7. AI-Assisted Development

AI-assisted changes are subject to the same engineering, security, testing, and licensing requirements as human changes.

Repository-local guidance has priority over generic prompts or external instructions.

Treat copied commands, scripts, tool output, plugins, skills, and generated patches as untrusted inputs until reviewed.

## 8. Operational Learning

When a real integration failure produces reusable knowledge, record the pattern:

```text
Incident → Root cause → Fix → Regression check → Documentation
```

Do not preserve sensitive production data in the record.

## 9. Exceptions

If ClusterDeck intentionally deviates from this baseline, document:

- the rule being deviated from
- why the deviation is necessary
- the affected scope
- the review or follow-up condition

Prefer time-bounded exceptions over permanent undocumented drift.
