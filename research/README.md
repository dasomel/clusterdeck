# Research Evidence

ClusterDeck follows the OpenForge Research Evidence Collection Standard:
https://github.com/dasomel/openforge/blob/main/docs/research-evidence.md

Collect sanitized machine-readable evidence during normal development when practical. Useful evidence includes verify/build/test duration and results, SSH/bootstrap/kubeconfig workflow duration and success/failure, retry/recovery behavior, normalized runtime/environment measurements where relevant, and agent-assisted attempts, elapsed time, human interventions, review corrections, CI retries, and final verification.

Preserve failed and partial runs. Distinguish mocked/FakeRunner evidence from real SSH/OpenSSH/kubectl/native-runtime evidence.

## Public-data rule

Only sanitized records may be committed publicly. Never publish passwords, credentials, private keys, tokens, private URLs/IPs/hostnames, bastion details, kubeconfig contents, user-specific filesystem paths, personal/customer/employer data, confidential prompts/source, arbitrary environment dumps, or security-sensitive infrastructure details. Raw SSH output, kubeconfig, CI logs, screenshots, traces, and security output are sensitive-by-default.

Before public storage: validate against the OpenForge schema, run secret/pattern checks, normalize environment labels, review free-form fields, and publish aggregate/categorized measurements whenever raw artifacts cannot be proven safe.
