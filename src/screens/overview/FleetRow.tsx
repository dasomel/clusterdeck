import type { Profile, VerificationResult } from '../../api/types';
import { deriveRail } from '../../lib/rail';
import PatchRail from '../../components/rail/PatchRail';

type FleetRowProps = {
  profile: Profile;
  status: VerificationResult | null;
  onOpen: (profileId: string) => void;
  onConnect: (profileId: string) => void;
};

export default function FleetRow({ profile, status, onOpen, onConnect }: FleetRowProps) {
  const steps = deriveRail(profile, null, status, false);
  const hostCount = profile.hosts.length;
  const isBastion = profile.bastion != null;

  return (
    <div
      className="cluster-list-row"
      style={{
        display: 'flex',
        flexDirection: 'row',
        alignItems: 'center',
        justifyContent: 'space-between',
        padding: 'var(--space-3) var(--space-4)',
        background: 'var(--bg-elevated)',
        border: '1px solid var(--border)',
        borderRadius: 'var(--radius-md)',
        cursor: 'default',
        gap: 'var(--space-3)',
      }}
    >
      <div
        style={{
          display: 'flex',
          flexDirection: 'column',
          gap: 'var(--space-1)',
          minWidth: '160px',
          cursor: 'pointer',
        }}
        onClick={() => onOpen(profile.id)}
      >
        <span
          style={{
            fontSize: 'var(--fs-md)',
            fontWeight: 600,
            color: 'var(--text-primary)',
          }}
        >
          {profile.name}
        </span>
        <div style={{ fontSize: 'var(--fs-xs)', color: 'var(--text-tertiary)' }}>
          <span>{hostCount} {hostCount === 1 ? 'host' : 'hosts'}</span>
          {' · '}
          <span>{isBastion ? 'bastion' : 'direct'}</span>
          {status?.kubernetes_version && (
            <>
              {' · '}
              <span className="mono">{status.kubernetes_version}</span>
            </>
          )}
        </div>
      </div>

      <div style={{ flex: 1, display: 'flex', justifyContent: 'center' }}>
        <PatchRail steps={steps} variant="mini" />
      </div>

      <div style={{ display: 'flex', alignItems: 'center', gap: 'var(--space-2)' }}>
        <button
          type="button"
          className="secondary-button compact-btn"
          style={{ marginTop: 0 }}
          onClick={() => onConnect(profile.id)}
        >
          Connect / Sync
        </button>
      </div>
    </div>
  );
}
