export type PillVariant = 'success' | 'warning' | 'danger' | 'info' | 'idle';

export type StatusPillConfig = {
  variant: PillVariant;
  label: string;
};

export function machineStatePill(state: string): StatusPillConfig {
  const s = state.toLowerCase();
  if (s === 'running') {
    return { variant: 'success', label: 'running' };
  }
  if (s === 'stale') {
    return { variant: 'warning', label: 'stale' };
  }
  if (s === 'stopped' || s === 'poweroff' || s === 'saved' || s === 'paused') {
    return { variant: 'idle', label: state };
  }
  // Anything unrecognized: idle with raw string (never guess ok)
  return { variant: 'idle', label: state || 'unknown' };
}

export function providerPill(status: string): StatusPillConfig {
  const s = status.toLowerCase();
  if (s === 'available') {
    return { variant: 'success', label: 'available' };
  }
  if (s === 'demo') {
    return { variant: 'warning', label: 'demo' };
  }
  if (s === 'error') {
    return { variant: 'danger', label: 'error' };
  }
  if (s === 'not-installed') {
    return { variant: 'idle', label: 'not installed' };
  }
  return { variant: 'idle', label: status };
}

export function k8sPill(kubernetes: string): StatusPillConfig {
  const k = kubernetes.trim().toLowerCase();
  if (!k || k === 'unknown' || k === 'none' || k === 'no' || k === 'not detected') {
    return { variant: 'idle', label: kubernetes || 'none' };
  }
  return { variant: 'info', label: kubernetes };
}
