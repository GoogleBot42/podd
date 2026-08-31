import moment from 'moment-timezone';
import type { Services } from '@api/services.ts';
import type { Schedules } from '@api/schedulesSchema.ts';
import type { Settings } from '@api/settingsSchema.ts';
import type { DeviceStatus } from '@api/deviceStatusSchema';
import type { MovementRecord } from '@api/movement.ts';
import type { SleepRecord } from '@api/sleepSchema.ts';
import type { VitalsRecord } from '@api/vitals.ts';
import type { ServerStatus } from '@api/serverStatusSchema.ts';
import type { UpdatesReport } from '@api/schemas/updatesSchema.ts';
import type { Jobs } from '@api/jobs.ts';
import type { MqttSettings, MqttSettingsPatch } from '@api/mqttSchema.ts';
import { UI_VERSION } from '@lib/version.ts';

type Side = 'left' | 'right';

type LogStore = Record<string, string[]>;

type QueryFilters = {
  startTime?: string;
  endTime?: string;
  side?: Side;
};

const now = new Date();
const HOURS_TO_MS = 60 * 60 * 1000;
const MINUTES_TO_MS = 60 * 1000;

const clone = <T>(value: T): T => {
  const structured = (globalThis as typeof globalThis & {
    structuredClone?: <U>(source: U) => U;
  }).structuredClone;
  if (typeof structured === 'function') {
    return structured(value);
  }
  return JSON.parse(JSON.stringify(value)) as T;
};

const toIso = (date: Date) => date.toISOString();

const createSleepRecord = (id: number, side: Side, nightsAgo: number, durationHours: number, exits: number): SleepRecord => {
  const start = new Date(now.getTime() - nightsAgo * 24 * HOURS_TO_MS + (side === 'left' ? -30 * MINUTES_TO_MS : 0));
  const end = new Date(start.getTime() + durationHours * HOURS_TO_MS);
  const presentInterval: [string, string] = [toIso(start), toIso(end)];
  const absenceStart = new Date(start.getTime() + (durationHours / 2) * HOURS_TO_MS);
  const absenceEnd = new Date(absenceStart.getTime() + 10 * MINUTES_TO_MS);
  const notPresentInterval: [string, string] = [toIso(absenceStart), toIso(absenceEnd)];

  return {
    id,
    side,
    entered_bed_at: presentInterval[0],
    left_bed_at: presentInterval[1],
    sleep_period_seconds: Math.round(durationHours * 60 * 60),
    times_exited_bed: exits,
    present_intervals: [presentInterval],
    not_present_intervals: exits > 0 ? [notPresentInterval] : [],
  };
};

const clamp = (v: number, lo: number, hi: number) => Math.max(lo, Math.min(hi, v));
const lerp = (a: number, b: number, t: number) => a + (b - a) * t;

/** 8 hourly samples over 8 hours:
 * 50  → 300 → 1400 → 50 (piecewise-linear)
 */
const createMovementRecords = (): MovementRecord[] => {
  const H = 8; // 8 total records, hourly
  const start = moment.tz(moment.tz.guess()).startOf('hour').subtract(H - 1, 'hours');

  const keyframes = [
    { f: 0.0, v: 50 },
    { f: 0.25, v: 300 },
    { f: 0.5, v: 1400 },
    { f: 1.0, v: 50 },
  ];

  const interp = (f: number) => {
    // find segment [k, k+1] where f lies
    for (let i = 0; i < keyframes.length - 1; i++) {
      const a = keyframes[i], b = keyframes[i + 1];
      if (f <= b.f) {
        const t = (f - a.f) / (b.f - a.f);
        return lerp(a.v, b.v, t);
      }
    }
    return keyframes[keyframes.length - 1].v;
  };

  const records: MovementRecord[] = [];
  for (let i = 0; i < H; i++) {
    const frac = i / (H - 1); // 0 → 1 across 8 points
    // Epoch seconds, matching MovementRecord (and what the API emits).
    const timestamp = start.clone().add(i, 'hours').unix();
    const value = Math.round(clamp(interp(frac), 1, 1400));
    const side: Side = i % 2 === 0 ? 'left' : 'right';

    records.push({
      id: i + 1,
      side,
      timestamp,
      total_movement: value, // 1 → 1400 following the 50→300→1400→50 curve
    });
  }
  return records;
};

