import { Check, AlertTriangle, X, Radar, KeyRound, Terminal, FileDown, ShieldCheck } from 'lucide-react';
import type { RailStep, RailStepId } from '../../lib/rail';

type PatchRailProps = {
  steps: RailStep[];
  variant?: 'full' | 'mini';
  className?: string;
};

const STEP_ICONS: Record<RailStepId, React.ComponentType<{ size?: number; strokeWidth?: number }>> = {
  discover: Radar,
  bootstrap: KeyRound,
  ssh: Terminal,
  kubeconfig: FileDown,
  verify: ShieldCheck,
};

export default function PatchRail({ steps, variant = 'full', className = '' }: PatchRailProps) {
  if (variant === 'mini') {
    const summary = steps.map((s) => `${s.label} ${s.state}`).join(', ');
    return (
      <div
        className={`patch-rail-mini ${className}`}
        role="img"
        aria-label={`Workflow: ${summary}`}
      >
        {steps.map((step, idx) => (
          <div key={step.id} style={{ display: 'inline-flex', alignItems: 'center' }}>
            <span
              className={`patch-jack-mini ${step.state}`}
              title={`${step.label}: ${step.state}${step.detail ? ` (${step.detail})` : ''}`}
            />
            {idx < steps.length - 1 && <span className="patch-rail-mini-line" />}
          </div>
        ))}
      </div>
    );
  }

  return (
    <ol className={`patch-rail ${className}`} aria-label="Connection workflow steps">
      <div className="patch-rail-track" aria-hidden="true" />
      {steps.map((step) => {
        const Icon = STEP_ICONS[step.id];
        return (
          <li
            key={step.id}
            className="patch-rail-step"
            aria-current={step.state === 'running' ? 'step' : undefined}
          >
            <div
              className={`patch-jack ${step.state}`}
              title={`${step.label}: ${step.state}${step.detail ? ` (${step.detail})` : ''}`}
            >
              {step.state === 'ok' ? (
                <Check size={13} strokeWidth={2.2} />
              ) : step.state === 'warn' ? (
                <AlertTriangle size={12} strokeWidth={2.2} />
              ) : step.state === 'fail' ? (
                <X size={13} strokeWidth={2.2} />
              ) : (
                <Icon size={12} strokeWidth={1.75} />
              )}
            </div>
            <div className={`patch-step-label ${step.state}`}>{step.label}</div>
            {step.detail && <div className="patch-step-detail">{step.detail}</div>}
          </li>
        );
      })}
    </ol>
  );
}
