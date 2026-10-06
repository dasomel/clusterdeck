import { useState, useMemo, useRef, useEffect } from 'react';
import { RefreshCw, Search, Server } from 'lucide-react';
import ScreenHeader from '../../components/layout/ScreenHeader';
import Segmented from '../../components/ui/Segmented';
import EmptyState from '../../components/ui/EmptyState';
import Skeleton from '../../components/ui/Skeleton';
import { useInventory } from '../../state/InventoryContext';
import { useProfiles } from '../../state/ProfilesContext';
import { useStatus } from '../../state/StatusContext';
import { formatTimeOnly } from '../../lib/format';
import type { Machine } from '../../api/types';
import DemoBand from './DemoBand';
import ProviderStrip from './ProviderStrip';
import ResourceSummary from './ResourceSummary';
import EnvironmentGroup from './EnvironmentGroup';
import LocalRuntimePanel from './LocalRuntimePanel';

type InfrastructureScreenProps = {
  onClusterReady?: (profileId: string) => void;
  isVisible?: boolean;
};

export default function InfrastructureScreen({
  onClusterReady,
  isVisible = true,
}: InfrastructureScreenProps) {
  const { inventory, loading, error, demo, setDemo, refresh, createFromEnvironment } = useInventory();
  const { select, reload: reloadProfiles } = useProfiles();
  const { pushStatus } = useStatus();

  const [filter, setFilter] = useState('');
  const [collapsedEnvs, setCollapsedEnvs] = useState<Set<string>>(new Set());
  const [busyEnvs, setBusyEnvs] = useState<Set<string>>(new Set());
  const filterInputRef = useRef<HTMLInputElement>(null);

  // Focus filter on '/' shortcut only when visible
  useEffect(() => {
    if (!isVisible) return;
    const handleKeyDown = (e: KeyboardEvent) => {
      if (e.key === '/' && document.activeElement?.tagName !== 'INPUT' && document.activeElement?.tagName !== 'TEXTAREA') {
        e.preventDefault();
        filterInputRef.current?.focus();
      }
    };
    window.addEventListener('keydown', handleKeyDown);
    return () => window.removeEventListener('keydown', handleKeyDown);
  }, [isVisible]);

  const toggleEnv = (env: string) => {
    setCollapsedEnvs((prev) => {
      const next = new Set(prev);
      if (next.has(env)) next.delete(env);
      else next.add(env);
      return next;
    });
  };

  const handleSetUpCluster = async (environment: string) => {
    setBusyEnvs((prev) => new Set(prev).add(environment));
    try {
      const result = await createFromEnvironment(environment);
      if (result.created) {
        pushStatus('success', `Cluster ${result.profile_id} created`);
      } else if (result.updated_hosts.length > 0) {
        pushStatus('success', `Cluster ${result.profile_id} updated (${result.updated_hosts.join(', ')} refreshed)`);
      } else {
        pushStatus('success', `Cluster ${result.profile_id} already up to date`);
      }
      await reloadProfiles();
      select(result.profile_id);
      onClusterReady?.(result.profile_id);
    } catch (err) {
      pushStatus('error', 'Failed to create cluster profile', [String(err)]);
    } finally {
      setBusyEnvs((prev) => {
        const next = new Set(prev);
        next.delete(environment);
        return next;
      });
    }
  };

  const filteredGroups = useMemo(() => {
    if (!inventory?.machines) return [];
    const q = filter.trim().toLowerCase();
    const map = new Map<string, Machine[]>();

    for (const m of inventory.machines) {
      if (
        !q ||
        m.name.toLowerCase().includes(q) ||
        m.environment.toLowerCase().includes(q) ||
        m.runtime.toLowerCase().includes(q) ||
        (m.orchestrator && m.orchestrator.toLowerCase().includes(q)) ||
        m.ips.some((ip) => ip.includes(q))
      ) {
        const list = map.get(m.environment) ?? [];
        list.push(m);
        map.set(m.environment, list);
      }
    }
    return Array.from(map.entries());
  }, [inventory?.machines, filter]);

  const hasData = inventory != null;
  const isRefreshing = loading && hasData;

  return (
    <div style={{ height: '100%', display: 'flex', flexDirection: 'column' }}>
      <ScreenHeader
        title="Infrastructure"
        meta={
          <div style={{ display: 'flex', alignItems: 'center', gap: 'var(--space-2)' }}>
            <span>VM inventory and local runtimes</span>
            {inventory?.timestamp && (
              <span className="mono" style={{ fontSize: 'var(--fs-xs)', color: 'var(--text-tertiary)' }}>
                · updated {formatTimeOnly(inventory.timestamp)}
              </span>
            )}
          </div>
        }
        actions={
          <div style={{ display: 'flex', alignItems: 'center', gap: 'var(--space-2)' }}>
            <Segmented
              options={[
                { value: 'live', label: 'Live' },
                { value: 'demo', label: 'Demo' },
              ]}
              value={demo ? 'demo' : 'live'}
              onChange={(val) => setDemo(val === 'demo')}
              ariaLabel="Discovery mode"
            />

            <div style={{ position: 'relative', display: 'flex', alignItems: 'center' }}>
              <Search
                size={14}
                style={{
                  position: 'absolute',
                  left: '8px',
                  color: 'var(--text-tertiary)',
                  pointerEvents: 'none',
                }}
              />
              <input
                ref={filterInputRef}
                type="text"
                className="form-input"
                placeholder="Filter machines (/)"
                value={filter}
                onChange={(e) => setFilter(e.target.value)}
                style={{
                  paddingLeft: '28px',
                  height: '30px',
                  width: '180px',
                  fontSize: 'var(--fs-xs)',
                }}
                aria-label="Filter machines by name, environment, or IP"
              />
            </div>

            <button
              type="button"
              className="icon-button"
              style={{ width: '30px', height: '30px' }}
              onClick={refresh}
              disabled={loading}
              title="Refresh inventory (⌘R)"
              aria-label="Refresh inventory"
            >
              <RefreshCw size={14} className={loading ? 'spin' : ''} />
            </button>
          </div>
        }
      />

      <div
        style={{
          flex: 1,
          overflowY: 'auto',
          padding: 'var(--space-5) var(--space-6)',
          display: 'flex',
          flexDirection: 'column',
          gap: 'var(--space-4)',
        }}
      >
        {demo && <DemoBand onSwitchToLive={() => setDemo(false)} />}

        {error && (
          <div className="status-banner error" style={{ flexDirection: 'row', alignItems: 'center', justifyContent: 'space-between' }}>
            <span>Could not read inventory: {error}</span>
            <button type="button" className="secondary-button compact-btn" onClick={refresh}>
              Retry
            </button>
          </div>
        )}

        {!hasData && loading ? (
          <div style={{ display: 'flex', flexDirection: 'column', gap: 'var(--space-4)' }}>
            <Skeleton height="36px" />
            <Skeleton height="80px" />
            <Skeleton height="200px" />
          </div>
        ) : !hasData && error ? (
          <EmptyState
            title="Failed to load inventory"
            description={error}
            actions={
              <button type="button" className="primary-button" onClick={refresh}>
                Retry
              </button>
            }
          />
        ) : inventory ? (
          <div
            style={{
              display: 'flex',
              flexDirection: 'column',
              gap: 'var(--space-4)',
              opacity: error ? 0.6 : 1,
              transition: 'opacity 0.2s ease',
            }}
          >
            <ProviderStrip providers={inventory.providers} />

            <ResourceSummary
              summary={inventory.summary}
              host={inventory.host}
              mode={inventory.mode}
            />

            {inventory.machines.length === 0 ? (
              <EmptyState
                icon={<Server size={24} />}
                title="No virtual machines found"
                description="ClusterDeck looks for Colima, VirtualBox, VMware Fusion, and Vagrant on this Mac."
                actions={
                  <div style={{ display: 'flex', gap: 'var(--space-2)' }}>
                    <button type="button" className="secondary-button" onClick={() => setDemo(true)}>
                      Try demo data
                    </button>
                    <button type="button" className="primary-button" onClick={refresh}>
                      Refresh
                    </button>
                  </div>
                }
              />
            ) : filteredGroups.length === 0 ? (
              <EmptyState
                title="No matching machines"
                description={`No machines match "${filter}".`}
                actions={
                  <button type="button" className="secondary-button" onClick={() => setFilter('')}>
                    Clear filter
                  </button>
                }
              />
            ) : (
              <div className="data-table-container">
                <table className="data-table" aria-busy={isRefreshing}>
                  <caption className="sr-only">Virtual Machine Inventory by Environment</caption>
                  <thead>
                    <tr>
                      <th scope="col" style={{ width: '100px' }}>State</th>
                      <th scope="col">Machine</th>
                      <th scope="col">Resources</th>
                      <th scope="col">Disk</th>
                      <th scope="col">Address</th>
                      <th scope="col">Created</th>
                      <th scope="col">K8s</th>
                    </tr>
                  </thead>
                  {filteredGroups.map(([env, machines]) => (
                    <EnvironmentGroup
                      key={env}
                      environment={env}
                      machines={machines}
                      filter={filter}
                      mode={inventory.mode}
                      expanded={!collapsedEnvs.has(env)}
                      onToggle={() => toggleEnv(env)}
                      onSetUp={handleSetUpCluster}
                      busy={busyEnvs.has(env)}
                    />
                  ))}
                </table>
              </div>
            )}
          </div>
        ) : null}

        <div style={{ marginTop: 'var(--space-2)' }}>
          <LocalRuntimePanel isVisible={isVisible} />
        </div>
      </div>
    </div>
  );
}
