import { RefreshCw, FileText, Trash2, ExternalLink } from 'lucide-react';
import type {
  ConnectionResult,
  DnsConflict,
  DnsSkippedHost,
  HostsFileStatus,
  LocalDnsStatus,
  Profile,
} from '../../api/tauri';
import { groupBlockingConflicts, hasBlockingConflicts } from '../../lib/hostsConflicts';
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
  dnsConflicts: DnsConflict[];
  dnsSkipped: DnsSkippedHost[];
  dnsStatus: LocalDnsStatus | null;
  approvingZone: string | null;
  onApproveZone: (zone: string) => void;
  onResolveConflicts: () => void;
  onOpenUrl: (url: string) => void;
};

function dnsConflictText(c: DnsConflict): string {
  switch (c.kind) {
    case 'unowned':
      return 'a /etc/resolver file for this zone already exists and is not managed by ClusterDeck.';
    case 'owned_by_other_profile':
      return `owned by profile ${c.profile_ids[c.profile_ids.length - 1] ?? 'unknown'}; switch the zone owner to use it here.`;
    case 'claimed_by_multiple':
      return `claimed by ${c.profile_ids.join(', ')}; only one profile may own a zone.`;
    case 'unreadable':
      return 'the /etc/resolver entry could not be read.';
    default:
      return 'not a valid private zone.';
  }
}

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
  dnsConflicts,
  dnsSkipped,
  dnsStatus,
  approvingZone,
  onApproveZone,
  onResolveConflicts,
  onOpenUrl,
}: EndpointsTabProps) {
  const isSynced = hostsStatus?.is_synced ?? false;
  const endpoints = lastResult?.endpoints ?? [];
  const conflicts = hostsStatus?.conflicts ?? [];
  const canResolve = hasBlockingConflicts(groupBlockingConflicts(conflicts, profile.id));
  const isLocalDns = profile.host_mapping === 'local_dns';
  const modeLabel =
    profile.host_mapping === 'local_dns'
      ? 'Local DNS'
      : profile.host_mapping === 'hosts'
        ? 'Hosts (/etc/hosts)'
        : 'Off';
  const aliasZone = `${profile.id.toLowerCase().replace(/_/g, '-')}.clusterdeck.local`;
  const zoneRows = [aliasZone, ...(profile.approved_dns_zones ?? [])].map((zone) => {
    const served = dnsStatus?.zones.find((z) => z.zone === zone);
    const state = !dnsStatus
      ? 'unknown'
      : !served
        ? 'not served'
        : served.owner === profile.id
          ? 'served'
          : `owned by ${served.owner}`;
    return { zone, state, records: served?.records ?? 0 };
  });
  const servedZoneFor = (host: string) =>
    zoneRows
      .filter((r) => host === r.zone || host.endsWith(`.${r.zone}`))
      .sort((a, b) => b.zone.length - a.zone.length)[0]?.zone;

  return (
    <div style={{ display: 'flex', flexDirection: 'column', gap: '16px' }}>
      {/* Hosts file controls strip */}
      <div className="panel-card" style={{ padding: '12px 16px' }}>
        <div style={{ display: 'flex', justifyContent: 'space-between', alignItems: 'center', flexWrap: 'wrap', gap: '10px' }}>
          <div style={{ display: 'flex', alignItems: 'center', gap: '8px', flexWrap: 'wrap' }}>
            <span style={{ fontSize: '13px', fontWeight: 600 }}>Name resolution</span>
            <Pill variant={profile.host_mapping === 'off' ? 'idle' : 'ok'}>{modeLabel}</Pill>
            {isLocalDns ? (
              <Pill variant={dnsStatus?.agent_loaded && !dnsStatus.last_error ? 'ok' : 'warn'}>
                {dnsStatus
                  ? dnsStatus.agent_loaded
                    ? `Responder running${dnsStatus.pid ? ` (pid ${dnsStatus.pid})` : ''} · gen ${dnsStatus.generation}`
                    : 'Responder not running'
                  : 'Responder status unknown'}
              </Pill>
            ) : (
              <Pill variant={isSynced ? 'ok' : 'warn'}>
                {isSynced ? '/etc/hosts Synced' : 'Not in /etc/hosts'}
              </Pill>
            )}
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
            {(isLocalDns || isSynced) && (
              <button
                type="button"
                className="secondary-button"
                style={{ width: 'auto', marginTop: 0, padding: '5px 10px', fontSize: '12px', gap: '5px', color: 'var(--danger)' }}
                onClick={onClear}
                disabled={clearingHosts}
                title={isLocalDns ? "Remove this profile's zones from local DNS" : "Remove this profile's block from /etc/hosts"}
              >
                <Trash2 size={12} />
                {isLocalDns ? 'Remove from local DNS' : 'Clear /etc/hosts'}
              </button>
            )}
            <button
              type="button"
              className="primary-button"
              style={{ width: 'auto', marginTop: 0, padding: '5px 12px', fontSize: '12px', gap: '5px' }}
              onClick={onSync}
              disabled={syncingHosts}
              title={isLocalDns ? 'Publish cluster endpoints and node aliases through local DNS (may require admin approval)' : 'Write cluster endpoints and profile hosts to /etc/hosts (requires admin approval)'}
            >
              {syncingHosts ? <RefreshCw size={12} className="spin" /> : <FileText size={12} />}
              {isLocalDns ? 'Sync to local DNS' : 'Sync to /etc/hosts'}
            </button>
          </div>
        </div>

        {isLocalDns ? (
          <>
            <p style={{ margin: '8px 0 0', fontSize: '12px', color: 'var(--text-secondary)' }}>
              Names are answered by the ClusterDeck local DNS responder via /etc/resolver. Nothing is written to /etc/hosts for this profile.
            </p>
            {dnsStatus?.last_error && (
              <div className="status-banner error" style={{ marginTop: '10px', fontSize: '12px' }}>
                {dnsStatus.last_error}
              </div>
            )}
            <div className="host-list" style={{ marginTop: '10px' }}>
              {zoneRows.map((r) => (
                <div className="host-row" key={r.zone}>
                  <span className="host-name mono" style={{ fontSize: '12px' }}>{r.zone}</span>
                  <span style={{ display: 'flex', gap: '6px', alignItems: 'center' }}>
                    <Tag>{r.state}</Tag>
                    <span style={{ fontSize: '12px', color: 'var(--text-secondary)' }}>
                      {r.records} record(s)
                    </span>
                  </span>
                </div>
              ))}
            </div>
          </>
        ) : (
          <p style={{ margin: '8px 0 0', fontSize: '12px', color: 'var(--text-secondary)' }}>
            Maps *.{profile.id}.clusterdeck.local node aliases and discovered Ingress/APISIX/Gateway hosts into /etc/hosts.
          </p>
        )}

        {isLocalDns && dnsConflicts.length > 0 && (
          <div className="status-banner warning" style={{ marginTop: '10px', fontSize: '12px' }}>
            <div style={{ fontWeight: 600, marginBottom: '4px' }}>
              Local DNS is not serving these zones.
            </div>
            <ul style={{ margin: 0, paddingLeft: '18px' }}>
              {dnsConflicts.map((c) => (
                <li key={`${c.zone}-${c.kind}`}>
                  <span style={{ fontFamily: 'var(--font-mono)' }}>{c.zone}</span>: {dnsConflictText(c)}
                </li>
              ))}
            </ul>
          </div>
        )}

        {isLocalDns && dnsSkipped.length > 0 && (
          <div className="status-banner warning" style={{ marginTop: '10px', fontSize: '12px' }}>
            <div style={{ fontWeight: 600, marginBottom: '4px' }}>
              Hosts skipped by local DNS. Approve a zone to publish them.
            </div>
            <ul style={{ margin: 0, paddingLeft: '18px' }}>
              {dnsSkipped.map((s) => (
                <li key={`${s.profile_id}-${s.host}`}>
                  <span style={{ fontFamily: 'var(--font-mono)' }}>{s.host}</span> ({s.reason})
                  {s.suggested_zones.map((zone) => (
                    <button
                      key={zone}
                      type="button"
                      className="secondary-button compact-btn"
                      style={{ marginLeft: '6px' }}
                      disabled={approvingZone !== null}
                      onClick={() => onApproveZone(zone)}
                      title={`Serve everything under ${zone} from local DNS (requires admin approval)`}
                    >
                      Approve zone {zone}
                    </button>
                  ))}
                </li>
              ))}
            </ul>
          </div>
        )}

        {!isLocalDns && conflicts.length > 0 && (
          <div className="status-banner warning" style={{ marginTop: '10px', fontSize: '12px' }}>
            <div style={{ fontWeight: 600, marginBottom: '4px' }}>
              Duplicate hostnames in /etc/hosts. The first entry wins, so the other cluster may answer.
            </div>
            <ul style={{ margin: 0, paddingLeft: '18px' }}>
              {conflicts.map((c, i) => (
                <li key={`${i}-${c.hostname}-${c.other_owner ?? 'unmanaged'}-${c.other_ip}`}>
                  <span style={{ fontFamily: 'var(--font-mono)' }}>{c.hostname}</span> is also mapped by{' '}
                  {c.other_owner ? `profile ${c.other_owner}` : 'an unmanaged /etc/hosts line'} →{' '}
                  <span style={{ fontFamily: 'var(--font-mono)' }}>{c.other_ip}</span>;{' '}
                  {c.we_win ? 'this profile wins' : 'this profile loses'} (ours: {c.our_ip}).
                </li>
              ))}
            </ul>
            <div style={{ marginTop: '4px', color: 'var(--text-secondary)' }}>
              Clear the other profile&apos;s /etc/hosts block (or the unmanaged line) to resolve this.
            </div>
            {canResolve && (
              <button
                type="button"
                className="secondary-button compact-btn"
                style={{ marginTop: '6px' }}
                onClick={onResolveConflicts}
              >
                Resolve…
              </button>
            )}
          </div>
        )}
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
                    {isLocalDns && ` · Served by local DNS (${aliasZone})`}
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
                    {profile.bastion.name}.{profile.id}.clusterdeck.local
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
                    {isLocalDns &&
                      (servedZoneFor(ep.host)
                        ? ` · Served by local DNS (${servedZoneFor(ep.host)})`
                        : ' · Not served (outside approved zones)')}
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
