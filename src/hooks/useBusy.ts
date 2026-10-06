import { useState, useCallback } from 'react';

export function useBusy() {
  const [busyKeys, setBusyKeys] = useState<Set<string>>(new Set());

  const isBusy = useCallback((key: string) => busyKeys.has(key), [busyKeys]);

  const run = useCallback(async <T>(key: string, fn: () => Promise<T>): Promise<T> => {
    setBusyKeys((prev) => new Set(prev).add(key));
    try {
      return await fn();
    } finally {
      setBusyKeys((prev) => {
        const next = new Set(prev);
        next.delete(key);
        return next;
      });
    }
  }, []);

  return { busyKeys, isBusy, run };
}
