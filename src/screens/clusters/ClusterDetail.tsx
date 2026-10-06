import { useState, useRef, useEffect, useCallback } from 'react';
import { api, type DiscoveredCaView, type Profile } from '../../api/tauri';
import { useClusterSession } from '../../hooks/useClusterSession';
import { useEndpoints } from '../../hooks/useEndpoints';
import { useProfiles } from '../../state/ProfilesContext';
import { useStatus } from '../../state/StatusContext';
import { deriveRail } from '../../lib/rail';
import ClusterHeader from './ClusterHeader';
import PatchRail from '../../components/rail/PatchRail';
import Tabs, { type TabItem } from '../../components/ui/Tabs';
import NodesTab from './NodesTab';
import EndpointsTab from './EndpointsTab';
import TrustTab from './TrustTab';
import KubeconfigTab from './KubeconfigTab';

type ClusterDetailProps = {
  profile: Profile;
  onEdit: () => void;
  onDelete: () => void;
  onNavigateToKubeconfig: () => void;
  autoFocusConnect?: boolean;
  isVisible?: boolean;
};

export default function ClusterDetail({
  profile,
  onEdit,
  onDelete,
  onNavigateToKubeconfig,
  autoFocusConnect = false,
  isVisible = true,
}: ClusterDetailProps) {
  const { trustVersion } = useProfiles();
  const { pushStatus } = useStatus();
  const [activeTab, setActiveTab] = useState('nodes');
  const [merging, setMerging] = useState(false);
  const connectButtonRef = useRef<HTMLButtonElement>(null);

  const endpointsRef = useRef<{
    loadHostsStatus: (id?: string) => Promise<void>;
    setCaViews: (views: DiscoveredCaView[]) => void;
  }>({
    loadHostsStatus: async () => {},
    setCaViews: () => {},
  });

  const handleHostsStatusReload = useCallback(() => {
    if (profile?.id) {
      void endpointsRef.current.loadHostsStatus(profile.id);
    }
  }, [profile?.id]);

  const handleCaViewsUpdated = useCallback((views: DiscoveredCaView[]) => {
    endpointsRef.current.setCaViews(views);
  }, []);

  const session = useClusterSession(
    profile,
    handleHostsStatusReload,
    handleCaViewsUpdated,
  );
  const { lastResult, setLastResult, status, refreshStatus } = session;

  const endpoints = useEndpoints(
    profile,
    lastResult,
    setLastResult,
    trustVersion[profile.id],
  );

  endpointsRef.current.loadHostsStatus = endpoints.loadHostsStatus;
  endpointsRef.current.setCaViews = endpoints.setCaViews;

  useEffect(() => {
    if (isVisible && autoFocusConnect && connectButtonRef.current) {
      connectButtonRef.current.focus();
    }
  }, [isVisible, autoFocusConnect]);

  // Cmd+Enter triggers Connect/Sync when detail is open, visible, and not in a modal
  useEffect(() => {
    if (!isVisible) return;
    const handleKeyDown = (e: KeyboardEvent) => {
      if ((e.metaKey || e.ctrlKey) && e.key === 'Enter') {
        if (document.querySelector('.modal-overlay')) return;
        e.preventDefault();
        if (!session.busy) {
          void session.connect();
        }
      }
    };
    window.addEventListener('keydown', handleKeyDown);
    return () => window.removeEventListener('keydown', handleKeyDown);
  }, [isVisible, session]);

  const railSteps = deriveRail(profile, lastResult, status, session.busy);

  const handleMergeKubeconfig = async () => {
    setMerging(true);
    try {
      const res = await api.mergeKubeconfigToSystem(profile.id, true);
      const details = [res.message];
      if (res.backup?.backup_path) {
        details.push(`Safety backup created: ${res.backup.backup_path}`);
      }
      pushStatus('success', 'Added to ~/.kube/config', details);
      const updatedStatus = await api.getProfileStatus(profile.id);
      if (updatedStatus) {
        setLastResult((prev) =>
          prev
            ? { ...prev, verification: updatedStatus }
            : {
                hosts: profile.hosts.map((h) => ({ host: h.name, reachable: false, detail: '' })),
                aliases_written: false,
                kubeconfig: null,
                verification: updatedStatus,
                endpoints: [],
                errors: [],
              },
        );
      }
      await refreshStatus();
    } catch (err) {
      pushStatus('error', 'Merge to ~/.kube/config failed', [String(err)]);
    } finally {
      setMerging(false);
    }
  };

  const tabs: TabItem[] = [
    {
      id: 'nodes',
      label: 'Nodes',
      content: (
        <NodesTab
          profile={profile}
          lastResult={lastResult}
          hostsStatus={endpoints.hostsStatus}
          onOpenSsh={endpoints.openSshSession}
        />
      ),
    },
    {
      id: 'endpoints',
      label: 'Endpoints',
      content: (
        <EndpointsTab
          profile={profile}
          lastResult={lastResult}
          hostsStatus={endpoints.hostsStatus}
          discoveringEndpoints={endpoints.discoveringEndpoints}
          syncingHosts={endpoints.syncingHosts}
          clearingHosts={endpoints.clearingHosts}
          onScan={endpoints.discoverEndpoints}
          onSync={endpoints.syncHosts}
          onClear={endpoints.clearHosts}
          onOpenUrl={endpoints.openUrl}
        />
      ),
    },
    {
      id: 'trust',
      label: 'Trust',
      content: (
        <TrustTab
          caViews={endpoints.caViews}
          caActionBusy={endpoints.caActionBusy}
          caRemoveBusy={endpoints.caRemoveBusy}
          onTrust={endpoints.executeCaTrustAction}
          onRemove={endpoints.executeCaRemoveAction}
        />
      ),
    },
    {
      id: 'kubeconfig',
      label: 'Kubeconfig',
      content: (
        <KubeconfigTab
          profile={profile}
          merging={merging}
          onMerge={handleMergeKubeconfig}
          onManage={onNavigateToKubeconfig}
        />
      ),
    },
  ];

  return (
    <div className="cluster-detail-pane">
      <ClusterHeader
        profile={profile}
        busy={session.busy}
        connecting={session.connecting}
        testing={session.testing}
        needsSshPassword={session.needsSshPassword}
        sshPassword={session.sshPassword}
        bootstrapPassword={session.bootstrapPassword}
        onChangeSshPassword={session.setSshPassword}
        onChangeBootstrapPassword={session.setBootstrapPassword}
        onConnect={session.connect}
        onTest={session.testConnection}
        onEdit={onEdit}
        onDelete={onDelete}
        connectButtonRef={connectButtonRef}
      />

      <PatchRail steps={railSteps} variant="full" />

      <Tabs tabs={tabs} activeTab={activeTab} onChange={setActiveTab} />
    </div>
  );
}
