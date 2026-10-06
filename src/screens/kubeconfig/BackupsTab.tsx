import { useState } from 'react';
import { Archive, FolderOpen, Undo2, Trash2, RefreshCw } from 'lucide-react';
import { api, type KubeconfigBackupInfo } from '../../api/tauri';
import { useStatus } from '../../state/StatusContext';
import { formatBytes } from '../../lib/format';
import ConfirmModal from '../../components/ConfirmModal';

type BackupsTabProps = {
  backups: KubeconfigBackupInfo[];
  busy: boolean;
  onReload: () => Promise<void>;
};

export default function BackupsTab({ backups, busy, onReload }: BackupsTabProps) {
  const { pushStatus } = useStatus();
  const [confirmMove, setConfirmMove] = useState(false);
  const [backupToRestore, setBackupToRestore] = useState<string | null>(null);
  const [backupToDelete, setBackupToDelete] = useState<string | null>(null);
  const [actionBusy, setActionBusy] = useState(false);

  const handleMoveCurrent = async () => {
    setActionBusy(true);
    try {
      const res = await api.backupKubeconfig(true);
      if (res.backed_up) {
        pushStatus('success', 'Kubeconfig moved to ~/.kube/bak', [res.message]);
      } else {
        pushStatus('warning', 'No ~/.kube/config to backup', [res.message]);
      }
      setConfirmMove(false);
      await onReload();
    } catch (err) {
      pushStatus('error', 'Backup failed', [String(err)]);
    } finally {
      setActionBusy(false);
    }
  };

  const handleRestore = async () => {
    if (!backupToRestore) return;
    setActionBusy(true);
    try {
      const res = await api.restoreKubeconfigBackup(backupToRestore);
      pushStatus('success', 'Restored kubeconfig', [res.message]);
      setBackupToRestore(null);
      await onReload();
    } catch (err) {
      pushStatus('error', 'Restore failed', [String(err)]);
    } finally {
      setActionBusy(false);
    }
  };

  const handleDelete = async () => {
    if (!backupToDelete) return;
    setActionBusy(true);
    try {
      await api.deleteKubeconfigBackup(backupToDelete);
      pushStatus('success', `Deleted backup: ${backupToDelete}`);
      setBackupToDelete(null);
      await onReload();
    } catch (err) {
      pushStatus('error', 'Failed to delete backup', [String(err)]);
    } finally {
      setActionBusy(false);
    }
  };

  const openFolderInFinder = async () => {
    const first = backups[0];
    if (first) {
      const dir = first.path.substring(0, first.path.lastIndexOf('/'));
      try {
        await api.openPathInFinder(dir);
      } catch (err) {
        pushStatus('error', 'Failed to open in Finder', [String(err)]);
      }
    }
  };

  return (
    <div style={{ display: 'flex', flexDirection: 'column', gap: '12px' }}>
      <div style={{ display: 'flex', justifyContent: 'space-between', alignItems: 'center' }}>
        <button
          type="button"
          className="secondary-button"
          style={{ width: 'auto', marginTop: 0, padding: '6px 12px', fontSize: '12px', gap: '6px' }}
          onClick={() => setConfirmMove(true)}
          disabled={busy || actionBusy}
          title="Move current active config to backup folder"
        >
          <Archive size={13} />
          Move current config to ~/.kube/bak
        </button>

        {backups.length > 0 && (
          <button
            type="button"
            className="secondary-button"
            style={{ width: 'auto', marginTop: 0, padding: '6px 12px', fontSize: '12px', gap: '6px' }}
            onClick={openFolderInFinder}
            title="Open backup folder in Finder"
          >
            <FolderOpen size={13} />
            Open in Finder
          </button>
        )}
      </div>

      <div className="data-table-container">
        <table className="data-table" aria-label="Kubeconfig Backups">
          <thead>
            <tr>
              <th scope="col">Filename</th>
              <th scope="col">Modified</th>
              <th scope="col">Size</th>
              <th scope="col" style={{ width: '80px', textAlign: 'right' }}>Actions</th>
            </tr>
          </thead>
          <tbody>
            {backups.map((b) => (
              <tr key={b.filename}>
                <td>
                  <span className="mono" style={{ fontSize: '12px' }}>{b.filename}</span>
                </td>
                <td style={{ color: 'var(--text-secondary)' }}>
                  {b.modified_at}
                </td>
                <td className="num" style={{ color: 'var(--text-secondary)' }}>
                  {formatBytes(b.size_bytes)}
                </td>
                <td style={{ textAlign: 'right' }}>
                  <div style={{ display: 'inline-flex', gap: '4px', alignItems: 'center' }}>
                    <button
                      type="button"
                      className="icon-button"
                      style={{ width: '24px', height: '24px' }}
                      title={`Restore ${b.filename} to ~/.kube/config`}
                      aria-label={`Restore ${b.filename}`}
                      onClick={() => setBackupToRestore(b.filename)}
                      disabled={busy || actionBusy}
                    >
                      <Undo2 size={13} />
                    </button>
                    <button
                      type="button"
                      className="icon-button"
                      style={{ width: '24px', height: '24px', color: 'var(--danger)' }}
                      title={`Delete backup ${b.filename}`}
                      aria-label={`Delete backup ${b.filename}`}
                      onClick={() => setBackupToDelete(b.filename)}
                      disabled={busy || actionBusy}
                    >
                      <Trash2 size={13} />
                    </button>
                  </div>
                </td>
              </tr>
            ))}
            {backups.length === 0 && (
              <tr>
                <td colSpan={4} style={{ textAlign: 'center', color: 'var(--text-tertiary)', padding: '16px' }}>
                  No backups found in ~/.kube/bak
                </td>
              </tr>
            )}
          </tbody>
        </table>
      </div>

      {confirmMove && (
        <ConfirmModal
          title="Move current ~/.kube/config to backup?"
          message="This moves your active ~/.kube/config to a timestamped file in ~/.kube/bak/. Your active config will be empty until a backup is restored or a cluster profile is merged."
          confirmLabel="Move to ~/.kube/bak"
          cancelLabel="Cancel"
          isDanger={false}
          busy={actionBusy}
          onConfirm={handleMoveCurrent}
          onCancel={() => {
            if (!actionBusy) setConfirmMove(false);
          }}
        />
      )}

      {backupToRestore && (
        <ConfirmModal
          title={`Restore "${backupToRestore}"?`}
          message={`This replaces ~/.kube/config with the contents of "${backupToRestore}". Any unsaved changes in your current config will be overwritten.`}
          confirmLabel="Restore backup"
          cancelLabel="Cancel"
          isDanger={false}
          busy={actionBusy}
          onConfirm={handleRestore}
          onCancel={() => {
            if (!actionBusy) setBackupToRestore(null);
          }}
        />
      )}

      {backupToDelete && (
        <ConfirmModal
          title={`Delete backup "${backupToDelete}"?`}
          message={`Are you sure you want to permanently delete backup "${backupToDelete}"? This cannot be undone.`}
          confirmLabel="Delete backup"
          cancelLabel="Cancel"
          isDanger={true}
          busy={actionBusy}
          onConfirm={handleDelete}
          onCancel={() => {
            if (!actionBusy) setBackupToDelete(null);
          }}
        />
      )}
    </div>
  );
}
