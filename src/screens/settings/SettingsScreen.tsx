import { useState, useEffect, useCallback } from 'react';
import { RefreshCw } from 'lucide-react';
import { api, type AppChoice, type AppSettings, type InstalledApps } from '../../api/tauri';
import { useStatus } from '../../state/StatusContext';
import ScreenHeader from '../../components/layout/ScreenHeader';

const SYSTEM_DEFAULT = '';

type AppSelectProps = {
  id: string;
  label: string;
  defaultLabel: string;
  helper: string;
  choices: AppChoice[];
  value: string | null;
  disabled: boolean;
  onChange: (bundleId: string | null) => void;
};

function AppSelect({ id, label, defaultLabel, helper, choices, value, disabled, onChange }: AppSelectProps) {
  // A saved app that is no longer installed stays visible (and selected) instead of silently
  // snapping to the default; launching it uses the default and shows a notice.
  const missing = value != null && !choices.some((c) => c.bundle_id === value);
  return (
    <div className="form-group">
      <label className="form-label" htmlFor={id}>
        {label}
      </label>
      <select
        id={id}
        className="form-select"
        value={value ?? SYSTEM_DEFAULT}
        disabled={disabled}
        onChange={(e) => onChange(e.target.value === SYSTEM_DEFAULT ? null : e.target.value)}
      >
        <option value={SYSTEM_DEFAULT}>{defaultLabel}</option>
        {choices.map((c) => (
          <option key={c.bundle_id} value={c.bundle_id}>
            {c.name}
          </option>
        ))}
        {missing && <option value={value}>{value} (not installed)</option>}
      </select>
      <div className="form-helper">{helper}</div>
    </div>
  );
}

export default function SettingsScreen({ isVisible = true }: { isVisible?: boolean }) {
  const { pushStatus } = useStatus();
  const [settings, setSettings] = useState<AppSettings>({ browser: null, terminal: null });
  const [apps, setApps] = useState<InstalledApps>({ browsers: [], terminals: [] });
  const [loading, setLoading] = useState(true);

  const load = useCallback(async () => {
    setLoading(true);
    try {
      const [current, installed] = await Promise.all([api.getAppSettings(), api.listInstalledApps()]);
      setSettings(current);
      setApps(installed);
    } catch (err) {
      pushStatus('error', 'Failed to load settings', [String(err)]);
    } finally {
      setLoading(false);
    }
  }, [pushStatus]);

  // Kept mounted while hidden; rescan on show so newly installed apps appear.
  useEffect(() => {
    if (!isVisible) return;
    void load();
  }, [isVisible, load]);

  const update = async (patch: Partial<AppSettings>) => {
    const next = { ...settings, ...patch };
    const previous = settings;
    setSettings(next);
    try {
      await api.setAppSettings(next);
      pushStatus('success', 'Settings saved');
    } catch (err) {
      setSettings(previous);
      pushStatus('error', 'Failed to save settings', [String(err)]);
    }
  };

  return (
    <div style={{ height: '100%', display: 'flex', flexDirection: 'column' }}>
      <ScreenHeader
        title="Settings"
        actions={
          <button
            type="button"
            className="icon-button"
            style={{ width: '28px', height: '28px' }}
            title="Rescan installed applications"
            aria-label="Rescan installed applications"
            onClick={() => void load()}
            disabled={loading}
          >
            <RefreshCw size={14} className={loading ? 'spin' : ''} />
          </button>
        }
      />

      <div style={{ flex: 1, overflowY: 'auto', padding: '20px 24px' }}>
        <div className="form-group-stack" style={{ maxWidth: 520 }}>
          <section className="form-section" aria-labelledby="settings-applications">
            <div className="form-section-title" id="settings-applications">
              Applications
            </div>
            <AppSelect
              id="settings-browser"
              label="Browser"
              defaultLabel="System default"
              helper="Used when ClusterDeck opens a URL."
              choices={apps.browsers}
              value={settings.browser}
              disabled={loading}
              onChange={(browser) => void update({ browser })}
            />
            <AppSelect
              id="settings-terminal"
              label="Terminal for SSH"
              defaultLabel="Terminal (default)"
              helper="Used for SSH sessions and VM shells."
              choices={apps.terminals}
              value={settings.terminal}
              disabled={loading}
              onChange={(terminal) => void update({ terminal })}
            />
          </section>
        </div>
      </div>
    </div>
  );
}
