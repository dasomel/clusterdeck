import { AlertTriangle } from 'lucide-react';
import CopyButton from '../../components/ui/CopyButton';

type StaleHintProps = {
  machineName: string;
};

export default function StaleHint({ machineName }: StaleHintProps) {
  const pruneCommand = 'vagrant global-status --prune';

  return (
    <tr>
      <td
        colSpan={7}
        style={{
          padding: '6px 12px 8px 36px',
          backgroundColor: 'var(--bg-sunken)',
          borderBottom: '1px solid var(--border)',
          fontSize: 'var(--fs-xs)',
          color: 'var(--text-secondary)',
        }}
      >
        <div style={{ display: 'flex', alignItems: 'center', gap: 'var(--space-2)', flexWrap: 'wrap' }}>
          <AlertTriangle size={13} style={{ color: 'var(--warning)', flexShrink: 0 }} />
          <span>
            <strong>{machineName}</strong>: no .vagrant data found (cached entry). Clean up:
          </span>
          <code
            className="mono"
            style={{
              padding: '2px 6px',
              backgroundColor: 'var(--bg-elevated)',
              border: '1px solid var(--border)',
              borderRadius: 'var(--radius-sm)',
              color: 'var(--text-primary)',
            }}
          >
            {pruneCommand}
          </code>
          <CopyButton text={pruneCommand} title="Copy cleanup command" />
        </div>
      </td>
    </tr>
  );
}
