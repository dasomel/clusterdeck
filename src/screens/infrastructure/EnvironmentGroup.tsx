import { useState } from 'react';
import { ChevronDown, ChevronRight, RefreshCw, Sparkles, Trash2 } from 'lucide-react';
import type { Machine } from '../../api/types';
import Tag from '../../components/ui/Tag';
import { groupIdenticalMachines } from '../../lib/groupMachines';
import MachineGroupRows from './MachineGroupRows';

type EnvironmentGroupProps = {
  environment: string;
  machines: Machine[];
  filter: string;
  mode: string;
  expanded: boolean;
  onToggle: () => void;
  onSetUp: (environment: string) => Promise<void>;
  onDelete: (environment: string) => void;
  onDeleteMachine: (machine: Machine) => void;
  /** Keys of in-flight deletes (environment names and machine ids). */
  busyKeys: Set<string>;
  busy: boolean;
};

export default function EnvironmentGroup({
  environment,
  machines,
  filter,
  mode,
  expanded,
  onToggle,
  onSetUp,
  onDelete,
  onDeleteMachine,
  busyKeys,
  busy,
}: EnvironmentGroupProps) {
  // Per-group manual toggles; absent means "follow the filter-driven default".
  const [overrides, setOverrides] = useState<Map<string, boolean>>(new Map());
  const groups = groupIdenticalMachines(machines);
  const isLive = mode === 'live';
  const isEligible =
    isLive &&
    machines.some(
      (m) =>
        m.orchestrator === 'vagrant' ||
        m.runtime === 'colima' ||
        m.environment.toLowerCase().includes('vagrant') ||
        m.environment.toLowerCase().includes('colima'),
    );

  // One machine the backend cannot delete makes the whole group non-deletable.
  const isDeletable = isLive && machines.every((m) => m.delete_kind != null);

  const k8sDetected = machines.some((m) => {
    const k = m.kubernetes.trim().toLowerCase();
    return k && !['none', 'no', 'unknown', 'not detected', ''].includes(k);
  });

  return (
    <tbody className="environment-group-tbody">
      <tr>
        <th
          scope="rowgroup"
          colSpan={7}
          className="table-group-header"
          style={{ padding: '8px 12px' }}
        >
          <div
            style={{
              display: 'flex',
              alignItems: 'center',
              justifyContent: 'space-between',
              gap: 'var(--space-3)',
              flexWrap: 'wrap',
            }}
          >
            <div style={{ display: 'flex', alignItems: 'center', gap: 'var(--space-2)' }}>
              <button
                type="button"
                className="icon-button"
                style={{ width: '24px', height: '24px', border: 'none', background: 'transparent' }}
                onClick={onToggle}
                aria-expanded={expanded}
                aria-label={expanded ? `Collapse ${environment}` : `Expand ${environment}`}
                title={expanded ? 'Collapse' : 'Expand'}
              >
                {expanded ? <ChevronDown size={15} /> : <ChevronRight size={15} />}
              </button>

              <span
                style={{
                  fontSize: 'var(--fs-md)',
                  fontWeight: 600,
                  color: 'var(--text-primary)',
                  fontFamily: 'var(--font-mono)',
                }}
              >
                {environment}
              </span>

              <Tag>
                {machines.length} machine{machines.length === 1 ? '' : 's'}
                {groups.length < machines.length && ` (${groups.length} distinct)`}
              </Tag>

              {k8sDetected && (
                <Tag>
                  <Sparkles size={11} style={{ marginRight: '3px', color: 'var(--accent)' }} />
                  k8s detected
                </Tag>
              )}
            </div>

            <div style={{ display: 'flex', alignItems: 'center', gap: 'var(--space-2)' }}>
              {isEligible ? (
                <button
                  type="button"
                  className="secondary-button compact-btn"
                  onClick={() => onSetUp(environment)}
                  disabled={busy}
                  style={{ display: 'inline-flex', alignItems: 'center', gap: '6px' }}
                >
                  {busy ? <RefreshCw size={12} className="spin" /> : null}
                  Set up cluster
                </button>
              ) : !isLive ? (
                <span
                  style={{
                    fontSize: 'var(--fs-xs)',
                    color: 'var(--text-tertiary)',
                    fontStyle: 'italic',
                  }}
                >
                  Demo data: nothing to connect
                </span>
              ) : null}
              {isDeletable && (
                <button
                  type="button"
                  className="danger-button compact-btn"
                  onClick={() => onDelete(environment)}
                  disabled={busy}
                  style={{ display: 'inline-flex', alignItems: 'center', gap: '6px' }}
                >
                  <Trash2 size={12} />
                  Delete group
                </button>
              )}
            </div>
          </div>
        </th>
      </tr>

      {expanded &&
        groups.map((g) => {
          // Reveal a filter hit hidden behind the representative's row.
          const q = filter.trim().toLowerCase();
          const autoOpen =
            q !== '' && g.machines.slice(1).some((m) => m.name.toLowerCase().includes(q));
          return (
            <MachineGroupRows
              key={g.key}
              group={g}
              open={overrides.get(g.key) ?? autoOpen}
              onDeleteMachine={isLive ? onDeleteMachine : undefined}
              busyKeys={busyKeys}
              groupBusy={busy}
              onToggle={() =>
                setOverrides((prev) => new Map(prev).set(g.key, !(prev.get(g.key) ?? autoOpen)))
              }
            />
          );
        })}
    </tbody>
  );
}
