# podd architecture

Distilled from the reverse-engineering in `docs/research/` and the plan in
`docs/REPLACEMENT_PLAN.md`. This describes the shape as built. The full userland
(protocol, control core, API, web UI, signed update system, on-device OTA agent,
CI, and installers) is implemented; what remains is the live hardware cutover —
MCU control writes are gated behind `PODD_DRY_RUN` (default on), and the Pod-4
sensor packet payloads are still being decoded.

## Runtime model

One daemon (`podd`), one process, supervised by systemd (`Restart=always`).
Following opensleep, the main loop loads config into a `tokio::sync::watch`,
resets both STM32s via the PCAL6416A I²C expander, then runs long-lived tasks in
a `JoinSet`/`select!`. State fan-out sits behind a **`StateBus`** (`broadcast`)
so MQTT *and* the web API are peer consumers of the same telemetry — neither is
privileged.

```
                 ┌───────────────── podd (one process) ─────────────────┐
   web UI  ◄──►  │  api (axum REST + SSE, serves embedded SPA)           │
   HA/MQTT ◄──►  │  mqtt                                                 │
                 │        ▲ StateBus (broadcast)     ▼ desired-target watch│
                 │  schedule/thermostat ──► frozen::run ─UART─► Frozen MCU │
                 │  sensor::run ─UART─► Sensor MCU     led ─I²C─► LED ring │
                 │  update agent (verifies signed manifests, atomic swap) │
                 │  mcu-flash (quiesces UART, flashes .bbin) [on demand]  │
                 └───────────────────────────────────────────────────────┘
```

## Hardware facts (ground truth)

- **Frozen subsystem** (TEC/pump/solenoid/water): `/dev/ttymxc2`, 38400
  (confirmed on Pod 4 hardware).
- **Sensor subsystem** (bed-temp/capacitance/piezo/vibration): `/dev/ttymxc0`.
  Bootloader baud 38400 (Pod 3; the Pod-4 bootloader baud is unconfirmed).
  Firmware baud is **115200 on a Pod 3 cover, 921600 on a Pod 4 cover** — the
  cover selects it (`config`'s `cover: pod3` / `cover: pod4`); opensleep's
  hard-coded 115200 is wrong for Pod 4. See
  `crates/podd-core/src/config/device.rs`.
- **LSP UART framing**: `0x7E | LEN | payload | CRC16`, CRC-CCITT seed `0x1D0F`,
  response opcode = request `| 0x80`. **No byte-stuffing exists** — a frame
  whose payload or CRC contains `0x7E` is silently dropped by the frozen MCU's
  parser (no echo, no error). podd avoids the byte by nudging setpoints instead
  (`FrozenTarget::delimiter_safe` in `pod-proto`). (Now the `pod-proto` crate,
  extracted from opensleep `common/`.)
- **I²C** (`/dev/i2c-1`): PCAL6416A `0x20` (MCU reset/enable + button),
  IS31FL3194 LED `0x53`, RV-3028 RTC `0x68`.
- **Temps on the API wire**: integer °F, 55–110 (free-sleep-compatible). Sides:
  `left` / `right`. Internally opensleep uses °C `f32`.

## Crates (as built)

The eight workspace members and how the earlier planned split maps onto them:

| Crate | Contents | Source |
|---|---|---|
| `pod-proto` | LSP framing/codec/CRC, both subsystems' packet+command tables, `profile.rs` thermostat math | extracted from opensleep `common/` + `frozen/profile.rs` |
| `podd-core` | `frozen`, `sensor`, `led`, `reset`, `config`, `mqtt`, `bus` kept ~1:1 with upstream for cherry-picks. (Absorbs the planned `pod-hal`: reset via PCAL6416A + IS31FL3194 LED.) | opensleep |
| `api` | axum REST + SSE + embedded SPA; free-sleep-compatible endpoints (biometrics deferred). Holds schedule/settings persistence + endpoints | new |
| `pod-update` | shared, host+device update core: manifest schema, SHA-256 digests, Ed25519 sign/verify, `TrustPolicy` | new |
| `podup` | host release CLI: `keygen` / `pack` / `release` / `verify` | new (on `pod-update`) |
| `pod-updater` | on-device OTA agent: fetch manifest → verify (`pod-update`) → atomic release swap → health-gate → rollback; Tier-1 OS + Tier-3 MCU `.bbin` flash plumbing (dry-run-gated). Absorbs the planned `mcu-flash` and `update` crates | new (on `pod-update`) |
| `pod-probe` | read-only serial probe validating `pod-proto` against live MCUs | new |
| `podd` | the daemon binary: wires `podd-core` + `api` + `pod-updater` together | opensleep fork |

**Planned, not yet built:** a dedicated `onboarding` path (config-file /
local-web WiFi bring-up), a full autonomous weekday scheduler loop (the
thermostat curve math exists in `pod-proto`'s `profile.rs`; the API stores
schedules today), and the L2 OS-image release artifact (`podd-rootfs.tar.gz`).

## The three gaps opensleep leaves us

1. **MCU flashing** — opensleep only *talks* to the MCUs (JumpToFirmware / GetFirmware);
   it has no erase/write/verify or `.bbin` parsing. That flashing path is new,
   and lives in `pod-updater` (Tier-3), gated behind a dry-run default.
2. **Scheduler** — opensleep has a per-side daily temperature curve
   (`profile.rs` lerp over a sleep→wake window) but no weekday schedules, manual
   override, or set-now. The curve math is reused in `pod-proto`; the `api`
   crate persists schedules/settings, and a full autonomous scheduler loop is
   still being wired.
3. **Web API** — opensleep is MQTT-only. `api` is all-new (axum), serving the
   forked free-sleep SPA and the compat endpoints.

## Compat API (free-sleep-compatible, phase 1)

All JSON under `/api`. Control endpoints (implemented):
`GET/POST /deviceStatus`, `GET/POST /settings`, `GET/POST /schedules`,
`POST /alarm`, `POST /execute`, `POST /jobs` (reboot/update),
`GET/POST /services`, `GET /serverStatus`, `GET /logs` + `GET /logs/:file` (SSE),
`GET/POST /metrics/presence`. POST bodies are deep-merged; `deviceStatus`/`jobs`
return 204, others return the merged doc.

`GET /serverStatus` keeps free-sleep's `StatusInfo` wire shape but **not** its
key set: it reports podd's own subsystems (`sensor`, `coverControl`, `mqtt`,
`clock`, `api`) from the `podd_core::health` registry, which the managers
publish into at transitions they already detect. free-sleep's twelve Node
service keys (`express`, `database`, `franken`, …) described a server that
doesn't exist here and were hardcoded healthy.

