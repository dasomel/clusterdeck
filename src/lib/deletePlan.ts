import type { DeletePlan, DeleteTarget } from '../api/types';

const targetLine = (t: DeleteTarget): string => {
  if (t.provider === 'vagrant') {
    return t.delete_kind === 'prune'
      ? `- ${t.name} (stale entry, ${t.project_dir ?? '?'}, id ${t.vagrant_id ?? '?'})`
      : `- ${t.name} (vagrant destroy in ${t.project_dir ?? '?'}, id ${t.vagrant_id ?? '?'})`;
  }
  return `- ${t.name} (${t.provider})`;
};

/**
 * Human-readable form of the plan the backend will re-check on delete. Pure rendering: the
 * plan itself is sent back unchanged, never rebuilt from this text (ADR-0012 D11).
 */
export function describeDeletePlan(plan: DeletePlan): string {
  const lines = plan.targets.map(targetLine);
  if (plan.stale_entries.length > 0) {
    lines.push(
      '',
      'This removes every stale Vagrant cache entry, not only the one above (vagrant global-status --prune):',
      ...plan.stale_entries.map((s) => `- ${s.machine_name} (${s.project_dir}, id ${s.vagrant_id})`),
    );
  }
  return lines.join('\n');
}
