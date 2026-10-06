import { ChevronDown, ChevronRight } from 'lucide-react';
import type { MachineGroup } from '../../lib/groupMachines';
import Tag from '../../components/ui/Tag';
import MachineRow from './MachineRow';
import StaleHint from './StaleHint';

type MachineGroupRowsProps = {
  group: MachineGroup;
  open: boolean;
  onToggle: () => void;
};

const SUMMARY_NAMES = 2;

export default function MachineGroupRows({ group, open, onToggle }: MachineGroupRowsProps) {
  const { machines, representative: rep } = group;
  const stale = rep.state === 'stale';

  if (machines.length === 1) {
    return (
      <>
        <MachineRow machine={rep} />
        {stale && <StaleHint machineName={rep.name} />}
      </>
    );
  }

  const names = machines.map((m) => m.name);
  const rest = names.length - SUMMARY_NAMES;
  const summary = `${names.slice(0, SUMMARY_NAMES).join(', ')}${rest > 0 ? ` +${rest}` : ''}`;
  const fullList = names.join(', ');
  const earliest = machines.reduce<number | null>(
    (min, m) => (m.created_at != null && (min == null || m.created_at < min) ? m.created_at : min),
    null,
  );

  const toggle = (
    <button
      type="button"
      onClick={onToggle}
      aria-expanded={open}
      aria-label={`${machines.length} identical machines: ${fullList}`}
      title={fullList}
      style={{
        display: 'flex',
        alignItems: 'center',
        gap: 'var(--space-2)',
        padding: 0,
        border: 'none',
        background: 'transparent',
        color: 'var(--text-primary)',
        font: 'inherit',
        cursor: 'pointer',
        textAlign: 'left',
      }}
    >
      {open ? <ChevronDown size={14} /> : <ChevronRight size={14} />}
      <Tag>×{machines.length}</Tag>
      <span style={{ fontSize: 'var(--fs-sm)', fontWeight: 600 }}>{summary}</span>
    </button>
  );

  return (
    <>
      <MachineRow machine={rep} nameCell={toggle} createdAt={earliest} />
      {open
        ? machines.map((m) => (
            <MachineRowWithHint key={m.id} machine={m} />
          ))
        : stale && <StaleHint machineName={`${machines.length} machines`} />}
    </>
  );
}

function MachineRowWithHint({ machine }: { machine: MachineGroup['representative'] }) {
  return (
    <>
      <MachineRow machine={machine} nested />
      {machine.state === 'stale' && <StaleHint machineName={machine.name} />}
    </>
  );
}
