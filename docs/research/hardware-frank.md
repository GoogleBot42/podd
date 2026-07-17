# Eight Sleep Pod 3 — `frankenfirmware` ("frank") Hardware-Interface Spec

Reverse-engineered from the stock rootfs at
`scratchpad/work/rootfs-original`. Binary:
`opt/eight/bin/frankenfirmware` — ELF aarch64, PIE, stripped, 445,296 bytes,
BuildID `d6b6f4f16bba72508648368353b5b1deb5e5a20c`, built for GNU/Linux, uses
standalone **ASIO** (C++20 coroutines) and **CBOR**. Debug source tree:
`/usr/src/debug/frank/0.1-r0/git/firmware/src/…`.

All string/offset evidence below is from
`strings -n 6` (saved to `scratchpad/franks.txt`), `readelf`, and hexdumps.
`.rodata` loads at vaddr==fileoff `0x52b20`, so string offsets are also vaddrs.

---

## 0. System context / process model

- Launched by **systemd** unit `lib/systemd/system/frank.service`
  (`Type=notify`, `Restart=always`, `RestartSec=13`,
  `Requires=capybara.service`, `After=cagekeeper.service capybara.service`).
  Comment in the unit: *"restart if capybara does (otherwise the self-test
  breaks uart comms)"*.
- `ExecStart=/opt/eight/bin/frank.sh`, which waits for
  `/persistent/burrowing_complete` and `/deviceinfo/device-id`, then:
  `DAC_SOCKET=/deviceinfo/dac.sock exec /opt/eight/bin/frankenfirmware`.
- **sd_notify watchdog**: emits `READY=1` ("frankenfirmware initialized!"),
  `WATCHDOG=1` ("woof (x10)"); reads `WATCHDOG_USEC` env
  (warns if missing). A `tick` timer ("tick (x10000)") and `blink` also run.
- Working dir `/persistent` (a bind-mount of the active `/cage/<root>`,
  set up by `burrow.sh`). State files (`settings/`, `heat/`, `alarm.cbr`,
  and a copy of `subsystem_updates/`) live under `/persistent`.
- **These are external STM32 MCUs over UART, not the SoC's Cortex-M4.** The
  `/boot/cm_*.bin` and `imx_rpmsg_*` modules are NXP RPMsg demo firmware for
  the i.MX8MM's internal M4 and are unrelated to frank's subsystems.

---

## 1. UART topology

frank opens exactly two UARTs (`utils.cpp`, string offsets 327–328):

| Device        | i.MX8MM node (DTB alias)       | Base addr    |
|---------------|--------------------------------|--------------|
| `/dev/ttymxc0`| `serial0` → `serial@30860000` (UART1) | 0x30860000 |
| `/dev/ttymxc2`| `serial2` → `serial@30880000` (UART3) | 0x30880000 |

DTB `aliases` (from `boot/imx8mm-var-som-symphony-eight.dtb`, board
*"VAR-SOM-MX8M-MINI on EightSleep New-Rat 0.8"*):
`serial0=30860000, serial1=30890000, serial2=30880000, serial3=30a60000`;
one UART has `uart-has-rtscts`. udev puts all `tty*` in group `dialout`
(`lib/udev/rules.d/50-udev-default.rules`); there is **no** frank-specific
udev rule — frank opens the device nodes by fixed path.

frank drives **two logical subsystems**, each a `Subsystem` state machine:

- **`Sensor`** MCU (`subsystem/Sensor.cpp`) — bed thermistors, piezo,
  capacitive presence, vibration motors, ADC sampling, and the **heat
  current/thermistor sensing + heat safety** ("Sensor MCU Heat Fault …").
- **`Frozen`** MCU (`subsystem/frozen.cpp`) — the hub's hydraulics/thermal:
  **pumps (left/right), solenoid valve, priming, water level, and 4
  temperatures** (TEC/water loop).

