import { createContext, useContext, useState, useEffect, useCallback, useMemo, type ReactNode } from 'react';
import { api, type Profile } from '../api/tauri';

export type ProfilesContextType = {
  profiles: Profile[];
  selectedId: string | null;
  selected: Profile | null;
  loadError: string | null;
  loading: boolean;
  trustVersion: Record<string, number>;
  reload: () => Promise<void>;
  select: (id: string | null) => void;
  remove: (profile: Profile) => Promise<void>;
  bumpTrust: (profileId: string) => void;
};

const ProfilesContext = createContext<ProfilesContextType | null>(null);

export function ProfilesProvider({ children }: { children: ReactNode }) {
  const [profiles, setProfiles] = useState<Profile[]>([]);
  const [selectedId, setSelectedId] = useState<string | null>(null);
  const [loadError, setLoadError] = useState<string | null>(null);
  const [loading, setLoading] = useState(true);
  const [trustVersion, setTrustVersion] = useState<Record<string, number>>({});

  const reload = useCallback(async () => {
    setLoading(true);
    try {
      const loaded = await api.listProfiles();
      setProfiles(loaded);
      setSelectedId((current) => {
        if (current && loaded.some((p) => p.id === current)) {
          return current;
        }
        return loaded[0]?.id ?? null;
      });
      setLoadError(null);
    } catch (err) {
      setLoadError(String(err));
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    reload();
  }, [reload]);

  const select = useCallback((id: string | null) => {
    setSelectedId(id);
  }, []);

  const remove = useCallback(async (profile: Profile) => {
    await api.deleteProfile(profile.id);
    setProfiles((prev) => {
      const remaining = prev.filter((p) => p.id !== profile.id);
      setSelectedId((curr) => {
        if (curr === profile.id) {
          return remaining[0]?.id ?? null;
        }
        return curr;
      });
      return remaining;
    });
  }, []);

  const bumpTrust = useCallback((profileId: string) => {
    setTrustVersion((prev) => ({
      ...prev,
      [profileId]: (prev[profileId] ?? 0) + 1,
    }));
  }, []);

  const selected = useMemo(() => {
    if (!selectedId) return null;
    return profiles.find((p) => p.id === selectedId) ?? null;
  }, [profiles, selectedId]);

  return (
    <ProfilesContext.Provider
      value={{
        profiles,
        selectedId,
        selected,
        loadError,
        loading,
        trustVersion,
        reload,
        select,
        remove,
        bumpTrust,
      }}
    >
      {children}
    </ProfilesContext.Provider>
  );
}

export function useProfiles() {
  const ctx = useContext(ProfilesContext);
  if (!ctx) {
    throw new Error('useProfiles must be used within a ProfilesProvider');
  }
  return ctx;
}
