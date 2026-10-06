import { useState } from 'react';
import { CircleAlert } from 'lucide-react';
import type { DiscoveredCaView } from '../../api/tauri';
import Pill from '../../components/ui/Pill';
import EmptyState from '../../components/ui/EmptyState';
import ConfirmModal from '../../components/ConfirmModal';

type TrustTabProps = {
  caViews: DiscoveredCaView[];
  caActionBusy: boolean;
  caRemoveBusy: boolean;
  onTrust: (ca: DiscoveredCaView) => Promise<void>;
  onRemove: (ca: DiscoveredCaView) => Promise<void>;
};

export default function TrustTab({
  caViews,
  caActionBusy,
  caRemoveBusy,
  onTrust,
  onRemove,
}: TrustTabProps) {
  const [trustTarget, setTrustTarget] = useState<DiscoveredCaView | null>(null);
  const [removeTarget, setRemoveTarget] = useState<DiscoveredCaView | null>(null);

  return (
    <div style={{ display: 'flex', flexDirection: 'column', gap: '16px' }}>
      <div className="panel-card" style={{ padding: '14px 16px' }}>
        <div style={{ display: 'flex', justifyContent: 'space-between', alignItems: 'center', marginBottom: '8px' }}>
          <span style={{ fontSize: '11px', fontWeight: 600, color: 'var(--text-tertiary)', textTransform: 'uppercase' }}>
            Cluster Certificate Authorities ({caViews.length})
          </span>
        </div>

        {caViews.length > 0 ? (
          <div className="host-list">
            {caViews.map((ca) => (
              <div className="host-row" key={ca.secret_ref}>
                <div>
                  <div style={{ display: 'flex', alignItems: 'center', gap: '8px' }}>
                    <span className="host-name mono" style={{ fontSize: '12px' }}>
                      {ca.subject_cn || ca.secret_ref}
                    </span>
                    <Pill
                      variant={
                        ca.status === 'trusted'
                          ? 'ok'
                          : ca.status === 'rotated'
                            ? 'warn'
                            : 'warn'
                      }
                    >
                      {ca.status === 'trusted'
                        ? 'CA Trusted'
                        : ca.status === 'rotated'
                          ? 'CA Changed'
                          : 'CA Untrusted'}
                    </Pill>
                  </div>
                  <div className="host-address">
                    {ca.source_hosts.length} host(s) · expires {ca.not_after || 'unknown'}
                  </div>

                  {ca.warnings.length > 0 && (
                    <div style={{ display: 'flex', flexDirection: 'column', gap: '2px', marginTop: '4px' }}>
                      {ca.warnings.map((warning, idx) => (
                        <div
                          key={idx}
                          style={{
                            display: 'flex',
                            alignItems: 'flex-start',
                            gap: '4px',
                            fontSize: '11px',
                            color: 'var(--warning)',
                          }}
                        >
                          <CircleAlert size={12} style={{ flexShrink: 0, marginTop: '1px' }} />
                          <span>{warning}</span>
                        </div>
                      ))}
                    </div>
                  )}
                </div>

                <div style={{ display: 'flex', alignItems: 'center', gap: '6px' }}>
                  {ca.status !== 'trusted' && (
                    <button
                      type="button"
                      className="secondary-button"
                      style={{ width: 'auto', marginTop: 0, padding: '5px 10px', fontSize: '11px' }}
                      onClick={() => setTrustTarget(ca)}
                    >
                      {ca.status === 'rotated' ? 'Update Trust' : 'Trust CA'}
                    </button>
                  )}
                  {ca.status === 'trusted' && (
                    <button
                      type="button"
                      className="secondary-button"
                      style={{ width: 'auto', marginTop: 0, padding: '5px 10px', fontSize: '11px', color: 'var(--danger)' }}
                      onClick={() => setRemoveTarget(ca)}
                    >
                      Remove
                    </button>
                  )}
                </div>
              </div>
            ))}
          </div>
        ) : (
          <EmptyState
            title="No certificate authorities found. Scan endpoints first."
            description="Certificate authority discovery runs automatically after connecting or scanning endpoints."
          />
        )}
      </div>

      {trustTarget && (
        <ConfirmModal
          title={
            trustTarget.status === 'rotated'
              ? `Update trust for "${trustTarget.subject_cn || trustTarget.secret_ref}"?`
              : `Trust CA "${trustTarget.subject_cn || trustTarget.secret_ref}"?`
          }
          message={
            trustTarget.status === 'rotated'
              ? `This cluster's CA has changed since it was last trusted (likely a clean reinstall). Remove the old trust entry and trust the new certificate (SHA-256 ${trustTarget.fingerprint_sha256.slice(0, 16)}..., expires ${trustTarget.not_after || 'unknown'}) for ${trustTarget.source_hosts.length} host(s)? macOS will ask you to confirm in a system dialog.`
              : `Add this certificate (SHA-256 ${trustTarget.fingerprint_sha256.slice(0, 16)}..., expires ${trustTarget.not_after || 'unknown'}) to your login keychain so Safari/Chrome stop warning on ${trustTarget.source_hosts.length} host(s) behind it? macOS will ask you to confirm in a system dialog.`
          }
          confirmLabel={trustTarget.status === 'rotated' ? 'Update Trust' : 'Trust CA'}
          cancelLabel="Cancel"
          isDanger={false}
          busy={caActionBusy}
          onConfirm={async () => {
            await onTrust(trustTarget);
            setTrustTarget(null);
          }}
          onCancel={() => {
            if (!caActionBusy) setTrustTarget(null);
          }}
        />
      )}

      {removeTarget && (
        <ConfirmModal
          title={`Remove local trust for "${removeTarget.subject_cn || removeTarget.secret_ref}"?`}
          message={`This removes the CA from your login keychain and stops ClusterDeck from tracking it as trusted for ${removeTarget.source_hosts.length} host(s). Safari/Chrome will warn on these hosts again until you re-trust the CA. macOS will ask you to confirm in a system dialog.`}
          confirmLabel="Remove"
          cancelLabel="Cancel"
          isDanger={true}
          busy={caRemoveBusy}
          onConfirm={async () => {
            await onRemove(removeTarget);
            setRemoveTarget(null);
          }}
          onCancel={() => {
            if (!caRemoveBusy) setRemoveTarget(null);
          }}
        />
      )}
    </div>
  );
}
