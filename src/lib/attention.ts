import type { Inventory, Profile, VerificationResult } from '../api/types';
import { formatNum } from './format';

export type AttentionItem = {
  id: string;
  severity: 'warn' | 'fail';
  message: string;
  target: 'clusters' | 'infrastructure';
  profileId?: string;
};

export function deriveAttention(
  profiles: Profile[],
  statuses: Record<string, VerificationResult | null>,
  inventory: Inventory | null,
): AttentionItem[] {
  const items: AttentionItem[] = [];

  // Check cluster statuses
  for (const p of profiles) {
    if (p.hosts.length === 0) {
      items.push({
        id: `profile-${p.id}-no-hosts`,
        severity: 'warn',
        message: `${p.name}: No hosts configured`,
        target: 'clusters',
        profileId: p.id,
      });
      continue;
    }

    const st = statuses[p.id];
    if (st) {
      if (!st.ssh) {
        items.push({
          id: `profile-${p.id}-ssh-unreachable`,
          severity: 'fail',
          message: `${p.name}: SSH unreachable on hosts`,
          target: 'clusters',
          profileId: p.id,
        });
      } else if (p.kubeconfig && !st.kubernetes) {
        items.push({
          id: `profile-${p.id}-k8s-unverified`,
          severity: 'warn',
          message: `${p.name}: Kubernetes API verification failed`,
          target: 'clusters',
          profileId: p.id,
        });
      }
    }
  }

  // Check infrastructure inventory
  if (inventory) {
    // 1. Stale machines
    for (const m of inventory.machines) {
      if (m.state.toLowerCase() === 'stale') {
        items.push({
          id: `machine-stale-${m.id}`,
          severity: 'warn',
          message: `Vagrant "${m.name}" is a stale cached entry`,
          target: 'infrastructure',
        });
      }
    }

    // 2. Resource over-allocation
    const host = inventory.host;
    const sum = inventory.summary;
    if (host.memory_gib != null && sum.memory_gib > host.memory_gib) {
      items.push({
        id: 'infra-memory-over-allocated',
        severity: 'warn',
        message: `Allocated memory ${formatNum(sum.memory_gib)} GiB exceeds host ${formatNum(host.memory_gib)} GiB`,
        target: 'infrastructure',
      });
    }

    if (host.cpu != null && sum.cpu > host.cpu) {
      items.push({
        id: 'infra-cpu-over-allocated',
        severity: 'warn',
        message: `Allocated CPU ${formatNum(sum.cpu)} cores exceeds host ${formatNum(host.cpu)} cores`,
        target: 'infrastructure',
      });
    }

    // 3. Provider errors
    for (const p of inventory.providers) {
      if (p.status.toLowerCase() === 'error') {
        items.push({
          id: `provider-error-${p.id}`,
          severity: 'fail',
          message: `Provider ${p.label}: ${p.message || 'Discovery error'}`,
          target: 'infrastructure',
        });
      }
    }
  }

  return items;
}
