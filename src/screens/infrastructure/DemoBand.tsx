import { AlertTriangle } from 'lucide-react';

type DemoBandProps = {
  onSwitchToLive: () => void;
};

export default function DemoBand({ onSwitchToLive }: DemoBandProps) {
  return (
    <div className="demo-hatch-band" role="status">
      <div style={{ display: 'flex', alignItems: 'center', gap: 'var(--space-2)' }}>
        <AlertTriangle size={15} style={{ color: 'var(--warning)', flexShrink: 0 }} />
        <span style={{ fontSize: 'var(--fs-sm)', fontWeight: 600, color: 'var(--text-primary)' }}>
          DEMO MODE
        </span>
        <span style={{ fontSize: 'var(--fs-xs)', color: 'var(--text-secondary)' }}>
          · Sample environments. No host commands are executed.
        </span>
      </div>
      <button
        type="button"
        className="secondary-button compact-btn"
        onClick={onSwitchToLive}
      >
        Switch to live
      </button>
    </div>
  );
}
