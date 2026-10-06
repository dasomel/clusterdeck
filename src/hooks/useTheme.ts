import { useState, useCallback } from 'react';

export function useTheme() {
  const [theme, setTheme] = useState<'light' | 'dark' | null>(() => {
    try {
      const urlTheme = new URLSearchParams(window.location.search).get('theme');
      if (urlTheme === 'light' || urlTheme === 'dark') {
        document.documentElement.setAttribute('data-theme', urlTheme);
        return urlTheme;
      }
      const stored = localStorage.getItem('clusterdeck-theme');
      if (stored === 'light' || stored === 'dark') {
        document.documentElement.setAttribute('data-theme', stored);
        return stored;
      }
    } catch {
      // ignore storage error
    }
    return null;
  });

  const [systemTheme] = useState<'light' | 'dark'>(() => {
    try {
      return window.matchMedia('(prefers-color-scheme: dark)').matches ? 'dark' : 'light';
    } catch {
      return 'dark';
    }
  });

  const effectiveTheme = theme ?? systemTheme;

  const toggleTheme = useCallback(() => {
    const nextTheme = effectiveTheme === 'dark' ? 'light' : 'dark';
    try {
      document.documentElement.setAttribute('data-theme', nextTheme);
      localStorage.setItem('clusterdeck-theme', nextTheme);
    } catch {
      // ignore storage or DOM error
    }
    setTheme(nextTheme);
  }, [effectiveTheme]);

  return {
    theme,
    effectiveTheme,
    toggleTheme,
  };
}
