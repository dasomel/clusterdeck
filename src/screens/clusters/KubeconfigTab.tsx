import { useState } from 'react';
import { FolderOpen, FilePlus, RefreshCw, ExternalLink } from 'lucide-react';
import { api, type Profile } from '../../api/tauri';
import DefinitionList, { type DefinitionItem } from '../../components/ui/DefinitionList';
import EmptyState from '../../components/ui/EmptyState';

type KubeconfigTabProps = {
  profile: Profile;
  merging: boolean;
  onMerge: () => Promise<void>;
  onManage: () => void;
};

export default function KubeconfigTab({ profile, merging, onMerge, onManage }: KubeconfigTabProps) {
  const [openingFinder, setOpeningFinder] = useState(false);

  const kc = profile.kubeconfig;

  const handleOpenInFinder = async () => {
    if (!kc) return;
    setOpeningFinder(true);
    try {
      await api.openPathInFinder(kc.local_path);
    } catch {
      // ignore
    } finally {
      setOpeningFinder(false);
    }
  };

  if (!kc) {
    return (
      <EmptyState
        title="No kubeconfig configured for this cluster."
        description="Add a kubeconfig source in Edit Cluster to fetch and merge Kubernetes credentials."
      />
    );
  }

  const items: DefinitionItem[] = [
    { term: 'Context Name', detail: <span className="mono">{kc.context}</span> },
    { term: 'Control Plane Host', detail: <span className="mono">{kc.control_plane}</span> },
    { term: 'Remote Path', detail: <span className="mono">{kc.remote_path}</span> },
    {
      term: 'Local Path',
      detail: (
        <div style={{ display: 'flex', alignItems: 'center', gap: '8px' }}>
          <span className="mono" style={{ fontSize: '11px', wordBreak: 'break-all' }}>{kc.local_path}</span>
          <button
            type="button"
            className="icon-button"
            style={{ width: '22px', height: '22px', flexShrink: 0 }}
            title="Reveal in Finder"
            aria-label="Reveal in Finder"
            onClick={handleOpenInFinder}
            disabled={openingFinder}
          >
            <FolderOpen size={12} />
          </button>
        </div>
      ),
    },
  ];

  return (
    <div style={{ display: 'flex', flexDirection: 'column', gap: '16px' }}>
      <div className="panel-card" style={{ padding: '14px 16px' }}>
        <div style={{ fontSize: '11px', fontWeight: 600, color: 'var(--text-tertiary)', textTransform: 'uppercase', marginBottom: '8px' }}>
          Profile Kubeconfig Source
        </div>
        <DefinitionList items={items} />

        <div style={{ marginTop: '16px', paddingTop: '12px', borderTop: '1px solid var(--border)', display: 'flex', justifyContent: 'space-between', alignItems: 'center', flexWrap: 'wrap', gap: '10px' }}>
          <div style={{ fontSize: '12px', color: 'var(--text-secondary)' }}>
            Merge this cluster&apos;s credentials and context into your workstation&apos;s <code>~/.kube/config</code>.
          </div>
          <div style={{ display: 'flex', gap: '8px' }}>
            <button
              type="button"
              className="primary-button"
              style={{ width: 'auto', marginTop: 0, padding: '6px 14px', fontSize: '12px', gap: '6px' }}
              title="Add profile kubeconfig to ~/.kube/config (backs up first)"
              onClick={onMerge}
              disabled={merging}
            >
              {merging ? <RefreshCw size={13} className="spin" /> : <FilePlus size={13} />}
              {merging ? 'Merging…' : 'Merge to ~/.kube/config'}
            </button>
            <button
              type="button"
              className="secondary-button"
              style={{ width: 'auto', marginTop: 0, padding: '6px 12px', fontSize: '12px', gap: '6px' }}
              title="Open the Kubeconfig manager section"
              onClick={onManage}
            >
              <ExternalLink size={13} />
              Manage ~/.kube/config
            </button>
          </div>
        </div>
      </div>
    </div>
  );
}
