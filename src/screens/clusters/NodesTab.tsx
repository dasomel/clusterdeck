import { Terminal } from 'lucide-react';
import type { ConnectionResult, HostsFileStatus, Profile } from '../../api/tauri';
import DefinitionList, { type DefinitionItem } from '../../components/ui/DefinitionList';
import Pill from '../../components/ui/Pill';
import Tag from '../../components/ui/Tag';
import CopyButton from '../../components/ui/CopyButton';

type NodesTabProps = {
  profile: Profile;
  lastResult: ConnectionResult | null;
  hostsStatus: HostsFileStatus | null;
  onOpenSsh: (hostName: string) => void;
};

export default function NodesTab({ profile, lastResult, hostsStatus, onOpenSsh }: NodesTabProps) {
  const verification = lastResult?.verification;

  const facts: DefinitionItem[] = [
    { term: 'Context', detail: <span className="mono">{profile.kubeconfig?.context ?? '—'}</span> },
    { term: 'API Endpoint', detail: <span className="mono">{verification?.api_endpoint ?? '—'}</span> },
    { term: 'Kubernetes Version', detail: <span>{verification?.kubernetes_version ?? '—'}</span> },
    {
      term: '/etc/hosts',
      detail: (
        <span>
          {hostsStatus?.is_synced ? 'Synced' : profile.manage_hosts_file ? 'Auto-sync' : 'Manual'}
        </span>
      ),
    },
  ];

  return (
    <div style={{ display: 'flex', flexDirection: 'column', gap: '16px' }}>
      <div className="panel-card" style={{ padding: '12px 16px' }}>
        <div style={{ fontSize: '11px', fontWeight: 600, color: 'var(--text-tertiary)', textTransform: 'uppercase', marginBottom: '8px' }}>
          Cluster facts
        </div>
        <DefinitionList items={facts} />
      </div>

      <div className="panel-card" style={{ padding: '14px 16px' }}>
        <div style={{ display: 'flex', justifyContent: 'space-between', alignItems: 'center', marginBottom: '10px' }}>
          <span style={{ fontSize: '13px', fontWeight: 600, color: 'var(--text-primary)' }}>
            Hosts ({profile.hosts.length + (profile.bastion ? 1 : 0)})
          </span>
        </div>

        <div className="host-list">
          {profile.bastion && (
            <div className="host-row" key="bastion">
              <div>
                <div style={{ display: 'flex', alignItems: 'center', gap: '6px' }}>
                  <span className="host-name">{profile.bastion.name}</span>
                  <Tag>Bastion</Tag>
                </div>
                <div className="host-address">
                  {profile.bastion.address}:{profile.bastion.port}
                </div>
              </div>
              <div style={{ display: 'flex', alignItems: 'center', gap: '6px' }}>
                <CopyButton text={`${profile.bastion.address}:${profile.bastion.port}`} title="Copy bastion address" />
                <Pill variant="ok">Bastion Host</Pill>
              </div>
            </div>
          )}

          {profile.hosts.map((host) => {
            const hostResult = lastResult?.hosts.find((h) => h.host === host.name);
            const reachable = hostResult?.reachable ?? false;
            const portSuffix = host.port !== 22 ? `:${host.port}` : '';
            return (
              <div className="host-row" key={host.name}>
                <div>
                  <div style={{ display: 'flex', alignItems: 'center', gap: '6px' }}>
                    <span className="host-name">{host.name}</span>
                    <Tag>{host.auth === 'password' ? 'password' : 'key'}</Tag>
                  </div>
                  <div className="host-address">
                    {host.address}{portSuffix}
                  </div>
                  {hostResult && !reachable && hostResult.detail && (
                    <div className="host-error-detail mono">{hostResult.detail.trim()}</div>
                  )}
                </div>

                <div style={{ display: 'flex', alignItems: 'center', gap: '6px' }}>
                  <button
                    type="button"
                    className="icon-button"
                    style={{ width: '26px', height: '26px' }}
                    title={`Open SSH session to ${host.name}`}
                    aria-label={`Open SSH session to ${host.name}`}
                    onClick={() => onOpenSsh(host.name)}
                  >
                    <Terminal size={13} />
                  </button>
                  <CopyButton text={`${host.address}${portSuffix}`} title={`Copy address of ${host.name}`} />
                  <Pill variant={reachable ? 'ok' : 'warn'}>
                    {reachable ? 'SSH reachable' : 'Needs retry'}
                  </Pill>
                </div>
              </div>
            );
          })}
        </div>
      </div>
    </div>
  );
}
