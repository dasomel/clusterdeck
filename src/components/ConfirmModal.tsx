import { useEffect, useRef, type KeyboardEvent } from 'react';
import { AlertTriangle, RefreshCw, X } from 'lucide-react';

type ConfirmModalProps = {
  title: string;
  message: string;
  confirmLabel?: string;
  cancelLabel?: string;
  isDanger?: boolean;
  busy?: boolean;
  onConfirm: () => void;
  onCancel: () => void;
};

export default function ConfirmModal({
  title,
  message,
  confirmLabel = 'Delete',
  cancelLabel = 'Cancel',
  isDanger = true,
  busy = false,
  onConfirm,
  onCancel,
}: ConfirmModalProps) {
  const cancelButtonRef = useRef<HTMLButtonElement>(null);
  const dialogRef = useRef<HTMLDivElement>(null);

  // Esc key closes modal; danger actions focus Cancel button initially
  useEffect(() => {
    cancelButtonRef.current?.focus();

    const handleKeyDown = (e: globalThis.KeyboardEvent) => {
      if (e.key === 'Escape' && !busy) {
        e.preventDefault();
        onCancel();
      }
    };

    window.addEventListener('keydown', handleKeyDown);
    return () => window.removeEventListener('keydown', handleKeyDown);
  }, [busy, onCancel]);

  // Focus trap inside the modal dialog
  const handleDialogKeyDown = (e: KeyboardEvent<HTMLDivElement>) => {
    if (e.key !== 'Tab') return;
    const focusable = dialogRef.current?.querySelectorAll<HTMLElement>(
      'button:not([disabled]), [tabindex]:not([tabindex="-1"])',
    );
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
    <div
      className="modal-overlay"
      onClick={() => {
        if (!busy) onCancel();
      }}
    >
      <div
        ref={dialogRef}
        role="dialog"
        aria-modal="true"
        aria-labelledby="confirm-modal-title"
        className="modal-dialog"
        onClick={(e) => e.stopPropagation()}
        onKeyDown={handleDialogKeyDown}
      >
        <div className="modal-header" style={{ padding: '0 0 var(--space-3)' }}>
          <div style={{ display: 'flex', alignItems: 'center', gap: 'var(--space-2)' }}>
            <AlertTriangle size={18} style={{ color: isDanger ? 'var(--danger)' : 'var(--accent)' }} />
            <h3 id="confirm-modal-title" style={{ margin: 0, fontSize: 'var(--fs-lg)', fontWeight: 600, color: 'var(--text-primary)' }}>
              {title}
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

        <p style={{ fontSize: 'var(--fs-md)', color: 'var(--text-secondary)', lineHeight: 'var(--lh-body)', margin: 'var(--space-3) 0 var(--space-5)' }}>
          {message}
        </p>

        <div className="modal-footer-actions">
          <button
            ref={cancelButtonRef}
            type="button"
            className="secondary-button compact-btn"
            onClick={onCancel}
            disabled={busy}
          >
            {cancelLabel}
          </button>
          <button
            type="button"
            className={`${isDanger ? 'danger-button' : 'primary-button'} compact-btn`}
            onClick={onConfirm}
            disabled={busy}
          >
            {busy && <RefreshCw size={13} className="spin" />}
            {confirmLabel}
          </button>
        </div>
      </div>
    </div>
  );
}