frank opens `ttymxc0` first (uart index 0) then `ttymxc2` (index 1). The
strings alone do **not** prove which physical port is Sensor vs Frozen
(objdump in this toolchain can't disassemble aarch64 to trace the xref); this
should be confirmed by probing. The two are structurally distinct so a
replacement can identify them by handshake (FW-version tag response).

### UART / LSP transport
- Baud is a device setting, `subsystem_start_baudrate` (offset 651), applied
  per port: `[uart%u]baudrate set to %u` (654). Open/attr errors:
  `[uart%u]failed to open uart: %s`, `…failed to set attributes: %s`.
  Raw read/write: `[uart%u]read: %i errno:%s`, `[uart%u]write: %d`.
- Framing layer = **"LSP"**: `subsystem_lsp_send` (658) /
  `subsystem_lsp_try_receive` (656); receive timeouts logged as
  `[sensor] lsp receive timeout` / `[frozen] lsp receive timeout`.
- Messages are **single-byte opcode commands** with a per-subsystem state
  machine that rejects out-of-state commands and retries:
  - `[sensor] wrong state %d dropped cmd 0x%x` / `[frozen] wrong state …`
  - `[sensor] retrying command 0x%02x`
  - `[sensor] gain response %d %d` (command→response pairs, i.e. acked).
- Subsystem state transitions logged: `[sensor] state: %s -> %s`,
  `[frozen] state: %s -> %s`; states include `unknown`, `bootloader`,
  `firmware`, `update`. Recovery: `[sensor] rebooting`,
  `[sensor] attempting to recover, %d reboot(s)`,
  `[sensor] sensor disconnected`.
- **The on-wire byte framing (magic/length/CRC) is not present as strings**
  (it is compiled binary logic in `RawProtocol.cpp` / LSP). A replacement
  will need a logic-analyzer/UART capture to lock down exact framing bytes.
  What is certain: framed, stateful, opcode-based, with ack/response and
  receive timeouts, plus a periodic heartbeat to Frozen (see §3).

---

## 2. Heating / cooling control (`heating/Thermostat.cpp`, `TemperatureScheduler.cpp`)

Two zones, **Left / Right**. Core telemetry format string (offset 621,
`handleCommand`):

```
[Heat] L%u setp%d %u%% R%u setp%d %u%% S%u %umA CMP%d th%d setp%d mcu%d
```
(`[HeatE]` = error variant, offset 628). Field reading:
- `L%u … R%u` — per-side on/off (or level) state.
- `setp%d` — per-side setpoint (signed **raw** integer level).
- `%u%%` — per-side commanded **power level in percent**.
- `S%u` — sensor/subsystem state.
- `%umA` — measured **heat current in milliamps** (sensed by the Sensor MCU).
- `CMP%d` — **compressor/TEC trigger** state (see `CompTrig` below).
- `th%d` — thermistor reading; `setp%d` — active setpoint; `mcu%d` — MCU temp.

Setpoint representation: a **discrete level** that maps to °C via lookup
tables — RTTI names `17TemperatureLevels`, `16FrozenTempLevels`,
`16SensorTempLevels` (offsets 411–414). Both forms are logged:
`set target temp (%s: %d, %.2fC)` (461) — integer level **and** the resolved
°C float. Thermostat push: `thermostat state pushed L %u %.2fC %u%% R %u
%.2fC %u%%` (577, `pushHeatingState`).

Control entry points: `setHeatingState`, `setHeatingLevel`,
`checkHeatingPowerLevel`, `logHeatingExpiration`. Cloud/DAC function:
`sparkSetHeatingLeft` (and a Right counterpart).

### Persisted heat state — `heat/state1.dat` (offset 469)
Loaded/saved via `loadState`/`saveState` ("state loaded from file",
"failed to write state"). Fields (offsets 463–468):
`heatLevelL, tgHeatLevelL, heatTimeL, heatLevelR, tgHeatLevelR, heatTimeR`
— current level, **target** level, and remaining time per side.

### Scheduler (`TemperatureScheduler.cpp`)
- `started scheduled thermostat for %dsec`, `stopped scheduled thermostat`,
  `schedule accepted, new state is valid`, `updateSchedule`.
- Schedule string parse (`parseStateString`) format
  `"%c%s%02d%02d%05d"` = side char + day mask `[smtwtfs]` + HH + MM +
  5-digit duration(sec); pretty-printed `schedule set %s at %02u:%02u for %us`.
- Auto-off: `%s side temperature turning off due to timeout (%s)`.

### Safety-critical faults (a replacement MUST honor these)
From `Thermostat.cpp` / `Sensor.cpp` (`heatInfoSafetyCheck`,
`bedtempSafetyCheck`):
- `Thermostat Fault %lu not allowed to turn on` (574) — gated startup.
- `Heat Fault %lu overcurrent %lu mA` (614) — **over-current cutoff**.
- `Heat Fault %lu off but drawing %lu mA` (616) — **stuck/leaky driver
  detection** (element drawing current while commanded off).
- `Sensor MCU Heat Fault %lu overtemp %lu C` (617) — **over-temperature**.
- `RAT bad thermistor, CompTrig set to min %.02f` (455) and
  `RAT CompTrig reduced to %.02f` (457) — on bad thermistor the compressor/TEC
  trigger threshold is clamped to a safe minimum ("RAT" = the New-Rat board).
- Sensor power gating: `SensorPower Full` / `SensorPower Limited` /
  `SensorPower Off %lu faults` (571/575/576).
Fault codes are bitmask `%lu` counters; any set fault forces the affected side
off. These are latched in the log via `Heat Fault`/`Thermostat Fault`.

---

## 3. Pump / water control (`frozen.cpp`)

Frozen MCU drives the hydraulic + thermal loop. Command/target strings:
- `pump[left]`, `pump[right]` (677/678) — per-side pump commands.
- `solenoid` (676) — valve.
- `priming` (697), `waterLevel` (698) — prime cycle + level sensing.
- `keepAlive` (696) + `frozen.heartbeat` (700); missing beat →
  `[frozen] failed to update heartbeat` (695). **Frank must send a periodic
  heartbeat or Frozen faults/shuts down** — a from-scratch controller has to
  reproduce this cadence.
- Setpoint push to MCU: `[frozen] -> FW: temps %.2f %.2f %.2f %.2f` (693)
  — four float temperatures (left/right × loop, or TEC hot/cold + water) —
  and `[frozen] -> FW: water %s` (694).
- Generic command channel: `[frozen] -> %s` (692),
  `[frozen] wrong state %d dropped cmd 0x%x` (674).

---

## 4. Sensors (`Sensor.cpp`, `sensor_timing.cpp`, `raw_*`)

Sensor MCU streams a synchronized multi-channel sample stream. Channels
(raw_logger `encodeChunkBody`, CBOR keys, offsets 533–542): `bedTemp`,
`piezo-dual`, `right1`, `right2`, `piezo-sub`, `samples`, `frzTemp`,
`capSense`, plus ambient. Interpretation:
- **Bed temperature** thermistors (per side). Log:
  `[Bedtemp] L[%d] %d %d %d R[%d] %d %d %d AMB %d %u%% MCU %d` (578) —
  each side: state + 3 values, plus **AMB**ient, a percent, and MCU temp.
- **Piezo** pressure sensors: `piezo-dual` and `piezo-sub` — ballistocardio /
  breathing / motion (HRV) signal.
- **capSense** — capacitive **bed-presence** sensing.
- **frzTemp** — Frozen loop temps mirrored into the sample stream.

### Sampling & timing (`sensor_timing.cpp`)
- `startSampling`, `setSamplingRate` ("[sensor] sampling rate update").
- Time-sync with drift correction: `updateRefMarker`, `getRefMillis`,
  `[sensor] drift %dms`, `RefTime now %u pred %u refM %u raw %u`,
  `msec rollover %u refM %u raw %u`, `[sensor] sample lost %u cur %u`.
- Throughput stats: `[sensor]samples collected: %u`,
  `[sensor]avg samples/sec: %f`; trace counters `SENSOR_UP`,
  `SENSOR_SAMPLES`, `SENSOR_RX_OVRFLW`, `SENSOR_SAMPLE_DROPS`,
  `SENSOR_SAMPLES_DROPPED`.

### ADC gain / auto-ranging (`raw_ext_adc_buffer.h`)
- Programmable gain: `setGain`, `getUpdatedGain`, `[sensor] set gain %d %d`,
  `[sensor] gain response %d %d`, `[ext adc buffer]gain change detected!`.
- Range states: `railed`, `locked`, `converged` — an auto-ranging loop that
  detects saturation and re-picks gain. Default gain persisted in
  `settings/settings.cbr` ("settings file empty, using default gain").

### Raw-data pipeline → cloud
`raw_ext_adc_buffer` → `raw_logger` (CBOR chunks: header + body,
`startNewLogFile`, `flushChunkToFile`) → `raw_sequencer`
(`%08lX.RAW` files, `SEQNO.RAW`, `_checkpoint`, monotonic sequence numbers) →
`raw_uploader` (`[Upload] Starting Batch`, batched multipart, retries) →
HTTPS host **`raw-api-upload.8slp.net`**. Files under `tracing/`;
uploaded files deleted (`[Upload] deleted -> %s`).

### Vibration alarm (also Sensor MCU)
- `triggerVibrationAlarm`, `[sensor] triggering vibration alarm side %s`.
- `setHighCurrentVibration` — "[sensor] enabling Pod 2.0 vibration
  (simultaneous motors)".
- Settings in **`alarm.cbr`** (`alarm_settings.h`, `SensorAlarm.h`): per side
  `[alarm] vib. left: time %u, power %u, pattern %s, dur %u` (309/311).
  DAC funcs `sparkAlarmL`/`sparkAlarmR`, `clear_alarm_settings`,
  `parseAlarmPattern`.

---

## 5. MCU firmware update (`SubsysFirmwareUpdate.cpp`)

Blobs in `/opt/eight/lib/subsystem_updates/` (also copied to `/persistent`):

| File                     | Size (bytes) | Body size (hdr says) | Target MCU |
|--------------------------|--------------|----------------------|------------|
| `firmware-frozen.bbin`   | 52,860       | 52,732 (0xCDFC)      | Frozen     |
| `firmware-sensor.bbin`   | 39,332       | 39,204 (0x9924)      | Sensor     |

### `.bbin` header format (128 bytes, then raw Cortex-M image)
Confirmed by hexdump; multi-byte scalar fields are **big-endian**:

```
0x00  4  magic            = 88 88 88 88
0x04  2  header length    = 00 80  (BE 0x0080 = 128)
0x06  2  header CRC16     = frozen 19 87 / sensor b5 fc
0x08  4  image CRC32(?)   = frozen 6d 1e 00 01 / sensor 63 ee 00 01
0x0c  4  body length (BE) = frozen 00 00 CD FC (52732) / sensor 00 00 99 24 (39204)  ✓
0x10  8  hash/GUID        = frozen 49e2 d04c 615d e51f / sensor 9cf2 f3d6 6257 4451
0x18  … TLV records (tag,len,value):
         target id byte (frozen=02, sensor=01) + version triple (printed %d.%d.%d)
         02 04 <crc32>   frozen 4e77 0fe2 / sensor 59fa 3615
         03 04 08 00 <sz>00  load descriptor, flash base 0x0800_xxxx
0x1c..0x7f  0xFF padding
0x80  →  raw firmware image; word0 = initial SP, word1 = reset vector:
         frozen  SP=0x20008000  reset=0x0800FF69   (STM32, 32 KB RAM)
         sensor  SP=0x20002000  reset=0x0800D845   (STM32,  8 KB RAM)
```
Both images are **STM32 Cortex-M** (flash base **0x08000000**; vectors point
into 0x0800xxxx). Frozen is the larger MCU (32 KB RAM), Sensor smaller (8 KB).

### Update flow
`getSensorBinfo` / `getFrozenBinfo` parse the header — errors
`Bad FW header`, `No FW version tag error`, `bad on-board … FW`. frank reads
the running MCU FW version (`current sensor FW version: %d.%d.%d`), compares to
the `.bbin`, and if different: `Update to FW: %d.%d.%d` → drives the MCU into
its **bootloader** state, streams the image (LSP), then `Jump to FW`
(app entry). If versions match: `no firmware update, starting fw anyway`.
Stop/verify handshake: `updateStop`, `updateHandleStopResponse`,
`update GOOD` / `update BAD %u`. `read_file_to_heap` loads the blob
(`[sys]read %lu bytes from %s to 0x%016x`), freed after
(`[sensor]freeing old sensor fw`).

**To preserve/replace:** keep the 128-byte header contract (magic, BE body
length, version triple, CRCs) and the bootloader LSP command sequence; the MCU
bootloader lives in low flash and jumps to the app at the reset vector.

---

## 6. Interfaces exposed upward — DAC socket & device API

- **Unix socket** `/deviceinfo/dac.sock` (env `DAC_SOCKET`), served by
  `Eight.Capybara` (76 MB, the sandboxed cloud/device agent). frank connects
  as a client: `dac_loop start/got ctx/endpoint set/socket connected?`,
  reads `dac_loop command:` + ` payload:` (ASIO `local::stream_protocol`).
  Reads `/deviceinfo/device-id`.
- **`device_api_client.cpp`** — `DeviceApiClient` (`raw::ApiClient`), methods
  `receive` (with exception guards) and `dac setVariable var:` /
  `updateSparkVariable` — Particle-"spark"-style named variables & functions
  bridged cloud↔device.
- **`RawProtocol.cpp`** — the DAC message envelope: CBOR/protobuf with keys
  **`proto`**, **`session`**, **`footer`**; `validateResponse`,
  `response proto key not found`, `response protocol not supported (%s)`.
- **Registered "spark" functions/commands** (invoked from cloud via the DAC
  socket): `helloFrank`, `sparkSetHeatingLeft` (+Right), `sparkfuncSetSettings`
  / `SetSettings` (CBOR map → `settings/settings.cbr`), `sparkAlarmL/R`,
  `clear_alarm_settings`, `logToRaw`, `sparkReset`,
  `sparkFormatFS` ("Remote format filesystem called, system reset now!"),
  `sparkParseBedside` (`single`/`double`, `left`/`right`).
- **`DeviceTest.cpp`** self-test: `[test] Found %d/%d devices`, `[test] I2C: %s`,
  `[test] Logging %d samples/s` — an I2C bus scan + sample-rate check
  (the "self-test breaks uart comms" the service comment warns about).

---

## 7. What a from-scratch FOSS replacement must implement

1. **Two UART links** (`/dev/ttymxc0`, `/dev/ttymxc2`; UART1/UART3) at the
   configured `subsystem_start_baudrate`, with the **LSP** framing: stateful,
   single-byte-opcode, ack/response, receive-timeout + retry, per-subsystem
   state machine (unknown→bootloader→firmware→update). Exact frame
   magic/CRC needs a live UART capture — not recoverable from the stripped
   binary.
2. **Sensor MCU protocol**: start/stop sampling, set sampling rate, set ADC
   gain (+ auto-range on railed/converged), receive time-stamped multi-channel
   samples (bedTemp, piezo-dual/sub, capSense, ambient, frzTemp), read heat
   current(mA)/thermistors, trigger vibration alarm (incl. high-current dual).
3. **Frozen MCU protocol**: per-side pump commands, solenoid, prime, water
   level, push 4 target temps + water command, and a **periodic heartbeat**
   (`keepAlive`) or it faults.
4. **Thermostat control loop**: level↔°C tables (17 levels), per-side
   setpoint/power%, expiration timers, `heat/state1.dat` persistence, schedule
   parser.
5. **Safety faults (non-negotiable)** — must latch and force-off on:
   over-current (`Heat Fault … overcurrent mA`), off-but-drawing-current,
   over-temperature (`Sensor MCU Heat Fault … overtemp C`), bad-thermistor →
   clamp CompTrig, `Thermostat Fault … not allowed to turn on`, and
   SensorPower Limited/Off gating. Honor the auto-off timeout.
6. **MCU firmware update** of both STM32s via the bootloader LSP flow, keeping
   the `.bbin` header contract (§5).
7. **Watchdog**: sd_notify `READY=1` + periodic `WATCHDOG=1` within
   `WATCHDOG_USEC`, else systemd restarts (RestartSec=13).
8. **Upward interface** (optional for FOSS): the `/deviceinfo/dac.sock` server
   side + RawProtocol envelope, or replace it entirely with your own control
   surface. Raw data upload targets `raw-api-upload.8slp.net` (drop for FOSS).

### Key file map
- Binary: `opt/eight/bin/frankenfirmware`
- MCU blobs: `opt/eight/lib/subsystem_updates/firmware-{frozen,sensor}.bbin`
- Launcher: `opt/eight/bin/frank.sh`; unit `lib/systemd/system/frank.service`
- DTB: `boot/imx8mm-var-som-symphony-eight.dtb`
- Persistent state: `/persistent/{settings/settings.cbr, heat/state1.dat, alarm.cbr, tracing/*.RAW}`
- Strings dump: `scratchpad/franks.txt`
