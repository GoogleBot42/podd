import axios from './api';
import { useQuery } from '@tanstack/react-query';

import { UpdatesReport, UpdateStatus } from './schemas/updatesSchema';
export * from './schemas/updatesSchema';


export const useUpdates = () => {
  return useQuery<UpdatesReport>({
    queryKey: ['useUpdates'],
    queryFn: async () => {
      const response = await axios.get<UpdatesReport>('/updates');
      return response.data;
    },
  });
};

// Polls the configured release channel out of band. Resolves with the
// refreshed status, so the panel needs no second round-trip.
export const postUpdatesCheck = async () => {
  const response = await axios.post<UpdateStatus>('/updates/check');
  return response.data;
};

// Flips the Tier-2 app symlink back to the previous release. On-device this
// restarts podd, so the response may never arrive — see UpdatesSection.
export const postUpdatesRollback = async () => {
  const response = await axios.post<{ restored: string }>('/updates/rollback');
  return response.data;
};
