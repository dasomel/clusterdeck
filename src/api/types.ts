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

export type HostMapping = 'off' | 'local_dns' | 'hosts';

export type Profile = {
  id: string;
  name: string;
  hosts: Host[];
  bastion: Bastion | null;
  bootstrap: BootstrapPolicy;
  kubeconfig: KubeconfigSource | null;
  host_mapping: HostMapping;
  approved_dns_zones?: string[];
  gateway_address?: string | null;
  sso_username_template?: string | null;
  trusted_cas: TrustedCa[];
  local_runtime?: LocalRuntimeSource | null;
};

export interface InvalidProfileView {
  key: string;
  reason: string;
  fingerprint: string;
}

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
  id?: string;
  host: string;
  ip: string;
  source: string;
  resource_name: string;
  backend_service?: string | null;
  backend_upstream?: string | null;
  sso_oidc?: boolean;
};

export type LoginErrorCode =
  | 'forbidden_caller'
  | 'forbidden'
  | 'not_found'
  | 'backend_mismatch'
  | 'unsupported_service'
  | 'impersonation_refused'
  | 'timeout'
  | 'kubectl_failed';

export type SsoHint = {
  // Realm from the backend pod's issuer env; null when only the APISIX openid-connect plugin is known.
  realm: string | null;
};

export type EndpointLoginOption = {
  endpoint_id: string;
  route_namespace: string;
  route_resource: string;
  host: string;
  namespace: string;
  service_name: string;
  secret_name: string;
  sso?: SsoHint | null;
};

export type LoginAccount = {
  label: string;
  // Secret data key of a realm user; shown when the template made the username differ.
  key: string | null;
  username: string | null;
  password: string;
};

// One source of accounts; a failing source has `error` set and no accounts.
export type LoginGroup = {
  label: string;
  namespace: string;
  secret_name: string;
  accounts: LoginAccount[];
  error: LoginErrorCode | null;
};

export type RevealedEndpointLogin = {
  endpoint_id: string;
  route_namespace: string;
  route_resource: string;
  host: string;
  namespace: string;
  service_name: string;
  groups: LoginGroup[];
};

export type ScanEndpointsResult = {
  endpoints: DiscoveredEndpoint[];
  warning?: string | null;
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

export type HostnameConflict = {
  hostname: string;
  our_ip: string;
  other_ip: string;
  /** Profile id owning the other entry; null for an unmanaged /etc/hosts line. */
  other_owner: string | null;
  /** True when our entry appears first in /etc/hosts (the resolver uses the first match). */
  we_win: boolean;
};

/** ADR-0011: a zone local DNS could not publish. `profile_ids` are the claimants (on-disk owner last). */
export type DnsConflict = {
  zone: string;
  kind:
    | 'unowned'
    | 'owned_by_other_profile'
    | 'invalid_zone'
    | 'unreadable'
    | 'claimed_by_multiple';
  profile_ids: string[];
};

export type SkippedReasonKind =
  | 'outside_zones'
  | 'covered_by_other_profile'
  | 'invalid_dns_name'
  | 'invalid_ipv4';

/** A discovered host local DNS did not publish; `suggested_zones` are approvable parents. */
export type DnsSkippedHost = {
  profile_id: string;
  host: string;
  reason: string;
  reason_kind: SkippedReasonKind;
  suggested_zones: string[];
};

export type SyncHostsResult = {
  success: boolean;
  endpoints_count: number;
  hosts_count: number;
  endpoints: DiscoveredEndpoint[];
  message: string;
  warning?: string | null;
  conflicts: HostnameConflict[];
  /** Absent from older backends; only set for local-DNS profiles. */
  dns_conflicts?: DnsConflict[];
  skipped_hosts?: DnsSkippedHost[];
  dns_prompted?: boolean;
};

export type DnsApplyView = {
  conflicts: DnsConflict[];
  skipped_hosts: DnsSkippedHost[];
  prompted: boolean;
  cancelled: boolean;
  zone_records?: Record<string, number>;
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
  conflicts: HostnameConflict[];
};

export type LocalRuntimeLifecycleProvider = 'colima' | 'lima';

export type LocalRuntimeDeleteProvider = 'colima' | 'lima' | 'vagrant';

export type LifecycleActionResult = {
  success: boolean;
  message: string;
};

/** One thing a delete removes, as the backend planned it (ADR-0012 D11). `name` is the Colima/Lima
 * instance or the Vagrant machine name; the project dir and id are Vagrant only. */
export type DeleteTarget = {
  provider: LocalRuntimeDeleteProvider;
  name: string;
  project_dir: string | null;
  vagrant_id: string | null;
  /** Vagrant destroy only: `index_uuid:provider:provider_id` of the local machine state (D15). */
  machine_identity: string | null;
  delete_kind: DeleteKind;
};

/** A Vagrant cache entry that `global-status --prune` drops. */
export type StaleEntry = {
  vagrant_id: string;
  machine_name: string;
  project_dir: string;
};

/** The exact delete the user confirmed; the backend re-derives it and aborts on any difference. */
export type DeletePlan = {
  targets: DeleteTarget[];
  stale_entries: StaleEntry[];
};

export type MachineDeleteResult = {
  name: string;
  success: boolean;
  message: string;
};

export type GroupDeleteResult = {
  environment: string;
  results: MachineDeleteResult[];
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
  /** Decided by the backend: how "delete" works for this machine, or null when it cannot be deleted. */
  delete_kind: DeleteKind | null;
};

export type DeleteKind = 'destroy' | 'prune';

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

export type LocalDnsZoneView = { zone: string; owner: string; records: number };

export type LocalDnsStatus = {
  agent_loaded: boolean;
  pid: number | null;
  generation: number;
  loaded_generation: number | null;
  last_error: string | null;
  port: number;
  zones: LocalDnsZoneView[];
};

export type SuggestedDnsZones = {
  zones: string[];
  by_host: Record<string, string[]>;
};

export type AppSettings = {
  /** Bundle id; null = system default. */
  browser: string | null;
  terminal: string | null;
};

export type AppChoice = { bundle_id: string; name: string; path: string };

export type InstalledApps = { browsers: AppChoice[]; terminals: AppChoice[] };
