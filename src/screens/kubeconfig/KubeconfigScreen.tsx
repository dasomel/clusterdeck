import { useState, useEffect, useCallback } from 'react';
import { RefreshCw, FolderOpen } from 'lucide-react';
import {
  api,
  type KubeconfigBackupInfo,
  type ManagedProfileKubeconfig,
  type Profile,
  type UserKubeconfigDetails,
} from '../../api/tauri';
import { useStatus } from '../../state/StatusContext';
import { useProfiles } from '../../state/ProfilesContext';
import ScreenHeader from '../../components/layout/ScreenHeader';
import Tabs, { type TabItem } from '../../components/ui/Tabs';
import ContextsTab from './ContextsTab';
import BackupsTab from './BackupsTab';
import ManagedTab from './ManagedTab';
import TrustedCasTab from './TrustedCasTab';

export default function KubeconfigScreen({ isVisible = true }: { isVisible?: boolean }) {
  const { pushStatus } = useStatus();
  const { bumpTrust } = useProfiles();
  const [userConfig, setUserConfig] = useState<UserKubeconfigDetails | null>(null);
  const [backups, setBackups] = useState<KubeconfigBackupInfo[]>([]);
  const [managedConfigs, setManagedConfigs] = useState<ManagedProfileKubeconfig[]>([]);
  const [profiles, setProfiles] = useState<Profile[]>([]);
  const [loading, setLoading] = useState(true);
  const [activeTab, setActiveTab] = useState('contexts');

  const reload = useCallback(async () => {
    setLoading(true);
    try {
      const [config, baks, managed, profs] = await Promise.all([
        api.getUserKubeconfigDetails(false),
        api.listKubeconfigBackups(),
        api.listManagedProfileKubeconfigs(),
        api.listProfiles(),
      ]);
      setUserConfig(config);
      setBackups(baks);
      setManagedConfigs(managed);
      setProfiles(profs);
    } catch (err) {
      pushStatus('error', 'Failed to load kubeconfig details', [String(err)]);
    } finally {
      setLoading(false);
    }
  }, [pushStatus]);

  // Kept mounted while hidden; refetch on show so ~/.kube/config details are not stale.
  useEffect(() => {
    if (!isVisible) return;
    reload();
  }, [isVisible, reload]);

  const openInFinder = async (path: string) => {
    try {
      await api.openPathInFinder(path);
    } catch (err) {
      pushStatus('error', 'Failed to open in Finder', [String(err)]);
    }
  };

  const tabs: TabItem[] = [
    {
      id: 'contexts',
      label: `Contexts (${userConfig?.contexts.length ?? 0})`,
      content: <ContextsTab userConfig={userConfig} busy={loading} onReload={reload} />,
    },
    {
      id: 'backups',
      label: `Backups (${backups.length})`,
      content: <BackupsTab backups={backups} busy={loading} onReload={reload} />,
    },
    {
      id: 'managed',
      label: `Managed (${managedConfigs.length})`,
      content: <ManagedTab managedConfigs={managedConfigs} />,
    },
    {
      id: 'trusted-cas',
      label: 'Trusted CAs',
      content: (
        <TrustedCasTab
          profiles={profiles}
          onCaRemoved={(pId) => bumpTrust(pId)}
          onReload={reload}
        />
      ),
    },
  ];

  const metaText = userConfig?.exists
    ? `~/.kube/config (${userConfig.contexts.length} contexts, current: ${userConfig.current_context ?? '—'})`
    : '~/.kube/config (does not exist)';

  return (
    <div className="kubeconfig-layout" style={{ height: '100%', display: 'flex', flexDirection: 'column' }}>
      <ScreenHeader
        title="Kubeconfig"
        meta={<span className="mono">{metaText}</span>}
        actions={
          <div style={{ display: 'flex', gap: '6px' }}>
            {userConfig?.exists && (
              <button
                type="button"
                className="icon-button"
                style={{ width: '28px', height: '28px' }}
                title="Reveal ~/.kube/config in Finder"
                aria-label="Reveal ~/.kube/config in Finder"
                onClick={() => openInFinder(userConfig.path)}
              >
                <FolderOpen size={14} />
              </button>
            )}
            <button
              type="button"
              className="icon-button"
              style={{ width: '28px', height: '28px' }}
              title="Refresh kubeconfig details (⌘R)"
              aria-label="Refresh kubeconfig details"
              onClick={reload}
              disabled={loading}
            >
              <RefreshCw size={14} className={loading ? 'spin' : ''} />
            </button>
          </div>
        }
      />

      <div style={{ flex: 1, overflowY: 'auto', padding: '20px 24px' }}>
        <Tabs tabs={tabs} activeTab={activeTab} onChange={setActiveTab} />
      </div>
    </div>
  );
}
