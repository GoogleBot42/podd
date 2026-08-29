import moment, { Moment } from 'moment-timezone';

/**
 * The moment the *next* occurrence of an alarm at `timeHhMm` (24-hour "HH:mm")
 * fires in `timeZone`: today if that time is still ahead, else tomorrow.
 *
 * The daemon judges an override against the alarm start it targets
 * (crates/podd-core/src/alarm.rs), so the expiry must be stamped just past a
 * *future* start. The old noon-pivot rule ("before noon → today") produced an
 * already-past expiry for any press between midnight and noon after the alarm
 * rang, silently making the override a no-op.
 */
export function nextAlarmOccurrence(timeHhMm: string, timeZone: string): Moment {
  const [hour, minute] = timeHhMm.split(':').map(Number);
  const now = moment.tz(timeZone);
  const todayAt = now.clone().hour(hour).minute(minute).second(0).millisecond(0);
  return todayAt.isAfter(now) ? todayAt : todayAt.add(1, 'day');
}

/** `expiresAt` for an override targeting that occurrence (+2 min, RFC 3339). */
export function overrideExpiry(timeHhMm: string, timeZone: string): string {
  return nextAlarmOccurrence(timeHhMm, timeZone).add(2, 'minutes').format();
}
