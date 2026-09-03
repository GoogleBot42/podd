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

// Installs the offered Tier-2 (app) release and restarts podd into it as a
// canary that commits itself or is rolled back automatically. Like rollback,
// the restart means the response may never arrive — see ApplyButton. Only the
// app tier is appliable here; the OS/MCU tiers answer 501 (issue #43).
export const postUpdatesApply = async () => {
  const response = await axios.post<UpdateStatus>('/updates/apply', { kind: 'app' });
  return response.data;
};

// True when a request died without any HTTP answer (connection reset, network
// error, timeout) — on apply/rollback that is podd restarting, not a failure.
export const isConnectionDrop = (e: unknown) =>
  typeof e === 'object' && e !== null && !('response' in e && (e as { response?: unknown }).response);

// After an apply/rollback restart, poll GET /updates until podd answers again
// (or the deadline passes) and resolve with the fresh report. A canary
// rollback restarts podd a second time, so keep polling a little past the
// first answer that shows the agent has settled (lastApplied set).
export const waitForPoddRestart = async (deadlineMs = 90_000, everyMs = 2_000): Promise<UpdatesReport | null> => {
  const until = Date.now() + deadlineMs;
  let last: UpdatesReport | null = null;
  while (Date.now() < until) {
    await new Promise(resolve => setTimeout(resolve, everyMs));
    try {
      const response = await axios.get<UpdatesReport>('/updates', { timeout: everyMs });
      last = response.data;
      if (last.updater?.lastApplied) return last;
    } catch {
      // still restarting
    }
  }
  return last;
};

// Switches the followed release channel. podd persists it, so the switch
// survives a restart; nothing is applied until the next check.
export const postUpdatesChannel = async (channel: string) => {
  const response = await axios.post<UpdateStatus>('/updates/channel', { channel });
  return response.data;
};

// Flips the Tier-2 app symlink back to the previous release. On-device this
// restarts podd, so the response may never arrive — see UpdatesSection.
export const postUpdatesRollback = async () => {
  const response = await axios.post<{ restored: string }>('/updates/rollback');
  return response.data;
};
