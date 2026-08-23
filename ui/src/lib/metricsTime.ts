/**
 * Vitals and movement records carry epoch **seconds** — that is what the zod
 * schemas declare (`timestamp: z.number().int()`) and what the podd API emits.
 *
 * JS date maths is in milliseconds, so every consumer has to convert. Feeding
 * the raw number to `new Date()` plots a whole night in January 1970, which is
 * exactly what the charts used to do (#108).
 */
export const EPOCH_SECONDS_TO_MS = 1000;

export const epochSecondsToMs = (seconds: number): number => seconds * EPOCH_SECONDS_TO_MS;

export const epochSecondsToDate = (seconds: number): Date => new Date(epochSecondsToMs(seconds));