const createVitalsRecords = (): VitalsRecord[] => {
  const records: VitalsRecord[] = [];
  const sampleHours = 12;
  const intervalMinutes = 15;
  for (let index = 0; index <= (sampleHours * 60) / intervalMinutes; index += 1) {
    const timestamp = Math.floor((now.getTime() - index * intervalMinutes * MINUTES_TO_MS) / 1000);
    const side: Side = index % 2 === 0 ? 'left' : 'right';
    const heartRate = 55 + ((index * 7) % 10);
    const hrv = 80 + ((index * 5) % 20);
    const breathingRate = 10 + ((index * 3) % 5);
    records.push({ side, timestamp, heart_rate: heartRate, hrv, breathing_rate: breathingRate });
  }
  return records;
};

const createSchedules = (): Schedules => ({
  left: {
    sunday: {
      temperatures: { '06:00': 82, '07:00': 100 },
      power: { on: '21:30', off: '07:30', enabled: true, onTemperature: 60 },
      alarm: { time: '07:30', vibrationIntensity: 2, vibrationPattern: 'rise', duration: 10, enabled: true, alarmTemperature: 82 },
    },
    monday: {
      temperatures: { '06:00': 82, '07:00': 100 },
      power: { on: '21:30', off: '07:00', enabled: true, onTemperature: 60 },
      alarm: { time: '07:00', vibrationIntensity: 3, vibrationPattern: 'double', duration: 10, enabled: true, alarmTemperature: 83 },
    },
    tuesday: {
      temperatures: { '06:00': 82, '07:00': 100 },
      power: { on: '21:30', off: '07:00', enabled: true, onTemperature: 60 },
      alarm: { time: '07:00', vibrationIntensity: 2, vibrationPattern: 'rise', duration: 8, enabled: true, alarmTemperature: 82 },
    },
    wednesday: {
      temperatures: { '06:00': 82, '07:00': 100 },
      power: { on: '21:30', off: '07:00', enabled: true, onTemperature: 60 },
      alarm: { time: '07:00', vibrationIntensity: 1, vibrationPattern: 'rise', duration: 8, enabled: true, alarmTemperature: 82 },
    },
    thursday: {
      temperatures: { '06:00': 82, '07:00': 100 },
      power: { on: '21:30', off: '07:00', enabled: true, onTemperature: 60 },
      alarm: { time: '07:00', vibrationIntensity: 2, vibrationPattern: 'rise', duration: 8, enabled: true, alarmTemperature: 81 },
    },
    friday: {
      temperatures: { '06:00': 82, '07:00': 100 },
      power: { on: '22:00', off: '08:00', enabled: true, onTemperature: 60 },
      alarm: { time: '08:00', vibrationIntensity: 3, vibrationPattern: 'rise', duration: 12, enabled: true, alarmTemperature: 84 },
    },
    saturday: {
      temperatures: { '06:00': 82, '07:00': 100 },
      power: { on: '22:30', off: '09:00', enabled: true, onTemperature: 60 },
      alarm: { time: '09:00', vibrationIntensity: 1, vibrationPattern: 'rise', duration: 12, enabled: true, alarmTemperature: 85 },
    },
  },
  right: {
    sunday: {
      temperatures: { '06:00': 82, '07:00': 100 },
      power: { on: '21:00', off: '07:00', enabled: true, onTemperature: 60 },
      alarm: { time: '07:00', vibrationIntensity: 2, vibrationPattern: 'rise', duration: 10, enabled: true, alarmTemperature: 84 },
    },
    monday: {
      temperatures: { '06:00': 82, '07:00': 100 },
      power: { on: '21:00', off: '08:30', enabled: true, onTemperature: 60 },
      alarm: { time: '06:30', vibrationIntensity: 3, vibrationPattern: 'double', duration: 10, enabled: true, alarmTemperature: 84 },
    },
    tuesday: {
      temperatures: { '06:00': 82, '07:00': 100 },
      power: { on: '21:15', off: '06:30', enabled: true, onTemperature: 60 },
      alarm: { time: '06:30', vibrationIntensity: 3, vibrationPattern: 'double', duration: 8, enabled: true, alarmTemperature: 83 },
    },
    wednesday: {
      temperatures: { '05:00': 82, '6:00': 100 },
      power: { on: '21:15', off: '06:30', enabled: true, onTemperature: 60 },
      alarm: { time: '06:30', vibrationIntensity: 2, vibrationPattern: 'double', duration: 8, enabled: true, alarmTemperature: 83 },
    },
    thursday: {
      temperatures: { '05:00': 82, '6:00': 100 },
      power: { on: '21:15', off: '06:30', enabled: true, onTemperature: 60 },
      alarm: { time: '06:30', vibrationIntensity: 2, vibrationPattern: 'double', duration: 8, enabled: true, alarmTemperature: 83 },
    },
    friday: {
      temperatures: { '05:00': 82, '6:00': 100 },
      power: { on: '22:00', off: '07:30', enabled: true, onTemperature: 60 },
      alarm: { time: '07:30', vibrationIntensity: 3, vibrationPattern: 'rise', duration: 12, enabled: true, alarmTemperature: 85 },
    },
    saturday: {
      temperatures: { '05:00': 82, '6:00': 100 },
      power: { on: '22:30', off: '08:30', enabled: true, onTemperature: 60 },
      alarm: { time: '08:30', vibrationIntensity: 2, vibrationPattern: 'rise', duration: 12, enabled: true, alarmTemperature: 86 },
    },
  },
});