Biometrics endpoints (`/metrics/sleep|vitals|movement`) are **deferred** — they
were free-sleep's Python/SQLite layer; we'll reimplement the piezo HR/HRV/breathing
DSP in Rust later (opensleep parses piezo samples but discards them; `rustfft` is
already a dependency).

## Update system

See `docs/REPLACEMENT_PLAN.md` §9. `pod-update` is the shared core; `podup`
builds/(optionally)signs releases on the host; the `pod-updater` crate applies
them on the device. App releases are read-only squashfs images swapped via a `current`
symlink behind a canary health check. Integrity (SHA-256) is always enforced;
authenticity (Ed25519 signature) is **owner-controlled and optional**
(`TrustPolicy::{AllowUnsigned, RequireSigned(keys)}`) so anyone can update their
own device or fork.

### Scope — what the update system can change (and what it can't)

| Tier | Component | `podup` builds it? | Applied by | Example |
|---|---|---|---|---|
| 2 | **App**: `podd` + web UI + config schema/migrations | yes (packs to squashfs) | `pod-updater` (symlink swap, no reboot) | ship a new scheduler / UI |
| 3 | **MCU Frozen fw** (`.bbin`) | yes (records blob) | `pod-updater` Tier-3 (quiesce UART, flash, verify; dry-run-gated) | restore/replace STM32 fw |
| 3 | **MCU Sensor fw** (`.bbin`) | yes | `pod-updater` Tier-3 (dry-run-gated) | " |
| 1 | **OS image** (kernel+DTB+rootfs) — L2 | yes (records the `os.raucb` bundle) | **RAUC** on the clean-room image: install to the inactive slot, U-Boot `BOOT_ORDER`/bootcount flip, auto-rollback (see [CLEANROOM-OS.md](CLEANROOM-OS.md)). `pod-updater` Tier-1 only fetches/verifies and logs the slot plan today — the live apply is unimplemented and the RAUC wiring is still open (#46–#48) | kernel/lib bump |
| 0 | **Bootloader** | version recorded only | manual (never auto) | — |

**Not in scope for `podup`:** personal runtime state (schedules/temps/history —
device-local, never shipped or clobbered; only the config *schema* migrates); the
initial eMMC install/provisioning (serial/UUU/mtkclient/SD — a separate flow);
and *applying* updates (that's the device-side `pod-updater` agent, not
`podup`). A `podup release` can bundle any subset of {app, os, mcu-frozen,
mcu-sensor} into one versioned manifest.
