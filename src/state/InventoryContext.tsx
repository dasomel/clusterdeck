import {
  createContext,
  useContext,
  useState,
  useEffect,
  useRef,
  useCallback,
  type ReactNode,
} from 'react';
import { api, type EnvironmentProfileResult, type Inventory } from '../api/tauri';

const DEMO_STORAGE_KEY = 'clusterdeck.inventoryDemo';

type InventoryContextValue = {
  inventory: Inventory | null;
  loading: boolean;
  error: string | null;
  demo: boolean;
  setDemo: (demo: boolean) => void;
  refresh: () => Promise<void>;
  createFromEnvironment: (environment: string) => Promise<EnvironmentProfileResult>;
};

const InventoryContext = createContext<InventoryContextValue | null>(null);

export function InventoryProvider({ children }: { children: ReactNode }) {
  const [demo, setDemoState] = useState<boolean>(() => {
    try {
      const urlDemo = new URLSearchParams(window.location.search).get('demo');
      if (urlDemo === '1' || urlDemo === 'true') return true;
      if (urlDemo === '0' || urlDemo === 'false') return false;
      return localStorage.getItem(DEMO_STORAGE_KEY) === 'true';
    } catch {
      return false;
    }
  });

  const [inventory, setInventory] = useState<Inventory | null>(null);
  const [loading, setLoading] = useState<boolean>(false);
  const [error, setError] = useState<string | null>(null);

  const requestCounter = useRef<number>(0);

  const setDemo = useCallback((newDemo: boolean) => {
    setDemoState(newDemo);
    try {
      localStorage.setItem(DEMO_STORAGE_KEY, String(newDemo));
    } catch {
      // ignore storage errors
    }
  }, []);

  const refresh = useCallback(async () => {
    const currentRequestId = ++requestCounter.current;
    setLoading(true);

    try {
      const data = await api.discoverInventory(demo);
      // Drop out-of-order responses
      if (currentRequestId === requestCounter.current) {
        setInventory(data);
        setError(null);
      }
    } catch (err) {
      if (currentRequestId === requestCounter.current) {
        setError(err instanceof Error ? err.message : String(err));
        // Note: keeps previous inventory data on failure as required by spec
      }
    } finally {
      if (currentRequestId === requestCounter.current) {
        setLoading(false);
      }
    }
  }, [demo]);

  useEffect(() => {
    void refresh();
  }, [refresh]);

  const createFromEnvironment = useCallback(
    async (environment: string): Promise<EnvironmentProfileResult> => {
      return api.createProfileFromEnvironment(environment);
    },
    [],
  );

  return (
    <InventoryContext.Provider
      value={{
        inventory,
        loading,
        error,
        demo,
        setDemo,
        refresh,
        createFromEnvironment,
      }}
    >
      {children}
    </InventoryContext.Provider>
  );
}

export function useInventory(): InventoryContextValue {
  const ctx = useContext(InventoryContext);
  if (!ctx) {
    throw new Error('useInventory must be used within an InventoryProvider');
  }
  return ctx;
}