const createSettings = (): Settings => ({
  id: 'demo-user',
  timeZone: 'America/Los_Angeles',
  temperatureFormat: 'fahrenheit',
  rebootDaily: true,
  left: {
    name: 'Left side',
    awayMode: false,
    scheduleOverrides: {
      temperatureSchedules: { disabled: false, expiresAt: '' },
      alarm: { disabled: false, timeOverride: '', expiresAt: '' },
    },
    taps: {
      doubleTap: {
        type: 'temperature',
        change: 'decrement',
        amount: 1,
      },
      tripleTap: {
        type: 'temperature',
        change: 'increment',
        amount: 1,
      },
      quadTap: {
        type: 'alarm',
        behavior: 'dismiss',
        snoozeDuration: 60,
        inactiveAlarmBehavior: 'power',
      },
    }
  },
  right: {
    name: 'Right side',
    awayMode: false,
    scheduleOverrides: {
      temperatureSchedules: { disabled: false, expiresAt: '' },
      alarm: { disabled: false, timeOverride: '', expiresAt: '' },
    },
    taps: {
      doubleTap: {
        type: 'temperature',
        change: 'decrement',
        amount: 1,
      },
      tripleTap: {
        type: 'temperature',
        change: 'increment',
        amount: 1,
      },
      quadTap: {
        type: 'alarm',
        behavior: 'dismiss',
        snoozeDuration: 60,
        inactiveAlarmBehavior: 'power',
      },
    }
  },
  primePodDaily: { enabled: true, time: '14:30' },
});

const createServices = (): Services => ({
  biometrics: {
    enabled: true,
    jobs: {
      installation: {
        name: 'Biometrics installation',
        description: 'Initial biometric sensor installation',
        status: 'healthy',
        message: 'Installation completed successfully',
        timestamp: now.toISOString(),
      },
      stream: {
        name: 'Biometrics stream',
        description: 'Sensor data ingestion service',
        status: 'healthy',
        message: 'Streaming data smoothly',
        timestamp: new Date(now.getTime() - 2 * MINUTES_TO_MS).toISOString(),
      },
      analyzeSleepLeft: {
        name: 'Analyze sleep - left',
        description: 'Analyzes sleep data for left side',
        status: 'healthy',
        message: 'Last run completed 15 minutes ago',
        timestamp: new Date(now.getTime() - 15 * MINUTES_TO_MS).toISOString(),
      },
      analyzeSleepRight: {
        name: 'Analyze sleep - right',
        description: 'Analyzes sleep data for right side',
        status: 'healthy',
        message: 'Next run scheduled soon',
        timestamp: new Date(now.getTime() - 12 * MINUTES_TO_MS).toISOString(),
      },
      calibrateLeft: {
        name: 'Calibration job - Left',
        description: 'Sensor calibration for left side',
        status: 'healthy',
        message: 'Calibrated this morning',
        timestamp: new Date(now.getTime() - 3 * HOURS_TO_MS).toISOString(),
      },
      calibrateRight: {
        name: 'Calibration job - Right',
        description: 'Sensor calibration for right side',
        status: 'healthy',
        message: 'Calibrated this morning',
        timestamp: new Date(now.getTime() - 3 * HOURS_TO_MS).toISOString(),
      },
    },
  },
});

const createDeviceStatus = (): DeviceStatus => ({
  left: {
    currentTemperatureLevel: 4,
    currentTemperatureF: 82,
    targetTemperatureF: 84,
    secondsRemaining: 1_200,
    isOn: true,
    isAlarmVibrating: false,
  },
  right: {
    currentTemperatureLevel: 5,
    currentTemperatureF: 85,
    targetTemperatureF: 86,
    secondsRemaining: 1_560,
    isOn: true,
    isAlarmVibrating: false,
  },
  waterLevel: 'true',
  isPriming: true,
  settings: {
    v: 12,
    gainLeft: 3,
    gainRight: 4,
    ledBrightness: 60,
  },
  coverVersion: 'Pod 5',
  hubVersion: 'Pod 5',
  freeSleep: {
    // The mock "daemon" is this bundle, so it reports this bundle's build stamp.
    version: UI_VERSION,
    branch: 'demo',
  },
  wifiStrength: 82,
});

