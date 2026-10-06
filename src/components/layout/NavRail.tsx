import { Boxes, LayoutDashboard, Server, Network, FileCog, Sun, Moon } from 'lucide-react';
import type { Section } from '../../hooks/useSection';

type NavRailProps = {
  section: Section;
  onSelect: (section: Section) => void;
  badges?: Partial<Record<Section, number>>;
  theme: 'light' | 'dark';
  onToggleTheme: () => void;
  version: string;
};

const NAV_ITEMS: { id: Section; label: string; icon: typeof LayoutDashboard; shortcut: string }[] = [
  { id: 'overview', label: 'Overview', icon: LayoutDashboard, shortcut: '⌘1' },
  { id: 'infrastructure', label: 'Infrastructure', icon: Server, shortcut: '⌘2' },
  { id: 'clusters', label: 'Clusters', icon: Network, shortcut: '⌘3' },
  { id: 'kubeconfig', label: 'Kubeconfig', icon: FileCog, shortcut: '⌘4' },
];

export default function NavRail({
  section,
  onSelect,
  badges = {},
  theme,
  onToggleTheme,
  version,
}: NavRailProps) {
  return (
    <nav className="nav-rail" aria-label="Primary">
      <div className="nav-rail-brand">
        <div className="brand-icon">
          <Boxes size={18} />
        </div>
        <div>
          <div className="brand-name" style={{ fontSize: '14px', lineHeight: 1.2 }}>ClusterDeck</div>
          <div className="brand-subtitle" style={{ fontSize: '10px' }}>cluster operator</div>
        </div>
      </div>

      <div className="nav-rail-items">
        {NAV_ITEMS.map((item) => {
          const Icon = item.icon;
          const isActive = section === item.id;
          const badgeCount = badges[item.id];
          return (
            <button
              key={item.id}
              type="button"
              className={`nav-rail-item ${isActive ? 'active' : ''}`}
              aria-current={isActive ? 'page' : undefined}
              title={`${item.label} (${item.shortcut})`}
              onClick={() => onSelect(item.id)}
            >
              <Icon size={16} strokeWidth={isActive ? 2 : 1.75} />
              <span>{item.label}</span>
              {badgeCount != null && badgeCount > 0 && (
                <span className="nav-rail-badge">{badgeCount}</span>
              )}
            </button>
          );
        })}
      </div>

      <div className="nav-rail-footer">
        <button
          type="button"
          className="icon-button"
          style={{ width: '28px', height: '28px' }}
          title={theme === 'dark' ? 'Switch to light theme' : 'Switch to dark theme'}
          aria-label={theme === 'dark' ? 'Switch to light theme' : 'Switch to dark theme'}
          onClick={onToggleTheme}
        >
          {theme === 'dark' ? <Sun size={15} /> : <Moon size={15} />}
        </button>
        <span className="mono">v{version}</span>
      </div>
    </nav>
  );
}
