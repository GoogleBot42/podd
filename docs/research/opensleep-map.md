# opensleep → `podd` Source Map & Integration Plan

Analysis of `LiamSnow/opensleep` @ HEAD (v2.0.0, Rust edition 2024, GPL-3.0).
Cloned to `scratchpad/os-analysis`. This maps every module, nails the serial/LSP
protocol layer we're reusing, answers the six diligence questions, and proposes a
cargo workspace for the fork.

---

## 1. High-level runtime model

`opensleep` is a **single binary** (`src/main.rs`, `#[tokio::main]`). It is NOT a
workspace — one crate, modules under `src/`. Startup sequence:

1. `env_logger::init()`.
2. Read device label from `/home/dac/app/sewer/device-label` (best-effort).
3. `Config::load("config.ron")` → `watch::channel(config)` (the config is the
   single source of truth, broadcast to all tasks via a `tokio::sync::watch`).
4. `ResetController::new()` (opens `/dev/i2c-1`) → `reset_subsystems()` toggles the
   PCAL6416A GPIO expander to hard-reset + enable both STM32s. The I2C device is
   then **moved into** `IS31FL3194Controller` (LED shares the same I2C bus).
5. `mpsc::channel` for calibration requests (MQTT → sensor task).
6. `MqttManager::new(...)` + `wait_for_conn()` (fatal if broker unreachable).
7. `tokio::select!` over **three long-lived futures** — whichever returns first
   brings the whole process down (systemd restarts it):
   - `frozen::run(PORT="/dev/ttymxc2", config_rx, led, mqtt_client)`
   - `sensor::run(PORT="/dev/ttymxc0", config_tx, config_rx, calibrate_rx, mqtt_client)`
   - `mqtt_man.run()`

There is **no supervisor/restart logic inside the process** — a subsystem error
kills the binary and relies on `Restart=always` in the unit file. Concurrency is
cooperative single-runtime tokio; the MQTT manager spawns short-lived tasks for
publishing (because you must be polling the eventloop to publish).

Wiring diagram (channels):

```
config.ron ──load──► watch::Sender<Config> ─┬─► frozen::run   (rx: away/prime/profile)
                                             ├─► sensor::run   (rx + tx: calibration writes back)
                                             └─► mqtt          (rx + tx: set_* actions write back)
MQTT set_* ──► config_tx.send() ──► all tasks + Config::save("config.ron")
MQTT calibrate ──mpsc──► sensor presence manager
subsystem packets ──► *State ──► mqtt_client.publish (fan-out to broker)
```

---

## 2. Module-by-module map

### `src/main.rs`
Entry point + top-level `select!`. Consts `VERSION="2.0.0"`, `NAME="opensleep"`.

### `src/reset.rs` — `ResetController`
- Drives **PCAL6416A** 16-bit I2C GPIO expander at `/dev/i2c-1` addr `0x20`.
- `reset_subsystems()`: configures port dir regs, asserts reset (`0xFF`), 100ms,
  de-asserts to enabled (`0xFD`). This is what powers on both STM32s.
- `take() -> I2cdev` hands the bus to the LED controller.
- Uses `linux-embedded-hal` `I2cdev` + `embedded-hal` `I2c` trait.

### `src/common/` — **the LSP serial protocol (crown jewel, see §3)**
`checksum.rs`, `codec.rs`, `packet.rs`, `serial.rs`. Subsystem-agnostic framing,
CRC, tokio-serial port creation, and shared packet helpers (pong/message/
hardware-info/jump-to-firmware parsers + `HardwareInfo` CBOR struct + `BedSide`).

