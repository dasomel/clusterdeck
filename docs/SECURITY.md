# ClusterDeck Security Guidelines

ClusterDeck is a local desktop application that handles SSH access information and Kubernetes credentials. Security is therefore a design requirement, not a later hardening task.

## Public Repository Rule

The repository is public by design. Source code and architecture documentation may be public, but operational secrets must never be committed.

Never commit:

- SSH private keys
- SSH passwords
- kubeconfig files containing client certificates or keys
- bearer tokens
- cloud credentials
- real internal hostnames or infrastructure inventories
- production IP addresses when they identify private infrastructure
- shell history or debug output containing credentials

Documentation and tests must use placeholders such as `192.0.2.10`, `example.internal`, and dummy credentials.

## Local Secret Handling

Persistent secrets should use macOS Keychain where practical. The ClusterDeck profile definition should reference a secret identifier rather than storing the secret itself.

Example:

```yaml
ssh:
  user: root
  identity_file: ~/.ssh/clusterdeck-demo
  password_ref: keychain://clusterdeck/profile/cka/bootstrap
```

The exact storage API is an implementation detail and must not expose secrets to the frontend unnecessarily.

## SSH

- Prefer public-key authentication after bootstrap.
- Initial password bootstrap must be opt-in.
- Passwords must not be printed to stdout/stderr.
- Use non-interactive verification where possible.
- Preserve existing user SSH configuration.
- Generated aliases should be isolated in the ClusterDeck-owned configuration area.

## kubeconfig

Generated kubeconfigs can contain client certificates and private keys.

- Store under a ClusterDeck-owned directory with restrictive permissions.
- Do not expose raw kubeconfig contents in UI logs.
- Do not upload kubeconfigs to telemetry or remote services.
- Back up user-managed kubeconfig before any merge operation.
- Avoid deleting contexts that ClusterDeck does not own.

## Logging

Logs should contain operational state but not secrets.

Safe example:

```text
[INFO] SSH bootstrap started: profile=cka-lab host=cka-m1
[INFO] SSH verification succeeded: profile=cka-lab host=cka-m1
[INFO] kubeconfig fetched: profile=cka-lab source=/etc/kubernetes/admin.conf
```

Unsafe examples:

```text
password=...
private_key=...
client-key-data=...
bearer_token=...
```

## Repository Hygiene

At minimum, `.gitignore` should exclude local ClusterDeck state, generated kubeconfigs, secret configuration, and development artifacts.

Recommended patterns include:

```gitignore
.clusterdeck/
*.kubeconfig
*.kubeconfig.yaml
*.key
*.pem
.env
.env.*
```

The actual project may use more specific patterns to avoid accidentally hiding intended test fixtures.

## Threat Model

The primary threat is accidental local credential disclosure rather than a remote multi-tenant service attack. The application should therefore minimize secret lifetime, avoid unnecessary persistence, and make local ownership boundaries explicit.
