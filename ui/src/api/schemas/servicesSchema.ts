// Vendored from throwaway31265/free-sleep (server/src). Canonical wire-type contract for podd's API; keep in sync with the podd backend implementation.

import { z } from 'zod';
import { StatusInfoSchema } from './serverStatusSchema';


export const ServicesSchema = z.object({
  biometrics: z.object({
    enabled: z.boolean(),
    jobs: z.object({
      analyzeSleepLeft: StatusInfoSchema,
      analyzeSleepRight: StatusInfoSchema,
      installation: StatusInfoSchema,
      stream: StatusInfoSchema,
      calibrateLeft: StatusInfoSchema,
      calibrateRight: StatusInfoSchema,
    }),
  }),
  sentryLogging: z.object({
    enabled: z.boolean(),
  }),
}).strict();

export type Services = z.infer<typeof ServicesSchema>;
