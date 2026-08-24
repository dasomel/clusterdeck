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
