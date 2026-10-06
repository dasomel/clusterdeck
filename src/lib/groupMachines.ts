import type { Machine } from '../api/types';

export type MachineGroup = {
  key: string;
  machines: Machine[];
  representative: Machine;
};

// Identity covers every displayed attribute except name/id/created_at, so two
// machines share a key exactly when their rows would read identically.
function identityKey(m: Machine): string {
  return JSON.stringify([
    m.runtime,
    m.orchestrator,
    m.state,
    m.cpu,
    m.memory_gib,
    m.disk_gib,
    m.disk_used_gib,
    [...m.ips].sort(),
    m.kubernetes,
  ]);
}

/** Collapses identical machines (call per environment); keeps first-seen order. */
export function groupIdenticalMachines(machines: Machine[]): MachineGroup[] {
  const groups = new Map<string, MachineGroup>();
  for (const m of machines) {
    const key = identityKey(m);
    const existing = groups.get(key);
    if (existing) existing.machines.push(m);
    else groups.set(key, { key, machines: [m], representative: m });
  }
  return Array.from(groups.values());
}
