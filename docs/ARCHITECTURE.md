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
| `api` | axum REST + SSE + embedded SPA; free-sleep-compatible endpoints. Holds schedule/settings persistence + endpoints | new |
| `pod-update` | shared, host+device update core: manifest schema, SHA-256 digests, Ed25519 sign/verify, `TrustPolicy` | new |
| `podup` | host release CLI: `keygen` / `pack` / `release` / `verify` | new (on `pod-update`) |
| `pod-updater` | on-device OTA agent: fetch manifest → verify (`pod-update`) → atomic release swap → health-gate → rollback; Tier-1 OS + Tier-3 MCU `.bbin` flash plumbing (dry-run-gated). Absorbs the planned `mcu-flash` and `update` crates | new (on `pod-update`) |
| `pod-probe` | read-only serial probe validating `pod-proto` against live MCUs | new |
| `podd` | the daemon binary: wires `podd-core` + `api` + `pod-updater` together | opensleep fork |

**Planned, not yet built:** a dedicated `onboarding` path (config-file /
local-web WiFi bring-up).

## The three gaps opensleep leaves us

1. **MCU flashing** — opensleep only *talks* to the MCUs (JumpToFirmware / GetFirmware);
   it has no erase/write/verify or `.bbin` parsing. That flashing path is new,
   and lives in `pod-updater` (Tier-3), gated behind a dry-run default.
2. **Scheduler** — opensleep has a per-side daily temperature curve
   (`profile.rs` lerp over a sleep→wake window) but no weekday schedules, manual
   override, or set-now. All three are new: the curve math is reused in
   `pod-proto`; manual override/set-now live in `podd-core`'s frozen manager;
   and `podd-core::schedule` resolves the per-weekday `schedules.json`
   document (free-sleep schema, persisted by `api`, step temps) into targets.
   Ownership rule: a side follows its weekly schedule iff any weekday row has
   `power.enabled`, else the `config.ron` profile — see the `schedule` module
   docs. Alarms follow the same rule (`podd_core::alarm`): an owned side's
   per-day alarm blocks drive the sensor manager's vibration alarms, an
   unowned side keeps the profile alarm (wake − offset), and the per-side
   one-shot overrides in `settings.json` (`scheduleOverrides`: skip/move the
   next alarm, suspend a temperature schedule) apply on top. The daemon also
   reads `settings.json` (boot + `Command::SetSettings`) for those overrides
   and the daily-reboot flag (reboot at prime − 1 h, NTP-gated).
3. **Web API** — opensleep is MQTT-only. `api` is all-new (axum), serving the
   forked free-sleep SPA and the compat endpoints.

## Compat API (free-sleep-compatible, phase 1)

All JSON under `/api`. Control endpoints (implemented):
`GET/POST /deviceStatus`, `GET/POST /settings`, `GET/POST /schedules`,
`POST /alarm`, `POST /execute`, `POST /jobs`, `GET /services`,
`GET /serverStatus`, `GET /logs` + `GET /logs/:file` (SSE),
`GET/POST /metrics/presence`, plus the podd-only `GET/POST /mqtt`. POST bodies
are deep-merged; `deviceStatus`/`jobs` return 204, others return the merged doc.

Endpoints whose backing subsystem doesn't exist yet answer **501**, never a
no-op 204 (#32, #107): `POST /jobs` for the biometrics jobs (and, through
`PoddControl`, for `update` until the updater is wired; `reboot` is real,
dry-run-gated),
`POST /services` (nothing to configure — the only service in the document is
biometrics), `POST /execute`, and `settings` inside `POST /deviceStatus`.
`GET /services` still serves the free-sleep document shape, with every
biometrics job reported as not-implemented rather than hardcoded healthy.

