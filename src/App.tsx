import { useMemo, useState } from 'react';
import {
  Check,
  CircleAlert,
  Plus,
  RefreshCw,
  RotateCcw,
  Terminal,
} from 'lucide-react';

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

type ConnectState = 'ready' | 'connecting' | 'partial-failure';
type StepState = 'waiting' | 'active' | 'success' | 'error';

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

const flowSteps = ['IP discovery', 'SSH bootstrap', 'kubeconfig fetch', 'API verify'];

function StatusBadge({ reachable }: { reachable: boolean }) {
  return (
    <span className={`status-badge ${reachable ? 'success' : 'warning'}`}>
      <span className="status-dot" />
      {reachable ? 'SSH reachable' : 'Needs retry'}
    </span>
  );
}

function ConnectionStep({ label, state }: { label: string; state: StepState }) {
  const stateLabel = {
    waiting: 'Waiting',
    active: 'In progress',
    success: 'Completed',
    error: 'Failed',
  }[state];

  return (
    <div className={`connection-step ${state}`}>
      <span className="step-indicator">
        {state === 'success' ? <Check size={14} /> : state === 'error' ? <CircleAlert size={14} /> : null}
      </span>
      <span className="step-copy">
        <strong>{label}</strong>
        <small>{stateLabel}</small>
      </span>
    </div>
  );
}

export default function App() {
  const [profiles, setProfiles] = useState(initialProfiles);
  const [selectedId, setSelectedId] = useState(initialProfiles[0].id);
  const [connectState, setConnectState] = useState<ConnectState>('ready');

  const selected = useMemo(
    () => profiles.find((profile) => profile.id === selectedId) ?? profiles[0],
    [profiles, selectedId],
  );

  const connect = async () => {
    setConnectState('connecting');
    // TODO: replace simulation with invoke('connect_profile', { profileId: selected.id }).
    await new Promise((resolve) => setTimeout(resolve, 900));
    setConnectState(selected.hosts.every((host) => host.reachable) ? 'ready' : 'partial-failure');
  };

  const retryFailedHosts = async () => {
    setConnectState('connecting');
    // TODO: replace with a targeted retry command from the Tauri backend.
    await new Promise((resolve) => setTimeout(resolve, 700));
    setProfiles((current) =>
      current.map((profile) =>
        profile.id === selected.id
          ? { ...profile, hosts: profile.hosts.map((host) => ({ ...host, reachable: true })) }
          : profile,
      ),
    );
    setConnectState('ready');
  };

  const refresh = () => setProfiles((current) => current.map((profile) => ({ ...profile })));

  const healthyHosts = selected.hosts.filter((host) => host.reachable).length;
  const hasFailures = healthyHosts !== selected.hosts.length;
  const isConnecting = connectState === 'connecting';

  const hero = {
    ready: {
      eyebrow: 'READY TO CONNECT',
      title: 'Bring the cluster to your local workstation.',
      description: 'Discover hosts, bootstrap SSH, sync kubeconfig, and verify API access from one profile.',
    },
    connecting: {
      eyebrow: 'CONNECTING',
      title: 'Preparing local cluster access.',
      description: 'ClusterDeck is validating hosts and synchronizing the Kubernetes context.',
    },
    'partial-failure': {
      eyebrow: 'ACTION REQUIRED',
      title: 'One host needs attention.',
      description: 'Healthy hosts remain available. Retry only the failed target without restarting the full flow.',
    },
  }[connectState];

  const stepStates: StepState[] = isConnecting
    ? ['success', 'active', 'waiting', 'waiting']
    : connectState === 'partial-failure'
      ? ['success', 'error', 'waiting', 'waiting']
      : ['success', 'success', 'success', 'success'];

  return (
    <div className="app-shell">
      <aside className="sidebar">
        <div className="brand-block">
          <div className="brand-name">ClusterDeck</div>
          <div className="brand-subtitle">macOS cluster access</div>
        </div>

        <div className="section-label">PROFILES</div>
        <div className="profile-list">
          {profiles.map((profile) => {
            const profileHealthy = profile.hosts.filter((host) => host.reachable).length;
            const selectedProfile = profile.id === selectedId;
            return (
              <button
                key={profile.id}
                className={`profile-item ${selectedProfile ? 'selected' : ''}`}
                onClick={() => {
                  setSelectedId(profile.id);
                  setConnectState('ready');
                }}
              >
                <span>
                  <strong>{profile.name}</strong>
                  <small>{profile.hosts.length} hosts · {profile.bastion ? 'Bastion' : 'Direct'}</small>
                </span>
                <span className={`profile-health ${profileHealthy === profile.hosts.length ? 'ok' : 'warn'}`} />
              </button>
            );
          })}
        </div>

        <button className="sidebar-action">
          <Plus size={15} /> Add profile
        </button>
      </aside>

      <main className="main-panel">
        <header className="page-header">
          <div>
            <div className="eyebrow muted">ENVIRONMENT</div>
            <h1>{selected.name}</h1>
          </div>
          <button className="icon-button" title="Refresh" onClick={refresh} aria-label="Refresh profile status">
            <RefreshCw size={16} />
          </button>
        </header>

        <section className={`hero-card ${connectState}`}>
          <div className="hero-copy">
            <div className={`eyebrow ${connectState === 'partial-failure' ? 'warning' : ''}`}>{hero.eyebrow}</div>
            <h2>{hero.title}</h2>
            <p>{hero.description}</p>
          </div>

          {connectState === 'partial-failure' ? (
            <button className="primary-button retry" onClick={retryFailedHosts}>
              <RotateCcw size={15} /> Retry failed host
            </button>
          ) : (
            <button className="primary-button" onClick={connect} disabled={isConnecting}>
              {isConnecting ? <RefreshCw size={15} className="spin" /> : <Terminal size={15} />}
              {isConnecting ? 'Connecting…' : 'Connect / Sync'}
            </button>
          )}
        </section>

        <section className="content-grid">
          <div className="panel-card hosts-panel">
            <div className="panel-title">Hosts</div>
            <div className="host-list">
              {selected.hosts.map((host) => (
                <div className={`host-row ${host.reachable ? '' : 'unhealthy'}`} key={host.name}>
                  <div>
                    <div className="host-name">{host.name}</div>
                    <div className="host-address">{host.address}</div>
                  </div>
                  <StatusBadge reachable={host.reachable} />
                </div>
              ))}
            </div>
            {hasFailures && connectState === 'partial-failure' && (
              <div className="failure-note">
                <CircleAlert size={14} /> {selected.hosts.length - healthyHosts} host requires SSH retry.
              </div>
            )}
          </div>

          <div className="panel-card kube-panel">
            <div className="panel-title">Kubernetes</div>
            <div className="status-stack">
              <div className="status-row"><span>SSH</span><strong>{hasFailures ? `${healthyHosts}/${selected.hosts.length} ready` : 'Ready'}</strong></div>
              <div className="status-row"><span>Kubeconfig</span><strong>{isConnecting ? 'Pending' : 'Synced'}</strong></div>
              <div className="status-row"><span>Context</span><strong>{selected.kubeContext}</strong></div>
              <div className="status-row"><span>API</span><strong>{isConnecting ? 'Pending' : hasFailures && connectState === 'partial-failure' ? 'Blocked' : 'Verified'}</strong></div>
            </div>
          </div>
        </section>

        <section className="panel-card flow-panel">
          <div className="panel-title">Connection flow</div>
          <div className="flow-grid">
            {flowSteps.map((step, index) => (
              <ConnectionStep key={step} label={step} state={stepStates[index]} />
            ))}
          </div>
        </section>
      </main>
    </div>
  );
}