### `src/frozen/` — Frozen subsystem (water temp / TEC / pumps / priming)
- `mod.rs`: re-exports `FrozenCommand`, `FrozenPacket`, `run`, `PORT`.
- `manager.rs`: `run()` main loop. `PORT="/dev/ttymxc2"`, `BAUD=38400` (fixed —
  Frozen has no separate firmware baud). Opens framed port, splits reader/writer.
  A **20 ms interval** drives `get_next_command()`; a `CommandTimers` struct
  rate-limits each command type (hwinfo 1s, temp set every 10s, prime within ±30s
  of `prime` time). Before sending, if the device isn't in Firmware mode it
  pings + `JumpToFirmware`, retrying up to `MAX_WAKE_ATTEMPTS=5` (Frozen sleeps
  when idle). Drives LED idle/active based on `state.is_active()`.
- `command.rs`: `FrozenCommand` enum + `to_bytes()`. Opcodes: `Ping=0x01`,
  `GetHardwareInfo=0x02`, `GetFirmware=0x04`, `JumpToFirmware=0x10`,
  `GetTemperatures=0x41`, `Prime=0x52`, `SetTargetTemperature=0x40 side en tHi tLo`.
  Rich reverse-engineering notes in comments (0x50/0x51 responses).
- `packet.rs`: `FrozenPacket` enum + `Packet::parse`. Response opcode = request+0x80.
  Handles `Message(0x07)`, `TemperatureUpdate(0x41)`, `GetTemperature(0xC1)`,
  `Heartbeat(0x53)`, `Pong(0x81)`, `HardwareInfo(0x82)`, `JumpingToFirmware(0x90)`,
  `TargetUpdate(0xC0)`, `PrimingStarted(0xD2)`. Temps are **centidegrees C** (u16).
- `state.rs`: `FrozenState` (device_mode, temp, l/r target, hwinfo, is_priming).
  `handle_packet()` mutates state + publishes to `opensleep/state/frozen/*`.
  Parses priming/water-tank text messages (`"FW: [priming] ..."`).
- `profile.rs`: **the thermostat math.** `FrozenTarget::calc_wanted()` →
  `SideConfig::calc_target(now)` → `calc_progress` (0-1 through sleep→wake window)
  + `lerp()` over the `temperatures` vec, converting °C→centi°C. `forward_duration`
  handles the wrap across midnight. Away mode → disabled target.

### `src/sensor/` — Sensor subsystem (cap/piezo/bed-temp/vibration)
- `mod.rs`: re-exports `SensorCommand`, `SensorPacket`, `run`, `PORT`.
- `manager.rs`: `run()` with a **discovery handshake** — try bootloader baud
  (`38400`), if it pongs, send `JumpToFirmware`, wait for mode switch, then reopen
  at **firmware baud `115200`**; else try firmware directly (program was recently
  running). Then a **50 ms interval** ticks a `CommandScheduler` holding a
  `Vec<RegisteredCommand>` (name, interval, last_run, `can_run` fn pointer). Each
  registered command re-asserts a config on the MCU until the state confirms it:
  `ping` (4s), `probe_temperature` (4s), `hwinfo`, `enable_vibration`, `piezo_gain`
  (400), `piezo_freq` (1000), `enable_piezo`, and `left/right_alarm` (5s). Alarm
  timing: fires between `wake - alarm.offset` and `+ alarm.duration`. 5s RX
  watchdog → `SensorError::Timeout`.
- `command.rs`: `SensorCommand` + `to_bytes()`. Opcodes: `Ping=0x01`,
  `GetHardwareInfo=0x02`, `GetFirmwareHash=0x04`, `JumpToFirmware=0x10`,
  `GetPiezoFreq=0x20`, `SetPiezoFreq=0x21`, `EnablePiezo=0x28`, `DisablePiezo=0x29`,
  `GetHeaterOffset=0x2A`, `SetAlarm=0x2C side intensity pattern dur32`,
  `ClearAlarm=0x2D`, `EnableVibration=0x2E`, `ProbeTemperature=0x2F 0xFF`,
  `SetPiezoGain=0x2B g1 g2`. Plus `AlarmPattern` (Single/Double/…) & `AlarmCommand`.