`POST /settings`'s bridged fields (`primePodDaily`, per-side `awayMode`,
`timeZone`) edit the live `config.ron` over the command bus. podd-core's
dispatcher republishes the affected retained `opensleep/state/config/...`
topic whenever such a command actually changes the config — the MQTT action
path always did this inline, so without it a UI settings save left Home
Assistant on a stale retained value until the next broker reconnect (#106).

Beyond the free-sleep set, `GET /api/updates` is podd's own update-observability
surface (`REPLACEMENT_PLAN` §9): the running build stamp plus the update agent's
published `UpdateStatus` (channel, mode, installed version per tier, last check,
available components, last error/apply), with `POST /api/updates/check` and
`POST /api/updates/rollback` driving the two controls `pod-updater` implements.
`updater: null` means no agent is wired — distinct from "no updates available".
Applying an update is not routed (issue #1's other half), and the channel is
read-only because it is fixed at start-up from `PODD_UPDATER_CHANNEL`. The UI
renders all of it in Settings → Updates.

`GET/POST /mqtt` is podd-only (free-sleep has no MQTT): the broker link's
settings — `enabled`, `server`, `port`, `user`, `passwordSet` — behind the UI's
Settings → MQTT section (#18). Three rules shape it:

* the **password never leaves podd-core**. `bus::MqttSnapshot` mirrors the live
  config into the api layer without it, and a POST that omits `password` keeps
  the stored one (`""` clears it), so the UI can change a port without ever
  handling the secret;
* the write path (`Command::SetMqtt` → `apply_mqtt`) touches **only** `cfg.mqtt`
  and round-trips the rest of `config.ron` — alarm and profile blocks above all;
* nothing is republished to MQTT. Broker credentials must not travel through the
  broker they authenticate against.

`enabled: false` means no broker link at all: no manager, no connect attempt, no
reconnect backoff. The managers still take an `AsyncClient`, but a discarding
one. The connection is built at startup, so an edit applies on the next restart
— which the UI says out loud.

`GET /serverStatus` keeps free-sleep's `StatusInfo` wire shape but **not** its
key set: it reports podd's own subsystems (`sensor`, `coverControl`, `mqtt`,
`clock`, `api`) from the `podd_core::health` registry, which the managers
publish into at transitions they already detect. free-sleep's twelve Node
service keys (`express`, `database`, `franken`, …) described a server that
doesn't exist here and were hardcoded healthy.

### Biometrics

free-sleep's Python/pandas/SQLite biometrics layer is reimplemented in Rust
under `podd_core::biometrics`, running **streaming** off the live MCU streams
rather than post-hoc over raw dumps (podd keeps no raw sensor files):

- `processor.rs` — per-side vitals (HR / HRV / breathing) from the piezo
  stream, presence-gated, ~1 record/side/minute (#12).
- `sleep.rs` — per-side sleep sessions and movement (#141). Presence is the
  piezo 10 s rolling range (>= 20 000 counts for >= 70 % of the window) AND
  the calibrated capacitance presence (>= 90 % of the window); runs under 60 s
  are ignored, runs <= 15 min apart merge into one session (one "exited bed"
  each), and a session is recorded once it exceeds 3 h in bed. Movement is the
  per-sample sum of |Δ| over a side's three capacitance channels, max-pooled
  into 2-minute buckets while occupied.
- `store.rs` — one generic append-only JSONL store per history
  (`vitals.jsonl`, `sleep.jsonl`, `movement.jsonl` next to `config.ron`),
  torn-line tolerant, pruned to 90 days at startup.

Nothing here touches actuation; it is analysis and recording only. Records are
persisted only after NTP sync (a pre-sync 1970 timestamp would poison the
history), and an in-progress session is lost across a podd/sensor restart.

`/metrics/sleep|vitals|movement` serve those stores (empty arrays when a store
could not be opened), and `PUT`/`DELETE /metrics/sleep/{id}` edit or delete a
detected session — an edit reclips the stored intervals to the new window and
recomputes the derived fields. The `?startTime=&endTime=&side=` query the UI
sends is parsed and applied by `api::metrics` (the UI does no client-side
filtering). Record timestamps follow the UI's zod schemas: sleep records carry
ISO-8601 strings (`entered_bed_at`), vitals and movement samples carry **epoch
seconds**. free-sleep's batch `analyzeSleep*` / calibration jobs stay 501:
detection runs continuously instead.

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
| 1 | **OS image** (kernel+DTB+rootfs) — L2 | yes (records `os-<ver>.ext4.zst`) | `pod-updater` Tier-1 (`AbSlotWriter`, dry-run-gated): stream onto the inactive SD slot, readback-verify, arm the U-Boot boot-count trial; U-Boot auto-reverts a slot that can't boot and podd marks-good after a healthy boot (state machine owned by `os/board/.../uboot-env.txt`; see [CLEANROOM-OS.md](CLEANROOM-OS.md)) | kernel/lib bump |
| 0 | **Bootloader** | version recorded only | manual (never auto) | — |

**Not in scope for `podup`:** personal runtime state (schedules/temps/history —
device-local, never shipped or clobbered; only the config *schema* migrates); the
initial eMMC install/provisioning (serial/UUU/mtkclient/SD — a separate flow);
and *applying* updates (that's the device-side `pod-updater` agent, not
`podup`). A `podup release` can bundle any subset of {app, os, mcu-frozen,
mcu-sensor} into one versioned manifest.
