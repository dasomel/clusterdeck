import { useState, useMemo, useEffect } from 'react';
import type { Profile } from '../../api/tauri';
import { useProfiles } from '../../state/ProfilesContext';
import { useStatus } from '../../state/StatusContext';
import { deleteProfileConfirm } from '../../lib/messages';
import ProfileList from './ProfileList';
import ClusterDetail from './ClusterDetail';
import ProfileEditor from '../../components/ProfileEditor';
import ConfirmModal from '../../components/ConfirmModal';
import EmptyState from '../../components/ui/EmptyState';

type ClustersScreenProps = {
  onNavigateToKubeconfig: () => void;
  focusConnectFor?: string;
  isVisible?: boolean;
};

export default function ClustersScreen({
  onNavigateToKubeconfig,
  focusConnectFor,
  isVisible = true,
}: ClustersScreenProps) {
  const { profiles, selectedId, selected, select, remove, reload } = useProfiles();
  const { pushStatus } = useStatus();

  const [editorState, setEditorState] = useState<{ open: boolean; profile: Profile | null }>({
    open: false,
    profile: null,
  });
  const [profileToDelete, setProfileToDelete] = useState<Profile | null>(null);
  const [deleting, setDeleting] = useState(false);

  const handleDeleteConfirm = async () => {
    if (!profileToDelete) return;
    setDeleting(true);
    try {
      await remove(profileToDelete);
      pushStatus('success', `Profile "${profileToDelete.name}" deleted`);
      setProfileToDelete(null);
      if (editorState.open && editorState.profile?.id === profileToDelete.id) {
        setEditorState({ open: false, profile: null });
      }
    } catch (err) {
      pushStatus('error', 'Failed to delete profile', [String(err)]);
    } finally {
      setDeleting(false);
    }
  };

  const deleteModalProps = useMemo(() => {
    if (!profileToDelete) return null;
    return deleteProfileConfirm(profileToDelete);
  }, [profileToDelete]);

  // Global shortcuts: Cmd+N opens editor, Esc closes editor if open and no modal (only when screen is visible)
  useEffect(() => {
    if (!isVisible) return;
    const handleKeyDown = (e: KeyboardEvent) => {
      if ((e.metaKey || e.ctrlKey) && (e.key === 'n' || e.key === 'N')) {
        if (document.querySelector('.modal-overlay')) return;
        e.preventDefault();
        setEditorState({ open: true, profile: null });
      } else if (e.key === 'Escape') {
        if (document.querySelector('.modal-overlay')) return;
        if (editorState.open) {
          e.preventDefault();
          setEditorState({ open: false, profile: null });
        }
      }
    };
    window.addEventListener('keydown', handleKeyDown);
    return () => window.removeEventListener('keydown', handleKeyDown);
  }, [isVisible, editorState.open]);

  return (
    <div className="clusters-layout">
      <ProfileList
        profiles={profiles}
        selectedId={selectedId}
        onSelect={(id) => {
          select(id);
          setEditorState({ open: false, profile: null });
        }}
        onAdd={() => setEditorState({ open: true, profile: null })}
        onEdit={(p) => setEditorState({ open: true, profile: p })}
        onDelete={(p) => setProfileToDelete(p)}
      />

      <main style={{ flex: 1, minWidth: 0, height: '100%', overflow: 'hidden' }}>
        {editorState.open ? (
          <div style={{ height: '100%', overflowY: 'auto', padding: '20px 24px' }}>
            <div style={{ marginBottom: '12px', fontSize: '12px', color: 'var(--text-tertiary)' }}>
              Clusters / {editorState.profile?.name ?? 'New Cluster'} / Edit
            </div>
            <ProfileEditor
              initial={editorState.profile}
              onClose={() => setEditorState({ open: false, profile: null })}
              onSaved={() => {
                reload();
                setEditorState({ open: false, profile: null });
              }}
              onDeleteRequest={(p) => setProfileToDelete(p)}
            />
          </div>
        ) : selected ? (
          <ClusterDetail
            key={selected.id}
            profile={selected}
            onEdit={() => setEditorState({ open: true, profile: selected })}
            onDelete={() => setProfileToDelete(selected)}
            onNavigateToKubeconfig={onNavigateToKubeconfig}
            autoFocusConnect={focusConnectFor === selected.id}
            isVisible={isVisible}
          />
        ) : (
          <div style={{ padding: '40px', display: 'grid', placeItems: 'center', height: '100%' }}>
            <EmptyState
              title="No cluster selected"
              description="Select a cluster from the list on the left or add a new cluster."
              actions={
                <button
                  type="button"
                  className="primary-button"
                  onClick={() => setEditorState({ open: true, profile: null })}
                >
                  Add cluster
                </button>
              }
            />
          </div>
        )}
      </main>

      {profileToDelete && deleteModalProps && (
        <ConfirmModal
          title={deleteModalProps.title}
          message={deleteModalProps.message}
          confirmLabel={deleteModalProps.confirmLabel}
          cancelLabel="Cancel"
          isDanger={true}
          busy={deleting}
          onConfirm={handleDeleteConfirm}
          onCancel={() => {
            if (!deleting) setProfileToDelete(null);
          }}
        />
      )}
    </div>
  );
}