- `packet.rs`: `SensorPacket` + parse. `Message(0x07)`, `Init(0x31)`, `Piezo(0x32)`,
  `Capacitance(0x33)`, `Pong(0x81)`, `HardwareInfo(0x82)`, `GetFirmware(0x84)`,
  `JumpingToFirmware(0x90)`, `PiezoFreqSet(0xA1)`, `PiezoEnabled(0xA8)`,
  `PiezoGainSet(0xAB)`, `AlarmSet(0xAC)`, `VibrationEnabled(0xAE)`,
  `Temperature(0xAF)`. `CapacitanceData{sequence,[u16;6]}`,
  `TemperatureData{[u16;8] bed, ambient, humidity, mcu}`,
  `PiezoData{freq, sequence, gain, left_samples, right_samples}` (raw ADC vecs).
- `presence.rs`: `PresenseManager`. Consumes `CapacitanceData`; threshold+debounce
  vs per-sensor baselines → left/right/any presence → MQTT. `start_calibration()`
  averages 10s of samples into new `PresenceConfig.baselines`, then writes it back
  through `config_tx` (persists to `config.ron`).
- `state.rs`: `SensorState` + `handle_packet()`; publishes bed/ambient/humidity/mcu
  temps and piezo_ok/vibration_enabled; parses `"FW: alarm..."` text to track alarm
  running state.

### `src/led/` — IS31FL3194 RGB LED controller
- `controller.rs`: `IS31FL3194Controller<T: I2c>` at addr `0x53`. Register-level
  driver: current bands, current-level vs pattern mode, per-pattern timing/colors/
  repeat. Note "eight sleep messed up PCB so its BRG" ordering.
- `model.rs`: `IS31FL3194Config`, `OperatingMode`, `PatternConfig`, `Timing`,
  `CurrentBand`, `Gamma`, `Repeat`, etc.
- `patterns.rs`: `LedPattern` enum (serde) → high-level named effects (SlowBreath,
  Rainbow, Fixed, …) mapping to register configs. This is what the `.ron` exposes.

### `src/config/` — configuration model
- `mod.rs`: all config structs (see §4.3), RON load/save with `IMPLICIT_SOME`
  extension, custom `Time`/`TimeZone` serde (jiff).
- `mqtt.rs`: publishes config to `opensleep/state/config/**` and parses `set_*`
  actions (string mini-DSL like `left.sleep=20:30`), writes back + saves file.
- `tests.rs`: round-trip tests.

### `src/mqtt.rs` — `MqttManager`
- `rumqttc` async client + eventloop, LWT `opensleep/availability=offline`,
  exponential backoff reconnect, subscribes to `actions/*`, dispatches to config
  handler / calibration mpsc. Helpers `publish_guaranteed_wait` (QoS2, awaited) and
  `publish_high_freq` (QoS0 try_publish). This is **the only external API surface.**

---

## 3. The LSP serial protocol layer (`src/common/`) — reuse verbatim

Both subsystems speak the same **framed byte protocol** over UART. This is the part
worth extracting into a standalone `pod-proto` crate.

### Framing (`codec.rs`)
```
┌──────┬──────┬───────────────┬──────────────┐
│ 0x7E │ LEN  │ PAYLOAD[LEN]  │ CRC16 (BE)   │
│ START│ u8   │               │ over PAYLOAD │
└──────┴──────┴───────────────┴──────────────┘
```
- `START = 0x7E`. `LEN` is a single byte (payload length, max 255).
- Checksum is **big-endian**, computed over the payload only (not START/LEN).
- Decoder is a `tokio_util::codec::Decoder`. It `memchr`s the START byte, needs
  `1+1+len+2` bytes, validates the CRC **without consuming** (so a bad frame only
  skips one byte and resynchronises), then hands the payload to `P::parse`.
- Encoder side: free fn `command(payload) -> Vec<u8>` prepends START+LEN, appends
  CRC. `CommandTrait::to_bytes()` is implemented per subsystem command enum.

