// podd's update-observability surface (REPLACEMENT_PLAN §9). Canonical wire
// contract for `GET /api/updates`; keep in sync with crates/api/src/updates.rs
// and crates/pod-updater/src/status.rs.
import { z } from 'zod';


// `ComponentKind` from crates/pod-update/src/manifest.rs — the update tiers.
export const ComponentKindSchema = z.enum([
  'app',
  'os',
  'mcu_frozen',
  'mcu_sensor',
  'bootloader',
]);

export type ComponentKind = z.infer<typeof ComponentKindSchema>;

export const VersionEntrySchema = z.object({
  kind: ComponentKindSchema,
  version: z.string(),
});

export type VersionEntry = z.infer<typeof VersionEntrySchema>;

export const AvailableUpdateSchema = z.object({
  kind: ComponentKindSchema,
  name: z.string(),
  version: z.string(),
});

export type AvailableUpdate = z.infer<typeof AvailableUpdateSchema>;

export const UpdateStatusSchema = z.object({
  enabled: z.boolean(),
  channel: z.string(),
  // 'auto' | 'manual'
  mode: z.string(),
  currentVersions: z.array(VersionEntrySchema),
  lastCheckUnix: z.number().nullable(),
  lastCheckOk: z.boolean(),
  available: z.array(AvailableUpdateSchema),
  lastError: z.string().nullable(),
  lastApplied: z.string().nullable(),
});

export type UpdateStatus = z.infer<typeof UpdateStatusSchema>;

export const UpdatesReportSchema = z.object({
  daemon: z.object({
    version: z.string(),
    rev: z.string(),
  }),
  // null = no update agent running (not the same as "no updates available")
  updater: UpdateStatusSchema.nullable(),
});

export type UpdatesReport = z.infer<typeof UpdatesReportSchema>;
