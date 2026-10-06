import { useState, useEffect, useCallback, useMemo, useRef } from 'react';
import { api, type ConnectionResult, type DiscoveredCaView, type Profile, type VerificationResult } from '../api/tauri';
import { connectMessage, testMessage } from '../lib/messages';
import { useStatus } from '../state/StatusContext';

export const EMPTY_VERIFICATION: VerificationResult = {
  ssh: false,
  kubeconfig: false,
  kubernetes: false,
  node_count: null,
  kubernetes_version: null,
  api_endpoint: null,
  last_verified: null,
};

export function useClusterSession(
  profile: Profile | null,
  onHostsStatusReload?: () => void | Promise<void>,
  onCaViewsUpdated?: (views: DiscoveredCaView[]) => void,
) {
  const { pushStatus, setStatusMessage } = useStatus();
  const [lastResult, setLastResult] = useState<ConnectionResult | null>(null);
  const [status, setStatus] = useState<VerificationResult | null>(null);
  const [connecting, setConnecting] = useState(false);
  const [testing, setTesting] = useState(false);
  const [sshPassword, setSshPassword] = useState('');
  const [bootstrapPassword, setBootstrapPassword] = useState('');

  const onHostsStatusReloadRef = useRef(onHostsStatusReload);
  onHostsStatusReloadRef.current = onHostsStatusReload;
  const onCaViewsUpdatedRef = useRef(onCaViewsUpdated);
  onCaViewsUpdatedRef.current = onCaViewsUpdated;

  // Reset ephemeral state when profile changes
  useEffect(() => {
    setLastResult(null);
    setStatus(null);
    setSshPassword('');
    setBootstrapPassword('');
  }, [profile?.id]);

  // Load profile verification status on profile select
  const refreshStatus = useCallback(async () => {
    if (!profile) {
      setStatus(null);
      return;
    }
    try {
      const res = await api.getProfileStatus(profile.id);
      setStatus(res);
    } catch {
      setStatus(null);
    }
  }, [profile]);

  useEffect(() => {
    refreshStatus();
  }, [refreshStatus]);

  const needsSshPassword = useMemo(
    () => profile?.hosts.some((h) => h.auth === 'password') ?? false,
    [profile],
  );

  const connect = useCallback(async () => {
    if (!profile) return;
    setConnecting(true);
    try {
      const result = await api.connectProfile(
        profile.id,
        bootstrapPassword || undefined,
        sshPassword || undefined,
      );
      setLastResult(result);
      onHostsStatusReloadRef.current?.();

      try {
        const cas = await api.discoverClusterCas(profile.id, result.endpoints ?? []);
        onCaViewsUpdatedRef.current?.(cas);
      } catch (caErr) {
        console.error('[ClusterDeck] discoverClusterCas failed during connect:', caErr);
        onCaViewsUpdatedRef.current?.([]);
      }

      const msg = connectMessage(result, profile);
      pushStatus(msg.type, msg.title, msg.details);
      await refreshStatus();
    } catch (err) {
      pushStatus('error', 'Connect & Sync error', [String(err)]);
    } finally {
      setConnecting(false);
      setBootstrapPassword('');
      setSshPassword('');
    }
  }, [profile, bootstrapPassword, sshPassword, pushStatus, refreshStatus]);

  const testConnection = useCallback(async () => {
    if (!profile) return;
    setTesting(true);
    try {
      const hosts = await api.probeProfileHosts(profile.id, sshPassword || undefined);
      setLastResult((prev) => ({
        aliases_written: prev?.aliases_written ?? false,
        kubeconfig: prev?.kubeconfig ?? null,
        verification: prev?.verification ?? { ...EMPTY_VERIFICATION },
        endpoints: prev?.endpoints ?? [],
        errors: prev?.errors ?? [],
        hosts,
      }));

      const msg = testMessage(hosts, profile);
      pushStatus(msg.type, msg.title, msg.details);
    } catch (err) {
      pushStatus('error', 'Test Connection error', [String(err)]);
    } finally {
      setTesting(false);
      setSshPassword('');
    }
  }, [profile, sshPassword, pushStatus]);

  return {
    lastResult,
    setLastResult,
    status,
    refreshStatus,
    connecting,
    testing,
    busy: connecting || testing,
    sshPassword,
    setSshPassword,
    bootstrapPassword,
    setBootstrapPassword,
    needsSshPassword,
    connect,
    testConnection,
  };
}