```rust
pub fn command(mut payload: Vec<u8>) -> Vec<u8> {
    let mut res = Vec::with_capacity(payload.len() + 4);
    let checksum = checksum::compute(&payload);
    res.push(START);
    res.push(payload.len() as u8);
    res.append(&mut payload);
    res.push((checksum >> 8) as u8);  // CRC high byte
    res.push(checksum as u8);         // CRC low byte
    res
}
```

### Checksum (`checksum.rs`) — CRC-CCITT variant
- `const CRC_START = 0x1D0F`, `CRC_POLY_CCITT = 0x1021`, table precomputed at
  compile time (`const fn make_crc_table`). This is CRC-CCITT (0x1D0F seed,
  "0xFFFF-ish" variant). Verified against golden vectors in tests, e.g.
  `compute(40 0001 0E10) == 0xE6A8`.

### Codec is generic over the packet type
```rust
pub struct PacketCodec<P: Packet> { _phantom: PhantomData<P> }
impl<P: Packet> Decoder for PacketCodec<P> { type Item = P; ... }
impl<P: Packet, C: CommandTrait> Encoder<C> for PacketCodec<P> { ... }
```
`Packet` trait: `fn parse(buf: BytesMut) -> Result<Self, PacketError>`.
So Frozen and Sensor share the *exact same* codec, differing only in their
`Packet::parse` opcode tables and their `CommandTrait` encoders.

### Addressing / how the two subsystems are distinguished
There is **no address field in the frame** — the two subsystems are on **separate
UART devices**, so addressing is by port:
- **Frozen**: `/dev/ttymxc2`, fixed **38400 8N1**.
- **Sensor**: `/dev/ttymxc0`, **38400** in bootloader mode, **115200** in firmware mode.

(Note: BACKGROUND.md text swaps these device paths vs the code; trust the code —
`frozen::PORT = /dev/ttymxc2`, `sensor::PORT = /dev/ttymxc0`.)

### Port creation (`serial.rs`)
```rust
tokio_serial::new(port_path, baud_rate)
    .data_bits(DataBits::Eight).flow_control(FlowControl::None)
    .parity(Parity::None).stop_bits(StopBits::One)
    .timeout(Duration::from_millis(1000))
    .open_native_async()?          // -> SerialStream
```
`create_framed_port::<P>()` wraps that in `Framed<SerialStream, PacketCodec<P>>`,
then `.split()` into a `SplitSink` writer + `SplitStream` reader.

### Shared packet primitives (`packet.rs`)
- `BedSide { Left=0, Right=1 }` (`FromRepr`).
- `parse_pong` → bool `in_firmware` (`0x46`=firmware, `0x42`=bootloader).
- `parse_message` → UTF-8 string (firmware log lines, `"FW: ..."`).
- `parse_jumping_to_firmware`, `parse_hardware_info` (**CBOR via `cbor4ii`** — a
  `(u8 status, HardwareInfo)` tuple). `HardwareInfo{serial_number, part_number,
  sku, hwrev, factoryline, datecode}`.
- Device state machine: `DeviceMode { Unknown, Bootloader, Firmware }`.

### Mode/bootloader handshake (important for the flashing gap)
`JumpToFirmware (0x10)` tells the STM32 bootloader to jump to the **already-present**
application firmware. `GetFirmware`/`GetFirmwareHash (0x04)` query it. There is **no
erase/write/flash** command anywhere — see §4.1.

---

## 4. Diligence answers (read from code, not guessed)

### 4.1 Does it flash/update STM32 firmware? — **NO.**
opensleep **assumes Eight's MCU firmware is already flashed** and only talks to it.
Evidence:
- The only bootloader interaction is `JumpToFirmware (0x10)` and `GetFirmware(Hash)
  (0x04)`. No opcodes for flash-erase, flash-write, set-address, or memory readout.
- No `.bbin` parsing anywhere (`grep bbin` → only doc/comment references to Eight's
  `firmware-sensor.bbin` / `firmware-frozen.bbin` paths in BACKGROUND.md).
