import { Terminal, RefreshCw, Pencil, Trash2 } from 'lucide-react';
import type { Profile } from '../../api/tauri';
import PasswordPrompt from './PasswordPrompt';

type ClusterHeaderProps = {
  profile: Profile;
  busy: boolean;
  connecting: boolean;
  testing: boolean;
  needsSshPassword: boolean;
  sshPassword: string;
  bootstrapPassword: string;
  onChangeSshPassword: (val: string) => void;
  onChangeBootstrapPassword: (val: string) => void;
  onConnect: () => void;
  onTest: () => void;
  onEdit: () => void;
  onDelete: () => void;
  connectButtonRef?: React.Ref<HTMLButtonElement>;
};

export default function ClusterHeader({
  profile,
  busy,
  connecting,
  testing,
  needsSshPassword,
  sshPassword,
  bootstrapPassword,
  onChangeSshPassword,
  onChangeBootstrapPassword,
  onConnect,
  onTest,
  onEdit,
  onDelete,
  connectButtonRef,
}: ClusterHeaderProps) {
  return (
    <div style={{ display: 'flex', flexDirection: 'column', gap: '8px' }}>
      <div className="cluster-header">
        <div className="cluster-header-info">
          <h2 className="cluster-header-title">{profile.name}</h2>
          <div className="cluster-header-meta">
            <span className="mono">{profile.id}</span>
            <span>·</span>
            <span>{profile.hosts.length} {profile.hosts.length === 1 ? 'host' : 'hosts'}</span>
            <span>·</span>
            <span>{profile.bastion ? 'Bastion' : 'Direct'}</span>
          </div>
        </div>

        <div className="cluster-header-actions">
          <button
            type="button"
            className="secondary-button"
            style={{ width: 'auto', marginTop: 0, padding: '6px 12px', fontSize: '12px', gap: '6px' }}
            onClick={onTest}
            disabled={busy}
            title="Probe host SSH reachability"
          >
            {testing ? <RefreshCw size={13} className="spin" /> : <Terminal size={13} />}
            {testing ? 'Testing…' : 'Test connection'}
          </button>

          <button
            ref={connectButtonRef}
            type="button"
            className="primary-button"
            style={{ width: 'auto', marginTop: 0, padding: '6px 14px', fontSize: '12px', gap: '6px' }}
            onClick={onConnect}
            disabled={busy}
            title="Bootstrap SSH, fetch kubeconfig, and sync endpoints (Cmd+Enter)"
          >
            {connecting ? <RefreshCw size={13} className="spin" /> : <Terminal size={13} />}
            {connecting ? 'Connecting…' : 'Connect / Sync'}
          </button>

          <button
            type="button"
            className="icon-button"
            title="Edit cluster"
            aria-label="Edit cluster"
            onClick={onEdit}
          >
            <Pencil size={14} />
          </button>

          <button
            type="button"
            className="icon-button"
            style={{ color: 'var(--danger)' }}
            title="Delete cluster"
            aria-label="Delete cluster"
            onClick={onDelete}
          >
            <Trash2 size={14} />
          </button>
        </div>
      </div>

      <PasswordPrompt
        needsSsh={needsSshPassword}
        needsBootstrap={profile.bootstrap.enabled}
        sshPassword={sshPassword}
        bootstrapPassword={bootstrapPassword}
        onChangeSshPassword={onChangeSshPassword}
        onChangeBootstrapPassword={onChangeBootstrapPassword}
      />
    </div>
  );
}
