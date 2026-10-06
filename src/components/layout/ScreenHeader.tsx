import type { ReactNode } from 'react';
import StatusBanner from '../StatusBanner';
import { useStatus } from '../../state/StatusContext';

type ScreenHeaderProps = {
  title: string;
  actions?: ReactNode;
  meta?: ReactNode;
};

export default function ScreenHeader({ title, actions, meta }: ScreenHeaderProps) {
  const { statusMessage, clearStatus } = useStatus();

  return (
    <>
      <header className="screen-header">
        <div className="screen-header-left">
          <h1 className="screen-header-title">{title}</h1>
          {meta && <div className="screen-header-meta">{meta}</div>}
        </div>
        {actions && <div className="screen-header-actions">{actions}</div>}
      </header>

      {statusMessage && (
        <div className="screen-status-dock">
          <StatusBanner message={statusMessage} onDismiss={clearStatus} />
        </div>
      )}
    </>
  );
}
