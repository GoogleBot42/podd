// `StatusInfo`/`Status` are vendored from throwaway31265/free-sleep
// (server/src); `ServerStatus`'s key set is podd's own. Canonical wire-type
// contract for podd's API; keep in sync with crates/api/src/wire.rs.
import { z } from 'zod';


const StatusSchema = z.enum([
  'failed',
  'healthy',
  'not_started',
  'restarting',
  'retrying',
  'started',
]);

export type Status = z.infer<typeof StatusSchema>;

export const StatusInfoSchema = z.object({
  name: z.string(),
  status: StatusSchema,
  description: z.string(),
  message: z.string(),
  timestamp: z.string().optional(),
});

export type StatusInfo = z.infer<typeof StatusInfoSchema>;

// podd's real subsystems, from the `podd_core::health` registry (see
// crates/api/src/wire.rs). free-sleep's Node service keys (express, database,
// franken, ...) are gone: they described a server podd doesn't run, and the
// backend hardcoded every one of them healthy.
//
// The page renders `Object.keys`, so extra keys the backend grows later show
// up without a UI change.
export type ServerStatus = {
  api: StatusInfo;
  clock: StatusInfo;
  coverControl: StatusInfo;
  mqtt: StatusInfo;
  sensor: StatusInfo;
};

// eslint-disable-next-line @typescript-eslint/no-type-alias
export type ServerStatusKey = keyof ServerStatus;
