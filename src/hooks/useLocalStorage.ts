import { useState, useCallback } from 'react';

export function useLocalStorage<T>(key: string, initialValue: T): [T, (value: T | ((prev: T) => T)) => void] {
  const [storedValue, setStoredValue] = useState<T>(() => {
    try {
      const item = window.localStorage.getItem(key);
      return item ? (JSON.parse(item) as T) : initialValue;
    } catch {
      return initialValue;
    }
  });

  const setValue = useCallback(
    (value: T | ((prev: T) => T)) => {
      try {
        setStoredValue((prev) => {
          const valueToStore = value instanceof Function ? value(prev) : value;
          try {
            window.localStorage.setItem(key, JSON.stringify(valueToStore));
          } catch {
            // ignore storage error
          }
          return valueToStore;
        });
      } catch {
        // ignore
      }
    },
    [key],
  );

  return [storedValue, setValue];
}
