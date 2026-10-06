import { useState, useEffect, useCallback } from 'react';

export type Section = 'overview' | 'infrastructure' | 'clusters' | 'kubeconfig';

const STORAGE_KEY = 'clusterdeck.section';

export function useSection(initialSection: Section = 'clusters', onRefreshCurrent?: () => void) {
  const [section, setSectionState] = useState<Section>(() => {
    try {
      const urlParam = new URLSearchParams(window.location.search).get('section') as Section | null;
      if (urlParam && ['overview', 'infrastructure', 'clusters', 'kubeconfig'].includes(urlParam)) {
        return urlParam;
      }
      const stored = localStorage.getItem(STORAGE_KEY) as Section | null;
      if (stored && ['overview', 'infrastructure', 'clusters', 'kubeconfig'].includes(stored)) {
        return stored;
      }
    } catch {
      // ignore
    }
    return initialSection;
  });

  const [visited, setVisited] = useState<Set<Section>>(() => new Set([section]));

  const setSection = useCallback((nextSection: Section) => {
    setSectionState(nextSection);
    setVisited((prev) => new Set(prev).add(nextSection));
    try {
      localStorage.setItem(STORAGE_KEY, nextSection);
    } catch {
      // ignore
    }
  }, []);

  // Global keyboard shortcuts (Cmd+1..4, Cmd+R)
  useEffect(() => {
    const handleKeyDown = (e: KeyboardEvent) => {
      if (!e.metaKey && !e.ctrlKey) return;

      if (e.key === '1') {
        e.preventDefault();
        setSection('overview');
      } else if (e.key === '2') {
        e.preventDefault();
        setSection('infrastructure');
      } else if (e.key === '3') {
        e.preventDefault();
        setSection('clusters');
      } else if (e.key === '4') {
        e.preventDefault();
        setSection('kubeconfig');
      } else if (e.key === 'r' || e.key === 'R') {
        // Intercept webview reload and refresh in-app state instead
        e.preventDefault();
        onRefreshCurrent?.();
      } else if (e.key === 'n' || e.key === 'N') {
        if (section !== 'clusters') {
          e.preventDefault();
          setSection('clusters');
        }
      }
    };

    window.addEventListener('keydown', handleKeyDown);
    return () => window.removeEventListener('keydown', handleKeyDown);
  }, [setSection, onRefreshCurrent]);

  return {
    section,
    setSection,
    visited,
  };
}
