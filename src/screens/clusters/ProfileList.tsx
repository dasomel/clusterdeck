import { useRef, type KeyboardEvent } from 'react';
import { Plus, Pencil, Trash2 } from 'lucide-react';
import type { Profile } from '../../api/tauri';
import type { RailStep } from '../../lib/rail';
import PatchRail from '../../components/rail/PatchRail';

type ProfileListProps = {
  profiles: Profile[];
  selectedId: string | null;
  stepsMap?: Record<string, RailStep[]>;
  onSelect: (id: string) => void;
  onAdd: () => void;
  onEdit: (profile: Profile) => void;
  onDelete: (profile: Profile) => void;
};

export default function ProfileList({
  profiles,
  selectedId,
  stepsMap = {},
  onSelect,
  onAdd,
  onEdit,
  onDelete,
}: ProfileListProps) {
  const itemRefs = useRef<Record<string, HTMLDivElement | null>>({});

  const handleKeyDown = (e: KeyboardEvent<HTMLDivElement>, index: number) => {
    let nextIndex = index;
    if (e.key === 'ArrowDown') {
      nextIndex = (index + 1) % profiles.length;
    } else if (e.key === 'ArrowUp') {
      nextIndex = (index - 1 + profiles.length) % profiles.length;
    } else if (e.key === 'Home') {
      nextIndex = 0;
    } else if (e.key === 'End') {
      nextIndex = profiles.length - 1;
    } else if (e.key === 'Enter' || e.key === ' ') {
      e.preventDefault();
      const p = profiles[index];
      if (p) onSelect(p.id);
      return;
    } else {
      return;
    }

    e.preventDefault();
    const nextProfile = profiles[nextIndex];
    if (nextProfile) {
      onSelect(nextProfile.id);
      itemRefs.current[nextProfile.id]?.focus();
    }
  };

  return (
    <aside className="cluster-list-pane">
      <div style={{ display: 'flex', justifyContent: 'space-between', alignItems: 'center', marginBottom: '4px' }}>
        <span style={{ fontSize: '11px', fontWeight: 600, color: 'var(--text-tertiary)', textTransform: 'uppercase', letterSpacing: '0.05em' }}>
          Clusters ({profiles.length})
        </span>
        <button
          type="button"
          className="icon-button"
          style={{ width: '24px', height: '24px' }}
          title="Add cluster (⌘N)"
          aria-label="Add cluster"
          onClick={onAdd}
        >
          <Plus size={14} />
        </button>
      </div>

      <div
        role="listbox"
        aria-label="Clusters"
        style={{ display: 'flex', flexDirection: 'column', gap: '4px' }}
      >
        {profiles.map((profile, idx) => {
          const isSelected = profile.id === selectedId;
          const steps = stepsMap[profile.id];
          return (
            <div
              key={profile.id}
              className={`cluster-list-row ${isSelected ? 'selected' : ''}`}
            >
              <div className="cluster-list-row-top">
                <div
                  ref={(el) => { itemRefs.current[profile.id] = el; }}
                  role="option"
                  aria-selected={isSelected}
                  tabIndex={isSelected ? 0 : -1}
                  onClick={() => onSelect(profile.id)}
                  onKeyDown={(e) => handleKeyDown(e, idx)}
                  style={{ flex: 1, minWidth: 0, cursor: 'pointer', outline: 'none' }}
                >
                  <span className="cluster-list-row-name" title={profile.name}>
                    {profile.name}
                  </span>
                </div>

                <div className="cluster-list-row-actions">
                  <button
                    type="button"
                    className="icon-button"
                    style={{ width: '20px', height: '20px', padding: 0 }}
                    title={`Edit ${profile.name}`}
                    aria-label={`Edit ${profile.name}`}
                    onClick={(e) => {
                      e.stopPropagation();
                      onEdit(profile);
                    }}
                  >
                    <Pencil size={11} />
                  </button>
                  <button
                    type="button"
                    className="icon-button"
                    style={{ width: '20px', height: '20px', padding: 0, color: 'var(--danger)' }}
                    title={`Delete ${profile.name}`}
                    aria-label={`Delete ${profile.name}`}
                    onClick={(e) => {
                      e.stopPropagation();
                      onDelete(profile);
                    }}
                  >
                    <Trash2 size={11} />
                  </button>
                </div>
              </div>

              <div
                className="cluster-list-row-meta"
                onClick={() => onSelect(profile.id)}
                style={{ cursor: 'pointer' }}
              >
                <span>{profile.hosts.length} hosts · {profile.bastion ? 'Bastion' : 'Direct'}</span>
                {steps && <PatchRail steps={steps} variant="mini" />}
              </div>
            </div>
          );
        })}

        {profiles.length === 0 && (
          <div style={{ color: 'var(--text-tertiary)', fontSize: '12px', padding: '12px 8px', textAlign: 'center' }}>
            No clusters yet. Add one to get started.
          </div>
        )}
      </div>
    </aside>
  );
}
