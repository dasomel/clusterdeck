import { useEffect, useMemo, useState } from 'react';
import { Archive, Boxes, CheckCircle2, CircleAlert, Copy, ExternalLink, FilePlus, FileText, Globe, Moon, Pencil, Plus, RefreshCw, Server, Settings, Sun, Terminal, Trash2 } from 'lucide-react';
import { api, type ConnectionResult, type DiscoveredCaView, type HostsFileStatus, type Profile, type VerificationResult } from './api/tauri';
import ProfileEditor from './components/ProfileEditor';
import KubeconfigManager from './components/KubeconfigManager';
import StatusBanner, { type StatusMessage } from './components/StatusBanner';
import ConfirmModal from './components/ConfirmModal';

const EMPTY_VERIFICATION: VerificationResult = {
  ssh: false,
  kubeconfig: false,
  kubernetes: false,
  node_count: null,
  kubernetes_version: null,
  api_endpoint: null,
  last_verified: null,
};

export default function App() {
  const [profiles, setProfiles] = useState<Profile[]>([]);
  const [selectedId, setSelectedId] = useState<string | null>(null);
  const [connecting, setConnecting] = useState(false);
  const [testing, setTesting] = useState(false);
  const [backingUp, setBackingUp] = useState(false);
  const [merging, setMerging] = useState(false);
  const [syncingHosts, setSyncingHosts] = useState(false);
  const [clearingHosts, setClearingHosts] = useState(false);
  const [discoveringEndpoints, setDiscoveringEndpoints] = useState(false);
  const [caViews, setCaViews] = useState<DiscoveredCaView[]>([]);
  const [caActionTarget, setCaActionTarget] = useState<DiscoveredCaView | null>(null);
  const [caActionBusy, setCaActionBusy] = useState(false);
  const [caRemoveTarget, setCaRemoveTarget] = useState<DiscoveredCaView | null>(null);
  const [caRemoveBusy, setCaRemoveBusy] = useState(false);
  const [hostsStatus, setHostsStatus] = useState<HostsFileStatus | null>(null);
  const [lastResult, setLastResult] = useState<ConnectionResult | null>(null);
  const [statusMessage, setStatusMessage] = useState<StatusMessage | null>(null);
  const [loadError, setLoadError] = useState<string | null>(null);

  const [editorState, setEditorState] = useState<{ open: boolean; profile: Profile | null }>({
    open: false,
    profile: null,
  });
  const [kubeconfigManagerOpen, setKubeconfigManagerOpen] = useState(false);
  const [bootstrapPassword, setBootstrapPassword] = useState('');
  // SSH password for hosts using AuthMode::Password. Stays in React state only, never
  // persisted, and cleared after every Connect/Test Connection attempt (success or failure).
  const [sshPassword, setSshPassword] = useState('');
  const [profileToDelete, setProfileToDelete] = useState<Profile | null>(null);
  const [deletingProfile, setDeletingProfile] = useState(false);

  const [theme, setTheme] = useState<'light' | 'dark' | null>(() => {
    try {
      const stored = localStorage.getItem('clusterdeck-theme');
      if (stored === 'light' || stored === 'dark') {
        document.documentElement.setAttribute('data-theme', stored);
        return stored;
      }
    } catch {
      // ignore storage error
    }
    return null;
  });

  const [systemTheme] = useState<'light' | 'dark'>(() => {
    try {
      return window.matchMedia('(prefers-color-scheme: dark)').matches ? 'dark' : 'light';
    } catch {
      return 'dark';
    }
  });

  const effectiveTheme = theme ?? systemTheme;

  const toggleTheme = () => {
    const nextTheme = effectiveTheme === 'dark' ? 'light' : 'dark';
    try {
      document.documentElement.setAttribute('data-theme', nextTheme);
      localStorage.setItem('clusterdeck-theme', nextTheme);
    } catch {
      // ignore storage or DOM error
    }
    setTheme(nextTheme);
  };

  const loadProfiles = async () => {
    try {
      const loaded = await api.listProfiles();
      setProfiles(loaded);
      setSelectedId((current) => current ?? loaded[0]?.id ?? null);
      setLoadError(null);
    } catch (err) {
      setLoadError(String(err));
    }
  };

  useEffect(() => {
    loadProfiles();
  }, []);

  const loadHostsStatus = async (profileId: string) => {
    try {
      const res = await api.getHostsFileStatus(profileId);
      setHostsStatus(res);
    } catch {
      setHostsStatus(null);
    }
  };

  useEffect(() => {
    if (selectedId) {
      loadHostsStatus(selectedId);
    } else {
      setHostsStatus(null);
    }
    // Switching profiles must never carry a typed-but-unsubmitted password over to a different
    // profile's hosts: both fields are ephemeral UI state, not tied to any particular profile.
    setSshPassword('');
    setBootstrapPassword('');
  }, [selectedId]);

  const selected = useMemo(
    () => profiles.find((profile) => profile.id === selectedId) ?? null,
    [profiles, selectedId],
  );

  const needsSshPassword = useMemo(
    () => selected?.hosts.some((h) => h.auth === 'password') ?? false,
    [selected],
  );

  const connect = async () => {
    if (!selected) return;
    setConnecting(true);
    try {
      const result = await api.connectProfile(selected.id, bootstrapPassword || undefined, sshPassword || undefined);
      setLastResult(result);
      loadHostsStatus(selected.id);

      // Best-effort, same reasoning as discoverEndpoints: a CA-discovery failure must not turn
      // a successful connect into a reported error.
      try {
        setCaViews(await api.discoverClusterCas(selected.id, result.endpoints ?? []));
      } catch (caErr) {
        console.error('[ClusterDeck] discoverClusterCas failed during connect:', caErr);
        setCaViews([]);
      }

      const failedHosts = result.hosts.filter((h) => !h.reachable);
      const hasErrors = result.errors.length > 0;
      const k8sVerified = result.verification.kubernetes;
      const now = new Date().toLocaleTimeString();

      const details: string[] = [];
      if (failedHosts.length > 0) {
        failedHosts.forEach((h) => {
          const hostConfig = selected.hosts.find((host) => host.name === h.host);
          const target = hostConfig ? `${h.host} (${hostConfig.address}:${hostConfig.port})` : h.host;
          details.push(`Host ${target}: ${h.detail?.trim() || 'SSH connection failed'}`);
        });
      }
      if (result.errors.length > 0) {
        details.push(...result.errors);
      }

      if (failedHosts.length === 0 && !hasErrors && (k8sVerified || !selected.kubeconfig)) {
        const successDetails: string[] = [
          `SSH: ${result.hosts.length} host(s) reachable and config written`,
        ];
        if (selected.kubeconfig) {
          successDetails.push(
            `Kubernetes: verified (${result.verification.kubernetes_version ?? 'unknown'} at ${result.verification.api_endpoint ?? selected.kubeconfig.context})`
          );
        }
        if (result.endpoints && result.endpoints.length > 0) {
          successDetails.push(
            `Endpoints: discovered ${result.endpoints.length} external service(s) (APISIX/Ingress/Gateways)`
          );
          if (selected.manage_hosts_file) {
            successDetails.push('Hosts file: synced cluster endpoints and hosts to /etc/hosts');
          }
        }
        setStatusMessage({
          type: 'success',
          title: 'Connect & Sync completed successfully',
          details: successDetails,
          time: now,
        });
      } else {
        if (k8sVerified) {
          details.push(
            `Kubernetes: API verified (${result.verification.kubernetes_version ?? ''} at ${result.verification.api_endpoint ?? ''})`
          );
        }
        if (result.endpoints && result.endpoints.length > 0) {
          details.push(
            `Endpoints: discovered ${result.endpoints.length} external service(s) (APISIX/Ingress/Gateways)`
          );
        }
        setStatusMessage({
          type: 'warning',
          title: failedHosts.length === result.hosts.length && !k8sVerified
            ? 'Connect & Sync failed'
            : 'Connect & Sync completed with warnings',
          details: details.length > 0 ? details : undefined,
          time: now,
        });
      }
    } catch (err) {
      setStatusMessage({
        type: 'error',
        title: 'Connect & Sync error',
        details: [String(err)],
        time: new Date().toLocaleTimeString(),
      });
    } finally {
      setConnecting(false);
      setBootstrapPassword('');
      setSshPassword('');
    }
  };

  const testConnection = async () => {
    if (!selected) return;
    setTesting(true);
    try {
      const hosts = await api.probeProfileHosts(selected.id, sshPassword || undefined);
      setLastResult({
        aliases_written: lastResult?.aliases_written ?? false,
        kubeconfig: lastResult?.kubeconfig ?? null,
        verification: lastResult?.verification ?? { ...EMPTY_VERIFICATION },
        endpoints: lastResult?.endpoints ?? [],
        errors: lastResult?.errors ?? [],
        hosts,
      });

      const failedHosts = hosts.filter((h) => !h.reachable);
      const now = new Date().toLocaleTimeString();

      if (failedHosts.length === 0) {
        setStatusMessage({
          type: 'success',
          title: 'Test Connection succeeded',
          details: [`All ${hosts.length} host(s) reachable via SSH`],
          time: now,
        });
      } else {
        const details = failedHosts.map((h) => {
          const hostConfig = selected.hosts.find((host) => host.name === h.host);
          const target = hostConfig ? `${h.host} (${hostConfig.address}:${hostConfig.port})` : h.host;
          return `${target}: ${h.detail?.trim() || 'SSH connection failed'}`;
        });
        setStatusMessage({
          type: 'warning',
          title: `Test Connection: ${failedHosts.length} of ${hosts.length} host(s) unreachable`,
          details,
          time: now,
        });
      }
    } catch (err) {
      setStatusMessage({
        type: 'error',
        title: 'Test Connection error',
        details: [String(err)],
        time: new Date().toLocaleTimeString(),
      });
    } finally {
      setTesting(false);
      setSshPassword('');
    }
  };

  const discoverEndpoints = async () => {
    if (!selected) return;
    setDiscoveringEndpoints(true);
    try {
      const eps = await api.discoverClusterEndpoints(selected.id);
      setLastResult((prev) => {
        if (!prev) {
          return {
            aliases_written: false,
            kubeconfig: null,
            verification: { ...EMPTY_VERIFICATION, kubernetes: true },
            endpoints: eps,
            errors: [],
            hosts: [],
          };
        }
        return { ...prev, endpoints: eps };
      });
      // Best-effort: CA status is a nice-to-have overlay on top of endpoint discovery, so a
      // failure here (e.g. RBAC denies reading Secrets) must not turn a successful endpoint
      // scan into a reported error -- it just means the CA summary stays empty.
      try {
        setCaViews(await api.discoverClusterCas(selected.id, eps));
      } catch (caErr) {
        console.error('[ClusterDeck] discoverClusterCas failed during endpoint scan:', caErr);
        setCaViews([]);
      }
      setStatusMessage({
        type: 'success',
        title: 'Endpoints Discovered',
        details: [`Found ${eps.length} external cluster endpoint(s) (APISIX, Ingress, Gateways)`],
        time: new Date().toLocaleTimeString(),
      });
    } catch (err) {
      setStatusMessage({
        type: 'error',
        title: 'Endpoint Discovery failed',
        details: [String(err)],
        time: new Date().toLocaleTimeString(),
      });
    } finally {
      setDiscoveringEndpoints(false);
    }
  };

  const executeCaTrustAction = async (target: DiscoveredCaView) => {
    if (!selected) return;
    setCaActionBusy(true);
    try {
      if (target.status === 'rotated') {
        await api.replaceCa(selected.id, target.secret_ref);
      } else {
        await api.trustCa(selected.id, target.secret_ref);
      }
      setCaActionTarget(null);
      setStatusMessage({
        type: 'success',
        title: target.status === 'rotated' ? 'CA Trust Updated' : 'CA Trusted',
        details: [
          `${target.subject_cn || target.secret_ref} is now trusted for ${target.source_hosts.length} host(s). Safari/Chrome will stop warning on them.`,
        ],
        time: new Date().toLocaleTimeString(),
      });
      // Best-effort: this refresh is a nice-to-have UI sync after a successful mutation, so a
      // failure here must not overwrite the success message just queued above with a false
      // "CA Trust failed" -- mirrors the same pattern discoverEndpoints uses for its own
      // best-effort CA overlay.
      try {
        setCaViews(await api.discoverClusterCas(selected.id, lastResult?.endpoints ?? []));
      } catch {
        // ignore: caViews just stays stale until the next successful scan/action
      }
    } catch (err) {
      setStatusMessage({
        type: 'error',
        title: 'CA Trust failed',
        details: [String(err)],
        time: new Date().toLocaleTimeString(),
      });
    } finally {
      setCaActionBusy(false);
    }
  };

  const executeCaRemoveAction = async (target: DiscoveredCaView) => {
    if (!selected) return;
    setCaRemoveBusy(true);
    try {
      await api.removeCa(selected.id, target.secret_ref);
      setCaRemoveTarget(null);
      setStatusMessage({
        type: 'success',
        title: 'CA Trust Removed',
        details: [
          `${target.subject_cn || target.secret_ref} is no longer trusted locally. You can re-trust it later if the cluster is rebuilt with a new CA.`,
        ],
        time: new Date().toLocaleTimeString(),
      });
      try {
        setCaViews(await api.discoverClusterCas(selected.id, lastResult?.endpoints ?? []));
      } catch {
        // best-effort refresh only
      }
    } catch (err) {
      setStatusMessage({
        type: 'error',
        title: 'CA Remove failed',
        details: [String(err)],
        time: new Date().toLocaleTimeString(),
      });
    } finally {
      setCaRemoveBusy(false);
    }
  };

  const syncHosts = async () => {
    if (!selected) return;
    setSyncingHosts(true);
    try {
      const res = await api.syncHostsFile(selected.id);
      setLastResult((prev) => {
        if (!prev) return null;
        return { ...prev, endpoints: res.endpoints };
      });
      await loadHostsStatus(selected.id);
      setStatusMessage({
        type: 'success',
        title: 'Hosts File Synced',
        details: [res.message],
        time: new Date().toLocaleTimeString(),
      });
    } catch (err) {
      setStatusMessage({
        type: 'error',
        title: 'Sync Hosts failed',
        details: [String(err)],
        time: new Date().toLocaleTimeString(),
      });
    } finally {
      setSyncingHosts(false);
    }
  };

  const clearHosts = async () => {
    if (!selected) return;
    setClearingHosts(true);
    try {
      const res = await api.removeHostsFile(selected.id);
      await loadHostsStatus(selected.id);
      setStatusMessage({
        type: 'success',
        title: 'Cleared from /etc/hosts',
        details: [res.message],
        time: new Date().toLocaleTimeString(),
      });
    } catch (err) {
      setStatusMessage({
        type: 'error',
        title: 'Clear /etc/hosts failed',
        details: [String(err)],
        time: new Date().toLocaleTimeString(),
      });
    } finally {
      setClearingHosts(false);
    }
  };

  const openSshSession = async (hostName: string) => {
    if (!selected) return;
    try {
      await api.openSshSession(selected.id, hostName);
    } catch (err) {
      setStatusMessage({
        type: 'error',
        title: 'Failed to open SSH session',
        details: [String(err)],
        time: new Date().toLocaleTimeString(),
      });
    }
  };

  const openUrl = async (url: string) => {
    try {
      await api.openUrlInBrowser(url);
    } catch (err) {
      setStatusMessage({
        type: 'error',
        title: 'Failed to open browser',
        details: [String(err)],
        time: new Date().toLocaleTimeString(),
      });
    }
  };

  const backupKubeconfig = async () => {
    setBackingUp(true);
    try {
      const res = await api.backupKubeconfig(true);
      const now = new Date().toLocaleTimeString();
      if (res.backed_up) {
        setStatusMessage({
          type: 'success',
          title: 'Kubeconfig moved to ~/.kube/bak',
          details: [res.message],
          time: now,
        });
      } else {
        setStatusMessage({
          type: 'warning',
          title: 'No ~/.kube/config to backup',
          details: [res.message],
          time: now,
        });
      }
    } catch (err) {
      setStatusMessage({
        type: 'error',
        title: 'Backup failed',
        details: [String(err)],
        time: new Date().toLocaleTimeString(),
      });
    } finally {
      setBackingUp(false);
    }
  };

  const mergeKubeconfig = async () => {
    if (!selected) return;
    setMerging(true);
    try {
      const res = await api.mergeKubeconfigToSystem(selected.id, true);
      const now = new Date().toLocaleTimeString();
      const details = [res.message];
      if (res.backup?.backup_path) {
        details.push(`Safety backup created: ${res.backup.backup_path}`);
      }
      setStatusMessage({
        type: 'success',
        title: 'Added to ~/.kube/config',
        details,
        time: now,
      });
      const updatedStatus = await api.getProfileStatus(selected.id);
      if (updatedStatus) {
        setLastResult((prev) =>
          prev
            ? { ...prev, verification: updatedStatus }
            : {
                hosts: selected.hosts.map((h) => ({ host: h.name, reachable: false, detail: '' })),
                aliases_written: false,
                kubeconfig: null,
                verification: updatedStatus,
                endpoints: [],
                errors: [],
              },
        );
      }
    } catch (err) {
      setStatusMessage({
        type: 'error',
        title: 'Merge to ~/.kube/config failed',
        details: [String(err)],
        time: new Date().toLocaleTimeString(),
      });
    } finally {
      setMerging(false);
    }
  };

  const executeDeleteProfile = async (profile: Profile) => {
    setDeletingProfile(true);
    try {
      await api.deleteProfile(profile.id);
      const remaining = profiles.filter((p) => p.id !== profile.id);
      setProfiles(remaining);
      if (selectedId === profile.id) {
        setSelectedId(remaining[0]?.id ?? null);
        setLastResult(null);
        setCaViews([]);
        setCaActionTarget(null);
        setCaRemoveTarget(null);
      }
      setStatusMessage({
        type: 'success',
        title: `Profile "${profile.name}" deleted`,
        time: new Date().toLocaleTimeString(),
      });
      setProfileToDelete(null);
      if (editorState.open && editorState.profile?.id === profile.id) {
        setEditorState({ open: false, profile: null });
      }
    } catch (err) {
      setStatusMessage({
        type: 'error',
        title: 'Failed to delete profile',
        details: [String(err)],
        time: new Date().toLocaleTimeString(),
      });
    } finally {
      setDeletingProfile(false);
    }
  };

  const refresh = () => {
    setStatusMessage(null);
    loadProfiles();
    if (selectedId) {
      loadHostsStatus(selectedId);
    }
  };

  return (
    <div className="app-shell">
      <aside className="sidebar">
        <div className="brand">
          <div className="brand-icon"><Boxes size={18} /></div>
          <div>
            <div className="brand-name">ClusterDeck</div>
            <div className="brand-subtitle">macOS cluster access</div>
          </div>
        </div>

        <div className="section-label">PROFILES</div>
        {profiles.length === 0 && !loadError ? (
          <p className="profile-meta" style={{ padding: '0 8px' }}>
            No profiles yet. Add one to ~/.clusterdeck/profiles.yaml.
          </p>
        ) : (
          <div className="profile-list">
            {profiles.map((profile) => {
              const isSelected = profile.id === selectedId;
              const healthyHosts = isSelected && lastResult
                ? profile.hosts.filter((host) => lastResult.hosts.find((h) => h.host === host.name)?.reachable).length
                : 0;
              return (
                <div
                  key={profile.id}
                  role="button"
                  tabIndex={0}
                  className={`profile-card ${isSelected ? 'selected' : ''}`}
                  onClick={() => {
                    setSelectedId(profile.id);
                    setLastResult(null);
                    setCaViews([]);
                    setCaActionTarget(null);
                    setCaRemoveTarget(null);
                    setStatusMessage(null);
                    setEditorState({ open: false, profile: null });
                  }}
                  onKeyDown={(e) => {
                    if (e.key === 'Enter' || e.key === ' ') {
                      setSelectedId(profile.id);
                      setLastResult(null);
                      setCaViews([]);
                      setCaActionTarget(null);
                      setCaRemoveTarget(null);
                      setStatusMessage(null);
                      setEditorState({ open: false, profile: null });
                    }
                  }}
                >
                  <div className="profile-card-top">
                    <span className="profile-name">{profile.name}</span>
                    <div style={{ display: 'flex', alignItems: 'center', gap: '6px' }}>
                      <button
                        type="button"
                        className="icon-button"
                        style={{ width: '22px', height: '22px', padding: 0 }}
                        title="Edit profile"
                        onClick={(e) => {
                          e.stopPropagation();
                          setEditorState({ open: true, profile });
                        }}
                      >
                        <Pencil size={13} />
                      </button>
                      <button
                        type="button"
                        className="icon-button"
                        style={{ width: '22px', height: '22px', padding: 0, color: 'var(--danger)' }}
                        title="Delete profile"
                        onClick={(e) => {
                          e.stopPropagation();
                          setProfileToDelete(profile);
                        }}
                      >
                        <Trash2 size={13} />
                      </button>
                      {healthyHosts === profile.hosts.length ? (
                        <CheckCircle2 className="status-ok" size={16} />
                      ) : (
                        <CircleAlert className="status-warn" size={16} />
                      )}
                    </div>
                  </div>
                  <div className="profile-meta">
                    {profile.hosts.length} hosts · {profile.bastion ? 'Bastion' : 'Direct'}
                  </div>
                </div>
              );
            })}
          </div>
        )}

        <button
          className="secondary-button"
          onClick={() => setEditorState({ open: true, profile: null })}
        >
          <Plus size={16} /> Add profile
        </button>
      </aside>

      <main className="main-panel">
        {loadError && <div className="pill warning" style={{ marginBottom: '16px' }}>{loadError}</div>}
        <header className="header">
          <div>
            <div className="eyebrow">ENVIRONMENT</div>
            <h1>{selected?.name ?? 'Cluster'}</h1>
          </div>
          <div className="header-actions">
            <button
              className="icon-button"
              title="Kubeconfig Manager"
              onClick={() => {
                setKubeconfigManagerOpen(!kubeconfigManagerOpen);
                setEditorState({ open: false, profile: null });
              }}
            >
              <Settings size={17} />
            </button>
            <button
              className="icon-button"
              title={effectiveTheme === 'dark' ? 'Switch to light theme' : 'Switch to dark theme'}
              onClick={toggleTheme}
            >
              {effectiveTheme === 'dark' ? <Sun size={17} /> : <Moon size={17} />}
            </button>
            <button className="icon-button" title="Refresh" onClick={refresh}>
              <RefreshCw size={17} />
            </button>
          </div>
        </header>

        {/* Action results arrive asynchronously; keep them visible when another view is open. */}
        {(kubeconfigManagerOpen || editorState.open) && statusMessage && (
          <StatusBanner message={statusMessage} onDismiss={() => setStatusMessage(null)} />
        )}

        {kubeconfigManagerOpen ? (
          <KubeconfigManager
            onClose={() => setKubeconfigManagerOpen(false)}
            onStatusMessage={setStatusMessage}
            onCaRemoved={(profileId) => {
              if (selected?.id === profileId) {
                api
                  .discoverClusterCas(profileId, lastResult?.endpoints ?? [])
                  .then(setCaViews)
                  .catch(() => setCaViews([]));
              }
            }}
          />
        ) : editorState.open ? (
          <ProfileEditor
            initial={editorState.profile}
            onClose={() => setEditorState({ open: false, profile: null })}
            onSaved={() => loadProfiles()}
            onDeleteRequest={(p) => setProfileToDelete(p)}
          />
        ) : (
          <>
            <section className="hero-card">
              <div>
                <div className="eyebrow">READY TO CONNECT</div>
                <h2>Bring the cluster to your local workstation.</h2>
                <p>Discover hosts, bootstrap SSH, fetch kubeconfig, and verify Kubernetes access from one profile.</p>
              </div>
              <div style={{ display: 'flex', flexDirection: 'column', gap: '6px', alignItems: 'flex-end', flexShrink: 0 }}>
                {needsSshPassword && (
                  <div style={{ display: 'flex', flexDirection: 'column', gap: '3px', width: '180px' }}>
                    <label className="form-label" style={{ fontSize: '10px' }}>
                      SSH Password
                    </label>
                    <input
                      type="password"
                      placeholder="Enter SSH password"
                      value={sshPassword}
                      onChange={(e) => setSshPassword(e.target.value)}
                      className="form-input mono"
                      style={{ padding: '5px 8px', fontSize: '11px' }}
                      autoComplete="off"
                    />
                  </div>
                )}
                {selected?.bootstrap.enabled && (
                  <div style={{ display: 'flex', flexDirection: 'column', gap: '3px', width: '180px' }}>
                    <label className="form-label" style={{ fontSize: '10px' }}>
                      SSH Bootstrap Password
                    </label>
                    <input
                      type="password"
                      placeholder="Enter SSH password"
                      value={bootstrapPassword}
                      onChange={(e) => setBootstrapPassword(e.target.value)}
                      className="form-input mono"
                      style={{ padding: '5px 8px', fontSize: '11px' }}
                      autoComplete="off"
                    />
                  </div>
                )}
                <div style={{ display: 'flex', gap: '6px' }}>
                  <button
                    className="secondary-button"
                    style={{ width: 'auto', marginTop: 0, padding: '6px 11px', fontSize: '12px', gap: '6px' }}
                    onClick={testConnection}
                    disabled={testing || connecting || !selected}
                  >
                    {testing ? <RefreshCw size={13} className="spin" /> : <Terminal size={13} />}
                    {testing ? 'Testing…' : 'Test Connection'}
                  </button>
                  <button
                    className="primary-button"
                    style={{ width: 'auto', marginTop: 0, padding: '6px 13px', fontSize: '12px', gap: '6px' }}
                    onClick={connect}
                    disabled={connecting || !selected}
                  >
                    {connecting ? <RefreshCw size={13} className="spin" /> : <Terminal size={13} />}
                    {connecting ? 'Connecting…' : 'Connect / Sync'}
                  </button>
                </div>
              </div>
            </section>

            {statusMessage && (
              <StatusBanner message={statusMessage} onDismiss={() => setStatusMessage(null)} />
            )}

            <section className="grid-two">
              <div className="panel-card">
                <div className="panel-title"><Server size={16} /> Hosts</div>
                <div className="host-list">
                  {selected?.hosts.map((host) => {
                    const hostResult = lastResult?.hosts.find((h) => h.host === host.name);
                    const reachable = hostResult?.reachable ?? false;
                    const portSuffix = host.port !== 22 ? `:${host.port}` : '';
                    return (
                      <div className="host-row" key={host.name}>
                        <div>
                          <div className="host-name">{host.name}</div>
                          <div className="host-address">{host.address}{portSuffix}</div>
                          {hostResult && !reachable && hostResult.detail && (
                            <div className="host-error-detail mono">{hostResult.detail.trim()}</div>
                          )}
                        </div>
                        <div style={{ display: 'flex', alignItems: 'center', gap: '8px' }}>
                          <button
                            type="button"
                            className="icon-button"
                            style={{ width: '26px', height: '26px', padding: 0 }}
                            title="Open SSH session"
                            onClick={() => openSshSession(host.name)}
                          >
                            <Terminal size={14} />
                          </button>
                          <span className={`pill ${reachable ? 'success' : 'warning'}`}>
                            {reachable ? 'SSH reachable' : 'Needs retry'}
                          </span>
                        </div>
                      </div>
                    );
                  })}
                </div>
              </div>

              <div className="panel-card">
                <div className="panel-title"><Boxes size={16} /> Kubernetes</div>
                <div className="status-stack">
                  <div className="status-row"><span>SSH</span><strong>{lastResult?.verification.ssh ? 'Ready' : '—'}</strong></div>
                  <div className="status-row"><span>Kubeconfig</span><strong>{lastResult?.verification.kubeconfig ? 'Synced' : '—'}</strong></div>
                  <div className="status-row"><span>Context</span><strong className="mono">{selected?.kubeconfig?.context ?? '—'}</strong></div>
                  <div className="status-row"><span>API</span><strong>{lastResult?.verification.kubernetes ? 'Verified' : '—'}</strong></div>
                  <div className="status-row"><span>Version</span><strong>{lastResult?.verification.kubernetes_version ?? '—'}</strong></div>
                  <div className="status-row"><span>/etc/hosts</span><strong>{hostsStatus?.is_synced ? 'Synced' : (selected?.manage_hosts_file ? 'Auto-sync' : 'Manual')}</strong></div>
                  <div className="status-row"><span>Endpoint</span><strong className="mono">{lastResult?.verification.api_endpoint ?? '—'}</strong></div>
                </div>

                <div style={{ marginTop: '14px', paddingTop: '12px', borderTop: '1px solid var(--border)' }}>
                  <div style={{ fontSize: '11px', color: 'var(--text-secondary)', marginBottom: '8px' }}>
                    ~/.kube/config Integration
                  </div>
                  <div style={{ display: 'flex', gap: '8px', flexWrap: 'wrap' }}>
                    <button
                      type="button"
                      className="secondary-button"
                      style={{ width: 'auto', marginTop: 0, padding: '6px 12px', fontSize: '12px' }}
                      title="Move existing ~/.kube/config to ~/.kube/bak/"
                      onClick={backupKubeconfig}
                      disabled={backingUp || merging}
                    >
                      {backingUp ? <RefreshCw size={14} className="spin" /> : <Archive size={14} />}
                      {backingUp ? 'Backing up…' : 'Backup to ~/.kube/bak'}
                    </button>
                    <button
                      type="button"
                      className="primary-button"
                      style={{ width: 'auto', marginTop: 0, padding: '6px 12px', fontSize: '12px' }}
                      title="Add profile kubeconfig to ~/.kube/config"
                      onClick={mergeKubeconfig}
                      disabled={merging || backingUp || !selected}
                    >
                      {merging ? <RefreshCw size={14} className="spin" /> : <FilePlus size={14} />}
                      {merging ? 'Merging…' : 'Merge to ~/.kube/config'}
                    </button>
                  </div>
                </div>
              </div>
            </section>

            <section className="panel-card" style={{ marginTop: '16px' }}>
              <div style={{ display: 'flex', justifyContent: 'space-between', alignItems: 'flex-start', marginBottom: '14px', flexWrap: 'wrap', gap: '10px' }}>
                <div>
                  <div className="panel-title" style={{ margin: 0, display: 'flex', alignItems: 'center', gap: '8px', flexWrap: 'wrap' }}>
                    <FileText size={16} /> Hosts File (/etc/hosts) & Endpoints
                    <span className={`pill ${hostsStatus?.is_synced ? 'success' : 'warning'}`} style={{ fontSize: '10px' }}>
                      {hostsStatus?.is_synced ? '/etc/hosts Synced' : 'Not in /etc/hosts'}
                    </span>
                    {selected?.manage_hosts_file ? (
                      <span className="pill success" style={{ fontSize: '10px' }} title="Profile has auto-sync enabled on Connect">
                        Auto-sync: On
                      </span>
                    ) : (
                      <span className="pill" style={{ fontSize: '10px', opacity: 0.75 }} title="Enable in Edit Profile to auto-sync on connect">
                        Auto-sync: Off
                      </span>
                    )}
                  </div>
                  <p style={{ margin: '4px 0 0', fontSize: '11.5px', color: 'var(--text-secondary)' }}>
                    Maps cluster node aliases (*.{selected?.id}.clusterdeck.local) and Ingress / APISIX endpoints into macOS <code>/etc/hosts</code>.
                  </p>
                </div>
                <div style={{ display: 'flex', gap: '8px', flexWrap: 'wrap' }}>
                  <button
                    type="button"
                    className="secondary-button"
                    style={{ width: 'auto', marginTop: 0, padding: '5px 10px', fontSize: '11px', gap: '5px' }}
                    onClick={discoverEndpoints}
                    disabled={discoveringEndpoints || !selected}
                    title="Scan cluster for Ingresses, ApisixRoutes, and Gateways"
                  >
                    {discoveringEndpoints ? <RefreshCw size={12} className="spin" /> : <RefreshCw size={12} />}
                    Scan Endpoints
                  </button>
                  {hostsStatus?.is_synced && (
                    <button
                      type="button"
                      className="secondary-button"
                      style={{ width: 'auto', marginTop: 0, padding: '5px 10px', fontSize: '11px', gap: '5px', color: 'var(--danger)' }}
                      onClick={clearHosts}
                      disabled={clearingHosts || !selected}
                      title="Remove this profile's block from /etc/hosts"
                    >
                      {clearingHosts ? <RefreshCw size={12} className="spin" /> : <Trash2 size={12} />}
                      Clear /etc/hosts
                    </button>
                  )}
                  <button
                    type="button"
                    className="primary-button"
                    style={{ width: 'auto', marginTop: 0, padding: '5px 10px', fontSize: '11px', gap: '5px' }}
                    onClick={syncHosts}
                    disabled={syncingHosts || !selected}
                    title="Write cluster endpoints and profile hosts to /etc/hosts (requires admin approval)"
                  >
                    {syncingHosts ? <RefreshCw size={12} className="spin" /> : <FileText size={12} />}
                    Sync to /etc/hosts
                  </button>
                </div>
              </div>

              {/* Node Hostnames Section */}
              <div style={{ marginTop: '12px', paddingBottom: '12px', borderBottom: '1px solid var(--border)' }}>
                <div style={{ fontSize: '11px', fontWeight: 700, color: 'var(--text-tertiary)', letterSpacing: '0.05em', textTransform: 'uppercase', marginBottom: '6px' }}>
                  Node Host Aliases (*.{selected?.id}.clusterdeck.local)
                </div>
                <div className="host-list">
                  {selected?.hosts.map((host) => {
                    const mappedFqdn = `${host.name}.${selected.id}.clusterdeck.local`;
                    return (
                      <div className="host-row" key={host.name}>
                        <div>
                          <div style={{ display: 'flex', alignItems: 'center', gap: '8px' }}>
                            <span className="host-name mono" style={{ fontSize: '12px' }}>{mappedFqdn}</span>
                            <span className="pill" style={{ fontSize: '10px', padding: '1px 6px' }}>Node</span>
                          </div>
                          <div className="host-address">
                            Target IP: <strong>{host.address}</strong> (port {host.port})
                          </div>
                        </div>
                        <div style={{ display: 'flex', alignItems: 'center', gap: '6px' }}>
                          <button
                            type="button"
                            className="icon-button"
                            style={{ width: '26px', height: '26px', padding: 0 }}
                            title={`Copy ${mappedFqdn}`}
                            onClick={() => navigator.clipboard.writeText(mappedFqdn)}
                          >
                            <Copy size={13} />
                          </button>
                          <button
                            type="button"
                            className="icon-button"
                            style={{ width: '26px', height: '26px', padding: 0 }}
                            title={`Open http://${mappedFqdn} in browser`}
                            onClick={() => openUrl(`http://${mappedFqdn}`)}
                          >
                            <ExternalLink size={13} />
                          </button>
                        </div>
                      </div>
                    );
                  })}
                  {selected?.bastion && (
                    <div className="host-row" key={selected.bastion.name}>
                      <div>
                        <div style={{ display: 'flex', alignItems: 'center', gap: '8px' }}>
                          <span className="host-name mono" style={{ fontSize: '12px' }}>{selected.bastion.name}.{selected.id}.clusterdeck.local</span>
                          <span className="pill" style={{ fontSize: '10px', padding: '1px 6px' }}>Bastion</span>
                        </div>
                        <div className="host-address">
                          Target IP: <strong>{selected.bastion.address}</strong> (port {selected.bastion.port})
                        </div>
                      </div>
                      <div style={{ display: 'flex', alignItems: 'center', gap: '6px' }}>
                        <button
                          type="button"
                          className="icon-button"
                          style={{ width: '26px', height: '26px', padding: 0 }}
                          title={`Copy ${selected.bastion.name}.${selected.id}.clusterdeck.local`}
                          onClick={() => navigator.clipboard.writeText(`${selected.bastion!.name}.${selected.id}.clusterdeck.local`)}
                        >
                          <Copy size={13} />
                        </button>
                      </div>
                    </div>
                  )}
                </div>
              </div>

              {/* Ingress / APISIX / Gateway Endpoints Section */}
              <div style={{ marginTop: '12px' }}>
                <div style={{ display: 'flex', justifyContent: 'space-between', alignItems: 'center', marginBottom: '6px' }}>
                  <div style={{ fontSize: '11px', fontWeight: 700, color: 'var(--text-tertiary)', letterSpacing: '0.05em', textTransform: 'uppercase' }}>
                    Discovered Cluster Endpoints (Ingress, APISIX, Gateway API)
                  </div>
                  {lastResult?.endpoints && lastResult.endpoints.length > 0 && (
                    <span className="pill" style={{ fontSize: '10px' }}>
                      {lastResult.endpoints.length} endpoint(s) discovered
                    </span>
                  )}
                </div>

                {caViews.length > 0 && (
                  <div className="host-list" style={{ marginBottom: '8px' }}>
                    {caViews.map((ca) => (
                      <div className="host-row" key={ca.secret_ref}>
                        <div>
                          <div style={{ display: 'flex', alignItems: 'center', gap: '8px' }}>
                            <span className="host-name mono" style={{ fontSize: '12px' }}>
                              {ca.subject_cn || ca.secret_ref}
                            </span>
                            <span
                              className="pill"
                              style={{ fontSize: '10px', padding: '1px 6px', textTransform: 'uppercase' }}
                            >
                              {ca.status === 'trusted'
                                ? 'CA Trusted'
                                : ca.status === 'rotated'
                                  ? 'CA Changed'
                                  : 'CA Untrusted'}
                            </span>
                          </div>
                          <div className="host-address">
                            {ca.source_hosts.length} host(s) &middot; expires {ca.not_after || 'unknown'}
                          </div>
                          {ca.warnings.length > 0 && (
                            <div style={{ display: 'flex', flexDirection: 'column', gap: '2px', marginTop: '4px' }}>
                              {ca.warnings.map((warning, idx) => (
                                <div
                                  key={idx}
                                  style={{ display: 'flex', alignItems: 'flex-start', gap: '4px', fontSize: '11px', color: 'var(--warning)' }}
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
                              onClick={() => setCaActionTarget(ca)}
                            >
                              {ca.status === 'rotated' ? 'Update Trust' : 'Trust CA'}
                            </button>
                          )}
                          {ca.status === 'trusted' && (
                            <button
                              type="button"
                              className="secondary-button"
                              style={{ width: 'auto', marginTop: 0, padding: '5px 10px', fontSize: '11px' }}
                              onClick={() => setCaRemoveTarget(ca)}
                            >
                              Remove
                            </button>
                          )}
                        </div>
                      </div>
                    ))}
                  </div>
                )}

                {lastResult?.endpoints && lastResult.endpoints.length > 0 ? (
                  <div className="host-list">
                    {lastResult.endpoints.map((ep) => (
                      <div className="host-row" key={`${ep.source}-${ep.host}`}>
                        <div>
                          <div style={{ display: 'flex', alignItems: 'center', gap: '8px' }}>
                            <span className="host-name mono" style={{ fontSize: '12px' }}>{ep.host}</span>
                            <span className="pill" style={{ fontSize: '10px', padding: '1px 6px', textTransform: 'uppercase' }}>
                              {ep.source}
                            </span>
                          </div>
                          <div className="host-address">
                            Target IP: <strong>{ep.ip}</strong> · Resource: {ep.resource_name}
                          </div>
                        </div>
                        <div style={{ display: 'flex', alignItems: 'center', gap: '6px' }}>
                          <button
                            type="button"
                            className="icon-button"
                            style={{ width: '26px', height: '26px', padding: 0 }}
                            title={`Copy ${ep.host}`}
                            onClick={() => navigator.clipboard.writeText(ep.host)}
                          >
                            <Copy size={13} />
                          </button>
                          <button
                            type="button"
                            className="icon-button"
                            style={{ width: '26px', height: '26px', padding: 0, display: 'inline-flex', alignItems: 'center', justifyContent: 'center' }}
                            title={`Open https://${ep.host} in browser`}
                            onClick={() => openUrl(`https://${ep.host}`)}
                          >
                            <ExternalLink size={13} />
                          </button>
                        </div>
                      </div>
                    ))}
                  </div>
                ) : (
                  <div style={{ padding: '14px', borderRadius: '8px', background: 'var(--bg-sunken)', border: '1px dashed var(--border-strong)', color: 'var(--text-secondary)', fontSize: '12px', textAlign: 'center' }}>
                    No external ingress or API gateway endpoints discovered yet. Connect to the cluster or click &quot;Scan Endpoints&quot;.
                  </div>
                )}
              </div>
            </section>
          </>
        )}
      </main>

      {profileToDelete && (
        <ConfirmModal
          title={`Delete Profile "${profileToDelete.name}"?`}
          message={`Are you sure you want to delete profile "${profileToDelete.name}" (${profileToDelete.id})? This will permanently remove its configuration, SSH alias, and locally synced kubeconfig.`}
          confirmLabel="Delete Profile"
          cancelLabel="Cancel"
          isDanger={true}
          busy={deletingProfile}
          onConfirm={() => executeDeleteProfile(profileToDelete)}
          onCancel={() => {
            if (!deletingProfile) setProfileToDelete(null);
          }}
        />
      )}

      {caActionTarget && (
        <ConfirmModal
          title={
            caActionTarget.status === 'rotated'
              ? `Update trust for "${caActionTarget.subject_cn || caActionTarget.secret_ref}"?`
              : `Trust CA "${caActionTarget.subject_cn || caActionTarget.secret_ref}"?`
          }
          message={
            caActionTarget.status === 'rotated'
              ? `This cluster's CA has changed since it was last trusted (likely a clean reinstall). Remove the old trust entry and trust the new certificate (SHA-256 ${caActionTarget.fingerprint_sha256.slice(0, 16)}..., expires ${caActionTarget.not_after || 'unknown'}) for ${caActionTarget.source_hosts.length} host(s)? macOS will ask you to confirm in a system dialog.`
              : `Add this certificate (SHA-256 ${caActionTarget.fingerprint_sha256.slice(0, 16)}..., expires ${caActionTarget.not_after || 'unknown'}) to your login keychain so Safari/Chrome stop warning on ${caActionTarget.source_hosts.length} host(s) behind it? macOS will ask you to confirm in a system dialog.`
          }
          confirmLabel={caActionTarget.status === 'rotated' ? 'Update Trust' : 'Trust CA'}
          cancelLabel="Cancel"
          isDanger={false}
          busy={caActionBusy}
          onConfirm={() => executeCaTrustAction(caActionTarget)}
          onCancel={() => {
            if (!caActionBusy) setCaActionTarget(null);
          }}
        />
      )}

      {caRemoveTarget && (
        <ConfirmModal
          title={`Remove local trust for "${caRemoveTarget.subject_cn || caRemoveTarget.secret_ref}"?`}
          message={`This removes the CA from your login keychain and stops ClusterDeck from tracking it as trusted for ${caRemoveTarget.source_hosts.length} host(s). Safari/Chrome will warn on these hosts again until you re-trust the CA. macOS will ask you to confirm in a system dialog.`}
          confirmLabel="Remove"
          cancelLabel="Cancel"
          isDanger={true}
          busy={caRemoveBusy}
          onConfirm={() => executeCaRemoveAction(caRemoveTarget)}
          onCancel={() => {
            if (!caRemoveBusy) setCaRemoveTarget(null);
          }}
        />
      )}
    </div>
  );
}
