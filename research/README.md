# Research Evidence

ClusterDeck follows the OpenForge Research Evidence Collection Standard:
https://github.com/dasomel/openforge/blob/main/docs/research-evidence.md

Collect machine-readable evidence during normal development when practical. Useful evidence includes verify/build/test duration/results, SSH/bootstrap/kubeconfig workflow duration and success/failure, retry/recovery behavior, runtime/environment measurements, and agent-assisted attempts/interventions/review corrections/CI retries/final verification. Preserve failed/partial runs and distinguish mocked/FakeRunner evidence from real SSH/OpenSSH/kubectl/native-runtime evidence.

## Legacy evidence on discovery

During implementation, fixes, verification, SSH/Kubernetes workflow testing, releases, or documentation, catalog historical verification results, FakeRunner/real-runtime test evidence, bootstrap/SSH/kubeconfig outcomes, CI outputs, failure/recovery records, and dated implementation evidence encountered from earlier work. Preserve originals and the mocked-vs-real distinction.

Use `dasomel/openforge#89` as the portfolio-level legacy catalog source of truth. Record source/path, known date, evidence class/strength, environment scope, metrics/facts, limitations, and likely paper use. Do not infer missing historical measurements. Preserve failed, partial, and superseded evidence when useful longitudinally.

## Public-data rule

This is a personal OSS/test project. Synthetic VM/cluster names, RFC1918 addresses, local endpoints, SSH workflow topology, Kubernetes object names, and reproducibility-relevant runtime details may remain when intentionally part of the public test setup.

Never publish actual passwords, credentials, private keys, tokens, secret-bearing kubeconfig, or accidental personal data. Review future third-party/non-public artifacts separately. Validate structured evidence against the OpenForge schema and run secret/pattern checks before publication.