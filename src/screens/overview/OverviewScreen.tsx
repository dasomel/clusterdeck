import { useState, useEffect, useCallback } from 'react';
import { Network, Plus, RefreshCw } from 'lucide-react';
import ScreenHeader from '../../components/layout/ScreenHeader';
import EmptyState from '../../components/ui/EmptyState';
import { useProfiles } from '../../state/ProfilesContext';
import { useInventory } from '../../state/InventoryContext';
import { useStatus } from '../../state/StatusContext';
import { api, type VerificationResult } from '../../api/tauri';
import { deriveAttention } from '../../lib/attention';
import FleetRow from './FleetRow';
import InfraGlance from './InfraGlance';
import AttentionList from './AttentionList';

type OverviewScreenProps = {
  onOpenCluster: (id: string) => void;
  onNavigate: (section: 'infrastructure' | 'clusters' | 'kubeconfig') => void;
  onAddCluster: () => void;
  onConnectCluster: (id: string) => void;
  isVisible?: boolean;
};

export default function OverviewScreen({
  onOpenCluster,
  onNavigate,
  onAddCluster,
  onConnectCluster,
  isVisible = true,
}: OverviewScreenProps) {
  const { profiles, reload: reloadProfiles } = useProfiles();
  const { inventory, loading: inventoryLoading, error: inventoryError, refresh: refreshInventory } = useInventory();
  const { clearStatus } = useStatus();

  const [statuses, setStatuses] = useState<Record<string, VerificationResult | null>>({});
  const [loadingStatuses, setLoadingStatuses] = useState(false);
  const [lastChecked, setLastChecked] = useState<string>(new Date().toLocaleTimeString());

  const loadStatuses = useCallback(async () => {
    if (profiles.length === 0) return;
    setLoadingStatuses(true);
    const results: Record<string, VerificationResult | null> = {};

    await Promise.all(
      profiles.map(async (p) => {
        try {
          const status = await api.getProfileStatus(p.id);
          results[p.id] = status;
        } catch {
          results[p.id] = null;
        }
      }),
    );

    setStatuses(results);
    setLoadingStatuses(false);
    setLastChecked(new Date().toLocaleTimeString());
  }, [profiles]);

  useEffect(() => {
    if (!isVisible) return;
    void loadStatuses();
  }, [isVisible, loadStatuses]);

  const handleRefresh = async () => {
    clearStatus();
    await Promise.all([reloadProfiles(), refreshInventory(), loadStatuses()]);
  };

  const attentionItems = deriveAttention(profiles, statuses, inventory);

  return (
    <div style={{ height: '100%', display: 'flex', flexDirection: 'column' }}>
      <ScreenHeader
        title="Overview"
        meta="Cluster fleet and workstation status"
        actions={
          <button
            type="button"
            className="icon-button"
            style={{ width: '30px', height: '30px' }}
            onClick={handleRefresh}
            disabled={loadingStatuses || inventoryLoading}
            title="Refresh overview (⌘R)"
            aria-label="Refresh overview"
          >
            <RefreshCw size={14} className={loadingStatuses || inventoryLoading ? 'spin' : ''} />
          </button>
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
        <div
          style={{
            display: 'grid',
            gridTemplateColumns: 'minmax(0, 1.3fr) minmax(0, 0.7fr)',
            gap: 'var(--space-4)',
          }}
        >
          {/* Clusters Card */}
          <div
            style={{
              padding: 'var(--space-4)',
              background: 'var(--bg-elevated)',
              border: '1px solid var(--border)',
              borderRadius: 'var(--radius-lg)',
              boxShadow: 'var(--shadow-card)',
              display: 'flex',
              flexDirection: 'column',
              gap: 'var(--space-3)',
            }}
          >
            <div style={{ display: 'flex', justifyContent: 'space-between', alignItems: 'center' }}>
              <div style={{ display: 'flex', alignItems: 'center', gap: 'var(--space-2)' }}>
                <Network size={16} style={{ color: 'var(--accent)' }} />
                <h3 style={{ margin: 0, fontSize: 'var(--fs-md)', fontWeight: 600, color: 'var(--text-primary)' }}>
                  Clusters ({profiles.length})
                </h3>
              </div>

              <button
                type="button"
                className="secondary-button compact-btn"
                style={{ marginTop: 0, display: 'inline-flex', alignItems: 'center', gap: '4px' }}
                onClick={onAddCluster}
              >
                <Plus size={13} />
                <span>Add</span>
              </button>
            </div>

            {profiles.length === 0 ? (
              <EmptyState
                title="No clusters yet"
                description="Detect VMs running on this Mac, or add one by hand."
                actions={
                  <div style={{ display: 'flex', gap: 'var(--space-2)' }}>
                    <button
                      type="button"
                      className="primary-button"
                      onClick={() => onNavigate('infrastructure')}
                    >
                      Detect local VMs
                    </button>
                    <button
                      type="button"
                      className="secondary-button"
                      style={{ marginTop: 0 }}
                      onClick={onAddCluster}
                    >
                      Add cluster manually
                    </button>
                  </div>
                }
              />
            ) : (
              <div style={{ display: 'flex', flexDirection: 'column', gap: 'var(--space-2)' }}>
                {profiles.map((p) => (
                  <FleetRow
                    key={p.id}
                    profile={p}
                    status={statuses[p.id] ?? null}
                    onOpen={onOpenCluster}
                    onConnect={onConnectCluster}
                  />
                ))}
              </div>
            )}
          </div>

          {/* Infrastructure Glance Card */}
          <InfraGlance
            inventory={inventory}
            loading={inventoryLoading}
            error={inventoryError}
            onRetry={refreshInventory}
            onOpen={() => onNavigate('infrastructure')}
          />
        </div>

        {/* Needs Attention Section */}
        <AttentionList
          items={attentionItems}
          lastCheckedTime={lastChecked}
          onOpenCluster={onOpenCluster}
          onOpenInfrastructure={() => onNavigate('infrastructure')}
        />
      </div>
    </div>
  );
}
