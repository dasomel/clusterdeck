import { CheckCircle2, AlertTriangle, XCircle, Info, Circle } from 'lucide-react';
import type { ReactNode } from 'react';

export type PillVariant = 'ok' | 'warn' | 'fail' | 'info' | 'idle' | 'skipped' | 'success' | 'warning' | 'danger';

type PillProps = {
  variant?: PillVariant;
  children: ReactNode;
  icon?: ReactNode;
  title?: string;
  className?: string;
};

export default function Pill({ variant = 'idle', children, icon, title, className = '' }: PillProps) {
  // Normalize alias variants
  const normalizedVariant =
    variant === 'success' ? 'ok' : variant === 'warning' ? 'warn' : variant === 'danger' ? 'fail' : variant;

  const defaultIcon = () => {
    switch (normalizedVariant) {
      case 'ok':
        return <CheckCircle2 size={12} strokeWidth={2} />;
      case 'warn':
        return <AlertTriangle size={12} strokeWidth={2} />;
      case 'fail':
        return <XCircle size={12} strokeWidth={2} />;
      case 'info':
        return <Info size={12} strokeWidth={2} />;
      case 'skipped':
      case 'idle':
      default:
        return <Circle size={8} strokeWidth={2} />;
    }
  };

  return (
    <span className={`pill ${normalizedVariant} ${className}`} title={title}>
      {icon ?? defaultIcon()}
      <span>{children}</span>
    </span>
  );
}
