import { RefreshCw, FileText, Trash2, ExternalLink } from 'lucide-react';
import type { ConnectionResult, HostsFileStatus, Profile } from '../../api/tauri';
import Pill from '../../components/ui/Pill';
import Tag from '../../components/ui/Tag';
import CopyButton from '../../components/ui/CopyButton';
import EmptyState from '../../components/ui/EmptyState';

type EndpointsTabProps = {
  profile: Profile;
  lastResult: ConnectionResult | null;
  hostsStatus: HostsFileStatus | null;
  discoveringEndpoints: boolean;
  syncingHosts: boolean;
  clearingHosts: boolean;
  onScan: () => void;
  onSync: () => void;
  onClear: () => void;
  onOpenUrl: (url: string) => void;
};

export default function EndpointsTab({
  profile,
  lastResult,
  hostsStatus,
  discoveringEndpoints,
  syncingHosts,
  clearingHosts,
  onScan,
  onSync,
  onClear,
  onOpenUrl,
}: EndpointsTabProps) {
  const isSynced = hostsStatus?.is_synced ?? false;
  const endpoints = lastResult?.endpoints ?? [];

  return (
    <div style={{ display: 'flex', flexDirection: 'column', gap: '16px' }}>
      {/* Hosts file controls strip */}
      <div className="panel-card" style={{ padding: '12px 16px' }}>
        <div style={{ display: 'flex', justifyContent: 'space-between', alignItems: 'center', flexWrap: 'wrap', gap: '10px' }}>
          <div style={{ display: 'flex', alignItems: 'center', gap: '8px', flexWrap: 'wrap' }}>
            <span style={{ fontSize: '13px', fontWeight: 600 }}>/etc/hosts Integration</span>
            <Pill variant={isSynced ? 'ok' : 'warn'}>
              {isSynced ? '/etc/hosts Synced' : 'Not in /etc/hosts'}
            </Pill>
            <Pill variant={profile.manage_hosts_file ? 'ok' : 'idle'}>
              {profile.manage_hosts_file ? 'Auto-sync: On' : 'Auto-sync: Off'}
            </Pill>
          </div>

          <div style={{ display: 'flex', gap: '6px', flexWrap: 'wrap' }}>
            <button
              type="button"
              className="secondary-button"
              style={{ width: 'auto', marginTop: 0, padding: '5px 10px', fontSize: '12px', gap: '5px' }}
              onClick={onScan}
              disabled={discoveringEndpoints}
              title="Scan cluster for Ingresses, ApisixRoutes, and Gateways"
            >
              <RefreshCw size={12} className={discoveringEndpoints ? 'spin' : ''} />
              Scan Endpoints
            </button>
            {isSynced && (
              <button
                type="button"
                className="secondary-button"
                style={{ width: 'auto', marginTop: 0, padding: '5px 10px', fontSize: '12px', gap: '5px', color: 'var(--danger)' }}
                onClick={onClear}
                disabled={clearingHosts}
                title="Remove this profile's block from /etc/hosts"
              >
                <Trash2 size={12} />
                Clear /etc/hosts
              </button>
            )}
            <button
              type="button"
              className="primary-button"
              style={{ width: 'auto', marginTop: 0, padding: '5px 12px', fontSize: '12px', gap: '5px' }}
              onClick={onSync}
              disabled={syncingHosts}
              title="Write cluster endpoints and profile hosts to /etc/hosts (requires admin approval)"
            >
              {syncingHosts ? <RefreshCw size={12} className="spin" /> : <FileText size={12} />}
              Sync to /etc/hosts
            </button>
          </div>
        </div>

        <p style={{ margin: '8px 0 0', fontSize: '12px', color: 'var(--text-secondary)' }}>
          Maps *.{profile.id}.clusterdeck.local node aliases and discovered Ingress/APISIX/Gateway hosts into /etc/hosts.
        </p>
      </div>

      {/* Node FQDNs */}
      <div className="panel-card" style={{ padding: '14px 16px' }}>
        <div style={{ fontSize: '11px', fontWeight: 600, color: 'var(--text-tertiary)', textTransform: 'uppercase', marginBottom: '8px' }}>
          Node Host Aliases (*.{profile.id}.clusterdeck.local)
        </div>
        <div className="host-list">
          {profile.hosts.map((host) => {
            const fqdn = `${host.name}.${profile.id}.clusterdeck.local`;
            return (
              <div className="host-row" key={host.name}>
                <div>
                  <div style={{ display: 'flex', alignItems: 'center', gap: '6px' }}>
                    <span className="host-name mono" style={{ fontSize: '12px' }}>{fqdn}</span>
                    <Tag>Node</Tag>
                  </div>
                  <div className="host-address">
                    Target IP: <strong>{host.address}</strong> (port {host.port})
                  </div>
                </div>
                <div style={{ display: 'flex', alignItems: 'center', gap: '6px' }}>
                  <CopyButton text={fqdn} title={`Copy ${fqdn}`} />
                  <button
                    type="button"
                    className="icon-button"
                    style={{ width: '26px', height: '26px' }}
                    title={`Open http://${fqdn} in browser`}
                    aria-label={`Open http://${fqdn} in browser`}
                    onClick={() => onOpenUrl(`http://${fqdn}`)}
                  >
                    <ExternalLink size={13} />
                  </button>
                </div>
              </div>
            );
          })}

          {profile.bastion && (
            <div className="host-row" key="bastion-alias">
              <div>
                <div style={{ display: 'flex', alignItems: 'center', gap: '6px' }}>
                  <span className="host-name mono" style={{ fontSize: '12px' }}>
                    {profile.bastion.name}.${profile.id}.clusterdeck.local
                  </span>
                  <Tag>Bastion</Tag>
                </div>
                <div className="host-address">
                  Target IP: <strong>{profile.bastion.address}</strong> (port {profile.bastion.port})
                </div>
              </div>
              <div style={{ display: 'flex', alignItems: 'center', gap: '6px' }}>
                <CopyButton
                  text={`${profile.bastion.name}.${profile.id}.clusterdeck.local`}
                  title="Copy bastion FQDN"
                />
              </div>
            </div>
          )}
        </div>
      </div>

      {/* Discovered Cluster Endpoints */}
      <div className="panel-card" style={{ padding: '14px 16px' }}>
        <div style={{ display: 'flex', justifyContent: 'space-between', alignItems: 'center', marginBottom: '8px' }}>
          <span style={{ fontSize: '11px', fontWeight: 600, color: 'var(--text-tertiary)', textTransform: 'uppercase' }}>
            Discovered Ingress / Gateway Endpoints ({endpoints.length})
          </span>
        </div>

        {endpoints.length > 0 ? (
          <div className="host-list">
            {endpoints.map((ep) => (
              <div className="host-row" key={`${ep.source}-${ep.host}`}>
                <div>
                  <div style={{ display: 'flex', alignItems: 'center', gap: '6px' }}>
                    <span className="host-name mono" style={{ fontSize: '12px' }}>{ep.host}</span>
                    <Tag>{ep.source}</Tag>
                  </div>
                  <div className="host-address">
                    Target IP: <strong>{ep.ip}</strong> · Resource: {ep.resource_name}
                  </div>
                </div>
                <div style={{ display: 'flex', alignItems: 'center', gap: '6px' }}>
                  <CopyButton text={ep.host} title={`Copy ${ep.host}`} />
                  <button
                    type="button"
                    className="icon-button"
                    style={{ width: '26px', height: '26px' }}
                    title={`Open https://${ep.host} in browser`}
                    aria-label={`Open https://${ep.host} in browser`}
                    onClick={() => onOpenUrl(`https://${ep.host}`)}
                  >
                    <ExternalLink size={13} />
                  </button>
                </div>
              </div>
            ))}
          </div>
        ) : (
          <EmptyState
            title="No endpoints scanned yet."
            description="Scan the cluster for Ingress, APISIX, and Gateway API services to map them."
            actions={
              <button
                type="button"
                className="secondary-button"
                style={{ width: 'auto', marginTop: 0, padding: '5px 12px', fontSize: '12px' }}
                onClick={onScan}
                disabled={discoveringEndpoints}
              >
                Scan endpoints
              </button>
            }
          />
        )}
      </div>
    </div>
  );
}
