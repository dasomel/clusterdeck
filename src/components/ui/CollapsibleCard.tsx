import { useId, useState, type CSSProperties, type ReactNode } from 'react';
import { ChevronRight } from 'lucide-react';

type CollapsibleCardProps = {
  /** Stable per card type (not per profile); keys the persisted open/closed state. */
  id: string;
  title: ReactNode;
  /** Header controls that stay visible while the card is collapsed. */
  actions?: ReactNode;
  defaultOpen?: boolean;
  onToggle?: (open: boolean) => void;
  style?: CSSProperties;
  children: ReactNode;
};

const STORAGE_KEY = 'clusterdeck.collapsedCards';

// Maps card id -> open state, only for cards the user has toggled. Storage can be
// unavailable or hold junk (private mode, blocked, hand-edited), so every access
// falls back to the card's default instead of throwing.
function readStored(): Record<string, boolean> {
  try {
    const parsed: unknown = JSON.parse(localStorage.getItem(STORAGE_KEY) ?? '{}');
    return parsed && typeof parsed === 'object' && !Array.isArray(parsed)
      ? (parsed as Record<string, boolean>)
      : {};
  } catch {
    return {};
  }
}

function writeStored(id: string, open: boolean) {
  try {
    localStorage.setItem(STORAGE_KEY, JSON.stringify({ ...readStored(), [id]: open }));
  } catch {
    // Persistence is a convenience; the in-memory state still works.
  }
}

export default function CollapsibleCard({
  id,
  title,
  actions,
  defaultOpen = true,
  onToggle,
  style,
  children,
}: CollapsibleCardProps) {
  const bodyId = useId();
  const [open, setOpen] = useState(() => {
    const stored = readStored()[id];
    return typeof stored === 'boolean' ? stored : defaultOpen;
  });

  const toggle = () => {
    const next = !open;
    setOpen(next);
    writeStored(id, next);
    onToggle?.(next);
  };

  return (
    <div className={`panel-card collapsible-card${open ? '' : ' collapsed'}`} style={style}>
      <div className="card-header">
        <button
          type="button"
          className="card-toggle"
          aria-expanded={open}
          aria-controls={bodyId}
          onClick={toggle}
        >
          <ChevronRight size={14} className="card-chevron" aria-hidden="true" />
          {title}
        </button>
        {actions}
      </div>
      <div id={bodyId} className="card-body" hidden={!open}>
        {children}
      </div>
    </div>
  );
}
