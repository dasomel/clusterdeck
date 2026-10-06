import { AlertTriangle, CheckCircle2, ChevronRight, XCircle } from 'lucide-react';
import type { AttentionItem } from '../../lib/attention';

type AttentionListProps = {
  items: AttentionItem[];
  lastCheckedTime: string;
  onOpenCluster: (profileId: string) => void;
  onOpenInfrastructure: () => void;
};

export default function AttentionList({
  items,
  lastCheckedTime,
  onOpenCluster,
  onOpenInfrastructure,
}: AttentionListProps) {
  return (
    <div
      style={{
        padding: 'var(--space-4)',
        background: 'var(--bg-elevated)',
        border: '1px solid var(--border)',
        borderRadius: 'var(--radius-lg)',
        boxShadow: 'var(--shadow-card)',
        display: 'flex',
        flexDirection: 'column',
        gap: 'var(--space-3)',
      }}
    >
      <div style={{ display: 'flex', alignItems: 'center', justifyContent: 'space-between' }}>
        <div style={{ display: 'flex', alignItems: 'center', gap: 'var(--space-2)' }}>
          <h3 style={{ margin: 0, fontSize: 'var(--fs-md)', fontWeight: 600, color: 'var(--text-primary)' }}>
            Needs attention
          </h3>
          {items.length > 0 && (
            <span
              className="pill warning"
              style={{ fontSize: 'var(--fs-xs)', padding: '1px 6px' }}
            >
              {items.length}
            </span>
          )}
        </div>

        <span style={{ fontSize: 'var(--fs-xs)', color: 'var(--text-tertiary)' }}>
          Checked {lastCheckedTime}
        </span>
      </div>

      {items.length === 0 ? (
        <div
          style={{
            display: 'flex',
            alignItems: 'center',
            gap: 'var(--space-2)',
            padding: 'var(--space-3) 0',
            color: 'var(--text-secondary)',
            fontSize: 'var(--fs-sm)',
          }}
        >
          <CheckCircle2 size={16} style={{ color: 'var(--success)' }} />
          <span>Nothing needs attention. All systems and clusters healthy.</span>
        </div>
      ) : (
        <div style={{ display: 'flex', flexDirection: 'column', gap: 'var(--space-2)' }}>
          {items.map((item) => {
            const isFail = item.severity === 'fail';
            return (
              <div
                key={item.id}
                style={{
                  display: 'flex',
                  alignItems: 'center',
                  justifyContent: 'space-between',
                  padding: 'var(--space-2) var(--space-3)',
                  background: 'var(--bg-sunken)',
                  border: '1px solid var(--border)',
                  borderRadius: 'var(--radius-md)',
                  gap: 'var(--space-3)',
                }}
              >
                <div style={{ display: 'flex', alignItems: 'center', gap: 'var(--space-2)', flex: 1, minWidth: 0 }}>
                  {isFail ? (
                    <XCircle size={15} style={{ color: 'var(--danger)', flexShrink: 0 }} />
                  ) : (
                    <AlertTriangle size={15} style={{ color: 'var(--warning)', flexShrink: 0 }} />
                  )}
                  <span
                    style={{
                      fontSize: 'var(--fs-sm)',
                      color: 'var(--text-primary)',
                      overflow: 'hidden',
                      textOverflow: 'ellipsis',
                      whiteSpace: 'nowrap',
                    }}
                  >
                    {item.message}
                  </span>
                </div>

                <button
                  type="button"
                  className="secondary-button compact-btn"
                  style={{ marginTop: 0, flexShrink: 0, display: 'inline-flex', alignItems: 'center', gap: '4px' }}
                  onClick={() => {
                    if (item.target === 'clusters' && item.profileId) {
                      onOpenCluster(item.profileId);
                    } else {
                      onOpenInfrastructure();
                    }
                  }}
                >
                  <span>{item.target === 'clusters' ? 'Open cluster' : 'Open infrastructure'}</span>
                  <ChevronRight size={13} />
                </button>
              </div>
            );
          })}
        </div>
      )}
    </div>
  );
}
