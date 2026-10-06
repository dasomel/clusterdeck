import type { ConnectionResult, Profile, VerificationResult } from '../api/tauri';
import { count } from './format';

export type RailStepId = 'discover' | 'bootstrap' | 'ssh' | 'kubeconfig' | 'verify';

export type RailStepState = 'idle' | 'running' | 'ok' | 'warn' | 'fail' | 'skipped';

export type RailStep = {
  id: RailStepId;
  label: string;
  state: RailStepState;
  detail?: string;
};

export function deriveRail(
  profile: Profile | null,
  lastResult: ConnectionResult | null,
  status: VerificationResult | null,
  busy = false,
): RailStep[] {
  if (!profile) {
    return [
      { id: 'discover', label: 'Discover', state: 'idle' },
      { id: 'bootstrap', label: 'Bootstrap', state: 'idle' },
      { id: 'ssh', label: 'SSH', state: 'idle' },
      { id: 'kubeconfig', label: 'Kubeconfig', state: 'idle' },
      { id: 'verify', label: 'Verify', state: 'idle' },
    ];
  }

  // Verification result: prefer lastResult.verification if available, otherwise persistent status
  const verification = lastResult?.verification ?? status;

  // 1. Discover
  const hostCount = profile.hosts.length;
  let discoverState: RailStepState = hostCount > 0 ? 'ok' : 'warn';
  let discoverDetail = count(hostCount, 'host');
  if (busy && !lastResult) {
    discoverState = 'running';
  }

  // 2. Bootstrap
  let bootstrapState: RailStepState = 'idle';
  let bootstrapDetail: string | undefined;
  if (!profile.bootstrap.enabled) {
    bootstrapState = 'skipped';
    bootstrapDetail = 'skipped';
  } else if (busy) {
    bootstrapState = 'running';
    bootstrapDetail = 'running';
  } else if (lastResult) {
    const unreachableCount = lastResult.hosts.filter((h) => !h.reachable).length;
    if (unreachableCount === 0 && hostCount > 0) {
      bootstrapState = 'ok';
      bootstrapDetail = 'ready';
    } else {
      bootstrapState = 'warn';
      bootstrapDetail = `${unreachableCount} unreachable`;
    }
  }

  // 3. SSH
  let sshState: RailStepState = 'idle';
  let sshDetail: string | undefined;
  if (busy) {
    sshState = 'running';
    sshDetail = 'probing';
  } else if (lastResult) {
    const reachable = lastResult.hosts.filter((h) => h.reachable).length;
    sshDetail = `${reachable}/${hostCount}`;
    if (reachable === hostCount && hostCount > 0) {
      sshState = 'ok';
    } else if (reachable > 0) {
      sshState = 'warn';
    } else if (hostCount > 0) {
      sshState = 'fail';
    }
  } else if (verification?.ssh) {
    sshState = 'ok';
    sshDetail = `${hostCount}/${hostCount}`;
  }

  // 4. Kubeconfig
  let kubeconfigState: RailStepState = 'idle';
  let kubeconfigDetail: string | undefined;
  if (!profile.kubeconfig) {
    kubeconfigState = 'skipped';
    kubeconfigDetail = 'none';
  } else if (busy) {
    kubeconfigState = 'running';
    kubeconfigDetail = 'fetching';
  } else if (verification?.kubeconfig) {
    kubeconfigState = 'ok';
    kubeconfigDetail = 'synced';
  } else if (lastResult) {
    kubeconfigState = 'warn';
    kubeconfigDetail = 'not synced';
  }

  // 5. Verify
  let verifyState: RailStepState = 'idle';
  let verifyDetail: string | undefined;
  if (!profile.kubeconfig) {
    verifyState = 'skipped';
    verifyDetail = 'none';
  } else if (busy) {
    verifyState = 'running';
    verifyDetail = 'verifying';
  } else if (verification?.kubernetes) {
    verifyState = 'ok';
    verifyDetail = verification.kubernetes_version ?? 'verified';
  } else if (lastResult) {
    verifyState = 'fail';
    verifyDetail = 'failed';
  }

  return [
    { id: 'discover', label: 'Discover', state: discoverState, detail: discoverDetail },
    { id: 'bootstrap', label: 'Bootstrap', state: bootstrapState, detail: bootstrapDetail },
    { id: 'ssh', label: 'SSH', state: sshState, detail: sshDetail },
    { id: 'kubeconfig', label: 'Kubeconfig', state: kubeconfigState, detail: kubeconfigDetail },
    { id: 'verify', label: 'Verify', state: verifyState, detail: verifyDetail },
  ];
}
