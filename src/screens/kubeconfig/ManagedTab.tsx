import { FolderOpen } from 'lucide-react';
import { api, type ManagedProfileKubeconfig } from '../../api/tauri';
import { useStatus } from '../../state/StatusContext';
import { formatBytes } from '../../lib/format';
import Pill from '../../components/ui/Pill';

type ManagedTabProps = {
  managedConfigs: ManagedProfileKubeconfig[];
};

export default function ManagedTab({ managedConfigs }: ManagedTabProps) {
  const { pushStatus } = useStatus();

  const openInFinder = async (path: string) => {
    try {
      await api.openPathInFinder(path);
    } catch (err) {
      pushStatus('error', 'Failed to open in Finder', [String(err)]);
    }
  };

  return (
    <div className="data-table-container">
      <table className="data-table" aria-label="Managed Profile Kubeconfigs">
        <thead>
          <tr>
            <th scope="col">Cluster Profile</th>
            <th scope="col">Server Endpoint</th>
            <th scope="col">Status / Size</th>
            <th scope="col" style={{ width: '60px', textAlign: 'right' }}>Actions</th>
          </tr>
        </thead>
        <tbody>
          {managedConfigs.map((mc) => (
            <tr key={mc.profile_id}>
              <td>
                <div>
                  <span style={{ fontWeight: 600 }}>{mc.profile_name}</span>
                  <div className="mono" style={{ fontSize: '11px', color: 'var(--text-tertiary)' }}>
                    {mc.profile_id}
                  </div>
                </div>
              </td>
              <td className="mono" style={{ fontSize: '11px', color: 'var(--text-secondary)' }}>
                {mc.exists ? (mc.server ?? '—') : 'Not synced'}
              </td>
              <td>
                <Pill variant={mc.exists ? 'ok' : 'idle'}>
                  {mc.exists ? formatBytes(mc.size_bytes) : 'Not synced'}
                </Pill>
              </td>
              <td style={{ textAlign: 'right' }}>
                {mc.exists && (
                  <button
                    type="button"
                    className="icon-button"
                    style={{ width: '24px', height: '24px' }}
                    title={`Reveal ${mc.profile_name} config in Finder`}
                    aria-label={`Reveal ${mc.profile_name} config in Finder`}
                    onClick={() => openInFinder(mc.path)}
                  >
                    <FolderOpen size={13} />
                  </button>
                )}
              </td>
            </tr>
          ))}
          {managedConfigs.length === 0 && (
            <tr>
              <td colSpan={4} style={{ textAlign: 'center', color: 'var(--text-tertiary)', padding: '16px' }}>
                No managed profile kubeconfigs configured
              </td>
            </tr>
          )}
        </tbody>
      </table>
    </div>
  );
}
