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
  return (
    <div
      style={{
        position: 'fixed',
        inset: 0,
        backgroundColor: 'rgba(0, 0, 0, 0.55)',
        backdropFilter: 'blur(3px)',
        display: 'flex',
        alignItems: 'center',
        justifyContent: 'center',
        zIndex: 9999,
      }}
      onClick={onCancel}
    >
      <div
        className="panel-card"
        style={{
          width: '420px',
          maxWidth: '90vw',
          padding: '22px 24px',
          backgroundColor: 'var(--bg-elevated)',
          boxShadow: 'var(--shadow-card)',
          borderRadius: '12px',
          border: '1px solid var(--border-strong)',
        }}
        onClick={(e) => e.stopPropagation()}
      >
        <div style={{ display: 'flex', justifyContent: 'space-between', alignItems: 'center', marginBottom: '14px' }}>
          <div style={{ display: 'flex', alignItems: 'center', gap: '8px' }}>
            <AlertTriangle size={18} style={{ color: isDanger ? 'var(--danger)' : 'var(--accent)' }} />
            <h3 style={{ margin: 0, fontSize: '15px', fontWeight: 700, color: 'var(--text-primary)' }}>{title}</h3>
          </div>
          <button
            type="button"
            className="icon-button"
            style={{ width: '26px', height: '26px', padding: 0 }}
            onClick={onCancel}
            disabled={busy}
            title="Cancel"
          >
            <X size={15} />
          </button>
        </div>

        <p style={{ fontSize: '13px', color: 'var(--text-secondary)', lineHeight: '1.5', margin: '0 0 20px' }}>
          {message}
        </p>

        <div style={{ display: 'flex', justifyContent: 'flex-end', gap: '8px' }}>
          <button
            type="button"
            className="secondary-button"
            style={{ width: 'auto', marginTop: 0, padding: '7px 14px', fontSize: '12px' }}
            onClick={onCancel}
            disabled={busy}
          >
            {cancelLabel}
          </button>
          <button
            type="button"
            className="primary-button"
            style={{
              width: 'auto',
              backgroundColor: isDanger ? 'var(--danger)' : 'var(--accent)',
              borderColor: isDanger ? 'var(--danger)' : 'var(--accent)',
              color: '#fff',
              padding: '7px 16px',
              fontSize: '12px',
              display: 'inline-flex',
              alignItems: 'center',
              gap: '6px',
            }}
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
