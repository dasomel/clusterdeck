import { useEffect, useRef, type KeyboardEvent } from 'react';
import { AlertTriangle, RefreshCw, X } from 'lucide-react';
import type { Profile } from '../api/types';
import type { ConflictGroups } from '../lib/hostsConflicts';

type HostsConflictModalProps = {
  groups: ConflictGroups;
  currentProfileName: string;
  profiles: Profile[];
  busy: boolean;
  error?: string | null;
  onConfirm: () => void;
  onCancel: () => void;
};

const mono = { fontFamily: 'var(--font-mono)' } as const;
const bodyText = {
  fontSize: 'var(--fs-md)',
  color: 'var(--text-secondary)',
  lineHeight: 'var(--lh-body)',
} as const;

export default function HostsConflictModal({
  groups,
  currentProfileName,
  profiles,
  busy,
  error = null,
  onConfirm,
  onCancel,
}: HostsConflictModalProps) {
  const cancelRef = useRef<HTMLButtonElement>(null);
  const dialogRef = useRef<HTMLDivElement>(null);
  const canRemove = groups.owners.length > 0;

  useEffect(() => {
    // Never focus the destructive button: a stray Enter must not remove a block.
    const opener = document.activeElement instanceof HTMLElement ? document.activeElement : null;
    cancelRef.current?.focus();
    return () => {
      if (opener?.isConnected) opener.focus();
    };
  }, []);

  useEffect(() => {
    const onKey = (e: globalThis.KeyboardEvent) => {
      if (e.key === 'Escape' && !busy) {
        e.preventDefault();
        onCancel();
      }
    };
    window.addEventListener('keydown', onKey);
    return () => window.removeEventListener('keydown', onKey);
  }, [busy, onCancel]);

  const trapFocus = (e: KeyboardEvent<HTMLDivElement>) => {
    if (e.key !== 'Tab') return;
    const focusable = dialogRef.current?.querySelectorAll<HTMLElement>('button:not([disabled])');
    if (!focusable || focusable.length === 0) return;
    const first = focusable[0];
    const last = focusable[focusable.length - 1];
    if (e.shiftKey && document.activeElement === first) {
      e.preventDefault();
      last?.focus();
    } else if (!e.shiftKey && document.activeElement === last) {
      e.preventDefault();
      first?.focus();
    }
  };

  return (
    <div className="modal-overlay" onClick={() => !busy && onCancel()}>
      <div
        ref={dialogRef}
        role="dialog"
        aria-modal="true"
        aria-labelledby="hosts-conflict-title"
        className="modal-dialog"
        onClick={(e) => e.stopPropagation()}
        onKeyDown={trapFocus}
      >
        <div className="modal-header" style={{ padding: '0 0 var(--space-3)' }}>
          <div style={{ display: 'flex', alignItems: 'center', gap: 'var(--space-2)' }}>
            <AlertTriangle size={18} style={{ color: 'var(--danger)' }} />
            <h3 id="hosts-conflict-title" style={{ margin: 0, fontSize: 'var(--fs-lg)', fontWeight: 600, color: 'var(--text-primary)' }}>
              Resolve /etc/hosts conflicts
            </h3>
          </div>
          <button
            type="button"
            className="icon-button"
            style={{ width: '26px', height: '26px' }}
            onClick={onCancel}
            disabled={busy}
            title="Cancel"
            aria-label="Cancel"
          >
            <X size={15} />
          </button>
        </div>

        <div style={{ ...bodyText, margin: 'var(--space-3) 0 var(--space-5)', display: 'flex', flexDirection: 'column', gap: 'var(--space-3)' }}>
          {canRemove && (
            <p style={{ margin: 0 }}>
              These entries come first in /etc/hosts, so they answer instead of {currentProfileName}. Remove the
              ClusterDeck block of each profile below from /etc/hosts? The administrator password is requested
              once per profile block removed, and only that profile&apos;s ClusterDeck block is removed.
            </p>
          )}

          {error && (
            <div className="status-banner error" style={{ fontSize: 'var(--fs-sm)' }} role="alert">
              {error}
            </div>
          )}

          {groups.owners.map((o) => {
            const owner = profiles.find((p) => p.id === o.ownerId);
            return (
              <div key={o.ownerId}>
                <div style={{ fontWeight: 600, color: 'var(--text-primary)' }}>
                  {owner?.name ?? o.ownerId} <span style={{ ...mono, fontWeight: 400 }}>({o.ownerId})</span>
                </div>
                <ul style={{ margin: '2px 0 0', paddingLeft: '18px' }}>
                  {o.hosts.map((h) => (
                    <li key={h.hostname}>
                      <span style={mono}>{h.hostname}</span> → <span style={mono}>{h.otherIp}</span>
                    </li>
                  ))}
                </ul>
                {owner?.host_mapping === 'hosts' && (
                  <div className="status-banner warning" style={{ marginTop: 'var(--space-2)', fontSize: 'var(--fs-sm)' }}>
                    Auto-sync is on for this profile, so its next Connect/Sync will add the block back. Turn off
                    auto-sync on {owner.name} to keep it removed.
                  </div>
                )}
              </div>
            );
          })}

          {groups.unmanaged.length > 0 && (
            <div>
              <div style={{ fontWeight: 600, color: 'var(--text-primary)' }}>Unmanaged /etc/hosts lines</div>
              <ul style={{ margin: '2px 0 0', paddingLeft: '18px' }}>
                {groups.unmanaged.map((h) => (
                  <li key={`${h.hostname}-${h.otherIp}`}>
                    <span style={mono}>{h.hostname}</span> → <span style={mono}>{h.otherIp}</span>
                  </li>
                ))}
              </ul>
              <div style={{ marginTop: '2px' }}>
                ClusterDeck never edits these. Edit /etc/hosts manually to remove them.
              </div>
            </div>
          )}
        </div>

        <div className="modal-footer-actions">
          <button ref={cancelRef} type="button" className="secondary-button compact-btn" onClick={onCancel} disabled={busy}>
            {canRemove ? 'Cancel' : 'Close'}
          </button>
          {canRemove && (
            <button type="button" className="danger-button compact-btn" onClick={onConfirm} disabled={busy}>
              {busy && <RefreshCw size={13} className="spin" />}
              {groups.owners.length > 1 ? 'Remove blocks' : 'Remove block'}
            </button>
          )}
        </div>
      </div>
    </div>
  );
}