- `sensor::run_discovery` uses the bootloader **only to bounce into firmware**, then
  reopens at 115200. It never stays in the bootloader to program it.
- `DeviceMode::Bootloader` exists but is a transient state, not a programming mode.

**⇒ `mcu_flash` (.bbin parse + STM32 bootloader program protocol) is entirely new
work.** You'll need to reverse the STM32 bootloader command set (erase/write/verify)
— none of it is in opensleep. The `.bbin` format is Eight's; opensleep gives you
zero help there beyond the file paths.

### 4.2 Thermostat/scheduler? — **PARTIAL: a time-of-day interpolating profile, no calendar.**
- **Yes**, there is a real thermostat: `frozen/profile.rs` interpolates a per-side
  temperature curve across a daily `sleep→wake` window (`calc_progress` + `lerp`),
  evaluated live every 10s and pushed as `SetTargetTemperature`. Away mode disables.
- **Yes**, alarms: `sensor/manager.rs` schedules vibration relative to `wake` time
  (`offset`, `duration`, `intensity`, `pattern`).
- **But** it's a *single daily profile* keyed only on wall-clock time-of-day. There
  is **no** notion of: immediate/manual setpoint override, per-day-of-week
  schedules, one-off events, a queue of future setpoints, or a REST-driven
  scheduler. The "schedule" is entirely the two `Time`s (`sleep`,`wake`) + a temp
  vector, re-derived from the clock each tick. There is no manual "set temp to X
  now" path at all — everything flows from the profile.

**⇒ A richer `schedule`/`thermostat` (manual override, weekday schedules, multiple
setpoints, calendar) is largely new, but the interpolation core in `profile.rs` is
reusable and worth keeping.**

### 4.3 Config schema (`config/mod.rs` + `.ron`)
Top-level `Config`:
```rust
struct Config {
    timezone: TimeZone,            // IANA string, jiff, custom serde
    away_mode: bool,
    prime: Time,                   // "HH:MM"
    led: LEDConfig { idle: LedPattern, active: LedPattern, band: CurrentBand },
    mqtt: MqttConfig { server: String, port: u16, user, password },
    profile: SidesConfig,          // Solo(SideConfig) | Couples{left,right}
    presence: Option<PresenceConfig>,   // written by calibration
}
struct SideConfig {
    temperatures: Vec<f32>,        // °C control points, evenly spread sleep→wake
    sleep: Time, wake: Time,       // "HH:MM"
    alarm: Option<AlarmConfig { pattern, intensity:u8, duration:u32, offset:u32 }>,
}
struct PresenceConfig { baselines:[u16;6], threshold:u16, debounce_count:u8 }
```
Format is **RON** with `IMPLICIT_SOME` (so `alarm: (...)` not `alarm: Some((...))`).
Loaded from `./config.ron` (cwd = `/opt/opensleep`), saved back on every `set_*`.

**Crucial gap for `podd`: hardware/device config is HARD-CODED, not in the schema.**
UART device paths (`/dev/ttymxc0`,`/dev/ttymxc2`), bauds (38400/115200), I2C bus
(`/dev/i2c-1`), expander addr `0x20`, LED addr `0x53`, piezo gain 400, piezo freq
1000 — all `const`s in code. There is **no** per-side thermistor calibration, no
device-path config, no I2C bus config. If `podd` needs to support Pod 4/5 or
configurable hardware, you must add a `hardware`/`device` config section.

### 4.4 Sensor pipeline — what's there vs missing
- **Bed temperature**: fully parsed (`0xAF` → `[u16;8]` bed + ambient + humidity +
  mcu, centi°C), published to MQTT. Not yet fed back into Frozen control (README
  roadmap item).
- **Capacitance (presence)**: fully implemented — 6 sensors @ ~2Hz, threshold +
  debounce vs calibrated baselines → left/right/any presence. Calibration flow
  works and persists.
