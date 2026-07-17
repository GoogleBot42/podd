# podd architecture (target)

Distilled from the reverse-engineering in `docs/research/` and the plan in
`docs/REPLACEMENT_PLAN.md`. This is the shape we are building toward; today only
`pod-update` + `podup` exist.

## Runtime model

One daemon (`podd`), one process, supervised by systemd (`Restart=always`).
Following opensleep, the main loop loads config into a `tokio::sync::watch`,
resets both STM32s via the PCAL6416A I²C expander, then runs long-lived tasks in
a `JoinSet`/`select!`. State fan-out sits behind a **`StateBus`** (`broadcast`)
so MQTT *and* the web API are peer consumers of the same telemetry — neither is
privileged.

```
                 ┌───────────────── podd (one process) ─────────────────┐
   web UI  ◄──►  │  api (axum REST + WS, serves embedded SPA)            │
   HA/MQTT ◄──►  │  mqtt                                                 │
                 │        ▲ StateBus (broadcast)     ▼ desired-target watch│
                 │  schedule/thermostat ──► frozen::run ─UART─► Frozen MCU │
                 │  sensor::run ─UART─► Sensor MCU     led ─I²C─► LED ring │
                 │  update agent (verifies signed manifests, atomic swap) │
                 │  mcu-flash (quiesces UART, flashes .bbin) [on demand]  │
                 └───────────────────────────────────────────────────────┘
```

## Hardware facts (ground truth)

- **Frozen subsystem** (TEC/pump/solenoid/water): `/dev/ttymxc2`, 38400.
- **Sensor subsystem** (bed-temp/capacitance/piezo/vibration): `/dev/ttymxc0`,
  38400 in bootloader → 115200 in firmware.
- **LSP UART framing**: `0x7E | LEN | payload | CRC16`, CRC-CCITT seed `0x1D0F`,
  response opcode = request `| 0x80`, `0x7E` escaped in payload. (opensleep
  `common/` — to be extracted into `pod-proto`.)
- **I²C** (`/dev/i2c-1`): PCAL6416A `0x20` (MCU reset/enable + button),
  IS31FL3194 LED `0x53`/`0x20`-driven, RV-3028 RTC `0x68`.
- **Temps on the API wire**: integer °F, 55–110 (free-sleep-compatible). Sides:
  `left` / `right`. Internally opensleep uses °C `f32`.

## Target crates

| Crate | Contents | Source |
|---|---|---|
| `pod-proto` | LSP framing/codec/CRC, both subsystems' packet+command tables, `profile.rs` thermostat math | extracted from opensleep `common/` + `frozen/profile.rs` |
| `pod-hal` | STM32 reset (PCAL6416A), LED (IS31FL3194) | opensleep `reset.rs` + `led/` |
| `podd-core` | `frozen`, `sensor`, `config` kept ~1:1 with upstream for cherry-picks | opensleep |
| `api` | axum REST + WS + embedded SPA; free-sleep-compatible endpoints | new |
| `schedule` | weekday schedules, manual override, set-now, alarms; publishes desired targets | new (wraps `profile.rs`) |
| `mcu-flash` | `.bbin` parser (128B big-endian header, magic `0x88888888`, STM32 flash `0x08000000`) + STM32 bootloader protocol | new |
| `update` | on-device agent: fetch manifest → verify (`pod-update`) → atomic release swap → health-gate → rollback | new (on top of `pod-update`) |
| `onboarding` | config-file / local-web WiFi bring-up | new |

## The three gaps opensleep leaves us (build these)

1. **MCU flashing** — opensleep only *talks* to the MCUs (JumpToFirmware / GetFirmware);
   it has no erase/write/verify or `.bbin` parsing. `mcu-flash` is entirely new.
2. **Scheduler** — opensleep has a per-side daily temperature curve
   (`profile.rs` lerp over a sleep→wake window) but no weekday schedules, manual
   override, or set-now. Reuse the curve, build the rest in `schedule`.
3. **Web API** — opensleep is MQTT-only. `api` is all-new (axum), serving the
   forked free-sleep SPA and the compat endpoints.

## Compat API (free-sleep-compatible, phase 1)

All JSON under `/api`. Control endpoints (implement now):
`GET/POST /deviceStatus`, `GET/POST /settings`, `GET/POST /schedules`,
`POST /alarm`, `POST /execute`, `POST /jobs` (reboot/update),
`GET/POST /services`, `GET /serverStatus`, `GET /logs` + `GET /logs/:file` (SSE),
`GET/POST /metrics/presence`. POST bodies are deep-merged; `deviceStatus`/`jobs`
return 204, others return the merged doc.

Biometrics endpoints (`/metrics/sleep|vitals|movement`) are **deferred** — they
were free-sleep's Python/SQLite layer; we'll reimplement the piezo HR/HRV/breathing
DSP in Rust later (opensleep parses piezo samples but discards them; `rustfft` is
already a dependency).

## Update system

See `docs/REPLACEMENT_PLAN.md` §9. Four tiers (App / OS / MCU / bootloader), all
signed (Ed25519, offline key), reproducible (content-addressed artifacts, nothing
built on device), atomic with auto-rollback. `pod-update` is the shared core;
`podup` builds/signs releases on the host; the `update` crate applies them on the
device. App releases are read-only squashfs images swapped via a `current` symlink
behind a canary health check.
