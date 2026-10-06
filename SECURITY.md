# Security Policy

## Scope

ClusterDeck handles sensitive local access material such as SSH configuration, private-key references, passwords used for one-time bootstrap, and Kubernetes kubeconfig data.

## Rules

- Never commit private keys, passwords, tokens, kubeconfigs, or real infrastructure endpoints.
- Keep generated credentials and kubeconfigs outside the repository.
- Use macOS Keychain or an equivalent secure local mechanism for secrets that must persist.
- Never print passwords, private-key contents, kubeconfig credentials, or bearer tokens in logs.
- Generated SSH and kubeconfig files must use restrictive permissions.
- ClusterDeck should modify only files that it owns or explicitly manages.
- Initial password authentication is a bootstrap mechanism, not the default long-term authentication method.
- Destructive operations must be explicit and should provide a safe recovery path where practical.

## Infrastructure & Local Discovery Security

- **Bounded command execution:** Provider CLI discovery runs fixed adapter commands without a shell, bounded by a 10-second timeout and 2 MiB stream caps on stdout and stderr (`CommandRunner::run_bounded`). Do not extend IPC to accept arbitrary executable names or arguments.
- **Trusted provider boundaries:** Installed provider CLIs (`colima`, `VBoxManage`, `vagrant`, `vmrun`) and local VM configuration files must be trusted; plugin initialization and provider subprocess behaviors occur outside ClusterDeck's process sandbox.
- **Path-segment validation:** Dynamic machine names, runtime IDs, and project directories joined into filesystem paths (`.vagrant/machines/<name>`, `.vmx` files, Lima configs) must pass strict path-segment validation (`is_safe_path_segment`), forbidding path separators, directory traversal (`..`), leading hyphens, NUL bytes, and newlines.
- **Safe UI rendering:** Discovered VM names, paths, and status strings from untrusted metadata are rendered strictly as plain text (no HTML or rich-text injection).
- **Execution limits & Demo safety:** No arbitrary shell commands, Vagrantfile code execution, registry pruning, or unconfirmed guest mutations are performed. Demo mode executes no host commands and reads no local VM files.

## Public Repository Rule

This repository is public. Examples, fixtures, screenshots, tests, issue reports, and documentation must use placeholders only.

Safe examples:

```text
192.0.2.10
cluster.example.invalid
user: example
```

Do not use real company IPs, hostnames, SSH credentials, kubeconfigs, certificates, or tokens.

## Reporting a Vulnerability

Please do not disclose an undisclosed security issue in a public GitHub Issue. Use GitHub's private vulnerability reporting/security advisory mechanism when available, or contact the maintainer privately before public disclosure.