- **Piezo**: raw ADC samples (`0x32`) are *parsed* into `left_samples`/
  `right_samples` `Vec<u16>` at freq 1000, gain 400 — **but they go nowhere.** They
  are not stored, not analysed, not published.
- **Heart rate / HRV / breathing**: **NOT implemented** (explicit roadmap TODO).
  `rustfft` and `cbor4ii` are in `Cargo.toml`, but **`rustfft` is never used**
  (grep confirms zero references). `cbor4ii` *is* used — only for `HardwareInfo`
  CBOR decode, **not** for sensor DSP. Likewise `csv` and `serde_json` deps are
  **declared but unused** in code. So the FFT/vitals pipeline is greenfield; the
  deps are aspirational placeholders.

### 4.5 HTTP / REST / WebSocket? — **NONE. MQTT only.**
The sole remote interface is `rumqttc` (MQTT) in `src/mqtt.rs`. No axum/warp/hyper/
tower, no TCP listener, no static file serving, no WebSocket. All state fan-out and
all commands go through MQTT topics (`opensleep/state/**`, `opensleep/actions/**`,
`opensleep/result/**`, `opensleep/config/**`; full map in MQTT.md).

**⇒ The entire web API + static UI is new work.** MQTT is a good template for the
*event/topic model* (state topics ↔ WS broadcast, action topics ↔ REST POST) but
shares no code with an HTTP server.

### 4.6 WiFi / onboarding / logging / systemd
- **WiFi/onboarding: none in code.** It's a manual/host-OS concern — SSH keys and
  WiFi creds are baked into `rootfs.tar.gz` at flash time (SETUP.md). Eight's
  Bluetooth onboarding (`Capybara`) is disabled. `podd`'s `onboarding` module is
  greenfield.
- **Logging**: `log` + `env_logger`, controlled by `RUST_LOG`. No file logging, no
  structured logs, no log endpoint.
- **systemd** (`opensleep.service`): `Type=simple`, `ExecStart=/opt/opensleep/
  opensleep`, `WorkingDirectory=/opt/opensleep` (so `config.ron`/`device-label`
  resolve relative), `Restart=always`, `RestartSec=5`, `After=NetworkManager`.
  Install steps disable Eight's services (`dac frank capybara swupdate …`).

---

## 5. Recommended cargo workspace for `podd`

Goal: keep opensleep's control core **recognisable** (so upstream fixes cherry-pick
cleanly) while adding new surfaces as separate crates that plug into the existing
tokio/`watch`/mpsc model.

```
podd/                              # workspace root (Cargo.toml [workspace])
├── Cargo.toml                     # [workspace] members + shared [workspace.dependencies]
├── .cargo/config.toml             # aarch64 linker (carry over verbatim)
│
├── crates/
│   ├── pod-proto/                 # ← EXTRACT from opensleep src/common/ (+ per-subsystem
│   │   │                            #   packet/command tables). Pure, no tokio-runtime deps
│   │   │                            #   beyond tokio-util codec + tokio-serial. Reusable, testable.
│   │   └── src/
│   │       ├── checksum.rs        # verbatim from common/checksum.rs
│   │       ├── codec.rs           # verbatim from common/codec.rs (PacketCodec, command())
│   │       ├── packet.rs          # common/packet.rs (Packet trait, HardwareInfo, BedSide)
│   │       ├── serial.rs          # common/serial.rs (create_framed_port, DeviceMode)
│   │       ├── frozen/            # frozen command.rs + packet.rs + profile.rs (pure protocol+math)
│   │       └── sensor/            # sensor command.rs + packet.rs (pure protocol)
│   │
│   ├── pod-hal/                   # reset.rs (PCAL6416A) + led/ (IS31FL3194). embedded-hal side.
│   │
│   ├── podd-core/                 # ← the opensleep control core, kept 1:1 with upstream layout
│   │   └── src/
│   │       ├── frozen/manager.rs  # frozen::run  (task)  -- upstream-tracking
│   │       ├── frozen/state.rs
│   │       ├── sensor/manager.rs  # sensor::run  (task)  -- upstream-tracking
│   │       ├── sensor/state.rs
│   │       ├── sensor/presence.rs
│   │       └── config/            # extended Config (see below)
│   │
│   ├── schedule/                  # NEW: thermostat/scheduler. Wraps profile.rs; adds manual
│   │                              #   override, weekday schedules, setpoint queue. Emits
│   │                              #   desired targets over a channel into frozen::run.
│   │
│   ├── api/                       # NEW: axum REST + WS + static UI (rust-embed the built SPA).
│   │                              #   Subscribes to the same watch/broadcast the MQTT layer uses.
│   │
│   ├── update/                    # NEW: signed/atomic self-update agent (manifest fetch, verify
│   │                              #   sig, download, atomic swap of the podd binary + rollback).
│   │
│   ├── mcu-flash/                 # NEW: .bbin parser + STM32 bootloader programming protocol
│   │                              #   (erase/write/verify). Uses pod-proto's port creation but a
│   │                              #   NEW command set. Highest-risk / most reverse-engineering.
│   │
│   └── onboarding/               # NEW: WiFi provisioning + first-run config.
│
└── podd/                          # the binary crate: main.rs wires all tasks in one select!/JoinSet
    └── src/main.rs
```

