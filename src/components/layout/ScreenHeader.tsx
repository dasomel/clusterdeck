import type { ReactNode } from 'react';
import StatusDock from './StatusDock';

type ScreenHeaderProps = {
  title: string;
  actions?: ReactNode;
  meta?: ReactNode;
};

export default function ScreenHeader({ title, actions, meta }: ScreenHeaderProps) {
  return (
    <>
      <header className="screen-header">
        <div className="screen-header-left">
          <h1 className="screen-header-title">{title}</h1>
          {meta && <div className="screen-header-meta">{meta}</div>}
        </div>
        {actions && <div className="screen-header-actions">{actions}</div>}
      </header>

      <StatusDock />
    </>
  );
}
