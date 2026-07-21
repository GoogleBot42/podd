import React, { useEffect } from 'react';
import { create } from 'zustand';
import moment from 'moment-timezone';

import { useSettings } from '@api/settings.ts';

export type Side = 'left' | 'right';

type AppState = {
  isUpdating: boolean;
  setIsUpdating: (isUpdating: boolean) => void;
  side: Side;
  setSide: (side: Side) => void;
};

const SIDE_KEY = 'side';

// Create Zustand store
export const useAppStore = create<AppState>((set) => ({
  isUpdating: false,
  setIsUpdating: (isUpdating: boolean) => set({ isUpdating }),
  side: localStorage.getItem(SIDE_KEY) as Side || 'left',
  setSide: (side: Side) => {
    set({ side });
    localStorage.setItem(SIDE_KEY, side);
  },
}));

// AppStoreProvider to sync Zustand with react-query's isFetching
export function AppStoreProvider({ children }: React.PropsWithChildren) {
  const { data: settings } = useSettings();

  useEffect(() => {
    if (!settings) return;
    moment.tz.setDefault(settings.timeZone);
  }, [settings]);

  return <>{ children }</>;
}