// podd's real subsystems (see crates/podd-core/src/health.rs). Demo mode shows
// a plausible mid-life mix rather than the all-green fiction free-sleep shipped.
const createServerStatus = (): ServerStatus => ({
  api: {
    name: 'api',
    status: 'healthy',
    description: 'This HTTP API',
    message: 'OK',
  },
  clock: {
    name: 'clock',
    status: 'healthy',
    description: 'System clock / NTP sync (gates scheduled alarms)',
    message: 'NTP-synced; scheduled alarms armed',
    timestamp: new Date(now.getTime() - 42 * MINUTES_TO_MS).toISOString(),
  },
  coverControl: {
    name: 'coverControl',
    status: 'healthy',
    description: 'Cover control MCU: TEC, pump, water level',
    message: 'cover MCU awake; driving the TEC/pump',
    timestamp: new Date(now.getTime() - 42 * MINUTES_TO_MS).toISOString(),
  },
  mqtt: {
    name: 'mqtt',
    status: 'retrying',
    description: 'MQTT broker link (Home Assistant)',
    message: 'network timeout; reconnecting in 8s (attempt 3)',
    timestamp: new Date(now.getTime() - 2 * MINUTES_TO_MS).toISOString(),
  },
  sensor: {
    name: 'sensor',
    status: 'started',
    description: 'Sensor MCU: presence, piezo/HR, taps, alarms',
    message: 'connected; MCU may ignore actuation writes for ~60 s after a restart',
    timestamp: new Date(now.getTime() - 30 * 1000).toISOString(),
  },
});

const createUpdates = (): UpdatesReport => ({
  daemon: { version: UI_VERSION, rev: 'demo0000' },
  updater: {
    enabled: true,
    channel: 'stable',
    mode: 'manual',
    currentVersions: [
      { kind: 'app', version: UI_VERSION },
      { kind: 'os', version: 'os-2026.08.1' },
    ],
    lastCheckUnix: Math.floor((now.getTime() - 12 * MINUTES_TO_MS) / 1000),
    lastCheckOk: true,
    available: [],
    lastError: null,
    lastApplied: `app -> ${UI_VERSION} (committed)`,
  },
});

const createLogs = (): LogStore => ({
  'podd.log': [
    `[${new Date(now.getTime() - 3 * MINUTES_TO_MS).toISOString()}] INFO Starting podd demo mode`,
    `[${new Date(now.getTime() - 2 * MINUTES_TO_MS).toISOString()}] INFO Schedules loaded successfully`,
    `[${new Date(now.getTime() - 90 * 1000).toISOString()}] INFO Biometrics stream connected`,
    `[${new Date(now.getTime() - 30 * 1000).toISOString()}] INFO Demo data refreshed`,
  ],
  'scheduler.log': [
    `[${new Date(now.getTime() - 6 * MINUTES_TO_MS).toISOString()}] INFO Prime job executed`,
    `[${new Date(now.getTime() - 4 * MINUTES_TO_MS).toISOString()}] INFO Temperature schedule updated`,
    `[${new Date(now.getTime() - 60 * 1000).toISOString()}] INFO Nightly reboot completed`,
  ],
});

let sleepRecords = [
  createSleepRecord(5, 'left', 3, 7.1, 0),
  createSleepRecord(6, 'right', 3, 7.0, 0),


  createSleepRecord(3, 'left', 2, 7.8, 1),
  createSleepRecord(4, 'right', 2, 7.4, 2),

  createSleepRecord(1, 'left', 1, 7.5, 1),
  createSleepRecord(2, 'right', 1, 7.2, 0),
];

const movementRecords = createMovementRecords();
const vitalsRecords = createVitalsRecords();
let schedules = createSchedules();
let settings = createSettings();
let services = createServices();
let mqtt: MqttSettings = {
  enabled: true,
  server: 'homeassistant.local',
  port: 1883,
  user: 'pod',
  passwordSet: true,
};
let deviceStatus = createDeviceStatus();
let serverStatus = createServerStatus();
let updates = createUpdates();
let logsStore = createLogs();

