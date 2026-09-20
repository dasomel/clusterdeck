import { useMemo, useState } from 'react';
import { Plus, Trash2, X, Download, Search, RefreshCw } from 'lucide-react';
import {
  api,
  type Profile,
  type Host,
  type Bastion,
  type LocalKubeContext,
  type DiscoveredLocalHost,
} from '../api/tauri';

export type ProfileEditorProps = {
  initial: Profile | null;
  onClose: () => void;
  onSaved: (profile: Profile) => void;
  onDeleteRequest: (profile: Profile) => void;
};

const deriveProjectName = (item: DiscoveredLocalHost) =>
  item.instance_name.includes('/') ? item.instance_name.split('/')[0] : item.instance_name;

const deriveBaseId = (provider: string, projectName: string) =>
  `${provider.toLowerCase()}-${projectName}`.replace(/[^a-z0-9_-]/g, '-');

const deriveFormattedName = (provider: string, projectName: string) =>
  projectName === 'default' ? `${provider} Local` : `${projectName} (${provider})`;

export default function ProfileEditor({ initial, onClose, onSaved, onDeleteRequest }: ProfileEditorProps) {
  const isEditing = initial !== null;

  const [id, setId] = useState(initial?.id ?? '');
  const [name, setName] = useState(initial?.name ?? '');
  const [hosts, setHosts] = useState<Host[]>(
    initial?.hosts ? JSON.parse(JSON.stringify(initial.hosts)) : []
  );

  const [useBastion, setUseBastion] = useState(Boolean(initial?.bastion));
  const [bastion, setBastion] = useState<Bastion>(
    initial?.bastion
      ? { ...initial.bastion }
      : { name: 'bastion', address: '', port: 22, user: 'root', identity_file: null }
  );

  const [useBootstrap, setUseBootstrap] = useState(
    initial?.bootstrap?.enabled ?? false
  );
  const [bootstrapRetries, setBootstrapRetries] = useState(
    initial?.bootstrap?.retries ?? 3
  );
  const [bootstrapRetryDelay, setBootstrapRetryDelay] = useState(
    initial?.bootstrap?.retry_delay_secs ?? 5
  );

  const [useKubeconfig, setUseKubeconfig] = useState(
    Boolean(initial?.kubeconfig)
  );
  const [kubeRemotePath, setKubeRemotePath] = useState(
    initial?.kubeconfig?.remote_path ?? '/etc/kubernetes/admin.conf'
  );
  const [kubeControlPlane, setKubeControlPlane] = useState(
    initial?.kubeconfig?.control_plane ?? ''
  );
  const [kubeContext, setKubeContext] = useState(
    initial?.kubeconfig?.context ?? (initial?.id ?? '')
  );

  const [manageHostsFile, setManageHostsFile] = useState(
    initial?.manage_hosts_file ?? false
  );

  const [localKubeContexts, setLocalKubeContexts] = useState<LocalKubeContext[] | null>(null);
  const [loadingContexts, setLoadingContexts] = useState(false);
  const [contextLoadError, setContextLoadError] = useState<string | null>(null);

  const [detectedHosts, setDetectedHosts] = useState<DiscoveredLocalHost[] | null>(null);
  const [detectingLocal, setDetectingLocal] = useState(false);
  const [detectError, setDetectError] = useState<string | null>(null);

  const [saving, setSaving] = useState(false);
  const [saveError, setSaveError] = useState<string | null>(null);

  const handleDelete = () => {
    if (!initial) return;
    onDeleteRequest(initial);
  };

  const handleDetectLocal = async () => {
    setDetectingLocal(true);
    setDetectError(null);
    try {
      const list = await api.detectLocalHosts();
      setDetectedHosts(list);
      if (list.length === 0) {
        setDetectError('No local VMs found (Colima or Lima is not running or not installed).');
      }
    } catch (err) {
      setDetectError(String(err));
    } finally {
      setDetectingLocal(false);
    }
  };

  const applyKubeconfigIfPresent = (source: DiscoveredLocalHost) => {
    if (source.kube_context) {
      setUseKubeconfig(true);
      setKubeControlPlane(source.host_name);
      setKubeContext(source.kube_context);
      if (source.kube_remote_path) {
        setKubeRemotePath(source.kube_remote_path);
      }
    }
  };

  const applyDetectedHost = (detected: DiscoveredLocalHost) => {
    const projectName = deriveProjectName(detected);

    if (!id.trim()) {
      setId(deriveBaseId(detected.provider, projectName));
    }
    if (!name.trim()) {
      setName(deriveFormattedName(detected.provider, projectName));
    }

    const existingIndex = hosts.findIndex(
      (h) => h.name === detected.host_name || (h.address === detected.address && h.port === detected.port)
    );

    const newHost: Host = {
      name: detected.host_name,
      address: detected.address,
      port: detected.port,
      user: detected.user,
      identity_file: detected.identity_file,
    };

    let updatedHosts: Host[];
    if (existingIndex >= 0) {
      updatedHosts = [...hosts];
      updatedHosts[existingIndex] = newHost;
    } else if (hosts.length === 1 && (!hosts[0].address || hosts[0].name.startsWith('host-'))) {
      updatedHosts = [newHost];
    } else {
      updatedHosts = [...hosts, newHost];
    }
    setHosts(updatedHosts);

    applyKubeconfigIfPresent(detected);

    setDetectedHosts(null);
  };

  const applyAllFromGroup = (items: DiscoveredLocalHost[]) => {
    if (items.length === 0) return;
    const first = items[0];
    const projectName = deriveProjectName(first);

    if (!id.trim()) {
      setId(deriveBaseId(first.provider, projectName));
    }
    if (!name.trim()) {
      setName(deriveFormattedName(first.provider, projectName));
    }

    const newHostList: Host[] = items.map((d) => ({
      name: d.host_name,
      address: d.address,
      port: d.port,
      user: d.user,
      identity_file: d.identity_file,
    }));
    setHosts(newHostList);

    const masterHost = items.find(
      (d) => d.host_name.includes('master') || d.host_name.includes('control') || d.kube_context
    ) || first;

    applyKubeconfigIfPresent(masterHost);

    setDetectedHosts(null);
  };

  const groupedDetected = useMemo(() => {
    if (!detectedHosts) return [];
    const groups: { [key: string]: DiscoveredLocalHost[] } = {};
    for (const d of detectedHosts) {
      const groupKey = d.instance_name.includes('/')
        ? `${d.provider}: ${d.instance_name.split('/')[0]}`
        : `${d.provider} (${d.instance_name})`;
      if (!groups[groupKey]) {
        groups[groupKey] = [];
      }
      groups[groupKey].push(d);
    }
    return Object.entries(groups).map(([groupName, items]) => ({
      groupName,
      items,
    }));
  }, [detectedHosts]);

  const handleUseBastionToggle = (enabled: boolean) => {
    setUseBastion(enabled);
    if (enabled && !bastion.name) {
      setBastion({ name: 'bastion', address: '', port: 22, user: 'root', identity_file: null });
    }
  };

  const handleUseKubeconfigToggle = (enabled: boolean) => {
    setUseKubeconfig(enabled);
    if (enabled && !kubeControlPlane && hosts.length > 0) {
      setKubeControlPlane(hosts[0].name);
    }
    if (enabled && !kubeContext && id) {
      setKubeContext(id);
    }
  };

  const addHost = () => {
    const newHostName = `host-${hosts.length + 1}`;
    const updated = [
      ...hosts,
      { name: newHostName, address: '', port: 22, user: 'root', identity_file: null },
    ];
    setHosts(updated);
    if (useKubeconfig && !kubeControlPlane) {
      setKubeControlPlane(newHostName);
    }
  };

  const updateHost = (index: number, field: keyof Host, value: string | number | null) => {
    const updated = [...hosts];
    const oldName = updated[index].name;
    updated[index] = { ...updated[index], [field]: value } as Host;
    setHosts(updated);

    if (field === 'name' && typeof value === 'string' && kubeControlPlane === oldName) {
      setKubeControlPlane(value);
    }
  };

  const removeHost = (index: number) => {
    const targetName = hosts[index]?.name;
    const updated = hosts.filter((_, i) => i !== index);
    setHosts(updated);
    if (targetName && kubeControlPlane === targetName) {
      setKubeControlPlane(updated[0]?.name ?? '');
    }
  };

  const loadLocalContexts = async () => {
    setLoadingContexts(true);
    setContextLoadError(null);
    try {
      const contexts = await api.listLocalKubeContexts();
      setLocalKubeContexts(contexts);
    } catch (err) {
      setContextLoadError(String(err));
    } finally {
      setLoadingContexts(false);
    }
  };

  const handleSave = async () => {
    const trimmedId = id.trim();
    const trimmedName = name.trim();

    if (!trimmedId) {
      setSaveError('Profile ID is required.');
      return;
    }
    if (!isEditing && !/^[a-z0-9_-]+$/.test(trimmedId)) {
      setSaveError('Profile ID must contain only lowercase letters, numbers, hyphens (-), and underscores (_).');
      return;
    }
    if (!trimmedName) {
      setSaveError('Profile Name is required.');
      return;
    }
    if (hosts.length === 0) {
      setSaveError('At least one host is required.');
      return;
    }
    for (let i = 0; i < hosts.length; i++) {
      const h = hosts[i];
      if (!h.name.trim() || !h.address.trim() || !h.user.trim()) {
        setSaveError(`Host #${i + 1} must have a name, address, and user.`);
        return;
      }
    }
    if (useBastion) {
      if (!bastion.name.trim() || !bastion.address.trim() || !bastion.user.trim()) {
        setSaveError('Bastion host must have a name, address, and user.');
        return;
      }
    }
    if (useKubeconfig) {
      if (!kubeRemotePath.trim()) {
        setSaveError('Kubeconfig remote path is required.');
        return;
      }
      if (!kubeControlPlane.trim()) {
        setSaveError('Control plane host must be selected for kubeconfig.');
        return;
      }
      if (!kubeContext.trim()) {
        setSaveError('Kubeconfig context name is required.');
        return;
      }
    }

    const finalProfile: Profile = {
      id: trimmedId,
      name: trimmedName,
      hosts: hosts.map((h) => ({
        name: h.name.trim(),
        address: h.address.trim(),
        port: Number(h.port) || 22,
        user: h.user.trim(),
        identity_file: h.identity_file?.trim() || null,
      })),
      bastion: useBastion
        ? {
            name: bastion.name.trim(),
            address: bastion.address.trim(),
            port: Number(bastion.port) || 22,
            user: bastion.user.trim(),
            identity_file: bastion.identity_file?.trim() || null,
          }
        : null,
      bootstrap: {
        enabled: useBootstrap,
        retries: Number(bootstrapRetries) || 3,
        retry_delay_secs: Number(bootstrapRetryDelay) || 5,
      },
      kubeconfig: useKubeconfig
        ? {
            remote_path: kubeRemotePath.trim(),
            control_plane: kubeControlPlane.trim(),
            local_path: `~/.clusterdeck/kubeconfigs/${trimmedId}.yaml`,
            context: kubeContext.trim(),
          }
        : null,
      manage_hosts_file: manageHostsFile,
    };

    try {
      setSaving(true);
      setSaveError(null);
      await api.saveProfile(finalProfile);
      onSaved(finalProfile);
      onClose();
    } catch (err) {
      setSaveError(String(err));
    } finally {
      setSaving(false);
    }
  };

  const computedLocalKubePath = `~/.clusterdeck/kubeconfigs/${id.trim() || '<id>'}.yaml`;

  return (
    <div className="panel-card editor-panel">
      <div className="modal-header">
        <h3>{isEditing ? 'Edit Profile' : 'Create Profile'}</h3>
        <button type="button" className="icon-button" onClick={onClose} title="Close">
          <X size={18} />
        </button>
      </div>

      <div className="modal-body">
          {/* Identity & Basic Info */}
          <div className="form-section">
            <div className="form-section-title">Profile Identity</div>
            <div className="grid-two-fields">
              <div className="form-group">
                <label className="form-label">Profile Name *</label>
                <input
                  type="text"
                  placeholder="e.g. CKA Lab"
                  value={name}
                  onChange={(e) => setName(e.target.value)}
                  className="form-input"
                />
              </div>
              <div className="form-group">
                <label className="form-label">Profile ID *</label>
                <input
                  type="text"
                  placeholder="e.g. cka-lab"
                  value={id}
                  onChange={(e) => setId(e.target.value)}
                  disabled={isEditing}
                  className={`form-input mono ${isEditing ? 'disabled' : ''}`}
                />
                {!isEditing && (
                  <span className="form-helper">lowercase letters, numbers, -, _ only</span>
                )}
              </div>
            </div>
          </div>

          {/* Hosts List */}
          <div className="form-section">
            <div className="form-section-title">
              <span>Hosts ({hosts.length}) *</span>
              <div style={{ display: 'flex', gap: '8px' }}>
                <button
                  type="button"
                  className="secondary-button compact-btn"
                  onClick={handleDetectLocal}
                  disabled={detectingLocal}
                  title="Detect local VMs (Colima, Lima)"
                >
                  {detectingLocal ? <RefreshCw size={13} className="spin" /> : <Search size={13} />}
                  {detectingLocal ? 'Detecting…' : 'Detect local VM'}
                </button>
                <button type="button" className="secondary-button compact-btn" onClick={addHost}>
                  <Plus size={14} /> Add host
                </button>
              </div>
            </div>

            {detectError && (
              <div className="form-error" style={{ marginBottom: '8px' }}>
                {detectError}
              </div>
            )}

            {detectedHosts !== null && (
              <div className="local-detect-box" style={{ marginBottom: '12px' }}>
                <div className="local-detect-header">
                  <span className="form-label" style={{ margin: 0 }}>
                    Detected Local VMs ({detectedHosts.length})
                  </span>
                  <button
                    type="button"
                    className="icon-button"
                    style={{ width: '22px', height: '22px', padding: 0 }}
                    onClick={() => setDetectedHosts(null)}
                    title="Close"
                  >
                    <X size={13} />
                  </button>
                </div>
                {detectedHosts.length === 0 ? (
                  <p className="form-helper" style={{ margin: '4px 0 0' }}>
                    No local VMs found (Colima, Lima, or Vagrant is not running or not installed).
                  </p>
                ) : (
                  <div className="local-detect-list">
                    {groupedDetected.map(({ groupName, items }) => (
                      <div key={groupName} className="local-detect-group">
                        <div className="local-detect-group-header">
                          <span className="local-detect-group-title">{groupName}</span>
                          {items.length > 1 && (
                            <button
                              type="button"
                              className="secondary-button compact-btn"
                              style={{ fontSize: '11px', padding: '2px 8px' }}
                              onClick={() => applyAllFromGroup(items)}
                            >
                              Apply all {items.length} hosts
                            </button>
                          )}
                        </div>
                        {items.map((d, idx) => (
                          <div key={idx} className="local-detect-row">
                            <div className="local-detect-info">
                              <div className="local-detect-name">
                                <strong>{d.host_name}</strong>
                                <span
                                  className={`pill ${d.status === 'Running' ? 'success' : 'warning'}`}
                                  style={{ fontSize: '9px', padding: '2px 6px' }}
                                >
                                  {d.status}
                                </span>
                                {d.runtime && <span className="local-detect-runtime mono">{d.runtime}</span>}
                              </div>
                              <div className="local-detect-meta mono">
                                {d.user}@{d.address}:{d.port}
                                {d.identity_file && ` · key: ${d.identity_file.split('/').pop()}`}
                                {d.kube_context && ` · k8s: ${d.kube_context}`}
                              </div>
                            </div>
                            <button
                              type="button"
                              className="secondary-button compact-btn"
                              onClick={() => applyDetectedHost(d)}
                            >
                              Apply
                            </button>
                          </div>
                        ))}
                      </div>
                    ))}
                  </div>
                )}
              </div>
            )}

            {hosts.length === 0 ? (
              <p className="form-helper" style={{ margin: 0 }}>
                No hosts added yet. Click &quot;Add host&quot; to configure target machines.
              </p>
            ) : (
              <div className="host-list-editor">
                <div className="host-header-row">
                  <span>Name *</span>
                  <span>Address *</span>
                  <span>Port</span>
                  <span>User *</span>
                  <span>Identity File</span>
                  <span></span>
                </div>
                {hosts.map((host, idx) => (
                  <div key={idx} className="host-row-editor">
                    <input
                      type="text"
                      placeholder="cka-m1"
                      value={host.name}
                      onChange={(e) => updateHost(idx, 'name', e.target.value)}
                      className="form-input mono"
                    />
                    <input
                      type="text"
                      placeholder="192.168.56.10"
                      value={host.address}
                      onChange={(e) => updateHost(idx, 'address', e.target.value)}
                      className="form-input mono"
                    />
                    <input
                      type="number"
                      placeholder="22"
                      value={host.port}
                      onChange={(e) => updateHost(idx, 'port', parseInt(e.target.value, 10) || 22)}
                      className="form-input mono"
                    />
                    <input
                      type="text"
                      placeholder="root"
                      value={host.user}
                      onChange={(e) => updateHost(idx, 'user', e.target.value)}
                      className="form-input mono"
                    />
                    <input
                      type="text"
                      placeholder="~/.ssh/id_rsa (optional)"
                      value={host.identity_file ?? ''}
                      onChange={(e) => updateHost(idx, 'identity_file', e.target.value || null)}
                      className="form-input mono"
                    />
                    <button
                      type="button"
                      className="danger-icon-button"
                      onClick={() => removeHost(idx)}
                      title="Remove host"
                    >
                      <Trash2 size={14} />
                    </button>
                  </div>
                ))}
              </div>
            )}
          </div>

          {/* Bastion Section */}
          <div className="form-section">
            <label className="form-checkbox-label">
              <input
                type="checkbox"
                checked={useBastion}
                onChange={(e) => handleUseBastionToggle(e.target.checked)}
              />
              Use a bastion host
            </label>

            {useBastion && (
              <div className="host-list-editor" style={{ marginTop: '8px' }}>
                <div className="host-header-row">
                  <span>Name *</span>
                  <span>Address *</span>
                  <span>Port</span>
                  <span>User *</span>
                  <span>Identity File</span>
                  <span></span>
                </div>
                <div className="host-row-editor">
                  <input
                    type="text"
                    placeholder="bastion"
                    value={bastion.name}
                    onChange={(e) => setBastion({ ...bastion, name: e.target.value })}
                    className="form-input mono"
                  />
                  <input
                    type="text"
                    placeholder="192.168.56.1"
                    value={bastion.address}
                    onChange={(e) => setBastion({ ...bastion, address: e.target.value })}
                    className="form-input mono"
                  />
                  <input
                    type="number"
                    placeholder="22"
                    value={bastion.port}
                    onChange={(e) => setBastion({ ...bastion, port: parseInt(e.target.value, 10) || 22 })}
                    className="form-input mono"
                  />
                  <input
                    type="text"
                    placeholder="root"
                    value={bastion.user}
                    onChange={(e) => setBastion({ ...bastion, user: e.target.value })}
                    className="form-input mono"
                  />
                  <input
                    type="text"
                    placeholder="~/.ssh/id_rsa (optional)"
                    value={bastion.identity_file ?? ''}
                    onChange={(e) => setBastion({ ...bastion, identity_file: e.target.value || null })}
                    className="form-input mono"
                  />
                  <div></div>
                </div>
              </div>
            )}
          </div>

          {/* Bootstrap Section */}
          <div className="form-section">
            <label className="form-checkbox-label">
              <input
                type="checkbox"
                checked={useBootstrap}
                onChange={(e) => setUseBootstrap(e.target.checked)}
              />
              Enable password bootstrap
            </label>

            {useBootstrap && (
              <div className="grid-two-fields" style={{ marginTop: '8px' }}>
                <div className="form-group">
                  <label className="form-label">Retries</label>
                  <input
                    type="number"
                    value={bootstrapRetries}
                    onChange={(e) => setBootstrapRetries(parseInt(e.target.value, 10) || 3)}
                    className="form-input mono"
                  />
                </div>
                <div className="form-group">
                  <label className="form-label">Retry Delay (seconds)</label>
                  <input
                    type="number"
                    value={bootstrapRetryDelay}
                    onChange={(e) => setBootstrapRetryDelay(parseInt(e.target.value, 10) || 5)}
                    className="form-input mono"
                  />
                </div>
              </div>
            )}
          </div>

          {/* Kubeconfig Section */}
          <div className="form-section">
            <label className="form-checkbox-label">
              <input
                type="checkbox"
                checked={useKubeconfig}
                onChange={(e) => handleUseKubeconfigToggle(e.target.checked)}
              />
              Fetch kubeconfig from this profile
            </label>

            {useKubeconfig && (
              <div className="form-group-stack" style={{ marginTop: '8px' }}>
                <div className="grid-two-fields">
                  <div className="form-group">
                    <label className="form-label">Remote Path *</label>
                    <input
                      type="text"
                      placeholder="/etc/kubernetes/admin.conf"
                      value={kubeRemotePath}
                      onChange={(e) => setKubeRemotePath(e.target.value)}
                      className="form-input mono"
                    />
                  </div>
                  <div className="form-group">
                    <label className="form-label">Control Plane Host *</label>
                    <select
                      value={kubeControlPlane}
                      onChange={(e) => setKubeControlPlane(e.target.value)}
                      className="form-select mono"
                    >
                      <option value="">Select control plane host…</option>
                      {hosts.map((h, i) => (
                        <option key={i} value={h.name}>
                          {h.name || `Host #${i + 1}`}
                        </option>
                      ))}
                    </select>
                  </div>
                </div>

                <div className="grid-two-fields">
                  <div className="form-group">
                    <label className="form-label">Local Kubeconfig Path (auto-managed)</label>
                    <input
                      type="text"
                      value={computedLocalKubePath}
                      disabled
                      className="form-input mono disabled"
                    />
                  </div>
                  <div className="form-group">
                    <label className="form-label">Context Name *</label>
                    <input
                      type="text"
                      placeholder="e.g. cka-lab"
                      value={kubeContext}
                      onChange={(e) => setKubeContext(e.target.value)}
                      className="form-input mono"
                    />
                  </div>
                </div>

                {/* Import from local kubeconfig */}
                <div className="kube-import-box">
                  <div className="kube-import-header">
                    <span className="form-label">Import from local kubeconfig</span>
                    <button
                      type="button"
                      className="secondary-button compact-btn"
                      onClick={loadLocalContexts}
                      disabled={loadingContexts}
                    >
                      <Download size={13} /> {loadingContexts ? 'Loading…' : 'Load contexts'}
                    </button>
                  </div>

                  {contextLoadError && (
                    <div className="form-error" style={{ marginTop: '6px' }}>{contextLoadError}</div>
                  )}

                  {localKubeContexts !== null && (
                    <div className="form-group" style={{ marginTop: '8px' }}>
                      <select
                        className="form-select mono"
                        onChange={(e) => {
                          if (e.target.value) {
                            setKubeContext(e.target.value);
                          }
                        }}
                        defaultValue=""
                      >
                        <option value="" disabled>
                          {localKubeContexts.length === 0
                            ? 'No contexts found in local ~/.kube/config'
                            : 'Select a context to fill context name…'}
                        </option>
                        {localKubeContexts.map((ctx) => (
                          <option key={ctx.context_name} value={ctx.context_name}>
                            {ctx.context_name} ({ctx.server})
                          </option>
                        ))}
                      </select>
                    </div>
                  )}
                </div>
              </div>
            )}
          </div>

          {/* Manage /etc/hosts */}
          <div className="form-section">
            <label className="form-checkbox-label">
              <input
                type="checkbox"
                checked={manageHostsFile}
                onChange={(e) => setManageHostsFile(e.target.checked)}
              />
              Manage /etc/hosts file entries
            </label>
            <p className="form-helper" style={{ marginLeft: '24px', marginTop: '2px' }}>
              Automatically maps profile host aliases in /etc/hosts bracketed by # BEGIN CLUSTERDECK MANAGED and # END CLUSTERDECK MANAGED markers.
            </p>
          </div>
        </div>

        <div className="modal-footer" style={{ display: 'flex', justifyContent: 'space-between', alignItems: 'center' }}>
          <div>
            {isEditing && (
              <button
                type="button"
                className="secondary-button"
                style={{
                  color: 'var(--danger)',
                  borderColor: 'var(--border)',
                  marginTop: 0,
                  width: 'auto',
                  padding: '7px 12px',
                  fontSize: '12px',
                  display: 'inline-flex',
                  alignItems: 'center',
                  gap: '6px',
                }}
                onClick={handleDelete}
                disabled={saving}
              >
                <Trash2 size={13} />
                Delete profile
              </button>
            )}
          </div>
          {saveError && <div className="form-error">{saveError}</div>}
          <div className="modal-footer-actions">
            <button type="button" className="secondary-button" onClick={onClose} disabled={saving}>
              Cancel
            </button>
            <button type="button" className="primary-button" onClick={handleSave} disabled={saving}>
              {saving ? 'Saving…' : 'Save'}
            </button>
          </div>
        </div>
    </div>
  );
}
