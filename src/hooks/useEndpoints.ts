import { useState, useEffect, useCallback } from 'react';
import {
  api,
  type ConnectionResult,
  type DiscoveredCaView,
  type DnsConflict,
  type DnsSkippedHost,
  type HostsFileStatus,
  type LocalDnsStatus,
  type Profile,
} from '../api/tauri';
import { EMPTY_VERIFICATION } from './useClusterSession';
import { useStatus } from '../state/StatusContext';
import { useProfiles } from '../state/ProfilesContext';
import {
  groupBlockingConflicts,
  hasBlockingConflicts,
  isSafeProfileId,
  type ConflictGroups,
} from '../lib/hostsConflicts';

export function useEndpoints(
  profile: Profile | null,
  lastResult: ConnectionResult | null,
  setLastResult: React.Dispatch<React.SetStateAction<ConnectionResult | null>>,
  trustVersion?: number,
) {
  const { pushStatus } = useStatus();
  const { reload: reloadProfiles } = useProfiles();
  const [conflictGroups, setConflictGroups] = useState<ConflictGroups | null>(null);
  const [resolvingConflicts, setResolvingConflicts] = useState(false);
  const [resolveError, setResolveError] = useState<string | null>(null);
  const [hostsStatus, setHostsStatus] = useState<HostsFileStatus | null>(null);
  const [caViews, setCaViews] = useState<DiscoveredCaView[]>([]);
  const [caActionTarget, setCaActionTarget] = useState<DiscoveredCaView | null>(null);
  const [caActionBusy, setCaActionBusy] = useState(false);
  const [caRemoveTarget, setCaRemoveTarget] = useState<DiscoveredCaView | null>(null);
  const [caRemoveBusy, setCaRemoveBusy] = useState(false);
  const [discoveringEndpoints, setDiscoveringEndpoints] = useState(false);
  const [syncingHosts, setSyncingHosts] = useState(false);
  const [clearingHosts, setClearingHosts] = useState(false);
  // Local-DNS (ADR-0011) outcome of the last sync; empty for Hosts profiles.
  const [dnsConflicts, setDnsConflicts] = useState<DnsConflict[]>([]);
  const [dnsSkipped, setDnsSkipped] = useState<DnsSkippedHost[]>([]);
  const [dnsStatus, setDnsStatus] = useState<LocalDnsStatus | null>(null);
  const [approvingZone, setApprovingZone] = useState<string | null>(null);
  const [scanWarning, setScanWarning] = useState<string | null>(null);

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

  const loadDnsStatus = useCallback(async () => {
    try {
      setDnsStatus(await api.localDnsStatus());
    } catch {
      setDnsStatus(null);
    }
  }, []);

  useEffect(() => {
    if (profile?.host_mapping === 'local_dns') {
      void loadDnsStatus();
    } else {
      setDnsStatus(null);
    }
  }, [profile?.id, profile?.host_mapping, loadDnsStatus]);

  useEffect(() => {
    if (profile?.id) {
      loadHostsStatus(profile.id);
    } else {
      setHostsStatus(null);
    }
    setCaViews([]);
    setCaActionTarget(null);
    setCaRemoveTarget(null);
    setDnsConflicts([]);
    setDnsSkipped([]);
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
      const res = await api.discoverClusterEndpoints(profile.id);
      const eps = res.endpoints;
      setScanWarning(res.warning ?? null);
      setLastResult((prev) => {
        if (!prev) {
          return {
            aliases_written: false,
            kubeconfig: null,
            verification: { ...EMPTY_VERIFICATION, kubernetes: true },
            endpoints: eps,
            errors: res.warning ? [res.warning] : [],
            hosts: [],
          };
        }
        return {
          ...prev,
          endpoints: eps,
          errors: res.warning ? [res.warning] : [],
        };
      });
      try {
        setCaViews(await api.discoverClusterCas(profile.id, eps));
      } catch (caErr) {
        console.error('[ClusterDeck] discoverClusterCas failed during endpoint scan:', caErr);
        setCaViews([]);
      }
      if (res.warning) {
        pushStatus('warning', 'Endpoint Discovery Warning', [
          `Found ${eps.length} external cluster endpoint(s), discovery or cache update was incomplete: ${res.warning}`,
        ]);
      } else {
        pushStatus('success', 'Endpoints Discovered', [
          `Found ${eps.length} external cluster endpoint(s) (APISIX, Ingress, Gateways)`,
        ]);
      }
    } catch (err) {
      setScanWarning(null);
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
      setDnsConflicts(res.dns_conflicts ?? []);
      setDnsSkipped(res.skipped_hosts ?? []);
      void loadDnsStatus();
      if (profile.host_mapping === 'local_dns') {
        const outsideCount = (res.skipped_hosts ?? []).filter(
          (s) => s.reason_kind === 'outside_zones',
        ).length;
        const approvedLower = (profile.approved_dns_zones ?? []).map((az) => az.toLowerCase());
        const skippedZones = Array.from(
          new Set((res.skipped_hosts ?? []).flatMap((s) => s.suggested_zones)),
        ).filter((z) => !approvedLower.includes(z.toLowerCase()));
        const details = [res.message];
        if (outsideCount > 0 && skippedZones.length > 0) {
          details.push(
            `${outsideCount} endpoint${outsideCount === 1 ? ' is' : 's are'} not served: approve ${skippedZones.join(', ')} to publish ${outsideCount === 1 ? 'it' : 'them'}.`,
          );
        }
        const isWarning = !res.success || Boolean(res.warning);
        pushStatus(isWarning ? 'warning' : 'success', 'Local DNS Synced', details);
      } else {
        pushStatus('success', 'Hosts File Synced', [res.message]);
      }
      const groups = groupBlockingConflicts(res.conflicts ?? [], profile.id);
      if (hasBlockingConflicts(groups)) setConflictGroups(groups);
    } catch (err) {
      pushStatus('error', 'Sync Hosts failed', [String(err)]);
    } finally {
      setSyncingHosts(false);
    }
  }, [profile, setLastResult, loadHostsStatus, loadDnsStatus, pushStatus]);

  const approveZone = useCallback(async (zone: string) => {
    if (!profile) return;
    setApprovingZone(zone);
    try {
      const view = await api.approveDnsZone(profile.id, zone);
      setDnsConflicts(view.conflicts);
      setDnsSkipped(view.skipped_hosts);
      void loadDnsStatus();
      await reloadProfiles();
      if (view.cancelled) {
        pushStatus('warning', 'Zone Approval Incomplete', [
          `Admin approval was cancelled. ${zone} was added to approved zones, but local DNS was not updated.`,
        ]);
      } else if (view.conflicts.length > 0) {
        pushStatus('warning', 'DNS Zone Approved with Conflicts', [
          `${zone} was approved, but DNS conflicts need resolution: ${view.conflicts.map((c) => c.zone).join(', ')}.`,
        ]);
      } else {
        const recordCount = view.zone_records?.[zone] ?? 0;
        if (scanWarning) {
          pushStatus('warning', 'DNS Zone Approved with Warning', [
            `${zone} approved, but the last scan was incomplete (${scanWarning}); press Scan or Sync.`,
          ]);
        } else if (recordCount > 0) {
          pushStatus('success', 'DNS Zone Approved', [
            `${zone} is now served by local DNS for ${profile.name}.`,
          ]);
        } else {
          pushStatus('warning', 'DNS Zone Approved', [
            `${zone} approved, but no endpoints are cached yet; press Scan or Sync.`,
          ]);
        }
      }
    } catch (err) {
      pushStatus('error', 'Approve zone failed', [String(err)]);
    } finally {
      setApprovingZone(null);
    }
  }, [profile, reloadProfiles, loadDnsStatus, pushStatus, scanWarning]);

  const clearHosts = useCallback(async () => {
    if (!profile) return;
    setClearingHosts(true);
    try {
      const res = await api.removeHostsFile(profile.id);
      await loadHostsStatus(profile.id);
      if (profile.host_mapping === 'local_dns') {
        setDnsConflicts([]);
        setDnsSkipped([]);
        void loadDnsStatus();
        pushStatus('success', 'Removed from local DNS', [res.message]);
      } else {
        pushStatus('success', 'Cleared from /etc/hosts', [res.message]);
      }
    } catch (err) {
      pushStatus('error', profile.host_mapping === 'local_dns' ? 'Remove from local DNS failed' : 'Clear /etc/hosts failed', [String(err)]);
    } finally {
      setClearingHosts(false);
    }
  }, [profile, loadHostsStatus, loadDnsStatus, pushStatus]);

  const openConflictResolver = useCallback(() => {
    if (!profile) return;
    const groups = groupBlockingConflicts(hostsStatus?.conflicts ?? [], profile.id);
    if (hasBlockingConflicts(groups)) setConflictGroups(groups);
  }, [profile, hostsStatus]);

  const closeConflictResolver = useCallback(() => {
    setConflictGroups(null);
    setResolveError(null);
  }, []);

  // Sequential on purpose: each removal shows its own admin prompt, and the first
  // failure must stop the rest so the user is not asked to authorize more work.
  const resolveConflicts = useCallback(async () => {
    if (!profile || !conflictGroups) return;
    setResolveError(null);
    setResolvingConflicts(true);
    const removed: string[] = [];
    try {
      for (const { ownerId } of conflictGroups.owners) {
        if (ownerId === profile.id || !isSafeProfileId(ownerId)) continue;
        await api.removeHostsFile(ownerId);
        removed.push(ownerId);
        // Drop it right away so a retry after a later failure never re-removes it.
        setConflictGroups((prev) =>
          prev ? { ...prev, owners: prev.owners.filter((o) => o.ownerId !== ownerId) } : prev,
        );
      }
      setConflictGroups(null);
      setResolveError(null);
      pushStatus('success', 'Removed conflicting /etc/hosts blocks', [
        removed.length > 0 ? `Removed ClusterDeck blocks for: ${removed.join(', ')}` : 'Nothing to remove.',
      ]);
    } catch (err) {
      setResolveError(String(err));
      pushStatus('error', 'Remove conflicting block failed', [
        ...(removed.length > 0 ? [`Already removed: ${removed.join(', ')}`] : []),
        String(err),
      ]);
    } finally {
      await loadHostsStatus(profile.id);
      setResolvingConflicts(false);
    }
  }, [profile, conflictGroups, loadHostsStatus, pushStatus]);

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
    dnsConflicts,
    dnsSkipped,
    dnsStatus,
    approvingZone,
    approveZone,
    conflictGroups,
    resolvingConflicts,
    resolveError,
    openConflictResolver,
    closeConflictResolver,
    resolveConflicts,
    openSshSession,
    openUrl,
  };
}
