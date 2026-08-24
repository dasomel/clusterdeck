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
};

export type HostStageResult = { host: string; reachable: boolean; detail: string };

export type BootstrapResult = { host: string; key_deployed: boolean; verified: boolean; detail: string };

export type KubeconfigSummary = { cluster_name: string; context_name: string; local_path: string };

export type VerificationResult = {
  ssh: boolean;
  kubeconfig: boolean;
  kubernetes: boolean;
  node_count: number | null;
  api_endpoint: string | null;
  last_verified: string | null;
};

export type ConnectionResult = {
  hosts: HostStageResult[];
  aliases_written: boolean;
  kubeconfig: KubeconfigSummary | null;
  verification: VerificationResult;
  errors: string[];
};

export type DiscoveredHost = { address: string; ssh_open: boolean };

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
};
