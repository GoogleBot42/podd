# free-sleep Frontend Fork + `podd` Compat API Report

Source analyzed: `https://github.com/throwaway31265/free-sleep` (MIT), cloned to
`/tmp/claude-1000/-home-googlebot-workspace-eightsleep/56a6c6ed-6821-4835-b1df-b32c2c62384a/scratchpad/fs-analysis`.

Repo layout (monorepo, no workspace tool — plain sibling dirs):
- `app/` — the React SPA (this is what we fork).
- `server/` — Node/Express + TypeScript backend (Prisma/SQLite + LowDB). **Not kept**; it defines the API `podd` must reimplement.
- `biometrics/` — Python (sensor stream, sleep detection, vitals). **Not kept**; deferred.

---

## 1. Frontend Inventory

### Stack / build tool
- **React 19** SPA, **TypeScript**, bundled with **Vite 7** (`app/vite.config.ts`). Not CRA, not Next.
- **MUI v7** (`@mui/material`, `@mui/icons-material`, `@mui/system`, `@mui/x-charts`, `@mui/x-date-pickers`) + Emotion for styling; one SCSS module (`Slider.module.scss`) via `sass`.
- **State/data:** `@tanstack/react-query` v5 for all server state (polling, no WS), **zustand** v5 for a small amount of local UI state. No Redux.
- **Routing:** `react-router-dom` v7 (`BrowserRouter basename="/"`), routes declared in `app/src/main.tsx`.
- **HTTP client:** **axios** (`app/src/api/api.ts`), single instance, `baseURL = <origin>/api/`.
- **Charts:** `@mui/x-charts` + `d3`. Dates via `moment-timezone` + `date-fns`.
- **Other:** `zod` (schemas shared with server), `semver` (update check), `idb`, `msw` (demo/mock mode), `@sentry/react` (optional telemetry), `react-circular-slider-svg` (temperature dial).

### package.json scripts (`app/`)
- `dev`: `VITE_ENV=dev vite`
- `build`: `VITE_ENV=prod tsc -b && vite build` → **outputs to `../server/public/`** (see §5).
- `build:demo`: `VITE_ENV=demo VITE_USE_MSW=true …` → outputs to `./dist/` and serves MSW mocks (fully static, no backend).
- `build:pr`, `lint`, `lint:fix`, `preview`.
- Node pinned via Volta to `24.11.0`; `.nvmrc` present.

### API base URL resolution (`app/src/api/api.ts`)
```ts
const inDev = import.meta.env.VITE_ENV === 'dev';
const baseURL = inDev && import.meta.env.VITE_POD_IP
  ? `http://${import.meta.env.VITE_POD_IP}:3000`   // dev: vite on laptop, API on pod:3000
  : `${window.location.origin}`;                    // prod: same origin as SPA
