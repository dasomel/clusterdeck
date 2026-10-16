import type { HostnameConflict } from '../api/types';

export type ShadowedHost = { hostname: string; otherIp: string };

export type ConflictOwner = { ownerId: string; hosts: ShadowedHost[] };

export type ConflictGroups = {
  owners: ConflictOwner[];
  /** Conflicting lines that are not ClusterDeck blocks; shown as info only, never removed. */
  unmanaged: ShadowedHost[];
};

// Mirrors services/validate.rs::is_safe_profile_id. The backend re-checks; this
// only keeps obviously bad ids from ever reaching the Tauri command.
export function isSafeProfileId(id: string): boolean {
  return id.length > 0 && id.length <= 64 && /^[A-Za-z0-9_-]+$/.test(id);
}

/**
 * Groups the conflicts where another entry shadows ours (`we_win === false`) by
 * the owning profile. The current profile and malformed ids are excluded so the
 * dialog can never offer to remove our own block or an unvalidated target.
 * Untested: the frontend has no test runner.
 */
export function groupBlockingConflicts(
  conflicts: HostnameConflict[],
  currentProfileId: string,
): ConflictGroups {
  const byOwner = new Map<string, ShadowedHost[]>();
  const unmanaged: ShadowedHost[] = [];
  for (const c of conflicts) {
    if (c.we_win) continue;
    const entry = { hostname: c.hostname, otherIp: c.other_ip };
    if (c.other_owner === null) {
      unmanaged.push(entry);
      continue;
    }
    if (c.other_owner === currentProfileId || !isSafeProfileId(c.other_owner)) continue;
    const hosts = byOwner.get(c.other_owner) ?? [];
    hosts.push(entry);
    byOwner.set(c.other_owner, hosts);
  }
  return {
    owners: [...byOwner].map(([ownerId, hosts]) => ({ ownerId, hosts })),
    unmanaged,
  };
}

export function hasBlockingConflicts(groups: ConflictGroups): boolean {
  return groups.owners.length > 0 || groups.unmanaged.length > 0;
}
