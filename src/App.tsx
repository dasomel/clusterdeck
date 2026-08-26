import { useMemo, useState } from 'react';
import {
  Check,
  CircleAlert,
  Command,
  Plus,
  RefreshCw,
  RotateCcw,
  Server,
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

const flowSteps = ['Discover hosts', 'Bootstrap SSH', 'Sync kubeconfig', 'Verify API'];

function StatusBadge({ reachable }: { reachable: boolean }) {
  return (
    <span className={`status-badge ${reachable ? 'success' : 'warning'}`}>
      <span className="status-dot" />
      {reachable ? 'Reachable' : 'Needs retry'}
    </span>
  );
}

function ConnectionStep({ label, state }: { label: string; state: StepState }) {
  const status = {
    waiting: 'Waiting',
    active: 'In progress',
    success: 'Completed',
    error: 'Failed',
  }[state];

  return (
    <div className={`connection-step ${state}`}>
      <span className="step-marker">
        {state === 'success' ? <Check size={13} /> : state === 'error' ? <CircleAlert size={13} /> : null}
      </span>
      <div className="step-copy">
        <strong>{label}</strong>
        <small>{status}</small>
      </div>
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

  const healthyHosts = selected.hosts.filter((host) => host.reachable).length;
  const hasFailures = healthyHosts !== selected.hosts.length;
  const isConnecting = connectState === 'connecting';
  const showFailure = connectState === 'partial-failure';

  const connect = async () => {
    setConnectState('connecting');
    await new Promise((resolve) => setTimeout(resolve, 900));
    setConnectState(selected.hosts.every((host) => host.reachable) ? 'ready' : 'partial-failure');
  };

  const retryFailedHosts = async () => {
    setConnectState('connecting');
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

  const stepStates: StepState[] = isConnecting
    ? ['success', 'active', 'waiting', 'waiting']
    : showFailure
      ? ['success', 'error', 'waiting', 'waiting']
      : ['success', 'success', 'success', 'success'];

  const environmentMessage = isConnecting
    ? 'Discovering hosts and synchronizing local access…'
    : showFailure
      ? 'One host needs attention. Healthy targets remain available.'
      : 'Local access is healthy and synchronized.';

  const apiStatus = isConnecting ? 'Pending' : showFailure ? 'Blocked' : 'Verified';
  const kubeStatus = isConnecting ? 'Syncing' : 'Synced';

  return (
    <div className="desktop-window">
      <header className="titlebar">
        <div className="window-controls" aria-hidden="true">
          <span className="traffic red" />
          <span className="traffic amber" />
          <span className="traffic green" />
        </div>
        <div className="titlebar-name">ClusterDeck</div>
        <button className="quick-action" type="button">
          <Command size={12} />K <span>Quick actions</span>
        </button>
      </header>

      <div className="workspace-shell">
        <aside className="sidebar">
          <div className="sidebar-top">
            <div className="product-kicker">CLUSTERDECK</div>
            <div className="sidebar-heading">Profiles</div>
            <div className="profile-list">
              {profiles.map((profile) => {
                const healthy = profile.hosts.filter((host) => host.reachable).length;
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
                    <span className={`profile-health ${healthy === profile.hosts.length ? 'ok' : 'warn'}`} />
                    <span className="profile-copy">
                      <strong>{profile.name}</strong>
                      <small>{profile.hosts.length} hosts · {profile.bastion ? 'Bastion' : 'Direct'}</small>
                    </span>
                  </button>
                );
              })}
            </div>
            <button className="add-profile" type="button">
              <Plus size={13} /> Add profile
            </button>
          </div>

          <div className="local-status">
            <div className="product-kicker">LOCAL WORKSTATION</div>
            <div className="local-platform">macOS · arm64</div>
            <div className="local-ready"><span /> Ready</div>
          </div>
        </aside>

        <main className="main-panel">
          <section className="environment-header">
            <div>
              <div className="eyebrow">ENVIRONMENT</div>
              <h1>{selected.name}</h1>
              <p className={showFailure ? 'warning-copy' : ''}>{environmentMessage}</p>
            </div>
            <div className="header-actions">
              <button className="secondary-button" onClick={refresh} type="button">
                <RefreshCw size={14} /> Refresh
              </button>
              {showFailure ? (
                <button className="primary-button" onClick={retryFailedHosts} type="button">
                  <RotateCcw size={14} /> Retry failed host
                </button>
              ) : (
                <button className="primary-button" onClick={connect} disabled={isConnecting} type="button">
                  {isConnecting ? <RefreshCw size={14} className="spin" /> : <Terminal size={14} />}
                  {isConnecting ? 'Connecting…' : 'Connect / Sync'}
                </button>
              )}
            </div>
          </section>

          <section className="metric-grid">
            <article className="metric-card">
              <span>Hosts</span>
              <strong>{healthyHosts} / {selected.hosts.length}</strong>
              <small className={hasFailures ? 'warning-text' : 'success-text'}>
                <i /> {hasFailures ? `${selected.hosts.length - healthyHosts} needs retry` : 'All reachable'}
              </small>
            </article>
            <article className="metric-card">
              <span>SSH</span>
              <strong>{hasFailures ? `${healthyHosts} ready` : 'Ready'}</strong>
              <small className="success-text"><i /> {selected.bastion ? 'Via bastion' : 'Direct access'}</small>
            </article>
            <article className="metric-card">
              <span>Kubeconfig</span>
              <strong>{kubeStatus}</strong>
              <small className="accent-text"><i /> {selected.kubeContext}</small>
            </article>
            <article className="metric-card">
              <span>Kubernetes API</span>
              <strong className={showFailure ? 'warning-text' : ''}>{apiStatus}</strong>
              <small className={showFailure ? 'warning-text' : 'success-text'}>
                <i /> {showFailure ? 'Awaiting SSH' : isConnecting ? 'Checking…' : '12 ms'}
              </small>
            </article>
          </section>

          <section className="content-grid">
            <article className="panel hosts-panel">
              <div className="panel-heading">
                <div>
                  <h2>Hosts</h2>
                  <p>{selected.hosts.length} targets</p>
                </div>
              </div>
              <div className="host-table-head">
                <span>HOST</span><span>ADDRESS</span><span>STATUS</span>
              </div>
              <div className="host-table">
                {selected.hosts.map((host) => (
                  <div className="host-row" key={host.name}>
                    <div className="host-identity">
                      <span className="host-icon"><Server size={14} /></span>
                      <span><strong>{host.name}</strong><small>Linux node</small></span>
                    </div>
                    <code>{host.address}</code>
                    <StatusBadge reachable={host.reachable} />
                  </div>
                ))}
              </div>
            </article>

            <article className="panel activity-panel">
              <div className="panel-heading">
                <div><h2>Connection activity</h2><p>Latest session</p></div>
              </div>
              <div className="activity-list">
                {flowSteps.map((step, index) => (
                  <ConnectionStep key={step} label={step} state={stepStates[index]} />
                ))}
              </div>
            </article>
          </section>

          <section className="workspace-strip">
            <div>
              <strong>{showFailure ? 'Partial workspace' : isConnecting ? 'Preparing workspace' : 'Connected workspace'}</strong>
              <div className="workspace-meta">
                <span>Context&nbsp; <b>{selected.kubeContext}</b></span>
                <span>Kubeconfig&nbsp; <b>~/.kube/config</b></span>
                <span>API&nbsp; <b>https://192.0.2.10:6443</b></span>
              </div>
            </div>
            <button className="secondary-button terminal-button" type="button">
              <Terminal size={14} /> Open Terminal ↗
            </button>
          </section>
        </main>
      </div>
    </div>
  );
}
