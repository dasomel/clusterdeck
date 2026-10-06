export type AuthMode = 'key' | 'password';

export type Host = {
  name: string;
  address: string;
  port: number;
  user: string;
  identity_file: string | null;
  auth: AuthMode;
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

export type LocalRuntimeSource = {
  provider: 'colima' | 'lima';
  instance: string;
};

export type Profile = {
  id: string;
  name: string;
  hosts: Host[];
  bastion: Bastion | null;
  bootstrap: BootstrapPolicy;
  kubeconfig: KubeconfigSource | null;
  manage_hosts_file: boolean;
  trusted_cas: TrustedCa[];
  local_runtime?: LocalRuntimeSource | null;
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

export type CaTrustStatus = 'new' | 'trusted' | 'rotated';

export type DiscoveredCaView = {
  secret_ref: string;
  source_hosts: string[];
  subject_cn: string;
  not_after: string;
  fingerprint_sha256: string;
  status: CaTrustStatus;
  warnings: string[];
};

export type TrustedCa = {
  secret_ref: string;
  fingerprint_sha256: string;
  fingerprint_sha1: string;
  subject_cn: string;
  not_after: string;
  trusted_at: string;
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
  arch: string | null;
  cpus: number | null;
  memory_bytes: number | null;
  disk_bytes: number | null;
  docker_context: string | null;
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

export type LocalRuntimeLifecycleProvider = 'colima' | 'lima';

export type LifecycleActionResult = {
  success: boolean;
  message: string;
};

// Inventory domain types matching backend serde contracts (D8: snake_case)
export type HostInfo = {
  platform: string;
  architecture: string;
  cpu: number | null;
  memory_gib: number | null;
};

export type Machine = {
  id: string;
  runtime_id: string | null;
  name: string;
  runtime: string;
  orchestrator: string | null;
  environment: string;
  state: string;
  cpu: number | null;
  memory_gib: number | null;
  disk_gib: number | null;
  disk_used_gib: number | null;
  created_at: number | null;
  ips: string[];
  kubernetes: string;
};

export type ProviderStatus = {
  id: string;
  label: string;
  status: string;
  message: string | null;
};

export type InventorySummary = {
  running: number;
  cpu: number;
  memory_gib: number;
  disk_gib: number;
  unknown_resources: number;
  warnings: string[];
};

export type Inventory = {
  mode: string;
  timestamp: string;
  host: HostInfo;
  machines: Machine[];
  providers: ProviderStatus[];
  summary: InventorySummary;
};

export type EnvironmentProfileResult = {
  profile_id: string;
  created: boolean;
  updated_hosts: string[];
};
