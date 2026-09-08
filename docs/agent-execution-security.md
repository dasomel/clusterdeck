# Agent Execution Security Profile

ClusterDeck adopts the OpenForge Agent Execution Security Contract as a **reduced local-tool execution profile**.

ClusterDeck is a desktop connectivity/bootstrap product, not a general Kubernetes administration agent. Its sensitive boundary is Rust-owned resolution of profile data into concrete filesystem/process/network operations such as SSH, ProxyJump, kubeconfig fetch/normalization, and opt-in managed host/SSH configuration.

Reference contract: https://github.com/dasomel/openforge/blob/main/docs/agent-execution-security.md

## Profile mapping

| OpenForge concept | ClusterDeck mapping |
|---|---|
| identity | local desktop user + selected ClusterDeck profile |
| tool contract | Rust service operation and `CommandRunner` call shape |
| resolved target | validated host/bastion/profile/path plus concrete operation |
| resolved arguments | final argv/environment/file mutation derived after `services/validate.rs` checks |
| request-side authorization | product workflow + explicit opt-in settings + Tauri/Rust command boundary |
| execution boundary | Rust service + `CommandRunner`; React is presentation only |
| post-state verification | SSH/kubectl/file verification already owned by the corresponding service |
| evidence | operation result/correlation metadata with credentials and private-key contents excluded |

## Risk classes

- **read/probe** — discovery, connectivity checks, kubeconfig verification, read-only inspection.
- **credential bootstrap** — password/key-assisted SSH bootstrap; sensitive even when the intended remote effect is bounded.
- **local mutation** — ClusterDeck-owned config/state writes.
- **external configuration mutation** — managed `~/.ssh/config` include/block, `/etc/hosts` managed block, kubeconfig normalization/write.
- **remote mutation** — out of the current product boundary unless explicitly introduced by ADR; must not emerge accidentally from a generic agent/tool interface.

## Required invariants

1. React or model-generated text never directly executes processes or writes sensitive files. Rust resolves the concrete operation first.
2. All process execution continues through `CommandRunner`; a parallel raw process path is a security defect.
3. Values reaching SSH argv, managed file paths, host identifiers, or other privileged sinks are revalidated at the sink using the existing validation boundary.
4. Passwords remain environment-only (`SSHPASS` with `sshpass -e`) and private-key contents never enter frontend state, logs, evidence, or an agent context.
5. Managed configuration writes remain scoped to ClusterDeck-owned files/blocks. Approval for one profile/target must never authorize replacement of an entire user configuration file.
6. A future conversational/autonomous path that can perform local/external configuration mutation must bind approval to the exact resolved operation, target, argv/environment shape, and managed file scope before execution.
7. Read/probe authority does not imply mutation authority. Discovery or verification must not silently bootstrap credentials or modify host/SSH/kubeconfig state.
8. Unit tests with `FakeRunner` prove service control flow, not real OpenSSH/kubectl/native behavior. Critical argv/file changes retain the repository's real-binary/runtime evidence requirement.
9. External command output, SSH banners, kubeconfig content, and remote text are untrusted tool output and cannot expand authority or override product/system policy.

## Adoption boundary

This reduced profile adds no new Tauri command, filesystem permission, process capability, SSH behavior, or Kubernetes administration feature. The full OpenForge session/invocation grant and exact-call human approval model becomes necessary if ClusterDeck evolves from its explicit desktop workflow into an autonomous executor capable of remote mutation or multi-step side effects.