const axiosInstance = axios.create({ baseURL: `${baseURL}/api/` });
```
So in production the SPA and API are **same-origin**; `podd` serves the static SPA and answers `/api/*` on the same port. Dev uses port **3000** on the pod.

### Screens / routes (`app/src/main.tsx`, nav in `app/src/components/pages.tsx`)
| Route | Component | Purpose |
|---|---|---|
| `/`, `/temperature`, `/left`, `/right` | `ControlTempPage` | Temperature control: on/off power, target-temp circular slider, +/- buttons, alarm override/dismissal, away/priming/water notifications. |
| `/schedules` | `SchedulePage` | Per-side, per-day power schedule (on/off/onTemperature), temperature adjustments, and alarm (time, vibration intensity/pattern, duration, alarm temperature). |
| `/status` | `StatusPage` | `serverStatus` health chips per service. |
| `/data` | `DataPage` (layout) | Parent for the Data tabs. |
| `/data/sleep` | `SleepPage` | Sleep records bar chart + editable/deletable records. **(biometrics)** |
| `/data/vitals` | `VitalsPage` | HR/HRV/breathing line charts + summary cards. **(biometrics)** |
| `/data/logs` | `LogsPage` | Live server logs via **SSE** (`EventSource`). |
| `/settings` | `SettingsPage` | Per-side name, away mode, temp format (°F/°C), timezone, daily reboot, daily priming, LED brightness, reboot/update buttons, Sentry toggle, license modal, donate links. |

### Frontend API layer (`app/src/api/*`)
One file per resource. Almost every `*Schema.ts` file **re-exports the zod schema/type directly from `server/src/...`** via relative import — e.g. `deviceStatusSchema.ts` is literally `export * from '../../../server/src/routes/deviceStatus/deviceStatusSchema'`. **The server TS/zod schemas are the single source of truth for the wire types.** When vendoring, these shared schema files must be copied into the app (see §5).

---

## 2. REST/WS API Contract (the compat surface `podd` must implement)

All paths are prefixed `/api`. Registered in `server/src/setup/routes.ts`. CORS allows localhost / LAN (`192.168.*`, `172.16.*`, `10.0.*`) / `ALLOWED_ORIGIN`. Body parsing is JSON; invalid JSON → `400 {error:{message:"Invalid JSON"}}`; unknown `/api/*` → `404 {error:{message:"Not Found"}}`. Zod validation failures → `400 {error:"Invalid request data", details:[...zodError]}`. Non-API routes fall through to static SPA + `index.html` catch-all.

**No WebSocket anywhere.** Live data is either react-query polling (`refetchInterval`) or one **SSE** stream for logs.

### Control endpoints — implement first (`podd` core)

| Method | Path | Used by | Request body | Success response |
|---|---|---|---|---|
| GET | `/api/deviceStatus` | ControlTempPage, appStore (polls every 30s) | — | `DeviceStatus` (200) |
| POST | `/api/deviceStatus` | ControlTempPage (power, temp, alarm dismiss, prime) | `DeepPartial<DeviceStatus>` | **204 No Content** |
| GET | `/api/settings` | SettingsPage, appStore | — | `Settings` (200) |
| POST | `/api/settings` | SettingsPage (name, awayMode, tz, temp format, reboot, prime) | `DeepPartial<Settings>` (server deletes `id`, deep-merges) | `Settings` (200, full merged doc) |
| GET | `/api/schedules` | SchedulePage | — | `Schedules` (200) |
| POST | `/api/schedules` | SchedulePage save | `DeepPartial<Schedules>` (merge per side/day: power merged, temperatures & alarm replaced) | `Schedules` (200, full merged doc) |
| POST | `/api/alarm` | AlarmTest (fire alarm now) | `AlarmJob` (full object, strict) | 200, returns `schedulesDB.data` (body ignored by UI) |
| POST | `/api/execute` | low-level command escape hatch | `{ command: string, arg?: string }` | `{ success: true, message: string }` (200); invalid command → 400 `"Invalid command"` |
| POST | `/api/jobs` | Settings reboot/update buttons; Status page triggers | `Job[]` (array of enum) | **204** |
| GET | `/api/services` | appStore, SettingsPage FeaturesSection | — | `Services` (200) |
| POST | `/api/services` | Settings (toggle Sentry / biometrics) | `DeepPartial<Services>` | `Services` (200, merged) |
| GET | `/api/serverStatus` | StatusPage | — | `ServerStatus` (200) |
| GET | `/api/logs` | LogsPage | — | `{ logs: string[] }` (filenames, newest first) |
| GET | `/api/logs/:filename` | LogsPage | — (query via `EventSource`) | **SSE** `text/event-stream`; each event `data: {"message": "<joined log lines>"}` |
| GET | `/api/metrics/presence` | (Home Assistant / integrations; no UI consumer found) | — | `{ left:{present,lastUpdatedAt}, right:{...} }` |
| POST | `/api/metrics/presence` | external presence push | `{ left?:{present:boolean}, right?:{present:boolean} }` (≥1 side) | 200 full presence state |

Notes:
- `POST /api/jobs` enum is `analyzeSleepLeft | analyzeSleepRight | biometricsCalibrationLeft | biometricsCalibrationRight | reboot | update`. Only **`reboot`** and **`update`** are control; the other four are biometrics (defer — accept but no-op or 501).
- `GET /api/services` returns a `biometrics` block whose `jobs.*` are `StatusInfo` health records. For a control-only `podd`, return `biometrics.enabled=false` with placeholder/healthy job stubs so the UI renders. `sentryLogging.enabled` is real.
- `serverStatus` has many required control keys plus optional biometrics keys (`analyzeSleep*`, `biometrics*`) — omit the optional ones when biometrics is off.

### Biometrics endpoints — DEFER (backed by SQLite/Python in free-sleep)

| Method | Path | Used by | Notes |
|---|---|---|---|
| GET | `/api/metrics/sleep?side&startTime&endTime` | SleepPage | `SleepRecord[]`; times ISO 8601. |
| PUT | `/api/metrics/sleep/:id` | SleepPage (edit) | `Partial<SleepRecord>` → recomputes `sleep_period_seconds`, `times_exited_bed`; returns updated `SleepRecord`. |
| DELETE | `/api/metrics/sleep/:id` | SleepPage (delete) | 204. |
| GET | `/api/metrics/vitals?side&startTime&endTime` | VitalsPage | `VitalsRecord[]`. |
| GET | `/api/metrics/vitals/summary?…` | VitalsPage | `VitalsSummary`. |
| GET | `/api/metrics/movement?side&startTime&endTime` | MovementChart | `MovementRecord[]`. |

Time query params are ISO 8601 strings; server converts to epoch seconds internally (`moment(x).unix()`). `side` = `"left"|"right"`.

### External call (NOT served by `podd`)
- `GET https://raw.githubusercontent.com/throwaway31265/free-sleep/main/server/src/serverInfo.json` — `app/src/api/serverInfo.ts` fetches this directly (bypasses axios baseURL) to compare `version` via `semver` for the "update available" banner. For the fork, repoint this to the `podd`/fork's own `serverInfo.json` (or drop it).

The complete endpoint set is corroborated by the MSW mock handlers in `app/src/mocks/handlers.ts` (demo mode), which mock exactly these paths.

---

## 3. Data Model (concrete TS/zod, source of truth = `server/src`)

### Temperature units
- **All wire temps are integer °F** (`targetTemperatureF`, `onTemperature`, `alarmTemperature`), range **55–110°F**. NOT tenths, NOT Celsius on the wire.
- `currentTemperatureLevel` is a separate internal "level" scale (roughly -100..100). Server maps F↔level: `level = (F - 82.5) / 27.5 * 100` (`updateDeviceStatus.ts`).
- Celsius is **display-only**, converted client-side (`app/src/lib/temperatureConversions.ts`, rounded to nearest 0.5°C). `settings.temperatureFormat ∈ {"celsius","fahrenheit"}` only chooses display.

### Sides
`type Side = 'left' | 'right'` (`SideSchema = z.enum(['right','left'])`). No "solo" on the wire; both sides always present in status/schedules/settings. "Away mode" per side; when either side is in away mode the server mirrors control commands to both sides.

### DeviceStatus (`server/src/routes/deviceStatus/deviceStatusSchema.ts`, `.strict()`)
```ts
const SideStatusSchema = z.object({
  currentTemperatureLevel: z.number(),
  currentTemperatureF: z.number(),
  targetTemperatureF: z.number().min(55).max(110),
  secondsRemaining: z.number(),
  isOn: z.boolean(),
  isAlarmVibrating: z.boolean(),
  taps: z.object({ doubleTap: z.number(), tripleTap: z.number(), quadTap: z.number() }).optional(),
}).strict();

export const DeviceStatusSchema = z.object({
  left: SideStatusSchema,
  right: SideStatusSchema,
  waterLevel: z.string(),            // note: string, e.g. "true"
  isPriming: z.boolean(),
  settings: z.object({ v: z.number(), gainLeft: z.number(), gainRight: z.number(), ledBrightness: z.number() }),
  coverVersion: z.string(),          // "Pod 3" | "Pod 4" | "Pod 5" | "Version not found"
  hubVersion: z.string(),
  freeSleep: z.object({ version: z.string(), branch: z.string() }),
  wifiStrength: z.number(),
}).strict();
```
POST semantics (`updateDeviceStatus.ts`): `isOn` → temp duration `43200`/`0` s; `targetTemperatureF` → level cmd; `isAlarmVibrating:false` → `ALARM_CLEAR` (can only clear, not set); `isPriming:true` → `PRIME`; `settings` → CBOR-encoded device settings. Away-mode sides are skipped/mirrored.

### Schedules + Alarm (`server/src/db/schedulesSchema.ts`, all `.strict()`)
```ts
export const TimeSchema = z.string().regex(/^([01]\d|2[0-3]):([0-5]\d)$/); // "HH:mm"
export const TemperatureSchema = z.number().int().min(55).max(110);       // °F

export const AlarmSchema = z.object({
  vibrationIntensity: z.number().int().min(1).max(100),
  vibrationPattern: z.enum(['double','rise']),
  duration: z.number().int().min(0).max(180),   // minutes
}).strict();

export const AlarmJobSchema = AlarmSchema.extend({ side: SideSchema, force: z.boolean().optional() }).strict();
export const AlarmScheduleSchema = AlarmSchema.extend({
  time: TimeSchema, enabled: z.boolean(), alarmTemperature: TemperatureSchema,
}).strict();

export const DailyScheduleSchema = z.object({
  temperatures: z.record(TimeSchema, TemperatureSchema),   // { "07:00": 72, "22:00": 68 }
  alarm: AlarmScheduleSchema,
  power: z.object({ on: TimeSchema, off: TimeSchema, onTemperature: TemperatureSchema, enabled: z.boolean() }),
}).strict();

export const SideScheduleSchema = z.object({ sunday:…, monday:…, …, saturday: DailyScheduleSchema });
export const SchedulesSchema = z.object({ left: SideScheduleSchema, right: SideScheduleSchema }).strict();
```
(NB: `server/API.md` shows a stale alarm shape with `vibrationIntensityStart/End` — the zod schema above is authoritative; the doc is out of date.)

### Settings (`server/src/db/settingsSchema.ts`, `.strict()`)
```ts
export const TEMPERATURES = ['celsius','fahrenheit'] as const;
const SideSettingsSchema = z.object({
  name: z.string().min(1).max(20),
  awayMode: z.boolean(),
  scheduleOverrides: z.object({
    temperatureSchedules: z.object({ disabled: z.boolean(), expiresAt: z.string() }),
    alarm: z.object({ disabled: z.boolean(), timeOverride: z.string(), expiresAt: z.string() }),
  }),
  taps: z.object({ doubleTap: TapConfig, tripleTap: TapConfig, quadTap: TapConfig }),
}).strict();

// TapConfig = discriminatedUnion('type'):
//   { type:'temperature', change:'increment'|'decrement', amount: 0..10 }
//   { type:'alarm', behavior:'snooze'|'dismiss', snoozeDuration: 60..600, inactiveAlarmBehavior:'power'|'none' }

export const SettingsSchema = z.object({
  id: z.string(),
  timeZone: z.enum(TIME_ZONES),          // big enum in server/src/db/timeZones.ts
  left: SideSettingsSchema, right: SideSettingsSchema,
  primePodDaily: z.object({ enabled: z.boolean(), time: TimeSchema }),
  temperatureFormat: z.enum(TEMPERATURES),
  rebootDaily: z.boolean(),
}).strict();
```
Defaults (`server/src/db/settings.ts`): `timeZone:'UTC'`, `temperatureFormat:'fahrenheit'`, `rebootDaily:true`, names 'Left'/'Right', awayMode false, tap defaults (double=decrement 1, triple=increment 1, quad=alarm dismiss). Persisted via LowDB JSON (`settingsDB.json`) — `podd` can back this with a JSON file or its own store.

### Services / ServerStatus / Jobs
```ts
type Status = 'failed'|'healthy'|'not_started'|'restarting'|'retrying'|'started';
type StatusInfo = { name:string; status:Status; description:string; message:string; timestamp?:string };
type ServerStatus = { alarmSchedule, database, express, franken, frankenMonitor, jobs, logger,
  powerSchedule, primeSchedule, rebootSchedule, systemDate, temperatureSchedule: StatusInfo;   // required
  analyzeSleepLeft?, analyzeSleepRight?, biometricsInstallation?, biometricsStream?,
  biometricsCalibrationLeft?, biometricsCalibrationRight?: StatusInfo };                        // optional
// Services: { biometrics:{ enabled:boolean, jobs:{ analyzeSleepLeft, analyzeSleepRight,
//   installation, stream, calibrateLeft, calibrateRight: StatusInfo } }, sentryLogging:{enabled:boolean} }
export const JobSchema = z.enum(['analyzeSleepLeft','analyzeSleepRight',
  'biometricsCalibrationLeft','biometricsCalibrationRight','reboot','update']);
```

### Biometrics record types (defer — SQLite/Prisma + Python)
```ts
// vitals: side, timestamp(epoch int), heart_rate 30..90, hrv 0..200, breathing_rate 5..30
// movement: id, side, timestamp(epoch int), total_movement:int
// sleep_records: id, side, entered_bed_at(ISO), left_bed_at(ISO), sleep_period_seconds,
//   times_exited_bed, present_intervals: [ISO,ISO][], not_present_intervals: [ISO,ISO][]
// VitalsSummary: { avgHeartRate, minHeartRate, maxHeartRate, avgHRV, avgBreathingRate }
```
Vitals/movement/sleep are populated by the Python biometrics pipeline into SQLite (Prisma). This is the clean control-vs-biometrics split: **temp / schedule / alarm / settings / presence = pure control**; **vitals / movement / sleep = biometrics (deferred)**.

---

## 4. Committed-Build-Artifacts Problem

Confirmed via `git ls-files`. free-sleep commits generated output into git:
1. **`server/public/index.js` (2.2 MB) + `server/public/index.js.map` (8.4 MB)** — the Vite-built SPA bundle, plus `index.css`, `index.html`, `manifest.json`, `mockServiceWorker.js`, icons. `app/vite.config.ts` sets `build.outDir: '../server/public/'`, so `npm run build` writes the compiled SPA straight into the server dir, and it is checked in.
2. **`server/dist/` (128 files: `*.js` + `*.js.map`)** — the compiled TS→JS backend (`tsc` output) committed to git.

Root `.gitignore` ignores `app/dist/` (the demo output) but NOT `server/public/` or `server/dist/`, which is why the build outputs are tracked. This is exactly the anti-pattern the owner objects to.

License: repo is MIT (`LICENSE.md`, "Open Source Disclaimer and License Agreement", Jan 2025). Source files carry **no per-file SPDX/copyright headers** (only `app/src/pages/SettingsPage/LicenseModal.tsx` references the license text for the UI). Preserve `LICENSE.md` / attribution at repo root and in the SPA's license modal.

---

## 5. Clean-Vendoring Plan (frontend source only, clean Vite/Nix build)

**Vendor (source only):**
- All of `app/` EXCEPT: `app/dist/`, `app/node_modules/`, `.env.sentry-build-plugin`, and `package-lock.json` (regenerate). Keep `src/`, `index.html`, `public/` (static assets: icons, manifest, `paypal.png`, `discord.svg` — these are real source assets, not build output), `vite.config.ts`, `tsconfig*.json`, `eslint.config.js`, `package.json`.
- **Do NOT vendor** `server/public/index.js`, `server/public/index.js.map`, or `server/dist/` — these are the committed build artifacts to eliminate.

**Break the cross-dir dependency on `server/`.** The app imports zod schemas via relative paths into `server/src/...` (see §1) and reads two JSON files from the server tree:
- `app/vite.config.ts` imports `../server/src/serverInfo.json` (for Sentry release name).
- `app/src/api/serverInfo.ts` imports `../../../server/src/serverInfo.json`.
- Every `app/src/api/*Schema.ts` re-exports from `server/src/...`.

Plan: copy the shared schema files into the app as the canonical copy, e.g. `app/src/api/schemas/` (deviceStatusSchema, settingsSchema, schedulesSchema, serverStatusSchema, servicesSchema, jobsSchema, vitalsRecordSchema, movementRecordSchema, sleepRecordsSchema, timeZones), and rewrite the `export * from '../../../server/...'` shims to point at the local copies. Add a `serverInfo.json` to the app (or replace the version-check with the fork's endpoint). These zod schemas are the shared contract `podd` reimplements in Rust — keep them as the reference.

**Clean build config:**
- Change `vite.config.ts` `build.outDir` from `'../server/public/'` to the default `'dist/'` (or a `podd`-owned static dir passed at build time). Remove the Sentry Vite plugin / `serverInfo.json` import, or gate it behind an env var so a default build has no external coupling.
- `.gitignore`: add `dist/`, keep `node_modules/`, `.env*`. Never commit build output.
- Build command: `npm ci && npm run build` produces `dist/` (static `index.js`/`index.css`/`index.html` + assets). `podd` serves that dir statically and provides the SPA history-fallback (serve `index.html` for non-`/api` routes), matching the Express catch-all in `server/src/setup/routes.ts`.

**Nix:** package the SPA with `buildNpmPackage` (or `pnpm`/`npm` + `fetchNpmDeps` / `importNpmLock`) — `npm ci` offline from a pinned `package-lock.json`, `npm run build`, install `dist/` into `$out`. `podd`'s Nix build references that derivation's `dist/` as its embedded/served static asset root. No compiled JS ever enters git; the artifact is reproducible from source. Node is pinned (`.nvmrc` / Volta `24.11.0`) — mirror that in the Nix toolchain.

**MSW / demo mode:** `app/src/mocks/*` + `build:demo` give a fully static, backend-free demo (mocks all `/api/*`). Worth keeping — it's a zero-backend way to smoke-test the vendored UI and doubles as living documentation of the endpoint set.

**License hygiene:** retain root MIT `LICENSE.md` and attribution to `throwaway31265/free-sleep`; keep the in-app license modal. Optionally add SPDX headers to vendored source (upstream has none).

---

## Appendix: key file paths (in clone)
- Frontend API layer: `app/src/api/*.ts` (axios instance `app/src/api/api.ts`).
- Shared schemas (source of truth): `server/src/routes/deviceStatus/deviceStatusSchema.ts`, `server/src/db/{settingsSchema,schedulesSchema,servicesSchema,vitalsRecordSchema,movementRecordSchema,sleepRecordsSchema,timeZones}.ts`, `server/src/routes/serverStatus/serverStatusSchema.ts`, `server/src/routes/jobs/jobsSchema.ts`.
- Route handlers: `server/src/routes/**`, registered in `server/src/setup/routes.ts` (middleware/CORS in `server/src/setup/middleware.ts`).
- Device write semantics: `server/src/routes/deviceStatus/updateDeviceStatus.ts`.
- Committed build artifacts: `server/public/index.js(.map)`, `server/dist/**`.
- API doc (partially stale): `server/API.md`.
