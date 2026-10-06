import type { ConnectionResult, HostStageResult, Profile } from '../api/tauri';
import type { StatusMessage } from '../components/StatusBanner';

export function connectMessage(result: ConnectionResult, selected: Profile): Omit<StatusMessage, 'time'> {
  const failedHosts = result.hosts.filter((h) => !h.reachable);
  const hasErrors = result.errors.length > 0;
  const k8sVerified = result.verification.kubernetes;

  const details: string[] = [];
  if (failedHosts.length > 0) {
    failedHosts.forEach((h) => {
      const hostConfig = selected.hosts.find((host) => host.name === h.host);
      const target = hostConfig ? `${h.host} (${hostConfig.address}:${hostConfig.port})` : h.host;
      details.push(`Host ${target}: ${h.detail?.trim() || 'SSH connection failed'}`);
    });
  }
  if (result.errors.length > 0) {
    details.push(...result.errors);
  }

  if (failedHosts.length === 0 && !hasErrors && (k8sVerified || !selected.kubeconfig)) {
    const successDetails: string[] = [
      `SSH: ${result.hosts.length} host(s) reachable and config written`,
    ];
    if (selected.kubeconfig) {
      successDetails.push(
        `Kubernetes: verified (${result.verification.kubernetes_version ?? 'unknown'} at ${result.verification.api_endpoint ?? selected.kubeconfig.context})`
      );
    }
    if (result.endpoints && result.endpoints.length > 0) {
      successDetails.push(
        `Endpoints: discovered ${result.endpoints.length} external service(s) (APISIX/Ingress/Gateways)`
      );
      if (selected.manage_hosts_file) {
        successDetails.push('Hosts file: synced cluster endpoints and hosts to /etc/hosts');
      }
    }
    return {
      type: 'success',
      title: 'Connect & Sync completed successfully',
      details: successDetails,
    };
  }

  if (k8sVerified) {
    details.push(
      `Kubernetes: API verified (${result.verification.kubernetes_version ?? ''} at ${result.verification.api_endpoint ?? ''})`
    );
  }
  if (result.endpoints && result.endpoints.length > 0) {
    details.push(
      `Endpoints: discovered ${result.endpoints.length} external service(s) (APISIX/Ingress/Gateways)`
    );
  }
  return {
    type: 'warning',
    title: failedHosts.length === result.hosts.length && !k8sVerified
      ? 'Connect & Sync failed'
      : 'Connect & Sync completed with warnings',
    details: details.length > 0 ? details : undefined,
  };
}

export function testMessage(hosts: HostStageResult[], selected: Profile): Omit<StatusMessage, 'time'> {
  const failedHosts = hosts.filter((h) => !h.reachable);
  if (failedHosts.length === 0) {
    return {
      type: 'success',
      title: 'Test Connection succeeded',
      details: [`All ${hosts.length} host(s) reachable via SSH`],
    };
  }
  const details = failedHosts.map((h) => {
    const hostConfig = selected.hosts.find((host) => host.name === h.host);
    const target = hostConfig ? `${h.host} (${hostConfig.address}:${hostConfig.port})` : h.host;
    return `${target}: ${h.detail?.trim() || 'SSH connection failed'}`;
  });
  return {
    type: 'warning',
    title: `Test Connection: ${failedHosts.length} of ${hosts.length} host(s) unreachable`,
    details,
  };
}

export function deleteProfileConfirm(profile: Profile): { title: string; message: string; confirmLabel: string } {
  return {
    title: `Delete Profile "${profile.name}"?`,
    message: `Are you sure you want to delete profile "${profile.name}" (${profile.id})? This will permanently remove its configuration, SSH alias, and locally synced kubeconfig.`,
    confirmLabel: 'Delete Profile',
  };
}
