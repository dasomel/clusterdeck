import { invoke } from '@tauri-apps/api/core';

export type Host = {
  name: string;
  address: string;
  port: number;
  user: string;
  identity_file: string | null;
};

export type Bastion = {
  name: string;
  address: string;
  port: number;
  user: string;
  identity_file: string | null;
};

export type BootstrapPolicy = {
  enabled: boolean;
  retries: number;
  retry_delay_secs: number;
};

export type KubeconfigSource = {
  remote_path: string;
  control_plane: string;
  local_path: string;
  context: string;
};

export type Profile = {
  id: string;
  name: string;
  hosts: Host[];
  bastion: Bastion | null;
  bootstrap: BootstrapPolicy;
  kubeconfig: KubeconfigSource | null;
  manage_hosts_file: boolean;
};

export type HostStageResult = { host: string; reachable: boolean; detail: string };

export type BootstrapResult = { host: string; key_deployed: boolean; verified: boolean; detail: string };

export type KubeconfigSummary = { cluster_name: string; context_name: string; local_path: string };

export type VerificationResult = {
  ssh: boolean;
  kubeconfig: boolean;
  kubernetes: boolean;
  node_count: number | null;
  kubernetes_version: string | null;
  api_endpoint: string | null;
  last_verified: string | null;
};

export type DiscoveredEndpoint = {
  host: string;
  ip: string;
  source: string;
  resource_name: string;
};

export type ConnectionResult = {
  hosts: HostStageResult[];
  aliases_written: boolean;
  kubeconfig: KubeconfigSummary | null;
  verification: VerificationResult;
  endpoints: DiscoveredEndpoint[];
  errors: string[];
};

export type SyncHostsResult = {
  success: boolean;
  endpoints_count: number;
  hosts_count: number;
  endpoints: DiscoveredEndpoint[];
  message: string;
};

export type DiscoveredHost = { address: string; ssh_open: boolean };

export type LocalKubeContext = {
  context_name: string;
  cluster_name: string;
  user_name: string;
  server: string;
};

export type DiscoveredLocalHost = {
  provider: string;
  instance_name: string;
  status: string;
  host_name: string;
  address: string;
  port: number;
  user: string;
  identity_file: string | null;
  runtime: string | null;
  kube_context: string | null;
  kube_remote_path: string | null;
};

export type BackupKubeconfigResult = {
  backed_up: boolean;
  backup_path: string | null;
  message: string;
};

export type MergeKubeconfigResult = {
  success: boolean;
  target_path: string;
  context_name: string;
  clusters_count: number;
  contexts_count: number;
  backup: BackupKubeconfigResult | null;
  message: string;
};

export type KubeconfigBackupInfo = {
  filename: string;
  path: string;
  size_bytes: number;
  modified_at: string;
};

export type KubeContextInfo = {
  name: string;
  cluster: string;
  user: string;
  server: string;
  is_current: boolean;
};

export type UserKubeconfigDetails = {
  path: string;
  exists: boolean;
  size_bytes: number;
  current_context: string | null;
  contexts: KubeContextInfo[];
  raw_yaml: string | null;
};

export type ManagedProfileKubeconfig = {
  profile_id: string;
  profile_name: string;
  path: string;
  exists: boolean;
  size_bytes: number;
  current_context: string | null;
  server: string | null;
};

export type HostsFileStatus = {
  managed_by_profile: boolean;
  is_synced: boolean;
  active_entries: string[];
  pending_entries: string[];
};

export const api = {
  listProfiles: () => invoke<Profile[]>('list_profiles'),
  getProfile: (profileId: string) => invoke<Profile>('get_profile_cmd', { profileId }),
  saveProfile: (profile: Profile) => invoke<void>('save_profile', { profile }),
  deleteProfile: (profileId: string) => invoke<void>('delete_profile_cmd', { profileId }),
  discoverHosts: (input: string, port?: number) => invoke<DiscoveredHost[]>('discover_hosts', { input, port }),
  probeProfileHosts: (profileId: string) => invoke<HostStageResult[]>('probe_profile_hosts', { profileId }),
  bootstrapProfile: (profileId: string, password: string) => invoke<BootstrapResult[]>('bootstrap_profile', { profileId, password }),
  generateAliases: (profileId: string) => invoke<void>('generate_aliases', { profileId }),
  fetchKubeconfig: (profileId: string) => invoke<KubeconfigSummary>('fetch_kubeconfig', { profileId }),
  verifyProfile: (profileId: string) => invoke<VerificationResult>('verify_profile', { profileId }),
  getProfileStatus: (profileId: string) => invoke<VerificationResult | null>('get_profile_status', { profileId }),
  connectProfile: (profileId: string, bootstrapPassword?: string) =>
    invoke<ConnectionResult>('connect_profile', { profileId, bootstrapPassword }),
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
};

