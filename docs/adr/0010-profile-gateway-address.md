# ADR-0010: Optional per-profile gateway address for discovered endpoints

- Status: Accepted
- Date: 2026-10-16
- Relates to: [ADR-0004](0004-hosts-file-toctou-mitigation.md), [ADR-0009](0009-bastion-api-tunnel.md)

## Context

`discover_cluster_endpoints` maps every Ingress, APISIX, Istio and Gateway API host to the first LoadBalancer Service IP, or else to the first host's address. For a cloud cluster with private nodes, the gateway is exposed by an external load balancer with a public IP that forwards to worker NodePorts. The gateway Service is `NodePort`, so Kubernetes reports no `status.loadBalancer`, and endpoints (and the `/etc/hosts` block) point at an unreachable private address.

## Decision

- **D1 — Schema.** `Profile.gateway_address: Option<String>` (`#[serde(default, skip_serializing_if = "Option::is_none")]`). Existing `profiles.yaml` files load and round-trip unchanged. This widens the profile schema and the Tauri `Profile` DTO; no new command is added.
- **D2 — Semantics.** When set it is the target IP of all discovered endpoints of that profile, overriding LB IPs and the fallback, because Kubernetes cannot know an external cloud LB address. Node host aliases in `/etc/hosts` are unchanged.
- **D3 — Validation.** Must satisfy `validate::is_safe_ip_address` (a bare IP). Enforced in `validate_profile` (so `store::upsert_profile` rejects it and `load_profiles` skips it), re-checked in `hosts_file::render_hosts_block`, and ignored by discovery if invalid.

## Consequences

- Opt-in in the profile editor; unset keeps the previous behaviour.
- A hostname is not accepted (IP only), since the value lands in the IP column of `/etc/hosts`.
