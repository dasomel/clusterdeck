import { useState, useEffect, useRef, useCallback } from 'react';
import { KeyRound, Eye, EyeOff, Copy, Check, RefreshCw } from 'lucide-react';
import {
  api,
  type DiscoveredEndpoint,
  type EndpointLoginOption,
  type LoginErrorCode,
  type RevealedEndpointLogin,
} from '../../api/tauri';

const REVEAL_TTL_MS = 60_000;
const CLIPBOARD_CLEAR_MS = 60_000;

export function getEndpointId(ep: DiscoveredEndpoint): string {
  return ep.id && ep.id.length > 0 ? ep.id : `${ep.source}:${ep.resource_name}:${ep.host}`;
}

function formatLoginError(code: string, secretRef: string, serviceName: string): string {
  switch (code.trim() as LoginErrorCode) {
    case 'forbidden':
      return `Your credentials for this cluster cannot read ${secretRef}`;
    case 'not_found':
      return `Secret ${secretRef} (or its credential key) was not found in the cluster`;
    case 'impersonation_refused':
      return 'Service login reveal is refused when kubeconfig uses impersonation (as / as-groups / as-user-extra)';
    case 'backend_mismatch':
      return 'The service behind this endpoint changed; scan endpoints again';
    case 'unsupported_service':
      return `Service ${serviceName} does not have a supported login secret mapping`;
    case 'timeout':
      return `Timed out after 10s waiting for kubectl while reading ${secretRef}`;
    case 'kubectl_failed':
    default:
      return `kubectl failed while reading ${secretRef}`;
  }
}

export type PermissionState = 'checking' | 'permitted' | 'forbidden' | 'unknown';

