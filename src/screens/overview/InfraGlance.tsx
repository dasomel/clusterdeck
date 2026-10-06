import { ChevronRight, Server } from 'lucide-react';
import type { Inventory } from '../../api/types';
import { formatNum } from '../../lib/format';
import { providerPill } from '../../lib/status';
import Skeleton from '../../components/ui/Skeleton';
import Meter from '../../components/ui/Meter';
import Pill from '../../components/ui/Pill';

type InfraGlanceProps = {
  inventory: Inventory | null;
  loading: boolean;
  error: string | null;
  onRetry: () => void;
  onOpen: () => void;
};

export default function InfraGlance({
  inventory,
  loading,
  error,
  onRetry,
  onOpen,
}: InfraGlanceProps) {
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
      <div style={{ display: 'flex', justifyContent: 'space-between', alignItems: 'center' }}>
        <div style={{ display: 'flex', alignItems: 'center', gap: 'var(--space-2)' }}>
          <Server size={16} style={{ color: 'var(--accent)' }} />
          <h3 style={{ margin: 0, fontSize: 'var(--fs-md)', fontWeight: 600, color: 'var(--text-primary)' }}>
            Infrastructure
          </h3>
        </div>

        <button
          type="button"
          className="secondary-button compact-btn"
          style={{ marginTop: 0, display: 'inline-flex', alignItems: 'center', gap: '4px' }}
          onClick={onOpen}
        >
          <span>Open</span>
          <ChevronRight size={13} />
        </button>
      </div>

      {!inventory && loading ? (
        <div style={{ display: 'flex', flexDirection: 'column', gap: 'var(--space-2)', padding: 'var(--space-2) 0' }}>
          <Skeleton height={20} />
          <Skeleton height={20} />
          <Skeleton height={20} />
        </div>
      ) : !inventory && error ? (
        <div style={{ display: 'flex', flexDirection: 'column', gap: 'var(--space-2)', padding: 'var(--space-2) 0' }}>
          <span style={{ fontSize: 'var(--fs-xs)', color: 'var(--danger)' }}>
            Could not load inventory: {error}
          </span>
          <button type="button" className="secondary-button compact-btn" onClick={onRetry}>
            Retry
          </button>
        </div>
      ) : inventory ? (
        <div style={{ display: 'flex', flexDirection: 'column', gap: 'var(--space-3)' }}>
          <div style={{ fontSize: 'var(--fs-sm)', fontWeight: 600, color: 'var(--text-primary)' }}>
            {inventory.summary.running} running / {inventory.machines.length} VMs
          </div>

          <div style={{ display: 'flex', flexDirection: 'column', gap: 'var(--space-2)' }}>
            <div style={{ display: 'flex', justifyContent: 'space-between', alignItems: 'center', fontSize: 'var(--fs-xs)' }}>
              <span style={{ color: 'var(--text-secondary)' }}>CPU</span>
              <div style={{ display: 'flex', alignItems: 'center', gap: 'var(--space-2)' }}>
                <span className="num">
                  {formatNum(inventory.summary.cpu)} / {formatNum(inventory.host.cpu, ' cores')}
                </span>
                <Meter
                  value={inventory.summary.cpu}
                  max={inventory.host.cpu}
                  warning={inventory.host.cpu != null && inventory.summary.cpu > inventory.host.cpu}
                />
              </div>
            </div>

            <div style={{ display: 'flex', justifyContent: 'space-between', alignItems: 'center', fontSize: 'var(--fs-xs)' }}>
              <span style={{ color: 'var(--text-secondary)' }}>Mem</span>
              <div style={{ display: 'flex', alignItems: 'center', gap: 'var(--space-2)' }}>
                <span className="num">
                  {formatNum(inventory.summary.memory_gib)} / {formatNum(inventory.host.memory_gib, ' GiB')}
                </span>
                <Meter
                  value={inventory.summary.memory_gib}
                  max={inventory.host.memory_gib}
                  warning={inventory.host.memory_gib != null && inventory.summary.memory_gib > inventory.host.memory_gib}
                />
              </div>
            </div>
          </div>

          <div style={{ display: 'flex', flexWrap: 'wrap', gap: 'var(--space-1)', marginTop: 'var(--space-1)' }}>
            {inventory.providers.map((p) => {
              const pill = providerPill(p.status);
              return (
                <Pill key={p.id} variant={pill.variant} className="compact-pill" title={p.message ?? undefined}>
                  {p.label} {pill.label}
                </Pill>
              );
            })}
          </div>
        </div>
      ) : null}
    </div>
  );
}
