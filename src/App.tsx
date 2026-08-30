import { useEffect, useMemo, useState } from 'react';
import { Boxes, CheckCircle2, CircleAlert, Moon, Pencil, Plus, RefreshCw, Server, Sun, Terminal } from 'lucide-react';
import { api, type ConnectionResult, type Profile } from './api/tauri';
import ProfileEditor from './components/ProfileEditor';

export default function App() {
  const [profiles, setProfiles] = useState<Profile[]>([]);
  const [selectedId, setSelectedId] = useState<string | null>(null);
  const [connecting, setConnecting] = useState(false);
  const [testing, setTesting] = useState(false);
  const [lastResult, setLastResult] = useState<ConnectionResult | null>(null);
  const [loadError, setLoadError] = useState<string | null>(null);

  const [editorState, setEditorState] = useState<{ open: boolean; profile: Profile | null }>({
    open: false,
    profile: null,
  });
  const [bootstrapPassword, setBootstrapPassword] = useState('');

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

  const selected = useMemo(
    () => profiles.find((profile) => profile.id === selectedId) ?? null,
    [profiles, selectedId],
  );

  const connect = async () => {
    if (!selected) return;
    setConnecting(true);
    try {
      const result = await api.connectProfile(selected.id, bootstrapPassword || undefined);
      setLastResult(result);
    } catch (err) {
      setLoadError(String(err));
    } finally {
      setConnecting(false);
      setBootstrapPassword('');
    }
  };

  const testConnection = async () => {
    if (!selected) return;
    setTesting(true);
    try {
      const hosts = await api.probeProfileHosts(selected.id);
      setLastResult({
        aliases_written: lastResult?.aliases_written ?? false,
        kubeconfig: lastResult?.kubeconfig ?? null,
        verification: lastResult?.verification ?? {
          ssh: false,
          kubeconfig: false,
          kubernetes: false,
          node_count: null,
          kubernetes_version: null,
          api_endpoint: null,
          last_verified: null,
        },
        errors: lastResult?.errors ?? [],
        hosts,
      });
    } catch (err) {
      setLoadError(String(err));
    } finally {
      setTesting(false);
    }
  };

  const openSshSession = async (hostName: string) => {
    if (!selected) return;
    try {
      await api.openSshSession(selected.id, hostName);
    } catch (err) {
      setLoadError(String(err));
    }
  };

  const refresh = () => loadProfiles();

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
                    setEditorState({ open: false, profile: null });
                  }}
                  onKeyDown={(e) => {
                    if (e.key === 'Enter' || e.key === ' ') {
                      setSelectedId(profile.id);
                      setLastResult(null);
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
                        <Pencil size={14} />
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

        {editorState.open ? (
          <ProfileEditor
            initial={editorState.profile}
            onClose={() => setEditorState({ open: false, profile: null })}
            onSaved={() => loadProfiles()}
          />
        ) : (
          <>
            <section className="hero-card">
              <div>
                <div className="eyebrow">READY TO CONNECT</div>
                <h2>Bring the cluster to your local workstation.</h2>
                <p>Discover hosts, bootstrap SSH, fetch kubeconfig, and verify Kubernetes access from one profile.</p>
              </div>
              <div style={{ display: 'flex', flexDirection: 'column', gap: '8px', alignItems: 'flex-end' }}>
                {selected?.bootstrap.enabled && (
                  <div style={{ display: 'flex', flexDirection: 'column', gap: '4px', width: '200px' }}>
                    <label className="form-label" style={{ fontSize: '11px' }}>
                      SSH Bootstrap Password
                    </label>
                    <input
                      type="password"
                      placeholder="Enter SSH password"
                      value={bootstrapPassword}
                      onChange={(e) => setBootstrapPassword(e.target.value)}
                      className="form-input mono"
                    />
                  </div>
                )}
                <div style={{ display: 'flex', gap: '8px' }}>
                  <button
                    className="secondary-button"
                    style={{ width: 'auto', marginTop: 0 }}
                    onClick={testConnection}
                    disabled={testing || connecting || !selected}
                  >
                    {testing ? <RefreshCw size={16} className="spin" /> : <Terminal size={16} />}
                    {testing ? 'Testing…' : 'Test Connection'}
                  </button>
                  <button className="primary-button" onClick={connect} disabled={connecting || !selected}>
                    {connecting ? <RefreshCw size={16} className="spin" /> : <Terminal size={16} />}
                    {connecting ? 'Connecting…' : 'Connect / Sync'}
                  </button>
                </div>
              </div>
            </section>

            <section className="grid-two">
              <div className="panel-card">
                <div className="panel-title"><Server size={16} /> Hosts</div>
                <div className="host-list">
                  {selected?.hosts.map((host) => {
                    const reachable = lastResult?.hosts.find((h) => h.host === host.name)?.reachable ?? false;
                    return (
                      <div className="host-row" key={host.name}>
                        <div>
                          <div className="host-name">{host.name}</div>
                          <div className="host-address">{host.address}</div>
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
                  <div className="status-row"><span>Endpoint</span><strong className="mono">{lastResult?.verification.api_endpoint ?? '—'}</strong></div>
                </div>
              </div>
            </section>

            <section className="panel-card flow-card">
              <div className="panel-title">Connection flow</div>
              <div className="flow">
                {['IP discovery', 'SSH bootstrap', 'kubeconfig fetch', 'Cluster check'].map((step, index) => (
                  <div className="flow-step" key={step}>
                    <span className="flow-index mono">{index + 1}</span>
                    <span>{step}</span>
                    {index < 3 && <span className="flow-arrow">→</span>}
                  </div>
                ))}
              </div>
            </section>
          </>
        )}
      </main>
    </div>
  );
}