export function useEndpointLogins(
  profileId: string,
  endpoints: DiscoveredEndpoint[],
  isVisible: boolean,
) {
  const [optionsById, setOptionsById] = useState<Record<string, EndpointLoginOption>>({});
  const [permissionsById, setPermissionsById] = useState<Record<string, PermissionState>>({});
  const [loadingEndpointId, setLoadingEndpointId] = useState<string | null>(null);
  const [revealed, setRevealed] = useState<RevealedEndpointLogin | null>(null);
  // Keys are `<group>:<account>`; a Set so each account's password is masked independently.
  const [visibleKeys, setVisibleKeys] = useState<ReadonlySet<string>>(new Set());
  const [copiedKey, setCopiedKey] = useState<string | null>(null);
  const [clipboardNote, setClipboardNote] = useState<string | null>(null);
  const [errorByEndpoint, setErrorByEndpoint] = useState<{
    endpointId: string;
    message: string;
  } | null>(null);

  const requestGenRef = useRef(0);
  const optionsGenRef = useRef(0);
  const activeProfileIdRef = useRef(profileId);
  const isVisibleRef = useRef(isVisible);
  const cardCollapsedRef = useRef(false);
  const revealTimerRef = useRef<number | null>(null);

  activeProfileIdRef.current = profileId;
  isVisibleRef.current = isVisible;

  const clearRevealTimer = useCallback(() => {
    if (revealTimerRef.current !== null) {
      window.clearTimeout(revealTimerRef.current);
      revealTimerRef.current = null;
    }
  }, []);

  const clearRevealedState = useCallback(() => {
    requestGenRef.current += 1;
    clearRevealTimer();
    setLoadingEndpointId(null);
    setRevealed(null);
    setVisibleKeys(new Set());
    setCopiedKey(null);
    setClipboardNote(null);
    setErrorByEndpoint(null);
  }, [clearRevealTimer]);

  const endpointsKey = JSON.stringify(endpoints.map((ep) => [
    getEndpointId(ep), ep.backend_service, ep.backend_upstream, ep.sso_oidc,
  ]));
  const secretsKey = JSON.stringify(Object.values(optionsById).map((option) => [
    option.endpoint_id, option.namespace, option.service_name, option.secret_name,
  ]));

  // Clear revealed credentials on profile switch, tab/screen change, or unmount (ADR-0014 D4).
  useEffect(() => {
    clearRevealedState();
  }, [profileId, isVisible, endpointsKey, secretsKey, clearRevealedState]);

  useEffect(() => {
    return () => {
      requestGenRef.current += 1;
      clearRevealTimer();
    };
  }, [clearRevealTimer]);

  // Load login-capable endpoints and pre-check RBAC with `kubectl auth can-i`.
  useEffect(() => {
    const gen = ++optionsGenRef.current;
    setOptionsById({});
    setPermissionsById({});
    if (!isVisible || endpoints.length === 0) {
      return;
    }
    const currentProfileId = profileId;

    api
      .endpointLoginOptions(currentProfileId)
      .then((opts) => {
        if (
          gen !== optionsGenRef.current ||
          activeProfileIdRef.current !== currentProfileId ||
          !isVisibleRef.current
        ) {
          return;
        }
        const nextMap: Record<string, EndpointLoginOption> = {};
        const initPerms: Record<string, PermissionState> = {};
        for (const opt of opts) {
          nextMap[opt.endpoint_id] = opt;
          initPerms[opt.endpoint_id] = 'checking';
        }
        setOptionsById(nextMap);
        setPermissionsById(initPerms);

        for (const opt of opts) {
          api
            .checkEndpointLogin(currentProfileId, opt.endpoint_id)
            .then((checked) => {
              if (
                gen !== optionsGenRef.current ||
                activeProfileIdRef.current !== currentProfileId ||
                !isVisibleRef.current
              ) {
                return;
              }
              // The backend resolves the real Secret from the workload; show that name.
              setOptionsById((prev) => ({ ...prev, [opt.endpoint_id]: checked }));
              setPermissionsById((prev) => ({ ...prev, [opt.endpoint_id]: 'permitted' }));
            })
            .catch((err: unknown) => {
              if (
                gen !== optionsGenRef.current ||
                activeProfileIdRef.current !== currentProfileId ||
                !isVisibleRef.current
              ) {
                return;
              }
              const code = String(err).trim();
              setPermissionsById((prev) => ({
                ...prev,
                [opt.endpoint_id]: code === 'forbidden' ? 'forbidden' : 'unknown',
              }));
            });
        }
      })
      .catch(() => {
        if (gen === optionsGenRef.current) {
          setOptionsById({});
          setPermissionsById({});
        }
      });
    return () => { optionsGenRef.current += 1; };
  }, [profileId, isVisible, endpoints.length, endpointsKey]);

  const handleCardToggle = useCallback(
    (open: boolean) => {
      cardCollapsedRef.current = !open;
      if (!open) {
        clearRevealedState();
      }
    },
    [clearRevealedState],
  );

  const handleRevealClick = useCallback(
    (option: EndpointLoginOption) => {
      if (revealed?.endpoint_id === option.endpoint_id) {
        clearRevealedState();
        return;
      }
      clearRevealTimer();
      setRevealed(null);
      setVisibleKeys(new Set());
      setCopiedKey(null);
      setClipboardNote(null);
      setErrorByEndpoint(null);

      const reqGen = ++requestGenRef.current;
      const reqProfileId = profileId;
      setLoadingEndpointId(option.endpoint_id);

      api
        .revealEndpointLogin(reqProfileId, option.endpoint_id)
        .then((res) => {
          if (
            reqGen !== requestGenRef.current ||
            activeProfileIdRef.current !== reqProfileId ||
            !isVisibleRef.current ||
            cardCollapsedRef.current
          ) {
            return;
          }
          setLoadingEndpointId(null);
          setRevealed(res);
          setVisibleKeys(new Set());
          setPermissionsById((prev) => ({ ...prev, [option.endpoint_id]: 'permitted' }));
          revealTimerRef.current = window.setTimeout(() => {
            clearRevealedState();
          }, REVEAL_TTL_MS);
        })
        .catch((err: unknown) => {
          if (
            reqGen !== requestGenRef.current ||
            activeProfileIdRef.current !== reqProfileId ||
            !isVisibleRef.current ||
            cardCollapsedRef.current
          ) {
            return;
          }
          setLoadingEndpointId(null);
          const code = String(err).trim();
          if (code === 'forbidden') {
            setPermissionsById((prev) => ({ ...prev, [option.endpoint_id]: 'forbidden' }));
          }
          setErrorByEndpoint({
            endpointId: option.endpoint_id,
            message: formatLoginError(
              code,
              `${option.namespace}/${option.secret_name}`,
              option.service_name,
            ),
          });
        });
    },
    [profileId, revealed, clearRevealTimer, clearRevealedState],
  );

  const handleCopyCredential = useCallback(async (secretToCopy: string, key: string | null) => {
    try {
      await navigator.clipboard.writeText(secretToCopy);
      if (key) setCopiedKey(key);
      window.setTimeout(() => setCopiedKey(null), 1200);
      // WKWebView denies clipboard.readText, so "unchanged" is judged by the native pasteboard
      // change count; the secret itself never leaves the webview.
      let count: number | null = null;
      try {
        count = await api.clipboardChangeCount();
      } catch {
        // Count unavailable: the note below says the clipboard will not be cleared.
      }
      if (count === null) {
        setClipboardNote(
          'Copied to clipboard (will NOT auto-clear; macOS clipboard managers may retain copied secrets).',
        );
        return;
      }
      setClipboardNote(
        'Copied to clipboard (clears after 60s if unchanged; macOS clipboard managers may retain copied secrets).',
      );
      const copiedCount = count;
      window.setTimeout(() => {
        // Never clobber a clipboard the user has since replaced.
        api.clearClipboardIfChangeCount(copiedCount).catch(() => undefined);
      }, CLIPBOARD_CLEAR_MS);
    } catch {
      setClipboardNote('Could not write to clipboard.');
    }
  }, []);

  const toggleVisible = useCallback((key: string) => {
    setVisibleKeys((prev) => {
      const next = new Set(prev);
      if (!next.delete(key)) next.add(key);
      return next;
    });
  }, []);

  return {
    optionsById,
    permissionsById,
    loadingEndpointId,
    revealed,
    visibleKeys,
    toggleVisible,
    copiedKey,
    clipboardNote,
    errorByEndpoint,
    handleCardToggle,
    handleRevealClick,
    handleCopyCredential,
    clearRevealedState,
  };
}

