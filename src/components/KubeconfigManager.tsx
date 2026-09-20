import { useCallback, useEffect, useState } from 'react';
import {
  Archive,
  CheckCircle2,
  ChevronDown,
  ChevronRight,
  ExternalLink,
  FilePlus,
  FolderOpen,
  RefreshCw,
  Star,
  Trash2,
  Undo2,
  X,
} from 'lucide-react';
import {
  api,
  type KubeconfigBackupInfo,
  type KubeContextInfo,
  type ManagedProfileKubeconfig,
  type UserKubeconfigDetails,
} from '../api/tauri';
import { type StatusMessage } from './StatusBanner';

type KubeconfigManagerProps = {
  onClose: () => void;
  onStatusMessage: (msg: StatusMessage) => void;
};

export default function KubeconfigManager({ onClose, onStatusMessage }: KubeconfigManagerProps) {
  const [userConfig, setUserConfig] = useState<UserKubeconfigDetails | null>(null);
  const [backups, setBackups] = useState<KubeconfigBackupInfo[]>([]);
  const [managedConfigs, setManagedConfigs] = useState<ManagedProfileKubeconfig[]>([]);
  const [loading, setLoading] = useState(true);
  const [busy, setBusy] = useState(false);
  const [expandedBackups, setExpandedBackups] = useState(false);
  const [expandedManaged, setExpandedManaged] = useState(false);

  const reload = useCallback(async () => {
    setLoading(true);
    try {
      const [config, baks, managed] = await Promise.all([
        api.getUserKubeconfigDetails(false),
        api.listKubeconfigBackups(),
        api.listManagedProfileKubeconfigs(),
      ]);
      setUserConfig(config);
      setBackups(baks);
      setManagedConfigs(managed);
    } catch (err) {
      onStatusMessage({
        type: 'error',
        title: 'Failed to load kubeconfig details',
        details: [String(err)],
        time: new Date().toLocaleTimeString(),
      });
    } finally {
      setLoading(false);
    }
  }, [onStatusMessage]);

  useEffect(() => {
    reload();
  }, [reload]);

  const switchContext = async (name: string) => {
    setBusy(true);
    try {
      await api.setCurrentContext(name);
      onStatusMessage({ type: 'success', title: `Switched to context: ${name}`, time: new Date().toLocaleTimeString() });
      await reload();
    } catch (err) {
      onStatusMessage({ type: 'error', title: 'Failed to switch context', details: [String(err)], time: new Date().toLocaleTimeString() });
    } finally {
      setBusy(false);
    }
  };

  const deleteContext = async (name: string) => {
    setBusy(true);
    try {
      await api.deleteUserKubeContext(name);
      onStatusMessage({ type: 'success', title: `Deleted context: ${name}`, time: new Date().toLocaleTimeString() });
      await reload();
    } catch (err) {
      onStatusMessage({ type: 'error', title: 'Failed to delete context', details: [String(err)], time: new Date().toLocaleTimeString() });
    } finally {
      setBusy(false);
    }
  };

  const restoreBackup = async (filename: string) => {
    setBusy(true);
    try {
      const res = await api.restoreKubeconfigBackup(filename);
      onStatusMessage({ type: 'success', title: 'Restored kubeconfig', details: [res.message], time: new Date().toLocaleTimeString() });
      await reload();
    } catch (err) {
      onStatusMessage({ type: 'error', title: 'Restore failed', details: [String(err)], time: new Date().toLocaleTimeString() });
    } finally {
      setBusy(false);
    }
  };

  const deleteBackup = async (filename: string) => {
    setBusy(true);
    try {
      await api.deleteKubeconfigBackup(filename);
      onStatusMessage({ type: 'success', title: `Deleted backup: ${filename}`, time: new Date().toLocaleTimeString() });
      await reload();
    } catch (err) {
      onStatusMessage({ type: 'error', title: 'Failed to delete backup', details: [String(err)], time: new Date().toLocaleTimeString() });
    } finally {
      setBusy(false);
    }
  };

  const openInFinder = async (path: string) => {
    try {
      await api.openPathInFinder(path);
    } catch (err) {
      onStatusMessage({ type: 'error', title: 'Failed to open in Finder', details: [String(err)], time: new Date().toLocaleTimeString() });
    }
  };

  const formatBytes = (bytes: number) => {
    if (bytes === 0) return '0 B';
    if (bytes < 1024) return `${bytes} B`;
    return `${(bytes / 1024).toFixed(1)} KB`;
  };

  if (loading) {
    return (
      <div className="panel-card" style={{ padding: '32px', textAlign: 'center' }}>
        <RefreshCw size={20} className="spin" style={{ margin: '0 auto 12px', display: 'block', color: 'var(--text-tertiary)' }} />
        <div style={{ color: 'var(--text-secondary)', fontSize: '13px' }}>Loading kubeconfig details…</div>
      </div>
    );
  }

  return (
    <div style={{ display: 'flex', flexDirection: 'column', gap: '16px' }}>
      {/* Header */}
      <div style={{ display: 'flex', justifyContent: 'space-between', alignItems: 'center' }}>
        <h2 style={{ margin: 0, fontSize: '18px' }}>Kubeconfig Manager</h2>
        <button type="button" className="icon-button" onClick={onClose} title="Close">
          <X size={18} />
        </button>
      </div>

      {/* ~/.kube/config overview */}
      <section className="panel-card">
        <div className="panel-title" style={{ display: 'flex', justifyContent: 'space-between', alignItems: 'center' }}>
          <span>~/.kube/config</span>
          <div style={{ display: 'flex', gap: '6px' }}>
            {userConfig?.exists && (
              <button
                type="button"
                className="icon-button"
                style={{ width: '26px', height: '26px', padding: 0 }}
                title="Reveal in Finder"
                onClick={() => openInFinder(userConfig.path)}
              >
                <FolderOpen size={14} />
              </button>
            )}
            <button
              type="button"
              className="icon-button"
              style={{ width: '26px', height: '26px', padding: 0 }}
              title="Refresh"
              onClick={reload}
            >
              <RefreshCw size={14} />
            </button>
          </div>
        </div>

        {!userConfig?.exists ? (
          <div style={{ color: 'var(--text-tertiary)', padding: '12px 0', fontSize: '13px' }}>
            ~/.kube/config does not exist. Use "Merge to ~/.kube/config" from a profile to create it.
          </div>
        ) : (
          <>
            <div style={{ display: 'flex', gap: '16px', fontSize: '12px', color: 'var(--text-secondary)', padding: '8px 0' }}>
              <span>Size: <strong>{formatBytes(userConfig.size_bytes)}</strong></span>
              <span>Current: <strong className="mono">{userConfig.current_context ?? '—'}</strong></span>
              <span>Contexts: <strong>{userConfig.contexts.length}</strong></span>
            </div>

            {/* Contexts list */}
            <div style={{ display: 'flex', flexDirection: 'column', gap: '1px' }}>
              {userConfig.contexts.map((ctx: KubeContextInfo) => (
                <div
                  key={ctx.name}
                  style={{
                    display: 'flex',
                    justifyContent: 'space-between',
                    alignItems: 'center',
                    padding: '8px 0',
                    borderTop: '1px solid var(--border)',
                  }}
                >
                  <div style={{ minWidth: 0, flex: 1 }}>
                    <div style={{ display: 'flex', alignItems: 'center', gap: '6px' }}>
                      {ctx.is_current && <Star size={12} style={{ color: 'var(--accent)', flexShrink: 0 }} />}
                      <span style={{ fontWeight: ctx.is_current ? 650 : 400, fontSize: '13px' }}>{ctx.name}</span>
                    </div>
                    <div className="mono" style={{ fontSize: '11px', color: 'var(--text-tertiary)', marginTop: '2px', overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap' }}>
                      {ctx.server}
                    </div>
                  </div>
                  <div style={{ display: 'flex', gap: '4px', flexShrink: 0, marginLeft: '8px' }}>
                    {!ctx.is_current && (
                      <button
                        type="button"
                        className="icon-button"
                        style={{ width: '24px', height: '24px', padding: 0 }}
                        title="Use this context"
                        onClick={() => switchContext(ctx.name)}
                        disabled={busy}
                      >
                        <CheckCircle2 size={13} />
                      </button>
                    )}
                    <button
                      type="button"
                      className="icon-button"
                      style={{ width: '24px', height: '24px', padding: 0, color: 'var(--danger)' }}
                      title="Remove context"
                      onClick={() => deleteContext(ctx.name)}
                      disabled={busy}
                    >
                      <Trash2 size={13} />
                    </button>
                  </div>
                </div>
              ))}
              {userConfig.contexts.length === 0 && (
                <div style={{ color: 'var(--text-tertiary)', fontSize: '12px', padding: '8px 0' }}>No contexts found</div>
              )}
            </div>
          </>
        )}
      </section>

      {/* ClusterDeck managed kubeconfigs */}
      <section className="panel-card">
        <button
          type="button"
          className="panel-title"
          style={{ display: 'flex', alignItems: 'center', gap: '6px', cursor: 'pointer', background: 'none', border: 'none', padding: 0, color: 'inherit', width: '100%', textAlign: 'left' }}
          onClick={() => setExpandedManaged(!expandedManaged)}
        >
          {expandedManaged ? <ChevronDown size={14} /> : <ChevronRight size={14} />}
          <span>ClusterDeck Profiles ({managedConfigs.length})</span>
        </button>

        {expandedManaged && (
          <div style={{ marginTop: '8px', display: 'flex', flexDirection: 'column', gap: '1px' }}>
            {managedConfigs.map((mc: ManagedProfileKubeconfig) => (
              <div
                key={mc.profile_id}
                style={{
                  display: 'flex',
                  justifyContent: 'space-between',
                  alignItems: 'center',
                  padding: '8px 0',
                  borderTop: '1px solid var(--border)',
                }}
              >
                <div style={{ minWidth: 0, flex: 1 }}>
                  <div style={{ fontSize: '13px', fontWeight: 500 }}>{mc.profile_name}</div>
                  <div className="mono" style={{ fontSize: '11px', color: 'var(--text-tertiary)', marginTop: '2px' }}>
                    {mc.exists ? (mc.server ?? '—') : 'Not synced'}
                  </div>
                </div>
                <div style={{ display: 'flex', alignItems: 'center', gap: '6px', flexShrink: 0, marginLeft: '8px' }}>
                  <span className={`pill ${mc.exists ? 'success' : 'warning'}`} style={{ fontSize: '10px' }}>
                    {mc.exists ? formatBytes(mc.size_bytes) : 'Missing'}
                  </span>
                  {mc.exists && (
                    <button
                      type="button"
                      className="icon-button"
                      style={{ width: '24px', height: '24px', padding: 0 }}
                      title="Reveal in Finder"
                      onClick={() => openInFinder(mc.path)}
                    >
                      <ExternalLink size={12} />
                    </button>
                  )}
                </div>
              </div>
            ))}
            {managedConfigs.length === 0 && (
              <div style={{ color: 'var(--text-tertiary)', fontSize: '12px', padding: '8px 0' }}>No profiles configured</div>
            )}
          </div>
        )}
      </section>

      {/* Backups */}
      <section className="panel-card">
        <button
          type="button"
          className="panel-title"
          style={{ display: 'flex', alignItems: 'center', gap: '6px', cursor: 'pointer', background: 'none', border: 'none', padding: 0, color: 'inherit', width: '100%', textAlign: 'left' }}
          onClick={() => setExpandedBackups(!expandedBackups)}
        >
          {expandedBackups ? <ChevronDown size={14} /> : <ChevronRight size={14} />}
          <Archive size={14} />
          <span>Backups ({backups.length})</span>
        </button>

        {expandedBackups && (
          <div style={{ marginTop: '8px', display: 'flex', flexDirection: 'column', gap: '1px' }}>
            {backups.length > 0 && (
              <div style={{ display: 'flex', justifyContent: 'flex-end', marginBottom: '4px' }}>
                <button
                  type="button"
                  className="icon-button"
                  style={{ width: '24px', height: '24px', padding: 0 }}
                  title="Open backup folder in Finder"
                  onClick={async () => {
                    const bak = backups[0];
                    if (bak) {
                      const dir = bak.path.substring(0, bak.path.lastIndexOf('/'));
                      await openInFinder(dir);
                    }
                  }}
                >
                  <FolderOpen size={13} />
                </button>
              </div>
            )}
            {backups.map((b: KubeconfigBackupInfo) => (
              <div
                key={b.filename}
                style={{
                  display: 'flex',
                  justifyContent: 'space-between',
                  alignItems: 'center',
                  padding: '6px 0',
                  borderTop: '1px solid var(--border)',
                }}
              >
                <div style={{ minWidth: 0, flex: 1 }}>
                  <div className="mono" style={{ fontSize: '12px' }}>{b.filename}</div>
                  <div style={{ fontSize: '11px', color: 'var(--text-tertiary)', marginTop: '2px' }}>
                    {b.modified_at} · {formatBytes(b.size_bytes)}
                  </div>
                </div>
                <div style={{ display: 'flex', gap: '4px', flexShrink: 0, marginLeft: '8px' }}>
                  <button
                    type="button"
                    className="icon-button"
                    style={{ width: '24px', height: '24px', padding: 0 }}
                    title="Restore this backup"
                    onClick={() => restoreBackup(b.filename)}
                    disabled={busy}
                  >
                    <Undo2 size={13} />
                  </button>
                  <button
                    type="button"
                    className="icon-button"
                    style={{ width: '24px', height: '24px', padding: 0, color: 'var(--danger)' }}
                    title="Delete this backup"
                    onClick={() => deleteBackup(b.filename)}
                    disabled={busy}
                  >
                    <Trash2 size={13} />
                  </button>
                </div>
              </div>
            ))}
            {backups.length === 0 && (
              <div style={{ color: 'var(--text-tertiary)', fontSize: '12px', padding: '8px 0' }}>No backups yet</div>
            )}
          </div>
        )}
      </section>
    </div>
  );
}
