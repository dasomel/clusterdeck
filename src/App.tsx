import { useState, useMemo } from 'react';
import { StatusProvider, useStatus } from './state/StatusContext';
import { ProfilesProvider, useProfiles } from './state/ProfilesContext';
import { InventoryProvider, useInventory } from './state/InventoryContext';
import { useTheme } from './hooks/useTheme';
import { useSection } from './hooks/useSection';
import { deriveAttention } from './lib/attention';
import NavRail from './components/layout/NavRail';
import ClustersScreen from './screens/clusters/ClustersScreen';
import KubeconfigScreen from './screens/kubeconfig/KubeconfigScreen';
import InfrastructureScreen from './screens/infrastructure/InfrastructureScreen';
import OverviewScreen from './screens/overview/OverviewScreen';

function AppShell() {
  const { effectiveTheme, toggleTheme } = useTheme();
  const { profiles, reload: reloadProfiles, select } = useProfiles();
  const { inventory, refresh: refreshInventory } = useInventory();
  const { clearStatus } = useStatus();

  const [focusConnectFor, setFocusConnectFor] = useState<string | undefined>();

  const handleRefresh = () => {
    clearStatus();
    void reloadProfiles();
    void refreshInventory();
  };

  const { section, setSection, visited } = useSection('clusters', handleRefresh);

  const attentionItems = useMemo(() => {
    return deriveAttention(profiles, {}, inventory);
  }, [profiles, inventory]);

  const badges = useMemo(() => {
    const infraCount = attentionItems.filter((i) => i.target === 'infrastructure').length;
    const clusterCount = attentionItems.filter((i) => i.target === 'clusters').length;
    return {
      ...(infraCount > 0 ? { infrastructure: infraCount } : {}),
      ...(clusterCount > 0 ? { clusters: clusterCount } : {}),
    };
  }, [attentionItems]);

  const handleClusterReady = (profileId: string) => {
    select(profileId);
    setFocusConnectFor(profileId);
    setSection('clusters');
  };

  const handleOpenCluster = (profileId: string) => {
    select(profileId);
    setFocusConnectFor(undefined);
    setSection('clusters');
  };

  const handleConnectCluster = (profileId: string) => {
    select(profileId);
    setFocusConnectFor(profileId);
    setSection('clusters');
  };

  return (
    <div className="app-shell">
      <NavRail
        section={section}
        onSelect={(s) => {
          setFocusConnectFor(undefined);
          setSection(s);
        }}
        badges={badges}
        theme={effectiveTheme}
        onToggleTheme={toggleTheme}
        version={__APP_VERSION__}
      />

      <div style={{ flex: 1, minWidth: 0, height: '100%', position: 'relative' }}>
        {visited.has('overview') && (
          <div className="screen" hidden={section !== 'overview'}>
            <OverviewScreen
              isVisible={section === 'overview'}
              onOpenCluster={handleOpenCluster}
              onConnectCluster={handleConnectCluster}
              onNavigate={(s) => setSection(s)}
              onAddCluster={() => setSection('clusters')}
            />
          </div>
        )}

        {visited.has('infrastructure') && (
          <div className="screen" hidden={section !== 'infrastructure'}>
            <InfrastructureScreen
              isVisible={section === 'infrastructure'}
              onClusterReady={handleClusterReady}
            />
          </div>
        )}

        {visited.has('clusters') && (
          <div className="screen" hidden={section !== 'clusters'}>
            <ClustersScreen
              isVisible={section === 'clusters'}
              onNavigateToKubeconfig={() => setSection('kubeconfig')}
              focusConnectFor={focusConnectFor}
            />
          </div>
        )}

        {visited.has('kubeconfig') && (
          <div className="screen" hidden={section !== 'kubeconfig'}>
            <KubeconfigScreen isVisible={section === 'kubeconfig'} />
          </div>
        )}
      </div>
    </div>
  );
}

export default function App() {
  return (
    <StatusProvider>
      <ProfilesProvider>
        <InventoryProvider>
          <AppShell />
        </InventoryProvider>
      </ProfilesProvider>
    </StatusProvider>
  );
}