type EndpointLoginButtonProps = {
  option: EndpointLoginOption;
  permission: PermissionState;
  loading: boolean;
  isRevealed: boolean;
  onClick: () => void;
};

export function EndpointLoginButton({
  option,
  permission,
  loading,
  isRevealed,
  onClick,
}: EndpointLoginButtonProps) {
  if (permission === 'forbidden') {
    return (
      <button
        type="button"
        className="secondary-button compact-btn"
        disabled
        title={`Your credentials for this cluster cannot read ${option.namespace}/${option.secret_name}`}
      >
        <KeyRound size={10} />
        Not permitted ({option.service_name})
      </button>
    );
  }

  return (
    <button
      type="button"
      className="secondary-button compact-btn"
      onClick={onClick}
      disabled={loading || permission !== 'permitted'}
      title={`Read login from ${option.namespace}/${option.secret_name}${
        option.sso
          ? ` and the Keycloak realm users (signs in via Keycloak${option.sso.realm ? `, realm ${option.sso.realm}` : ''})`
          : ''
      } (auto-clears after 60s)${
        option.route_namespace !== option.namespace
          ? ` — cross-namespace route ${option.route_resource}: whoever controls namespace ${option.route_namespace} controls this route`
          : ''
      }`}
    >
      {loading ? <RefreshCw size={10} className="spin" /> : <KeyRound size={10} />}
      {isRevealed ? `Hide login (${option.service_name})` : `Reveal login (${option.service_name})`}
    </button>
  );
}

type EndpointLoginPanelProps = {
  revealed: RevealedEndpointLogin | null;
  errorMessage: string | null;
  visibleKeys: ReadonlySet<string>;
  copiedKey: string | null;
  clipboardNote: string | null;
  onToggleVisible: (key: string) => void;
  onCopy: (value: string, key: string | null) => void;
  onClose: () => void;
};

