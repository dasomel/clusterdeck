import { useState } from 'react';
import { Trash2 } from 'lucide-react';
import { api, type Profile } from '../../api/tauri';
import { useStatus } from '../../state/StatusContext';
import { dateShort } from '../../lib/format';
import ConfirmModal from '../../components/ConfirmModal';

type TrustedCaEntry = {
  profileId: string;
  profileName: string;
  secret_ref: string;
  subject_cn: string;
  not_after: string;
  trusted_at: string;
};

type TrustedCasTabProps = {
  profiles: Profile[];
  onCaRemoved: (profileId: string) => void;
  onReload: () => Promise<void>;
};

export default function TrustedCasTab({ profiles, onCaRemoved, onReload }: TrustedCasTabProps) {
  const { pushStatus } = useStatus();
  const [caToRemove, setCaToRemove] = useState<TrustedCaEntry | null>(null);
  const [busy, setBusy] = useState(false);

  const allTrustedCas: TrustedCaEntry[] = profiles.flatMap((p) =>
    p.trusted_cas.map((ca) => ({
      ...ca,
      profileId: p.id,
      profileName: p.name,
    })),
  );

  const handleRemoveConfirm = async () => {
    if (!caToRemove) return;
    setBusy(true);
    try {
      await api.removeCa(caToRemove.profileId, caToRemove.secret_ref);
      pushStatus('success', 'CA Trust Removed', [
        `${caToRemove.subject_cn || caToRemove.secret_ref} removed from profile "${caToRemove.profileName}".`,
      ]);
      setCaToRemove(null);
      onCaRemoved(caToRemove.profileId);
      await onReload();
    } catch (err) {
      pushStatus('error', 'CA Remove failed', [String(err)]);
    } finally {
      setBusy(false);
    }
  };

  return (
    <div style={{ display: 'flex', flexDirection: 'column', gap: '8px' }}>
      <div className="data-table-container">
        <table className="data-table" aria-label="Trusted Certificate Authorities">
          <thead>
            <tr>
              <th scope="col">Certificate CN / Reference</th>
              <th scope="col">Cluster Profile</th>
              <th scope="col">Expires</th>
              <th scope="col">Trusted Date</th>
              <th scope="col" style={{ width: '60px', textAlign: 'right' }}>Actions</th>
            </tr>
          </thead>
          <tbody>
            {allTrustedCas.map((ca) => (
              <tr key={`${ca.profileId}-${ca.secret_ref}`}>
                <td>
                  <div>
                    <span className="mono" style={{ fontWeight: 600, fontSize: '12px' }}>
                      {ca.subject_cn || ca.secret_ref}
                    </span>
                    {ca.subject_cn && (
                      <div className="mono" style={{ fontSize: '10px', color: 'var(--text-tertiary)' }}>
                        {ca.secret_ref}
                      </div>
                    )}
                  </div>
                </td>
                <td>{ca.profileName}</td>
                <td style={{ color: 'var(--text-secondary)' }}>{ca.not_after || 'unknown'}</td>
                <td style={{ color: 'var(--text-secondary)' }}>{dateShort(ca.trusted_at)}</td>
                <td style={{ textAlign: 'right' }}>
                  <button
                    type="button"
                    className="icon-button"
                    style={{ width: '24px', height: '24px', color: 'var(--danger)' }}
                    title={`Remove trust for ${ca.subject_cn || ca.secret_ref}`}
                    aria-label={`Remove trust for ${ca.subject_cn || ca.secret_ref}`}
                    onClick={() => setCaToRemove(ca)}
                    disabled={busy}
                  >
                    <Trash2 size={13} />
                  </button>
                </td>
              </tr>
            ))}
            {allTrustedCas.length === 0 && (
              <tr>
                <td colSpan={5} style={{ textAlign: 'center', color: 'var(--text-tertiary)', padding: '16px' }}>
                  No certificate authorities trusted yet across any profile
                </td>
              </tr>
            )}
          </tbody>
        </table>
      </div>

      {caToRemove && (
        <ConfirmModal
          title={`Remove local trust for "${caToRemove.subject_cn || caToRemove.secret_ref}"?`}
          message={`This removes the CA from your login keychain for profile "${caToRemove.profileName}". Safari/Chrome will warn on its hosts again until you re-trust the CA.`}
          confirmLabel="Remove"
          cancelLabel="Cancel"
          isDanger={true}
          busy={busy}
          onConfirm={handleRemoveConfirm}
          onCancel={() => {
            if (!busy) setCaToRemove(null);
          }}
        />
      )}
    </div>
  );
}