### What goes in `pod-proto` vs the binary
- **`pod-proto`** = anything pure and I/O-shaped-but-not-policy: framing, CRC,
  codec, `Packet`/`CommandTrait` impls, packet/command enums for both subsystems,
  and the profile interpolation math. No MQTT, no `watch`, no LEDs. This is what
  makes upstream protocol fixes trivial to track and lets `mcu-flash` reuse the
  port/codec.
- **`podd` binary** = the runtime wiring: the `select!`/`JoinSet`, the `watch<Config>`
  fan-out, channel plumbing between `schedule`→`frozen`, `api`↔state, etc.
- `state.rs`/`manager.rs` currently publish **directly to MQTT**. For `podd`, refactor
  the fan-out behind a small `StateBus` trait (or a `tokio::sync::broadcast` of state
  deltas) so both MQTT *and* the WS/`api` layer subscribe. Keep MQTT as one consumer.

### How each new module plugs into the existing runtime model
- **`api`** — spawn `axum::serve` as a 4th arm of the top-level `select!`/`JoinSet`.
  REST mutations call the *same* `config_tx.send()` + `Config::save()` path the MQTT
  `handle_action` uses (factor that into `podd-core::config::apply_action`). WS pushes
  the state-broadcast stream. Static UI via `rust-embed` + `tower-http::ServeDir`.
- **`schedule`/`thermostat`** — owns the desired-setpoint decision. Today
  `frozen::get_next_command` calls `FrozenTarget::calc_wanted` inline; instead have
  `schedule` compute the target (respecting manual overrides + weekday schedule) and
  publish it on a `watch<DesiredTargets>` that `frozen::run` reads. Minimal change to
  `frozen::run`, big capability gain.
- **`update`** — independent tokio task (or separate process invoked by systemd
  timer). Verifies a signed manifest, downloads, atomically renames the binary,
  triggers `systemctl restart podd`. No hook into subsystem tasks needed.
- **`mcu-flash`** — invoked out-of-band (CLI subcommand / REST endpoint that quiesces
  the subsystem tasks first). Reuses `pod-proto::serial` port creation + framing but
  a *new* bootloader command enum. Must pause `frozen::run`/`sensor::run` (drop the
  `Framed` port) before programming — model it as a mode the top-level supervisor can
  enter, not a concurrent task on the same UART.
- **`onboarding`** — first-run only; gates `main()` before the `select!` if config is
  absent (serve a captive setup page via `api`, write initial `config.ron`).

