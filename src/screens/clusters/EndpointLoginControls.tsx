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

function formatLoginError(code: string, option: EndpointLoginOption): string {
  const secretRef = `${option.namespace}/${option.secret_name}`;
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
      return `Service ${option.service_name} does not have a supported login secret mapping`;
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
  const [passwordVisible, setPasswordVisible] = useState(false);
  const [copiedPassword, setCopiedPassword] = useState(false);
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
    setPasswordVisible(false);
    setCopiedPassword(false);
    setClipboardNote(null);
    setErrorByEndpoint(null);
  }, [clearRevealTimer]);

  const endpointsKey = JSON.stringify(endpoints.map((ep) => [
    getEndpointId(ep), ep.backend_service, ep.backend_upstream,
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
            .then(() => {
              if (
                gen !== optionsGenRef.current ||
                activeProfileIdRef.current !== currentProfileId ||
                !isVisibleRef.current
              ) {
                return;
              }
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
      setPasswordVisible(false);
      setCopiedPassword(false);
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
          setPasswordVisible(false);
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
            message: formatLoginError(code, option),
          });
        });
    },
    [profileId, revealed, clearRevealTimer, clearRevealedState],
  );

  const handleCopyCredential = useCallback(async (secretToCopy: string, password = false) => {
    try {
      await navigator.clipboard.writeText(secretToCopy);
      if (password) setCopiedPassword(true);
      window.setTimeout(() => setCopiedPassword(false), 1200);
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

  const handleCopyPassword = () => {
    if (revealed) void handleCopyCredential(revealed.password, true);
  };
  const handleCopyUsername = () => {
    if (revealed?.username != null) void handleCopyCredential(revealed.username);
  };

  return {
    optionsById,
    permissionsById,
    loadingEndpointId,
    revealed,
    passwordVisible,
    setPasswordVisible,
    copiedPassword,
    clipboardNote,
    errorByEndpoint,
    handleCardToggle,
    handleRevealClick,
    handleCopyPassword,
    handleCopyUsername,
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
      title={`Read admin login from ${option.namespace}/${option.secret_name} (auto-clears after 60s)`}
    >
      {loading ? <RefreshCw size={10} className="spin" /> : <KeyRound size={10} />}
      {isRevealed ? `Hide login (${option.service_name})` : `Reveal login (${option.service_name})`}
    </button>
  );
}

type EndpointLoginPanelProps = {
  revealed: RevealedEndpointLogin | null;
  errorMessage: string | null;
  passwordVisible: boolean;
  copiedPassword: boolean;
  clipboardNote: string | null;
  onTogglePassword: () => void;
  onCopyPassword: () => void;
  onCopyUsername: () => void;
  onClose: () => void;
};

export function EndpointLoginPanel({
  revealed,
  errorMessage,
  passwordVisible,
  copiedPassword,
  clipboardNote,
  onTogglePassword,
  onCopyPassword,
  onCopyUsername,
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
          Service <span className="mono">{revealed.namespace}/{revealed.service_name}</span> · Secret{' '}
          <span className="mono">{revealed.namespace}/{revealed.secret_name}</span> · Auto-clears in 60s
        </span>
        <button
          type="button"
          className="secondary-button compact-btn"
          onClick={onClose}
        >
          Close
        </button>
      </div>
      <div style={{ display: 'flex', alignItems: 'center', gap: '12px', flexWrap: 'wrap' }}>
        <div style={{ display: 'flex', alignItems: 'center', gap: '6px' }}>
          <span style={{ color: 'var(--text-secondary)' }}>Username:</span>
          {revealed.username !== null ? (
            <>
              <span className="mono">{revealed.username}</span>
              <button
                type="button"
                className="icon-button"
                onClick={onCopyUsername}
                title="Copy username"
                aria-label="Copy username"
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
          <span className="mono">{passwordVisible ? revealed.password : '••••••••••••'}</span>
          <button
            type="button"
            className="secondary-button compact-btn"
            onClick={onTogglePassword}
            title={passwordVisible ? 'Hide password' : 'Reveal password'}
          >
            {passwordVisible ? <EyeOff size={10} /> : <Eye size={10} />}
            {passwordVisible ? 'Hide' : 'Reveal'}
          </button>
          <button
            type="button"
            className="secondary-button compact-btn"
            onClick={onCopyPassword}
            title="Copy password to clipboard"
          >
            {copiedPassword ? <Check size={10} /> : <Copy size={10} />}
            {copiedPassword ? 'Copied' : 'Copy'}
          </button>
        </div>
      </div>
      {clipboardNote && (
        <div style={{ fontSize: '11px', color: 'var(--text-tertiary)' }}>{clipboardNote}</div>
      )}
    </div>
  );
}
