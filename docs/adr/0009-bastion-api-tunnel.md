# ADR-0009: App-managed SSH API tunnel for bastion profiles

- Status: Accepted
- Date: 2026-10-16
- Relates to: [ADR-0002](0002-kubeconfig-stays-isolated-from-user-kube-config.md), [ADR-0001](0001-macos-first-tauri-architecture.md)

## Context

For a profile with a `bastion`, `connect_profile` fetches the kubeconfig over SSH (through ProxyJump) and `kubeconfig::normalize_with_host` rewrites a loopback API server to the control-plane host's private address, for example `https://10.0.0.10:6443`. The Mac cannot reach that address, so `kubectl get nodes` (and the curl fallback) in Verify fails and API, version and node information stay empty even though SSH works.

## Decision

- **D1 — Scope.** Only profiles with both `bastion` and `kubeconfig` get a tunnel. Direct profiles keep the existing endpoint rewrite with zero behaviour change.
- **D2 — Tunnel process.** `ssh -F <profile ssh conf> -o BatchMode=yes -o ConnectTimeout=5 -o ExitOnForwardFailure=yes -o ServerAliveInterval=15 -o ServerAliveCountMax=3 -N -L 127.0.0.1:<L>:<orig_host>:<orig_port> -- <profile>-<control-plane>`. The alias is the one the kubeconfig fetch already uses, so ProxyJump, identity and `StrictHostKeyChecking=accept-new` come from the generated config. `<orig_host>:<orig_port>` is the server of the *original* fetched kubeconfig, resolved from the control plane (loopback for k3s, node IP for kubeadm); port defaults to 443. The host is validated at the sink (hostname or IP shape only, IPv6 bracketed in the `-L` spec).
- **D3 — Stored kubeconfig.** The server becomes `https://127.0.0.1:<L>` and `tls-server-name` is set to the original host (an existing `tls-server-name` is never overwritten). This is an explicit `ApiEndpoint::{Direct, Tunnel}` parameter of the normalize path, so a non-loopback original is rewritten too, for tunnelled profiles only.
- **D4 — Local port.** Preferred port `16443 + FNV-1a-32(profile.id) % 1000` (a hand-rolled hash, because `std`'s `DefaultHasher` is not stable across releases). If it is not bindable on `127.0.0.1`, the next ports are tried sequentially, wrapping inside the 1000-port range.
- **D5 — Process model.** `CommandRunner` gains `spawn_background` (default: unsupported, so existing fakes are untouched) returning a `BackgroundProcess` (exit status, kill, bounded stderr tail). `SystemRunner` spawns with stdin/stdout null, stderr drained into a 4 KiB tail, and `kill_on_drop`. `services/tunnel.rs` holds a process-wide `TunnelManager` that reuses a live tunnel with the same target, otherwise replaces it, and waits up to 10 s for the local port to accept connections (failing early with the ssh stderr if the process exits). Tunnels are killed on `RunEvent::Exit` and when a profile is deleted. The manager is a `static` (like the profile write mutex) because `fetch_and_store` is reached from several commands. A tunnel failure is non-fatal: the kubeconfig is written with the direct endpoint and the reason is reported in `ConnectionResult.errors`. Readiness requires the local port to accept connections on two consecutive probes while the ssh child is alive and, when `lsof` is available, the ssh pid to be among the port's listeners (ownership verified). Without `lsof` the window for a foreign listener to pass for the tunnel is narrowed, not closed; ssh exiting on `ExitOnForwardFailure` is the backstop. A bind failure is retried once on the next port. On app exit, stored tunnels are killed and the pids of ssh children still in their readiness wait are SIGKILLed through the runner. Each profile has its own lock, so a slow tunnel never blocks other profiles or app exit. The original target is persisted in `state.json` so Verify can re-establish the tunnel after an app restart. If the target is missing or the tunnel cannot start, Verify does not abort: it logs the reason, still computes SSH reachability, and the Kubernetes check fails as it did before tunnels existed.

## Consequences

- Verify, endpoint discovery and kubectl through the profile kubeconfig work for bastion profiles.
- The tunnel lives only while ClusterDeck runs. A context merged into `~/.kube/config` for a bastion profile therefore points at `127.0.0.1:<L>` and works only while the app is running.
- One long-lived `ssh` per bastion profile; ports 16443-17442 are reserved for this purpose.
- The local port can change (the preferred port may be taken after a restart). Verify then retargets the profile kubeconfig and, only if the profile is already merged into `~/.kube/config`, re-merges it automatically.
- A tunnel is revived only by Connect or Verify; nothing restarts it in the background if ssh dies.
- Known limitation: after a crash or force-quit the `ssh -N` child can be orphaned and keep its port. There is no pidfile reaping yet; the next start picks another port, and the orphan must be killed manually.

## Alternatives considered

- **SOCKS proxy plus kubeconfig `proxy-url`.** Needs a dynamic forward and `proxy-url` support in every consumer (kubectl versions, the curl fallback, IDE plugins); the certificate would still be validated against the private address through the proxy. A plain loopback forward works with any client.
- **Verify remotely only (run `kubectl` on the control plane over SSH).** Keeps the Mac out of the data path but leaves the stored kubeconfig unusable locally, so endpoint discovery, the merged `~/.kube/config` and every local tool stay broken.
