// podd-only (free-sleep has no MQTT): the broker link's settings, issue #18.
// Keep in sync with `crates/api/src/wire.rs` (`MqttSettings`).
//
// The password is never part of what the API returns — `passwordSet` only says
// whether one is stored — so it lives on the *patch* schema alone.

import { z } from 'zod';

export const MqttSettingsSchema = z.object({
  enabled: z.boolean(),
  server: z.string(),
  port: z.number().int().min(1).max(65535),
  user: z.string(),
  passwordSet: z.boolean(),
}).strict();

export const MqttSettingsPatchSchema = MqttSettingsSchema.partial().extend({
  // Absent = keep the stored password; '' = clear it.
  password: z.string().optional(),
});

export type MqttSettings = z.infer<typeof MqttSettingsSchema>;
export type MqttSettingsPatch = z.infer<typeof MqttSettingsPatchSchema>;
