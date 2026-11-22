import type { ReactNode } from 'react';
import type { Machine } from '../../api/types';
import { machineStatePill, k8sPill } from '../../lib/status';
import { formatNum, formatDisk, formatDateMs, formatDateTimeTooltip } from '../../lib/format';
import { Trash2 } from 'lucide-react';
import Pill from '../../components/ui/Pill';

type MachineRowProps = {
  machine: Machine;
  /** Replaces the name cell content (used by collapsed identical-machine groups). */
  nameCell?: ReactNode;
  /** Overrides created_at; null renders as an em dash. */
  createdAt?: number | null;
  nested?: boolean;
  /** Opens the delete confirmation; the button shows only when the backend allows deleting. */
  onDelete?: (machine: Machine) => void;
  deleteBusy?: boolean;
};

export default function MachineRow({
  machine: m,
  nameCell,
  createdAt = m.created_at,
  nested = false,
  onDelete,
  deleteBusy = false,
}: MachineRowProps) {
  const statePill = machineStatePill(m.state);
  const kPill = k8sPill(m.kubernetes);

  const hasResources = m.cpu != null || m.memory_gib != null;
  const createdTooltip = formatDateTimeTooltip(createdAt);

  return (
    <tr style={{ height: 'var(--row-h)' }}>
      <td>
        <div style={{ display: 'flex', alignItems: 'center', gap: 'var(--space-2)' }}>
          <Pill variant={statePill.variant}>{statePill.label}</Pill>
          {/* A group row stands for several machines; only concrete machines get a button. */}
          {onDelete && !nameCell && m.delete_kind != null && (
            <button
              type="button"
              className="icon-button"
              style={{ width: '22px', height: '22px', color: 'var(--danger)' }}
              onClick={() => onDelete(m)}
              disabled={deleteBusy}
              aria-label={`Delete ${m.name}`}
              title={m.delete_kind === 'prune' ? 'Remove stale entry' : 'Delete machine'}
            >
              <Trash2 size={12} />
            </button>
          )}
        </div>
      </td>

      <td style={nested ? { paddingLeft: 'var(--space-6)' } : undefined}>
        {nameCell ?? (
        <div style={{ display: 'flex', flexDirection: 'column', gap: '1px' }}>
          <strong style={{ color: 'var(--text-primary)', fontSize: 'var(--fs-sm)' }}>
            {m.name}
          </strong>
          <small style={{ color: 'var(--text-tertiary)', fontSize: '10px' }}>
            {m.orchestrator ? `${m.orchestrator} → ` : ''}
            {m.runtime}
          </small>
        </div>
        )}
      </td>

      <td>
        {hasResources ? (
          <span className="num" style={{ fontSize: 'var(--fs-xs)' }}>
            {m.cpu != null ? `${formatNum(m.cpu)} CPU` : '—'}
            {' · '}
            {m.memory_gib != null ? `${formatNum(m.memory_gib)} GiB` : '—'}
          </span>
        ) : (
          <span style={{ color: 'var(--text-tertiary)' }}>—</span>
        )}
      </td>

      <td>
        <span className="num" style={{ fontSize: 'var(--fs-xs)' }}>
          {formatDisk(m.disk_gib, m.disk_used_gib)}
        </span>
      </td>

      <td>
        {m.ips && m.ips.length > 0 ? (
          <span className="mono" style={{ fontSize: 'var(--fs-xs)' }}>
            {m.ips.join(', ')}
          </span>
        ) : (
          <span style={{ color: 'var(--text-tertiary)', fontSize: 'var(--fs-xs)' }}>
            IP unknown
          </span>
        )}
      </td>

      <td>
        <span
          className="num"
          style={{ fontSize: 'var(--fs-xs)' }}
          title={createdTooltip}
        >
          {formatDateMs(createdAt)}
        </span>
      </td>

      <td>
        <Pill variant={kPill.variant}>{kPill.label}</Pill>
      </td>
    </tr>
  );
}
