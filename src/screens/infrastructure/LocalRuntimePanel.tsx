import { useState, useEffect, useCallback } from 'react';
import { Server, Play, Square, RotateCw, Terminal, Link2, Copy, ChevronDown, ChevronRight, RefreshCw } from 'lucide-react';
import { api, type DiscoveredLocalHost, type LocalRuntimeLifecycleProvider } from '../../api/tauri';
import { useStatus } from '../../state/StatusContext';
import { gib } from '../../lib/format';
import Pill from '../../components/ui/Pill';
import Tag from '../../components/ui/Tag';
import ConfirmModal from '../../components/ConfirmModal';

export default function LocalRuntimePanel({ isVisible = true }: { isVisible?: boolean }) {
  const { pushStatus } = useStatus();
  const [localHosts, setLocalHosts] = useState<DiscoveredLocalHost[]>([]);
  const [loading, setLoading] = useState(false);
  const [expanded, setExpanded] = useState(true);
  const [busyInstanceKeys, setBusyInstanceKeys] = useState<Set<string>>(new Set());
  const [lifecycleConfirm, setLifecycleConfirm] = useState<{
    action: 'stop' | 'restart';
    host: DiscoveredLocalHost;
  } | null>(null);

  const reload = useCallback(async () => {
    setLoading(true);
    try {
      const hosts = await api.detectLocalHosts();
      setLocalHosts(hosts);
    } catch {
      setLocalHosts([]);
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    if (!isVisible) return;
    reload();
  }, [isVisible, reload]);

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
        action === 'start'
          ? api.startLocalRuntime
          : action === 'stop'
            ? api.stopLocalRuntime
            : api.restartLocalRuntime;
      const result = await call(provider, host.instance_name);
      pushStatus(
        result.success ? 'success' : 'error',
        `${host.instance_name}: ${action} ${result.success ? 'succeeded' : 'failed'}`,
        result.success ? undefined : [result.message],
      );
      await reload();
    } catch (err) {
      pushStatus('error', `Failed to ${action} ${host.instance_name}`, [String(err)]);
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
      pushStatus('error', `Failed to open shell for ${host.instance_name}`, [String(err)]);
    }
  };

  const openLocalRuntimeContext = async (host: DiscoveredLocalHost) => {
    const provider = toLifecycleProvider(host.provider);
    if (!provider) return;
    try {
      await api.openLocalRuntimeContext(provider, host.instance_name);
    } catch (err) {
      pushStatus('error', `Failed to open runtime context for ${host.instance_name}`, [String(err)]);
    }
  };

  const copyRuntimeInfo = async (host: DiscoveredLocalHost) => {
    const lines = [
      `Provider: ${host.provider}`,
      `Name: ${host.instance_name}`,
      `Status: ${host.status}`,
      `Arch: ${host.arch ?? '—'}`,
      `CPU: ${host.cpus ?? '—'}`,
      `Memory: ${gib(host.memory_bytes)}`,
      `Disk: ${gib(host.disk_bytes)}`,
      `Runtime: ${host.runtime ?? '—'}`,
      `Address: ${host.address}:${host.port}`,
      `Docker context: ${host.docker_context ?? '—'}`,
      `Kube context: ${host.kube_context ?? '—'}`,
    ];
    try {
      await navigator.clipboard.writeText(lines.join('\n'));
      pushStatus('success', `Copied ${host.instance_name} info`);
    } catch (err) {
      pushStatus('error', 'Failed to copy to clipboard', [String(err)]);
    }
  };

  return (
    <div className="panel-card" style={{ padding: '14px 16px' }}>
      <div style={{ display: 'flex', justifyContent: 'space-between', alignItems: 'center' }}>
        <button
          type="button"
          style={{ display: 'flex', alignItems: 'center', gap: '8px', cursor: 'pointer', background: 'none', border: 'none', padding: 0, color: 'inherit', textAlign: 'left' }}
          onClick={() => setExpanded(!expanded)}
          aria-expanded={expanded}
        >
          {expanded ? <ChevronDown size={14} /> : <ChevronRight size={14} />}
          <Server size={14} />
          <span style={{ fontSize: '13px', fontWeight: 600 }}>Local Runtimes ({localHosts.length})</span>
        </button>

        <button
          type="button"
          className="icon-button"
          style={{ width: '24px', height: '24px' }}
          title="Refresh local runtimes"
          aria-label="Refresh local runtimes"
          onClick={reload}
          disabled={loading}
        >
          <RefreshCw size={13} className={loading ? 'spin' : ''} />
        </button>
      </div>

      {expanded && (
        <div style={{ marginTop: '10px' }}>
          <div style={{ fontSize: '11px', color: 'var(--text-tertiary)', marginBottom: '10px' }}>
            Discovery for Colima, Lima, and Vagrant. Colima/Lima instances support Start/Stop/Restart, VM shell, and Docker/kube context access.
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
                        <Pill variant={isRunning ? 'ok' : 'idle'}>{host.status}</Pill>
                        <Tag>{host.provider}</Tag>
                      </div>
                      <div className="host-address">
                        {host.arch ?? 'arch unknown'} · {host.cpus == null ? 'CPU —' : `${host.cpus} CPU`} · {gib(host.memory_bytes)} memory · {gib(host.disk_bytes)} disk
                      </div>
                      <div className="host-address">
                        Runtime: {host.runtime ?? '—'} · Docker: {host.docker_context ?? '—'} · Kube: {host.kube_context ?? '—'}
                      </div>
                    </div>

                    <div style={{ display: 'flex', gap: '4px', flexShrink: 0, marginLeft: '8px' }}>
                      {lifecycleProvider && !isRunning && (
                        <button
                          type="button"
                          className="icon-button"
                          style={{ width: '26px', height: '26px' }}
                          title="Start instance"
                          aria-label="Start instance"
                          onClick={() => runLifecycleAction(host, 'start')}
                          disabled={isBusy}
                        >
                          <Play size={13} />
                        </button>
                      )}

                      {lifecycleProvider && isRunning && (
                        <>
                          <button
                            type="button"
                            className="icon-button"
                            style={{ width: '26px', height: '26px', color: 'var(--danger)' }}
                            title="Stop instance"
                            aria-label="Stop instance"
                            onClick={() => setLifecycleConfirm({ action: 'stop', host })}
                            disabled={isBusy}
                          >
                            <Square size={13} />
                          </button>
                          <button
                            type="button"
                            className="icon-button"
                            style={{ width: '26px', height: '26px' }}
                            title="Restart instance"
                            aria-label="Restart instance"
                            onClick={() => setLifecycleConfirm({ action: 'restart', host })}
                            disabled={isBusy}
                          >
                            <RotateCw size={13} />
                          </button>
                          <button
                            type="button"
                            className="icon-button"
                            style={{ width: '26px', height: '26px' }}
                            title="Open VM shell"
                            aria-label="Open VM shell"
                            onClick={() => openLocalRuntimeShell(host)}
                            disabled={isBusy}
                          >
                            <Terminal size={13} />
                          </button>
                          {hasContext && (
                            <button
                              type="button"
                              className="icon-button"
                              style={{ width: '26px', height: '26px' }}
                              title="Open in runtime context"
                              aria-label="Open in runtime context"
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
                        style={{ width: '26px', height: '26px' }}
                        title="Copy runtime info"
                        aria-label="Copy runtime info"
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
            <div style={{ color: 'var(--text-tertiary)', fontSize: '12px', padding: '12px 0' }}>
              No local runtimes discovered on this Mac.
            </div>
          )}
        </div>
      )}

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
    </div>
  );
}
