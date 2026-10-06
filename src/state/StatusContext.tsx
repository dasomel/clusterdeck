import { createContext, useContext, useState, useCallback, type ReactNode } from 'react';
import type { StatusMessage } from '../components/StatusBanner';

export type StatusContextType = {
  statusMessage: StatusMessage | null;
  pushStatus: (type: 'success' | 'warning' | 'error', title: string, details?: string[]) => void;
  setStatusMessage: (msg: StatusMessage | null) => void;
  clearStatus: () => void;
};

const StatusContext = createContext<StatusContextType | null>(null);

export function StatusProvider({ children }: { children: ReactNode }) {
  const [statusMessage, setStatusMessage] = useState<StatusMessage | null>(null);

  const pushStatus = useCallback((type: 'success' | 'warning' | 'error', title: string, details?: string[]) => {
    setStatusMessage({
      type,
      title,
      details,
      time: new Date().toLocaleTimeString(),
    });
  }, []);

  const clearStatus = useCallback(() => {
    setStatusMessage(null);
  }, []);

  return (
    <StatusContext.Provider value={{ statusMessage, pushStatus, setStatusMessage, clearStatus }}>
      {children}
    </StatusContext.Provider>
  );
}

export function useStatus() {
  const ctx = useContext(StatusContext);
  if (!ctx) {
    throw new Error('useStatus must be used within a StatusProvider');
  }
  return ctx;
}
