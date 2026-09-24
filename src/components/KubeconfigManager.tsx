import { useCallback, useEffect, useState } from 'react';
import {
  Archive,
  CheckCircle2,
  ChevronDown,
  ChevronRight,
  Copy,
  ExternalLink,
  FilePlus,
  FolderOpen,
  Link2,
  Play,
  RefreshCw,
  RotateCw,
  Server,
  Square,
  Star,
  Terminal,
  Trash2,
  Undo2,
  X,
} from 'lucide-react';
import {
  api,
  type DiscoveredLocalHost,
  type KubeconfigBackupInfo,
  type KubeContextInfo,
  type LocalRuntimeLifecycleProvider,
  type ManagedProfileKubeconfig,
  type Profile,
  type UserKubeconfigDetails,
} from '../api/tauri';
import { type StatusMessage } from './StatusBanner';
import ConfirmModal from './ConfirmModal';

type KubeconfigManagerProps = {
  onClose: () => void;
  onStatusMessage: (msg: StatusMessage) => void;
  onCaRemoved?: (profileId: string) => void;
};

export default function KubeconfigManager({ onClose, onStatusMessage, onCaRemoved }: KubeconfigManagerProps) {
  const [userConfig, setUserConfig] = useState<UserKubeconfigDetails | null>(null);
  const [backups, setBackups] = useState<KubeconfigBackupInfo[]>([]);
  const [managedConfigs, setManagedConfigs] = useState<ManagedProfileKubeconfig[]>([]);
  const [profiles, setProfiles] = useState<Profile[]>([]);
  const [localHosts, setLocalHosts] = useState<DiscoveredLocalHost[]>([]);
  const [loading, setLoading] = useState(true);
  const [busy, setBusy] = useState(false);
  const [expandedBackups, setExpandedBackups] = useState(false);
  const [expandedManaged, setExpandedManaged] = useState(false);
  const [expandedCas, setExpandedCas] = useState(false);
  const [expandedLocalRuntime, setExpandedLocalRuntime] = useState(false);
  const [removingCa, setRemovingCa] = useState<{ profileId: string; profileName: string; secretRef: string; subjectCn: string } | null>(null);
  // A Set, not a single key: two different instances can each have an action in flight at once
  // (e.g. start A, then start B while A is still running), and each op must only clear its own
  // key when it finishes, not whichever one last started.
  const [busyInstanceKeys, setBusyInstanceKeys] = useState<Set<string>>(new Set());
  const [lifecycleConfirm, setLifecycleConfirm] = useState<{ action: 'stop' | 'restart'; host: DiscoveredLocalHost } | null>(null);

  const reload = useCallback(async () => {
    setLoading(true);
    try {
      const [config, baks, managed, profs, hosts] = await Promise.all([
        api.getUserKubeconfigDetails(false),
        api.listKubeconfigBackups(),
        api.listManagedProfileKubeconfigs(),
        api.listProfiles(),
        api.detectLocalHosts().catch(() => [] as DiscoveredLocalHost[]),
      ]);
      setUserConfig(config);
      setBackups(baks);
      setManagedConfigs(managed);
      setProfiles(profs);
      setLocalHosts(hosts);
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

  const formatRuntimeBytes = (bytes: number | null) => (bytes == null ? '—' : `${(bytes / 1024 ** 3).toFixed(1)} GiB`);

  const allTrustedCas = profiles.flatMap((p) => p.trusted_cas.map((ca) => ({ ...ca, profileId: p.id, profileName: p.name })));

  // Phase 2 (issue #14) lifecycle actions are only wired up for Colima/Lima — Vagrant rows stay
  // read-only-plus-Copy, per ADR-0007 D1.
  const instanceKey = (host: DiscoveredLocalHost) => `${host.provider}-${host.instance_name}`;
  const toLifecycleProvider = (provider: string): LocalRuntimeLifecycleProvider | null => {
    const lower = provider.toLowerCase();
    return lower === 'colima' || lower === 'lima' ? lower : null;
  };

  const runLifecycleAction = async (host: DiscoveredLocalHost, action: 'start' | 'stop' | 'restart') => {
    const provider = toLifecycleProvider(host.provider);
    if (!provider) return;
    const key = instanceKey(host);
    setBusyInstanceKeys((prev) => new Set(prev).add(key));
    try {
      const call =
        action === 'start' ? api.startLocalRuntime : action === 'stop' ? api.stopLocalRuntime : api.restartLocalRuntime;
      const result = await call(provider, host.instance_name);
      onStatusMessage({
        type: result.success ? 'success' : 'error',
        title: `${host.instance_name}: ${action} ${result.success ? 'succeeded' : 'failed'}`,
        // result.message is a raw stdout/stderr tail from the CLI (JSON blobs, k3s install logs
        // on success). On success a concise confirmation is enough; the CLI tail is only useful
        // as failure diagnostics, so it's shown only then.
        details: result.success ? undefined : [result.message],
        time: new Date().toLocaleTimeString(),
      });
      await reload();
    } catch (err) {
      onStatusMessage({
        type: 'error',
        title: `Failed to ${action} ${host.instance_name}`,
        details: [String(err)],
        time: new Date().toLocaleTimeString(),
      });
    } finally {
      setBusyInstanceKeys((prev) => {
        const next = new Set(prev);
        next.delete(key);
        return next;
      });
    }
  };

  const openLocalRuntimeShell = async (host: DiscoveredLocalHost) => {
    const provider = toLifecycleProvider(host.provider);
    if (!provider) return;
    try {
      await api.openLocalRuntimeShell(provider, host.instance_name);
    } catch (err) {
      onStatusMessage({
        type: 'error',
        title: `Failed to open shell for ${host.instance_name}`,
        details: [String(err)],
        time: new Date().toLocaleTimeString(),
      });
    }
  };

  const openLocalRuntimeContext = async (host: DiscoveredLocalHost) => {
    const provider = toLifecycleProvider(host.provider);
    if (!provider) return;
    try {
      await api.openLocalRuntimeContext(provider, host.instance_name);
    } catch (err) {
      onStatusMessage({
        type: 'error',
        title: `Failed to open runtime context for ${host.instance_name}`,
        details: [String(err)],
        time: new Date().toLocaleTimeString(),
      });
    }
  };

  const copyRuntimeInfo = async (host: DiscoveredLocalHost) => {
    // Deliberately excludes identity_file (ADR-0007 D4) — everything else here is already
    // visible in the panel.
    const lines = [
      `Provider: ${host.provider}`,
      `Name: ${host.instance_name}`,
      `Status: ${host.status}`,
      `Arch: ${host.arch ?? '—'}`,
      `CPU: ${host.cpus ?? '—'}`,
      `Memory: ${formatRuntimeBytes(host.memory_bytes)}`,
      `Disk: ${formatRuntimeBytes(host.disk_bytes)}`,
      `Runtime: ${host.runtime ?? '—'}`,
      `Address: ${host.address}:${host.port}`,
      `Docker context: ${host.docker_context ?? '—'}`,
      `Kube context: ${host.kube_context ?? '—'}`,
    ];
    try {
      await navigator.clipboard.writeText(lines.join('\n'));
      onStatusMessage({ type: 'success', title: `Copied ${host.instance_name} info`, time: new Date().toLocaleTimeString() });
    } catch (err) {
      onStatusMessage({ type: 'error', title: 'Failed to copy to clipboard', details: [String(err)], time: new Date().toLocaleTimeString() });
    }
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

      {/* Trusted CAs across all profiles */}
      <section className="panel-card">
        <button
          type="button"
          className="panel-title"
          style={{ display: 'flex', alignItems: 'center', gap: '6px', cursor: 'pointer', background: 'none', border: 'none', padding: 0, color: 'inherit', width: '100%', textAlign: 'left' }}
          onClick={() => setExpandedCas(!expandedCas)}
        >
          {expandedCas ? <ChevronDown size={14} /> : <ChevronRight size={14} />}
          <span>Trusted CAs ({allTrustedCas.length})</span>
        </button>

        {expandedCas && (
          <div style={{ marginTop: '8px', display: 'flex', flexDirection: 'column', gap: '1px' }}>
            {allTrustedCas.map((ca) => (
              <div
                key={`${ca.profileId}-${ca.secret_ref}`}
                style={{
                  display: 'flex',
                  justifyContent: 'space-between',
                  alignItems: 'center',
                  padding: '8px 0',
                  borderTop: '1px solid var(--border)',
                }}
              >
                <div style={{ minWidth: 0, flex: 1 }}>
                  <div style={{ fontSize: '13px', fontWeight: 500 }}>{ca.subject_cn || ca.secret_ref}</div>
                  <div className="mono" style={{ fontSize: '11px', color: 'var(--text-tertiary)', marginTop: '2px' }}>
                    {ca.profileName} · expires {ca.not_after || 'unknown'} · trusted {new Date(ca.trusted_at).toLocaleDateString()}
                  </div>
                </div>
                <div style={{ display: 'flex', alignItems: 'center', gap: '6px', flexShrink: 0, marginLeft: '8px' }}>
                  <button
                    type="button"
                    className="icon-button"
                    style={{ width: '24px', height: '24px', padding: 0, color: 'var(--danger)' }}
                    title="Remove local trust"
                    onClick={() =>
                      setRemovingCa({
                        profileId: ca.profileId,
                        profileName: ca.profileName,
                        secretRef: ca.secret_ref,
                        subjectCn: ca.subject_cn,
                      })
                    }
                  >
                    <Trash2 size={12} />
                  </button>
                </div>
              </div>
            ))}
            {allTrustedCas.length === 0 && (
              <div style={{ color: 'var(--text-tertiary)', fontSize: '12px', padding: '8px 0' }}>No CAs trusted yet</div>
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

      {/* Local runtime discovery (Colima/Lima/Vagrant) */}
      <section className="panel-card">
        <button
          type="button"
          className="panel-title"
          style={{ display: 'flex', alignItems: 'center', gap: '6px', cursor: 'pointer', background: 'none', border: 'none', padding: 0, color: 'inherit', width: '100%', textAlign: 'left' }}
          onClick={() => setExpandedLocalRuntime(!expandedLocalRuntime)}
        >
          {expandedLocalRuntime ? <ChevronDown size={14} /> : <ChevronRight size={14} />}
          <Server size={14} />
          <span>Local Runtime ({localHosts.length})</span>
        </button>

        {expandedLocalRuntime && (
          <div style={{ marginTop: '8px' }}>
            <div style={{ fontSize: '11px', color: 'var(--text-tertiary)', marginBottom: '8px' }}>
              Discovery for Colima, Lima, and Vagrant. Colima/Lima instances also support Start/Stop/Restart, opening a VM shell, and opening a shell scoped to the instance's Docker/kube context — Vagrant stays read-only.
            </div>
            {localHosts.length > 0 ? (
              <div className="host-list">
                {localHosts.map((host) => {
                  const key = instanceKey(host);
                  const isRunning = host.status.toLowerCase() === 'running';
                  const lifecycleProvider = toLifecycleProvider(host.provider);
                  const isBusy = busyInstanceKeys.has(key);
                  const hasContext = Boolean(host.docker_context || host.kube_context);
                  return (
                    <div className="host-row" key={key}>
                      <div>
                        <div style={{ display: 'flex', alignItems: 'center', gap: '8px', flexWrap: 'wrap' }}>
                          <span className="host-name mono">{host.instance_name}</span>
                          <span className={`pill ${isRunning ? 'success' : 'warning'}`}>{host.status}</span>
                          <span className="pill">{host.provider}</span>
                        </div>
                        <div className="host-address">{host.arch ?? 'arch unknown'} · {host.cpus == null ? 'CPU —' : `${host.cpus} CPU`} · {formatRuntimeBytes(host.memory_bytes)} memory · {formatRuntimeBytes(host.disk_bytes)} disk</div>
                        <div className="host-address">Runtime: {host.runtime ?? '—'} · Docker: {host.docker_context ?? '—'} · Kube: {host.kube_context ?? '—'}</div>
                      </div>
                      <div style={{ display: 'flex', gap: '4px', flexShrink: 0, marginLeft: '8px' }}>
                        {lifecycleProvider && !isRunning && (
                          <button
                            type="button"
                            className="icon-button"
                            style={{ width: '24px', height: '24px', padding: 0 }}
                            title="Start"
                            onClick={() => runLifecycleAction(host, 'start')}
                            disabled={isBusy}
                          >
                            {isBusy ? <RefreshCw size={13} className="spin" /> : <Play size={13} />}
                          </button>
                        )}
                        {lifecycleProvider && isRunning && (
                          <>
                            <button
                              type="button"
                              className="icon-button"
                              style={{ width: '24px', height: '24px', padding: 0, color: 'var(--danger)' }}
                              title="Stop"
                              onClick={() => setLifecycleConfirm({ action: 'stop', host })}
                              disabled={isBusy}
                            >
                              {isBusy ? <RefreshCw size={13} className="spin" /> : <Square size={13} />}
                            </button>
                            <button
                              type="button"
                              className="icon-button"
                              style={{ width: '24px', height: '24px', padding: 0 }}
                              title="Restart"
                              onClick={() => setLifecycleConfirm({ action: 'restart', host })}
                              disabled={isBusy}
                            >
                              {isBusy ? <RefreshCw size={13} className="spin" /> : <RotateCw size={13} />}
                            </button>
                            <button
                              type="button"
                              className="icon-button"
                              style={{ width: '24px', height: '24px', padding: 0 }}
                              title="Open VM shell"
                              onClick={() => openLocalRuntimeShell(host)}
                              disabled={isBusy}
                            >
                              <Terminal size={13} />
                            </button>
                            {hasContext && (
                              <button
                                type="button"
                                className="icon-button"
                                style={{ width: '24px', height: '24px', padding: 0 }}
                                title="Open in runtime context"
                                onClick={() => openLocalRuntimeContext(host)}
                                disabled={isBusy}
                              >
                                <Link2 size={13} />
                              </button>
                            )}
                          </>
                        )}
                        <button
                          type="button"
                          className="icon-button"
                          style={{ width: '24px', height: '24px', padding: 0 }}
                          title="Copy runtime info"
                          onClick={() => copyRuntimeInfo(host)}
                        >
                          <Copy size={13} />
                        </button>
                      </div>
                    </div>
                  );
                })}
              </div>
            ) : (
              <div style={{ color: 'var(--text-tertiary)', fontSize: '12px', padding: '8px 0' }}>No local runtimes discovered.</div>
            )}
          </div>
        )}
      </section>

      {lifecycleConfirm && (
        <ConfirmModal
          title={`${lifecycleConfirm.action === 'stop' ? 'Stop' : 'Restart'} "${lifecycleConfirm.host.instance_name}"?`}
          message={
            lifecycleConfirm.action === 'stop'
              ? `This stops the ${lifecycleConfirm.host.provider} instance "${lifecycleConfirm.host.instance_name}". Containers and any workload running in it will be shut down.`
              : `This stops and restarts the ${lifecycleConfirm.host.provider} instance "${lifecycleConfirm.host.instance_name}". Containers and any workload running in it will be interrupted.`
          }
          confirmLabel={lifecycleConfirm.action === 'stop' ? 'Stop' : 'Restart'}
          cancelLabel="Cancel"
          isDanger={lifecycleConfirm.action === 'stop'}
          busy={busyInstanceKeys.has(instanceKey(lifecycleConfirm.host))}
          onConfirm={async () => {
            const { action, host } = lifecycleConfirm;
            await runLifecycleAction(host, action);
            setLifecycleConfirm(null);
          }}
          onCancel={() => setLifecycleConfirm(null)}
        />
      )}

      {removingCa && (
        <ConfirmModal
          title={`Remove local trust for "${removingCa.subjectCn || removingCa.secretRef}"?`}
          message={`This removes the CA from your login keychain for profile "${removingCa.profileName}". Safari/Chrome will warn on its hosts again until you re-trust the CA from that profile's endpoint scan.`}
          confirmLabel="Remove"
          cancelLabel="Cancel"
          isDanger={true}
          onConfirm={async () => {
            try {
              await api.removeCa(removingCa.profileId, removingCa.secretRef);
              setRemovingCa(null);
              onStatusMessage({
                type: 'success',
                title: 'CA Trust Removed',
                details: [`${removingCa.subjectCn || removingCa.secretRef} removed from profile "${removingCa.profileName}".`],
                time: new Date().toLocaleTimeString(),
              });
              reload();
              onCaRemoved?.(removingCa.profileId);
            } catch (err) {
              onStatusMessage({
                type: 'error',
                title: 'CA Remove failed',
                details: [String(err)],
                time: new Date().toLocaleTimeString(),
              });
            }
          }}
          onCancel={() => setRemovingCa(null)}
        />
      )}
    </div>
  );
}
