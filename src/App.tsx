import { useMemo, useState } from 'react';
import { Boxes, CheckCircle2, CircleAlert, Plus, RefreshCw, Server, Terminal } from 'lucide-react';

type Host = {
  name: string;
  address: string;
  reachable: boolean;
};

type Profile = {
  id: string;
  name: string;
  hosts: Host[];
  bastion?: string;
  kubeContext: string;
};

const initialProfiles: Profile[] = [
  {
    id: 'cka-lab',
    name: 'CKA Lab',
    hosts: [
      { name: 'cka-m1', address: '192.0.2.10', reachable: true },
      { name: 'cka-w1', address: '192.0.2.11', reachable: true },
      { name: 'cka-w2', address: '192.0.2.12', reachable: false },
    ],
    kubeContext: 'cka-lab',
  },
  {
    id: 'dev-cluster',
    name: 'Dev Cluster',
    hosts: [{ name: 'dev-m1', address: '198.51.100.20', reachable: true }],
    bastion: 'bastion.dev',
    kubeContext: 'dev',
  },
];

export default function App() {
  const [profiles, setProfiles] = useState(initialProfiles);
  const [selectedId, setSelectedId] = useState(initialProfiles[0].id);
  const [connecting, setConnecting] = useState(false);

  const selected = useMemo(
    () => profiles.find((profile) => profile.id === selectedId) ?? profiles[0],
    [profiles, selectedId],
  );

  const connect = async () => {
    setConnecting(true);
    // TODO: Replace with Tauri invoke("connect_profile", { profileId: selected.id }) once the Rust command exists.
    await new Promise((resolve) => setTimeout(resolve, 700));
    setConnecting(false);
  };

  const refresh = async () => {
    // TODO: invoke discovery/health commands from Rust backend.
    setProfiles((current) => current.map((profile) => ({ ...profile })));
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
        <div className="profile-list">
          {profiles.map((profile) => {
            const healthyHosts = profile.hosts.filter((host) => host.reachable).length;
            const isSelected = profile.id === selectedId;
            return (
              <button
                key={profile.id}
                className={`profile-card ${isSelected ? 'selected' : ''}`}
                onClick={() => setSelectedId(profile.id)}
              >
                <div className="profile-card-top">
                  <span className="profile-name">{profile.name}</span>
                  {healthyHosts === profile.hosts.length ? (
                    <CheckCircle2 className="status-ok" size={16} />
                  ) : (
                    <CircleAlert className="status-warn" size={16} />
                  )}
                </div>
                <div className="profile-meta">
                  {profile.hosts.length} hosts · {profile.bastion ? 'Bastion' : 'Direct'}
                </div>
              </button>
            );
          })}
        </div>

        <button className="secondary-button">
          <Plus size={16} /> Add profile
        </button>
      </aside>

      <main className="main-panel">
        <header className="header">
          <div>
            <div className="eyebrow">ENVIRONMENT</div>
            <h1>{selected?.name ?? 'Cluster'}</h1>
          </div>
          <button className="icon-button" title="Refresh" onClick={refresh}>
            <RefreshCw size={17} />
          </button>
        </header>

        <section className="hero-card">
          <div>
            <div className="eyebrow">READY TO CONNECT</div>
            <h2>Bring the cluster to your local workstation.</h2>
            <p>Discover hosts, bootstrap SSH, fetch kubeconfig, and verify Kubernetes access from one profile.</p>
          </div>
          <button className="primary-button" onClick={connect} disabled={connecting}>
            {connecting ? <RefreshCw size={16} className="spin" /> : <Terminal size={16} />}
            {connecting ? 'Connecting…' : 'Connect / Sync'}
          </button>
        </section>

        <section className="grid-two">
          <div className="panel-card">
            <div className="panel-title"><Server size={16} /> Hosts</div>
            <div className="host-list">
              {selected?.hosts.map((host) => (
                <div className="host-row" key={host.name}>
                  <div>
                    <div className="host-name">{host.name}</div>
                    <div className="host-address">{host.address}</div>
                  </div>
                  <span className={`pill ${host.reachable ? 'success' : 'warning'}`}>
                    {host.reachable ? 'SSH reachable' : 'Needs retry'}
                  </span>
                </div>
              ))}
            </div>
          </div>

          <div className="panel-card">
            <div className="panel-title"><Boxes size={16} /> Kubernetes</div>
            <div className="status-stack">
              <div className="status-row"><span>SSH</span><strong>Ready</strong></div>
              <div className="status-row"><span>Kubeconfig</span><strong>Synced</strong></div>
              <div className="status-row"><span>Context</span><strong>{selected?.kubeContext}</strong></div>
              <div className="status-row"><span>API</span><strong>Verified</strong></div>
            </div>
          </div>
        </section>

        <section className="panel-card flow-card">
          <div className="panel-title">Connection flow</div>
          <div className="flow">
            {['IP discovery', 'SSH bootstrap', 'kubeconfig fetch', 'Cluster check'].map((step, index) => (
              <div className="flow-step" key={step}>
                <span className="flow-index">{index + 1}</span>
                <span>{step}</span>
                {index < 3 && <span className="flow-arrow">→</span>}
              </div>
            ))}
          </div>
        </section>
      </main>
    </div>
  );
}
