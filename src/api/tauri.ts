import { invoke } from '@tauri-apps/api/core';
import type {
  BackupKubeconfigResult,
  ConnectionResult,
  DiscoveredCaView,
  DiscoveredEndpoint,
  DiscoveredHost,
  DiscoveredLocalHost,
  EnvironmentProfileResult,
  HostStageResult,
  HostsFileStatus,
  Inventory,
  KubeconfigBackupInfo,
  KubeconfigSummary,
  LifecycleActionResult,
  LocalKubeContext,
  LocalRuntimeLifecycleProvider,
  ManagedProfileKubeconfig,
  MergeKubeconfigResult,
  Profile,
  SyncHostsResult,
  TrustedCa,
  UserKubeconfigDetails,
  VerificationResult,
} from './types';

export * from './types';

export const api = {
  listProfiles: () => invoke<Profile[]>('list_profiles'),
  getProfile: (profileId: string) => invoke<Profile>('get_profile_cmd', { profileId }),
  saveProfile: (profile: Profile) => invoke<void>('save_profile', { profile }),
  deleteProfile: (profileId: string) => invoke<void>('delete_profile_cmd', { profileId }),
  discoverHosts: (input: string, port?: number) => invoke<DiscoveredHost[]>('discover_hosts', { input, port }),
  probeProfileHosts: (profileId: string, password?: string) =>
    invoke<HostStageResult[]>('probe_profile_hosts', { profileId, password }),
  bootstrapProfile: (profileId: string, password: string) =>
    invoke<{ host: string; key_deployed: boolean; verified: boolean; detail: string }[]>('bootstrap_profile', { profileId, password }),
  generateAliases: (profileId: string) => invoke<void>('generate_aliases', { profileId }),
  fetchKubeconfig: (profileId: string, password?: string) =>
    invoke<KubeconfigSummary>('fetch_kubeconfig', { profileId, password }),
  verifyProfile: (profileId: string) => invoke<VerificationResult>('verify_profile', { profileId }),
  getProfileStatus: (profileId: string) => invoke<VerificationResult | null>('get_profile_status', { profileId }),
  connectProfile: (profileId: string, bootstrapPassword?: string, password?: string) =>
    invoke<ConnectionResult>('connect_profile', { profileId, bootstrapPassword, password }),
  openSshSession: (profileId: string, hostName: string) => invoke<void>('open_ssh_session', { profileId, hostName }),
  backupKubeconfig: (moveFile?: boolean) =>
    invoke<BackupKubeconfigResult>('backup_kubeconfig', { moveFile }),
  mergeKubeconfigToSystem: (profileId: string, backupFirst?: boolean) =>
    invoke<MergeKubeconfigResult>('merge_kubeconfig_to_system', { profileId, backupFirst }),
  listKubeconfigBackups: () => invoke<KubeconfigBackupInfo[]>('list_kubeconfig_backups'),
  restoreKubeconfigBackup: (filename: string) =>
    invoke<BackupKubeconfigResult>('restore_kubeconfig_backup', { filename }),
  deleteKubeconfigBackup: (filename: string) =>
    invoke<void>('delete_kubeconfig_backup', { filename }),
  getUserKubeconfigDetails: (includeRaw?: boolean) =>
    invoke<UserKubeconfigDetails>('get_user_kubeconfig_details', { includeRaw }),
  setCurrentContext: (contextName: string) =>
    invoke<void>('set_current_context', { contextName }),
  deleteUserKubeContext: (contextName: string) =>
    invoke<void>('delete_user_kube_context', { contextName }),
  listManagedProfileKubeconfigs: () =>
    invoke<ManagedProfileKubeconfig[]>('list_managed_profile_kubeconfigs'),
  openPathInFinder: (path: string) => invoke<void>('open_path_in_finder', { path }),
  listLocalKubeContexts: () => invoke<LocalKubeContext[]>('list_local_kube_contexts_cmd'),
  detectLocalHosts: () => invoke<DiscoveredLocalHost[]>('detect_local_hosts'),
  startLocalRuntime: (provider: LocalRuntimeLifecycleProvider, instanceName: string) =>
    invoke<LifecycleActionResult>('start_local_runtime', { provider, instanceName }),
  stopLocalRuntime: (provider: LocalRuntimeLifecycleProvider, instanceName: string) =>
    invoke<LifecycleActionResult>('stop_local_runtime', { provider, instanceName }),
  restartLocalRuntime: (provider: LocalRuntimeLifecycleProvider, instanceName: string) =>
    invoke<LifecycleActionResult>('restart_local_runtime', { provider, instanceName }),
  openLocalRuntimeShell: (provider: LocalRuntimeLifecycleProvider, instanceName: string) =>
    invoke<void>('open_local_runtime_shell', { provider, instanceName }),
  openLocalRuntimeContext: (provider: LocalRuntimeLifecycleProvider, instanceName: string) =>
    invoke<void>('open_local_runtime_context', { provider, instanceName }),
  discoverClusterEndpoints: (profileId: string) =>
    invoke<DiscoveredEndpoint[]>('discover_cluster_endpoints_cmd', { profileId }),
  syncHostsFile: (profileId: string) =>
    invoke<SyncHostsResult>('sync_hosts_file_cmd', { profileId }),
  getHostsFileStatus: (profileId: string) =>
    invoke<HostsFileStatus>('get_hosts_file_status', { profileId }),
  removeHostsFile: (profileId: string) =>
    invoke<SyncHostsResult>('remove_hosts_file_cmd', { profileId }),
  openUrlInBrowser: (url: string) =>
    invoke<void>('open_url_in_browser', { url }),
  discoverClusterCas: (profileId: string, endpoints: DiscoveredEndpoint[]) =>
    invoke<DiscoveredCaView[]>('discover_cluster_cas_cmd', { profileId, endpoints }),
  trustCa: (profileId: string, secretRef: string) =>
    invoke<TrustedCa>('trust_ca_cmd', { profileId, secretRef }),
  replaceCa: (profileId: string, secretRef: string) =>
    invoke<TrustedCa>('replace_ca_cmd', { profileId, secretRef }),
  removeCa: (profileId: string, secretRef: string) =>
    invoke<void>('remove_ca_cmd', { profileId, secretRef }),
  // Inventory commands (Step 7)
  discoverInventory: (demo: boolean) =>
    invoke<Inventory>('discover_inventory', { demo }),
  createProfileFromEnvironment: (environment: string) =>
    invoke<EnvironmentProfileResult>('create_profile_from_environment', { environment }),
};