### Config schema extension (needed by the fork)
Add a `hardware`/`device` section to `Config` (currently all hard-coded consts):
```ron
device: (
    frozen_uart: "/dev/ttymxc2", frozen_baud: 38400,
    sensor_uart: "/dev/ttymxc0", sensor_bootloader_baud: 38400, sensor_firmware_baud: 115200,
    i2c_bus: "/dev/i2c-1", expander_addr: 0x20, led_addr: 0x53,
    piezo_gain: 400, piezo_freq: 1000,
    // NEW: per-side thermistor calibration offsets, pod model, etc.
),
```
Plus new sections for `api` (bind addr, auth), `update` (channel, pubkey), and a
richer `schedule`.

---

## 6. Rust edition, deps, cross-compilation

- **Edition 2024** (`Cargo.toml`), Rust ≥1.85. Uses let-chains (`if let … && …`) —
  keep the toolchain current.
- **Key deps**: `tokio` (full), `tokio-serial 5.4`, `tokio-util` (codec), `bytes`,
  `memchr`, `rumqttc 0.24` (MQTT), `ron 0.10`, `serde`, `jiff 0.2`
  (tzdb-bundle-always → timezone data compiled in, good for embedded), `cbor4ii`
  (used), `linux-embedded-hal 0.4` + `embedded-hal 1.0` (I2C), `thiserror`, `strum`,
  `log`+`env_logger`, `hex-literal`.
- **Declared-but-unused deps** (drop or wire up): `rustfft`, `csv`, `serde_json`,
  `async-trait`, `futures-util` (futures-util *is* used for Sink/Stream split;
  keep). Confirmed via grep: `rustfft`/`csv`/`serde_json` have **zero** source refs.
- **Cross-compilation to aarch64** (Variscite i.MX 8M Mini SOM):
  - `.cargo/config.toml` already sets `linker = "aarch64-linux-gnu-gcc"` for both
    `aarch64-unknown-linux-gnu` and `-musl`, and a size-optimised release profile
    (`strip`, `lto`, `codegen-units=1`). Carry this over.
  - `linux-embedded-hal`/`I2cdev` and `tokio-serial` both bind to **Linux
    `/dev/i2c-*` + termios/serial ioctls** — they compile fine for aarch64-gnu but
    require the target's libc; simplest path is `cross` or a gnu sysroot. musl works
    but watch for serial ioctl differences; the project offers both targets.
  - `jiff` with `tzdb-bundle-always` avoids needing `/usr/share/zoneinfo` on the
    minimal Yocto rootfs — keep this feature.
  - `rustfft` (when you actually add vitals DSP): pure Rust, SIMD via `std`; builds
    for aarch64 but benefits from `RUSTFLAGS="-C target-feature=+neon"` — verify NEON
    on the i.MX 8M Mini (Cortex-A53, has NEON). No C deps, so no extra cross toolchain.
  - No `openssl` today (rumqttc used without TLS feature). If `update`/`api` add TLS,
    prefer `rustls` to avoid cross-compiling OpenSSL for aarch64.
  - The GitHub CI only does a native `cargo build`/`test` on ubuntu — there is **no
    aarch64 build in CI**; add a `cross`-based target build for the fork.

---

## 7. The three big gaps `podd` must fill (summary)

| Capability            | In opensleep?        | Effort in fork |
|-----------------------|----------------------|----------------|
| MCU `.bbin` flashing  | ❌ none (jump-to-fw only) | High — new bootloader protocol + .bbin parser (`mcu-flash`) |
| Thermostat/scheduler  | ⚠️ daily time-of-day profile + wake-relative alarms only | Medium — reuse `profile.rs`, add override/weekday/queue (`schedule`) |
| Web REST + WS + UI    | ❌ MQTT only          | High — all-new `api` crate; MQTT is a model, not code to reuse |

Also greenfield: WiFi onboarding, signed/atomic self-update, piezo→vitals DSP
(HR/HRV/breathing; `rustfft` is a placeholder), configurable hardware/device paths,
and Sensor-temp→Frozen-control feedback.