export function EndpointLoginPanel({
  revealed,
  errorMessage,
  visibleKeys,
  copiedKey,
  clipboardNote,
  onToggleVisible,
  onCopy,
  onClose,
}: EndpointLoginPanelProps) {
  if (errorMessage) {
    return (
      <div
        style={{
          marginTop: '8px',
          padding: '8px 10px',
          borderRadius: '6px',
          border: '1px solid var(--border)',
          background: 'var(--bg-elevated)',
          fontSize: '11px',
          color: 'var(--warning)',
        }}
      >
        {errorMessage}
      </div>
    );
  }

  if (!revealed) return null;

  return (
    <div
      style={{
        marginTop: '8px',
        padding: '8px 10px',
        borderRadius: '6px',
        border: '1px solid var(--border)',
        background: 'var(--bg-elevated)',
        display: 'flex',
        flexDirection: 'column',
        gap: '6px',
        fontSize: '12px',
      }}
    >
      <div style={{ display: 'flex', justifyContent: 'space-between', alignItems: 'center', gap: '8px', flexWrap: 'wrap' }}>
        <span style={{ fontSize: '11px', color: 'var(--text-secondary)' }}>
          Service <span className="mono">{revealed.namespace}/{revealed.service_name}</span> · Auto-clears in 60s
        </span>
        <button type="button" className="secondary-button compact-btn" onClick={onClose}>
          Close
        </button>
      </div>
      {revealed.groups.map((group, gi) => (
        <div key={gi} style={{ display: 'flex', flexDirection: 'column', gap: '4px' }}>
          <span style={{ fontSize: '11px', color: 'var(--text-secondary)' }}>
            {group.label}
            {group.namespace && (
              <>
                {' · Secret '}
                <span className="mono">{group.namespace}/{group.secret_name}</span>
              </>
            )}
          </span>
          {group.error && (
            <span style={{ fontSize: '11px', color: 'var(--warning)' }}>
              {formatLoginError(
                group.error,
                `${group.namespace || 'keycloak'}/${group.secret_name}`,
                revealed.service_name,
              )}
            </span>
          )}
          {group.accounts.map((account, ai) => {
            const key = `${gi}:${ai}`;
            const visible = visibleKeys.has(key);
            return (
              <div key={key} style={{ display: 'flex', alignItems: 'center', gap: '12px', flexWrap: 'wrap' }}>
                <div style={{ display: 'flex', alignItems: 'center', gap: '6px' }}>
                  <span style={{ color: 'var(--text-secondary)' }}>Username:</span>
                  {account.username !== null ? (
                    <>
                      <span className="mono">{account.username}</span>
                      {account.key !== null && account.key !== account.username && (
                        <span style={{ fontSize: '10px', color: 'var(--text-tertiary)' }}>
                          key: {account.key}
                        </span>
                      )}
                      <button
                        type="button"
                        className="icon-button"
                        onClick={() => onCopy(account.username as string, null)}
                        title="Copy username"
                        aria-label={`Copy username ${account.username}`}
                      >
                        <Copy size={13} />
                      </button>
                    </>
                  ) : (
                    <span style={{ color: 'var(--text-tertiary)' }}>Not set in secret</span>
                  )}
                </div>
                <div style={{ display: 'flex', alignItems: 'center', gap: '6px' }}>
                  <span style={{ color: 'var(--text-secondary)' }}>Password:</span>
                  <span className="mono">{visible ? account.password : '••••••••••••'}</span>
                  <button
                    type="button"
                    className="secondary-button compact-btn"
                    onClick={() => onToggleVisible(key)}
                    title={visible ? 'Hide password' : 'Reveal password'}
                  >
                    {visible ? <EyeOff size={10} /> : <Eye size={10} />}
                    {visible ? 'Hide' : 'Reveal'}
                  </button>
                  <button
                    type="button"
                    className="secondary-button compact-btn"
                    onClick={() => onCopy(account.password, key)}
                    title="Copy password to clipboard"
                  >
                    {copiedKey === key ? <Check size={10} /> : <Copy size={10} />}
                    {copiedKey === key ? 'Copied' : 'Copy'}
                  </button>
                </div>
              </div>
            );
          })}
        </div>
      ))}
      {revealed.route_namespace !== revealed.namespace && (
        <div role="alert" style={{ fontSize: '11px', color: 'var(--warning)' }}>
          Cross-namespace route: <span className="mono">{revealed.route_resource}</span> (host{' '}
          <span className="mono">{revealed.host}</span>) forwards to Service{' '}
          <span className="mono">{revealed.namespace}/{revealed.service_name}</span>. Whoever controls
          namespace <span className="mono">{revealed.route_namespace}</span> controls this route — only
          sign in if you trust it.
        </div>
      )}
      {clipboardNote && (
        <div style={{ fontSize: '11px', color: 'var(--text-tertiary)' }}>{clipboardNote}</div>
      )}
    </div>
  );
}
