import { CheckCircle2, CircleAlert, X } from 'lucide-react';

export type StatusMessage = {
  type: 'success' | 'warning' | 'error';
  title: string;
  details?: string[];
  time: string;
};

type StatusBannerProps = {
  message: StatusMessage;
  onDismiss: () => void;
};

export default function StatusBanner({ message, onDismiss }: StatusBannerProps) {
  return (
    <section className={`status-banner ${message.type}`}>
      <div className="status-banner-header">
        <div className="status-banner-title">
          {message.type === 'success' && <CheckCircle2 size={16} className="status-ok" />}
          {message.type === 'warning' && <CircleAlert size={16} className="status-warn" />}
          {message.type === 'error' && <CircleAlert size={16} className="status-danger" />}
          <span>{message.title}</span>
        </div>
        <div className="status-banner-meta">
          <span className="status-banner-time">{message.time}</span>
          <button
            type="button"
            className="status-banner-close"
            title="Dismiss"
            onClick={onDismiss}
          >
            <X size={14} />
          </button>
        </div>
      </div>
      {message.details && message.details.length > 0 && (
        <div className="status-banner-details mono">
          {message.details.map((detail, idx) => (
            <div key={idx} className="status-banner-detail-item">{detail}</div>
          ))}
        </div>
      )}
    </section>
  );
}
