import StatusBanner from '../StatusBanner';
import { useStatus } from '../../state/StatusContext';

export default function StatusDock() {
  const { statusMessage, clearStatus } = useStatus();

  if (!statusMessage) return null;

  return (
    <div className="screen-status-dock">
      <StatusBanner message={statusMessage} onDismiss={clearStatus} />
    </div>
  );
}
