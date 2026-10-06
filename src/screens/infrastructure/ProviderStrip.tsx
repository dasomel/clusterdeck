import type { ProviderStatus } from '../../api/types';
import { providerPill } from '../../lib/status';
import Pill from '../../components/ui/Pill';

type ProviderStripProps = {
  providers: ProviderStatus[];
};

export default function ProviderStrip({ providers }: ProviderStripProps) {
  const errors = providers.filter((p) => p.status === 'error' && p.message);

  return (
    <div style={{ display: 'flex', flexDirection: 'column', gap: 'var(--space-2)' }}>
      <div
        style={{
          display: 'flex',
          alignItems: 'center',
          gap: 'var(--space-2)',
          flexWrap: 'wrap',
        }}
      >
        <span
          style={{
            fontSize: 'var(--fs-xs)',
            fontWeight: 600,
            color: 'var(--text-tertiary)',
            textTransform: 'uppercase',
            letterSpacing: '0.05em',
            marginRight: 'var(--space-1)',
          }}
        >
          Providers
        </span>

        {providers.map((p) => {
          const pill = providerPill(p.status);
          const tooltip = p.message ? `${p.label}: ${p.message}` : `${p.label}: ${p.status}`;
          return (
            <div
              key={p.id}
              style={{
                display: 'inline-flex',
                alignItems: 'center',
                gap: 'var(--space-2)',
                padding: '4px 10px',
                background: 'var(--bg-sunken)',
                border: '1px solid var(--border)',
                borderRadius: 'var(--radius-md)',
                fontSize: 'var(--fs-xs)',
              }}
              title={tooltip}
            >
              <span style={{ fontWeight: 600, color: 'var(--text-primary)' }}>{p.label}</span>
              <Pill variant={pill.variant}>{pill.label}</Pill>
            </div>
          );
        })}
      </div>

      {errors.length > 0 && (
        <div style={{ display: 'flex', flexDirection: 'column', gap: 'var(--space-1)' }}>
          {errors.map((p) => (
            <div
              key={`err-${p.id}`}
              className="form-error"
              style={{ fontSize: 'var(--fs-xs)', padding: '6px 10px' }}
            >
              <strong>{p.label}:</strong> {p.message}
            </div>
          ))}
        </div>
      )}
    </div>
  );
}
