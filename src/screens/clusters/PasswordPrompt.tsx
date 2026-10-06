type PasswordPromptProps = {
  needsSsh: boolean;
  needsBootstrap: boolean;
  sshPassword: string;
  bootstrapPassword: string;
  onChangeSshPassword: (val: string) => void;
  onChangeBootstrapPassword: (val: string) => void;
};

export default function PasswordPrompt({
  needsSsh,
  needsBootstrap,
  sshPassword,
  bootstrapPassword,
  onChangeSshPassword,
  onChangeBootstrapPassword,
}: PasswordPromptProps) {
  if (!needsSsh && !needsBootstrap) return null;

  return (
    <div className="password-prompt-row">
      {needsSsh && (
        <div style={{ display: 'flex', alignItems: 'center', gap: '8px' }}>
          <label htmlFor="ssh-password-input" className="form-label" style={{ fontSize: '11px', whiteSpace: 'nowrap' }}>
            SSH Password:
          </label>
          <input
            id="ssh-password-input"
            type="password"
            placeholder="Enter SSH password"
            value={sshPassword}
            onChange={(e) => onChangeSshPassword(e.target.value)}
            className="form-input mono"
            style={{ width: '160px', padding: '4px 8px', fontSize: '12px' }}
            autoComplete="off"
          />
        </div>
      )}

      {needsBootstrap && (
        <div style={{ display: 'flex', alignItems: 'center', gap: '8px' }}>
          <label htmlFor="bootstrap-password-input" className="form-label" style={{ fontSize: '11px', whiteSpace: 'nowrap' }}>
            Bootstrap Password:
          </label>
          <input
            id="bootstrap-password-input"
            type="password"
            placeholder="Enter bootstrap password"
            value={bootstrapPassword}
            onChange={(e) => onChangeBootstrapPassword(e.target.value)}
            className="form-input mono"
            style={{ width: '160px', padding: '4px 8px', fontSize: '12px' }}
            autoComplete="off"
          />
        </div>
      )}

      <span className="password-prompt-hint">Not saved. Cleared after each attempt.</span>
    </div>
  );
}
