import { useState } from 'react';
import { Star, CheckCircle2, Trash2 } from 'lucide-react';
import { api, type KubeContextInfo, type UserKubeconfigDetails } from '../../api/tauri';
import { useStatus } from '../../state/StatusContext';
import Pill from '../../components/ui/Pill';
import ConfirmModal from '../../components/ConfirmModal';

type ContextsTabProps = {
  userConfig: UserKubeconfigDetails | null;
  busy: boolean;
  onReload: () => Promise<void>;
};

export default function ContextsTab({ userConfig, busy, onReload }: ContextsTabProps) {
  const { pushStatus } = useStatus();
  const [contextToDelete, setContextToDelete] = useState<string | null>(null);
  const [actionBusy, setActionBusy] = useState(false);

  const switchContext = async (name: string) => {
    setActionBusy(true);
    try {
      await api.setCurrentContext(name);
      pushStatus('success', `Switched to context: ${name}`);
      await onReload();
    } catch (err) {
      pushStatus('error', 'Failed to switch context', [String(err)]);
    } finally {
      setActionBusy(false);
    }
  };

  const handleDeleteConfirm = async () => {
    if (!contextToDelete) return;
    setActionBusy(true);
    try {
      await api.deleteUserKubeContext(contextToDelete);
      pushStatus('success', `Deleted context: ${contextToDelete}`);
      setContextToDelete(null);
      await onReload();
    } catch (err) {
      pushStatus('error', 'Failed to delete context', [String(err)]);
    } finally {
      setActionBusy(false);
    }
  };

  if (!userConfig?.exists) {
    return (
      <div style={{ color: 'var(--text-tertiary)', padding: '24px 0', fontSize: '13px', textAlign: 'center' }}>
        ~/.kube/config does not exist. Use &quot;Merge to ~/.kube/config&quot; from a cluster to create it.
      </div>
    );
  }

  const contexts = userConfig.contexts ?? [];

  return (
    <div style={{ display: 'flex', flexDirection: 'column', gap: '8px' }}>
      <div className="data-table-container">
        <table className="data-table" aria-label="Kubernetes Contexts">
          <thead>
            <tr>
              <th scope="col" style={{ width: '40px' }}><span className="sr-only">Status</span></th>
              <th scope="col">Context Name</th>
              <th scope="col">Cluster Server</th>
              <th scope="col">User</th>
              <th scope="col" style={{ width: '80px', textAlign: 'right' }}>Actions</th>
            </tr>
          </thead>
          <tbody>
            {contexts.map((ctx: KubeContextInfo) => (
              <tr key={ctx.name}>
                <td>
                  {ctx.is_current && (
                    <span title="Current active context">
                      <Star size={14} style={{ color: 'var(--accent)' }} />
                    </span>
                  )}
                </td>
                <td>
                  <div style={{ display: 'flex', alignItems: 'center', gap: '8px' }}>
                    <span className="mono" style={{ fontWeight: ctx.is_current ? 600 : 400 }}>
                      {ctx.name}
                    </span>
                    {ctx.is_current && <Pill variant="info">current</Pill>}
                  </div>
                </td>
                <td className="mono" style={{ fontSize: '11px', color: 'var(--text-secondary)' }}>
                  {ctx.server}
                </td>
                <td style={{ color: 'var(--text-secondary)' }}>
                  {ctx.user || '—'}
                </td>
                <td style={{ textAlign: 'right' }}>
                  <div style={{ display: 'inline-flex', gap: '4px', alignItems: 'center' }}>
                    {!ctx.is_current && (
                      <button
                        type="button"
                        className="icon-button"
                        style={{ width: '24px', height: '24px' }}
                        title={`Switch to context ${ctx.name}`}
                        aria-label={`Switch to context ${ctx.name}`}
                        onClick={() => switchContext(ctx.name)}
                        disabled={busy || actionBusy}
                      >
                        <CheckCircle2 size={13} />
                      </button>
                    )}
                    <button
                      type="button"
                      className="icon-button"
                      style={{ width: '24px', height: '24px', color: 'var(--danger)' }}
                      title={`Delete context ${ctx.name}`}
                      aria-label={`Delete context ${ctx.name}`}
                      onClick={() => setContextToDelete(ctx.name)}
                      disabled={busy || actionBusy}
                    >
                      <Trash2 size={13} />
                    </button>
                  </div>
                </td>
              </tr>
            ))}
            {contexts.length === 0 && (
              <tr>
                <td colSpan={5} style={{ textAlign: 'center', color: 'var(--text-tertiary)', padding: '16px' }}>
                  No contexts found in ~/.kube/config
                </td>
              </tr>
            )}
          </tbody>
        </table>
      </div>

      {contextToDelete && (
        <ConfirmModal
          title={`Delete context "${contextToDelete}"?`}
          message={`Are you sure you want to delete context "${contextToDelete}" from ~/.kube/config? This modifies your local kubeconfig file.`}
          confirmLabel="Delete context"
          cancelLabel="Cancel"
          isDanger={true}
          busy={actionBusy}
          onConfirm={handleDeleteConfirm}
          onCancel={() => {
            if (!actionBusy) setContextToDelete(null);
          }}
        />
      )}
    </div>
  );
}