export const mergeDeep = (target: unknown, source: unknown): unknown => {
  if (source === undefined || source === null) {
    return target;
  }
  if (Array.isArray(source)) {
    return Array.isArray(target) ? source.slice() : source.slice();
  }
  if (typeof source === 'object') {
    const targetObj = typeof target === 'object' && target !== null ? target as Record<string, unknown> : {};
    const sourceObj = source as Record<string, unknown>;
    const result: Record<string, unknown> = { ...targetObj };
    Object.entries(sourceObj).forEach(([key, value]) => {
      result[key] = mergeDeep(result[key], value);
    });
    return result;
  }
  return source;
};

export const getServices = () => services;
export const updateServices = (partial: Partial<Services>) => {
  services = mergeDeep(clone(services), partial) as Services;
  return services;
};

export const getSchedules = () => schedules;
export const updateSchedules = (partial: Partial<Schedules>) => {
  schedules = mergeDeep(clone(schedules), partial) as Schedules;
  return schedules;
};

export const getSettings = () => settings;
export const updateSettings = (partial: Partial<Settings>) => {
  const partialCopy = { ...partial };
  // Never allow overwriting the generated ID in demo mode
  delete (partialCopy as { id?: string }).id;
  settings = mergeDeep(clone(settings), partialCopy) as Settings;
  return settings;
};

export const getMqtt = () => mqtt;
/// Mirrors the real endpoint: the password is stored, never returned — an
/// absent one keeps what is there, an empty one clears it.
export const updateMqtt = (patch: MqttSettingsPatch) => {
  const { password, ...rest } = patch;
  mqtt = {
    ...mqtt,
    ...rest,
    passwordSet: password === undefined ? mqtt.passwordSet : password !== '',
  };
  return mqtt;
};

export const getDeviceStatus = () => deviceStatus;
export const updateDeviceStatus = (partial: Partial<DeviceStatus>) => {
  deviceStatus = mergeDeep(clone(deviceStatus), partial) as DeviceStatus;
  return deviceStatus;
};

export const getServerStatus = () => serverStatus;
export const setServerStatus = (next: ServerStatus) => {
  serverStatus = clone(next);
  return serverStatus;
};

export const getUpdates = () => updates;

// Demo "check now": the channel is always reachable and always up to date.
export const checkUpdates = () => {
  if (updates.updater) {
    updates = {
      ...updates,
      updater: {
        ...updates.updater,
        lastCheckUnix: Math.floor(Date.now() / 1000),
        lastCheckOk: true,
        lastError: null,
      },
    };
  }
  return updates.updater;
};

// Demo "switch channel": the daemon persists the choice and drops the previous
// channel's offers + check verdict, so the demo does the same.
export const setUpdatesChannel = (channel: string) => {
  if (updates.updater) {
    updates = {
      ...updates,
      updater: {
        ...updates.updater,
        channel,
        available: [],
        lastCheckUnix: null,
        lastCheckOk: false,
        lastError: null,
      },
    };
  }
  return updates.updater;
};

export const listSleepRecords = () => sleepRecords;
export const setSleepRecords = (records: SleepRecord[]) => {
  sleepRecords = records;
  return sleepRecords;
};

export const listMovementRecords = () => movementRecords;


export const listVitalsRecords = () => vitalsRecords;


export const listLogs = () => logsStore;
export const setLogs = (next: LogStore) => {
  logsStore = next;
  return logsStore;
};

export const getLogFiles = () => Object.keys(logsStore);

export const appendLogEntry = (file: string, message: string) => {
  if (!logsStore[file]) {
    logsStore[file] = [];
  }
  logsStore[file].push(message);
  if (logsStore[file].length > 1000) {
    logsStore[file] = logsStore[file].slice(-1000);
  }
};

// `side` is widened to `string` because sleep records type theirs that way.
export const filterByQuery = <T extends { side?: string }>(records: T[], filters: QueryFilters, getTimestamp: (record: T) => number) => {
  const start = filters.startTime ? Date.parse(filters.startTime) : undefined;
  const end = filters.endTime ? Date.parse(filters.endTime) : undefined;
  const side = filters.side;

  return records.filter((record) => {
    if (side && record.side !== side) {
      return false;
    }
    const timestamp = getTimestamp(record);
    if (Number.isFinite(start) && start !== undefined && timestamp < start) {
      return false;
    }
    if (Number.isFinite(end) && end !== undefined && timestamp > end) {
      return false;
    }
    return true;
  });
};

export const handleJobs = (jobs: Jobs) => {
  const timestamp = new Date().toISOString();
  jobs.forEach((job) => {
    appendLogEntry('podd.log', `[${timestamp}] INFO Job executed: ${job}`);
  });
};

