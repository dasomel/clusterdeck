import { AlertTriangle, Info } from 'lucide-react';
import type { HostInfo, InventorySummary } from '../../api/types';
import { formatNum } from '../../lib/format';
import Meter from '../../components/ui/Meter';

type ResourceSummaryProps = {
  summary: InventorySummary;
  host: HostInfo;
  mode: string;
};

export default function ResourceSummary({
  summary,
  host,
  mode: _mode,
}: ResourceSummaryProps) {
  const isCpuOver = host.cpu != null && summary.cpu > host.cpu;
  const isMemOver = host.memory_gib != null && summary.memory_gib > host.memory_gib;

  return (
    <div style={{ display: 'flex', flexDirection: 'column', gap: 'var(--space-3)' }}>
      <div
        style={{
          display: 'grid',
          gridTemplateColumns: 'repeat(auto-fit, minmax(200px, 1fr))',
          gap: 'var(--space-3)',
        }}
        aria-label="Resource allocation metrics"
      >
        <div
          style={{
            padding: 'var(--space-3) var(--space-4)',
            background: 'var(--bg-elevated)',
            border: '1px solid var(--border)',
            borderRadius: 'var(--radius-md)',
            display: 'flex',
            flexDirection: 'column',
            gap: 'var(--space-1)',
          }}
        >
          <span style={{ fontSize: 'var(--fs-xs)', color: 'var(--text-tertiary)' }}>
            Running machines
          </span>
          <strong className="num" style={{ fontSize: 'var(--fs-xl)', color: 'var(--text-primary)' }}>
            {summary.running}
          </strong>
        </div>

        <div
          style={{
            padding: 'var(--space-3) var(--space-4)',
            background: 'var(--bg-elevated)',
            border: isCpuOver ? '1px solid var(--warning)' : '1px solid var(--border)',
            borderRadius: 'var(--radius-md)',
            display: 'flex',
            flexDirection: 'column',
            gap: 'var(--space-1)',
          }}
        >
          <div style={{ display: 'flex', justifyContent: 'space-between', alignItems: 'center' }}>
            <span style={{ fontSize: 'var(--fs-xs)', color: isCpuOver ? 'var(--warning)' : 'var(--text-tertiary)' }}>
              CPU allocated
            </span>
            <Meter
              value={summary.cpu}
              max={host.cpu}
              warning={isCpuOver}
              ariaLabel="CPU allocation progress"
            />
          </div>
          <strong className="num" style={{ fontSize: 'var(--fs-xl)', color: 'var(--text-primary)' }}>
            {formatNum(summary.cpu)} <span style={{ fontSize: 'var(--fs-sm)', fontWeight: 400, color: 'var(--text-secondary)' }}>/ {formatNum(host.cpu, ' cores')}</span>
          </strong>
        </div>

        <div
          style={{
            padding: 'var(--space-3) var(--space-4)',
            background: 'var(--bg-elevated)',
            border: isMemOver ? '1px solid var(--warning)' : '1px solid var(--border)',
            borderRadius: 'var(--radius-md)',
            display: 'flex',
            flexDirection: 'column',
            gap: 'var(--space-1)',
          }}
        >
          <div style={{ display: 'flex', justifyContent: 'space-between', alignItems: 'center' }}>
            <span style={{ fontSize: 'var(--fs-xs)', color: isMemOver ? 'var(--warning)' : 'var(--text-tertiary)' }}>
              Memory allocated
            </span>
            <Meter
              value={summary.memory_gib}
              max={host.memory_gib}
              warning={isMemOver}
              ariaLabel="Memory allocation progress"
            />
          </div>
          <strong className="num" style={{ fontSize: 'var(--fs-xl)', color: 'var(--text-primary)' }}>
            {formatNum(summary.memory_gib)} <span style={{ fontSize: 'var(--fs-sm)', fontWeight: 400, color: 'var(--text-secondary)' }}>/ {formatNum(host.memory_gib, ' GiB')}</span>
          </strong>
        </div>

        <div
          style={{
            padding: 'var(--space-3) var(--space-4)',
            background: 'var(--bg-elevated)',
            border: '1px solid var(--border)',
            borderRadius: 'var(--radius-md)',
            display: 'flex',
            flexDirection: 'column',
            gap: 'var(--space-1)',
          }}
        >
          <span style={{ fontSize: 'var(--fs-xs)', color: 'var(--text-tertiary)' }}>
            Configured VM disks
          </span>
          <strong className="num" style={{ fontSize: 'var(--fs-xl)', color: 'var(--text-primary)' }}>
            {formatNum(summary.disk_gib, ' GiB')}
          </strong>
        </div>
      </div>

      {(summary.warnings.length > 0 || summary.unknown_resources > 0 || isCpuOver || isMemOver) && (
        <div style={{ display: 'flex', flexDirection: 'column', gap: 'var(--space-2)' }}>
          {summary.warnings.map((w, i) => (
            <div
              key={`warn-${i}`}
              className="status-banner warning"
              style={{ padding: '8px 12px', fontSize: 'var(--fs-xs)', flexDirection: 'row', alignItems: 'center', gap: 'var(--space-2)' }}
            >
              <AlertTriangle size={14} style={{ color: 'var(--warning)', flexShrink: 0 }} />
              <span>{w}</span>
            </div>
          ))}

          {summary.unknown_resources > 0 && (
            <div
              className="status-banner warning"
              style={{ padding: '8px 12px', fontSize: 'var(--fs-xs)', flexDirection: 'row', alignItems: 'center', gap: 'var(--space-2)' }}
            >
              <AlertTriangle size={14} style={{ color: 'var(--warning)', flexShrink: 0 }} />
              <span>
                {summary.unknown_resources} running machine{summary.unknown_resources === 1 ? ' has' : 's have'} unknown resource allocations; totals are incomplete.
              </span>
            </div>
          )}
        </div>
      )}

      <div
        style={{
          display: 'flex',
          alignItems: 'center',
          gap: 'var(--space-2)',
          fontSize: 'var(--fs-xs)',
          color: 'var(--text-tertiary)',
        }}
      >
        <Info size={12} />
        <span>Allocated resources are configuration values, not measured host usage. Unknown values are shown as —.</span>
      </div>
    </div>
  );
}
