import { useState, useEffect, useCallback } from 'react';
import {
  api,
  type ConnectionResult,
  type DiscoveredCaView,
  type HostsFileStatus,
  type Profile,
} from '../api/tauri';
import { EMPTY_VERIFICATION } from './useClusterSession';
import { useStatus } from '../state/StatusContext';

export function useEndpoints(
  profile: Profile | null,
  lastResult: ConnectionResult | null,
  setLastResult: React.Dispatch<React.SetStateAction<ConnectionResult | null>>,
  trustVersion?: number,
) {
  const { pushStatus } = useStatus();
  const [hostsStatus, setHostsStatus] = useState<HostsFileStatus | null>(null);
  const [caViews, setCaViews] = useState<DiscoveredCaView[]>([]);
  const [caActionTarget, setCaActionTarget] = useState<DiscoveredCaView | null>(null);
  const [caActionBusy, setCaActionBusy] = useState(false);
  const [caRemoveTarget, setCaRemoveTarget] = useState<DiscoveredCaView | null>(null);
  const [caRemoveBusy, setCaRemoveBusy] = useState(false);
  const [discoveringEndpoints, setDiscoveringEndpoints] = useState(false);
  const [syncingHosts, setSyncingHosts] = useState(false);
  const [clearingHosts, setClearingHosts] = useState(false);

  const loadHostsStatus = useCallback(async (profileId?: string) => {
    const id = profileId ?? profile?.id;
    if (!id) {
      setHostsStatus(null);
      return;
    }
    try {
      const res = await api.getHostsFileStatus(id);
      setHostsStatus(res);
    } catch {
      setHostsStatus(null);
    }
  }, [profile?.id]);

  useEffect(() => {
    if (profile?.id) {
      loadHostsStatus(profile.id);
    } else {
      setHostsStatus(null);
    }
    setCaViews([]);
    setCaActionTarget(null);
    setCaRemoveTarget(null);
  }, [profile?.id, loadHostsStatus]);

  // Refetch CA views when trustVersion increments (e.g. from KubeconfigManager)
  useEffect(() => {
    if (profile?.id && trustVersion && trustVersion > 0) {
      api
        .discoverClusterCas(profile.id, lastResult?.endpoints ?? [])
        .then(setCaViews)
        .catch(() => setCaViews([]));
    }
  }, [profile?.id, trustVersion, lastResult?.endpoints]);

  const discoverEndpoints = useCallback(async () => {
    if (!profile) return;
    setDiscoveringEndpoints(true);
    try {
      const eps = await api.discoverClusterEndpoints(profile.id);
      setLastResult((prev) => {
        if (!prev) {
          return {
            aliases_written: false,
            kubeconfig: null,
            verification: { ...EMPTY_VERIFICATION, kubernetes: true },
            endpoints: eps,
            errors: [],
            hosts: [],
          };
        }
        return { ...prev, endpoints: eps };
      });
      try {
        setCaViews(await api.discoverClusterCas(profile.id, eps));
      } catch (caErr) {
        console.error('[ClusterDeck] discoverClusterCas failed during endpoint scan:', caErr);
        setCaViews([]);
      }
      pushStatus('success', 'Endpoints Discovered', [
        `Found ${eps.length} external cluster endpoint(s) (APISIX, Ingress, Gateways)`,
      ]);
    } catch (err) {
      pushStatus('error', 'Endpoint Discovery failed', [String(err)]);
    } finally {
      setDiscoveringEndpoints(false);
    }
  }, [profile, setLastResult, pushStatus]);

  const executeCaTrustAction = useCallback(async (target: DiscoveredCaView) => {
    if (!profile) return;
    setCaActionBusy(true);
    try {
      if (target.status === 'rotated') {
        await api.replaceCa(profile.id, target.secret_ref);
      } else {
        await api.trustCa(profile.id, target.secret_ref);
      }
      setCaActionTarget(null);
      pushStatus(
        'success',
        target.status === 'rotated' ? 'CA Trust Updated' : 'CA Trusted',
        [
          `${target.subject_cn || target.secret_ref} is now trusted for ${target.source_hosts.length} host(s). Safari/Chrome will stop warning on them.`,
        ],
      );
      try {
        setCaViews(await api.discoverClusterCas(profile.id, lastResult?.endpoints ?? []));
      } catch {
        // ignore
      }
    } catch (err) {
      pushStatus('error', 'CA Trust failed', [String(err)]);
    } finally {
      setCaActionBusy(false);
    }
  }, [profile, lastResult?.endpoints, pushStatus]);

  const executeCaRemoveAction = useCallback(async (target: DiscoveredCaView) => {
    if (!profile) return;
    setCaRemoveBusy(true);
    try {
      await api.removeCa(profile.id, target.secret_ref);
      setCaRemoveTarget(null);
      pushStatus('success', 'CA Trust Removed', [
        `${target.subject_cn || target.secret_ref} is no longer trusted locally. You can re-trust it later if the cluster is rebuilt with a new CA.`,
      ]);
      try {
        setCaViews(await api.discoverClusterCas(profile.id, lastResult?.endpoints ?? []));
      } catch {
        // ignore
      }
    } catch (err) {
      pushStatus('error', 'CA Remove failed', [String(err)]);
    } finally {
      setCaRemoveBusy(false);
    }
  }, [profile, lastResult?.endpoints, pushStatus]);

  const syncHosts = useCallback(async () => {
    if (!profile) return;
    setSyncingHosts(true);
    try {
      const res = await api.syncHostsFile(profile.id);
      setLastResult((prev) => (prev ? { ...prev, endpoints: res.endpoints } : null));
      await loadHostsStatus(profile.id);
      pushStatus('success', 'Hosts File Synced', [res.message]);
    } catch (err) {
      pushStatus('error', 'Sync Hosts failed', [String(err)]);
    } finally {
      setSyncingHosts(false);
    }
  }, [profile, setLastResult, loadHostsStatus, pushStatus]);

  const clearHosts = useCallback(async () => {
    if (!profile) return;
    setClearingHosts(true);
    try {
      const res = await api.removeHostsFile(profile.id);
      await loadHostsStatus(profile.id);
      pushStatus('success', 'Cleared from /etc/hosts', [res.message]);
    } catch (err) {
      pushStatus('error', 'Clear /etc/hosts failed', [String(err)]);
    } finally {
      setClearingHosts(false);
    }
  }, [profile, loadHostsStatus, pushStatus]);

  const openSshSession = useCallback(async (hostName: string) => {
    if (!profile) return;
    try {
      await api.openSshSession(profile.id, hostName);
    } catch (err) {
      pushStatus('error', 'Failed to open SSH session', [String(err)]);
    }
  }, [profile, pushStatus]);

  const openUrl = useCallback(async (url: string) => {
    try {
      await api.openUrlInBrowser(url);
    } catch (err) {
      pushStatus('error', 'Failed to open browser', [String(err)]);
    }
  }, [pushStatus]);

  return {
    hostsStatus,
    loadHostsStatus,
    caViews,
    setCaViews,
    caActionTarget,
    setCaActionTarget,
    caActionBusy,
    caRemoveTarget,
    setCaRemoveTarget,
    caRemoveBusy,
    discoveringEndpoints,
    syncingHosts,
    clearingHosts,
    discoverEndpoints,
    executeCaTrustAction,
    executeCaRemoveAction,
    syncHosts,
    clearHosts,
    openSshSession,
    openUrl,
  };
}
