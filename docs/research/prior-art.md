# Prior Art: FOSS replacement of Eight Sleep Pod 3 firmware

Research date: 2026-07-17. Goal: assess existing community projects as prior art for a
full, blob-free FOSS replacement of the Pod 3 stack.

## TL;DR

- **opensleep** (github.com/LiamSnow/opensleep) is the single most relevant prior art. It is
  a **complete replacement** of Eight Sleep's stack (DAC + Frank/frankenfirmware + Capybara),
  written in Rust, talking **directly to the two STM32 microcontrollers over UART/USART** and to
  the LED / I2C mux directly. It is GPL-3.0 and ships the reverse-engineered protocol docs
  (`BACKGROUND.md`) that are the crown jewel here. This is the natural foundation to build on.
- **free-sleep** (github.com/throwaway31265/free-sleep) — what the user runs today — is a
  Node/TypeScript + React app **bolted on top of** stock firmware. It keeps Frank and talks to it
  through the `dac.sock` Unix socket (replacing only the `dac` cloud bridge). MIT licensed. Nice
  UX and Home Assistant story, but architecturally exactly the "bolted-on" design the user wants
  to leave.
- **ninesleep** (github.com/bobobo1618/ninesleep) is the original `dac.sock` reverse-engineering
  work that free-sleep is built on. A minimal `dac` replacement exposing a REST API. Foundational
  research; small.
- **8rp** (github.com/Schluggi/8rp), and the root/teardown blog posts by **blopker (ZeroSleep)**
  and **Adam Schaal** document rooting, the boot chain, and hardware.

## The stock stack (confirmed by multiple sources)

Hardware: **Variscite VAR-SOM-MX8M-MINI** SoM (NXP i.MX8M Mini), Yocto Linux (FSLC), systemd.
Two **STM32F030CCT6** microcontrollers hang off the SoM UARTs:

- **Frozen subsystem** — `/dev/ttymxc0`, 38400 baud. Controls 2x TECs (Peltier) for water temp
  with PID, 2x water pumps, tank solenoid, tank water-level sensor. Handles priming, water temp
  control, safety. Stock firmware blob: `/opt/eight/lib/subsystem_updates/firmware-frozen.bbin`.
- **Sensor subsystem** — `/dev/ttymxc2`. Bootloader mode 38400 baud, firmware mode 115200 baud.
  Manages 6x capacitance (presence) sensors @ 2Hz, 8x bed temperature sensors, ambient
  temp+humidity sensor, 2x piezo sensors (HR/breathing, sampled ~500x/s per Frank params), and
  vibration alarm motors. Stock blob: `/opt/eight/lib/subsystem_updates/firmware-sensor.bbin`.

I2C peripherals: **IS31FL3194** LED controller (breathing LED) over I2C; **PCAL6416A** I2C GPIO
expander at address `0x20` on `/dev/i2c-1` enables/resets the Frozen and Sensor boards.

Software components (codenames):
- **Frank / frankenfirmware** — C++ hardware controller at `/opt/eight/bin/frakenfirmware`
  (note the stock binary is misspelled "fraken"). Owns the UART links to both MCUs.
- **DAC (device-api-client) / "PizzaRat"** — Node/TypeScript app in `/home/dac/app/`. Cloud
  bridge (CoAP/CBOR to *.8slp.net), wraps Frank, and exposes the local `dac.sock` Unix socket.
- **Eight.Capybara** — compiled .NET app in `/opt/eight/bin`. BLE/WiFi onboarding, LED, and
  subsystem restart logic.
- Support services observed/masked when rooting: `defibrillator` (watchdog), `burrow`,
  `swupdate`/`swupdate-progress`, `eight-kernel`, `telegraf`, `vector`.
- Update endpoint: `https://update-api.8slp.net/v1/updates/p1/1`; images are `swupdate` `.swu`.

---

## 1. opensleep — LiamSnow/opensleep

- **URL:** https://github.com/LiamSnow/opensleep · writeup https://liamsnow.com/projects/opensleep/
- **What it is:** "open-source Rust firmware for the Eight Sleep Pod 3 that completely replaces
  Eight Sleep's proprietary software stack." Local-only, no cloud.
- **License:** **GPL-3.0.** ~97% Rust. Educational/research disclaimer.
- **Replaces vs keeps:** Replaces **all three** stock programs (DAC, Frank, Capybara). Keeps only
  the stock **MCU firmware blobs** (`firmware-frozen.bbin` / `firmware-sensor.bbin`) — it flashes
  and drives them, it does not reimplement the STM32 firmware. So the SoM-side stack is fully
  FOSS, but the two MCUs still run Eight Sleep's proprietary firmware. (Relevant for "no blobs":
  opensleep is blob-free on the Linux side; the MCU firmware remains a proprietary blob.)

### Hardware interface — this is the reusable gold

opensleep talks **directly to the MCUs over USART**, replacing Frank entirely. Documented in
`BACKGROUND.md`:

- UART device paths and baud rates as listed in the stock-stack section above.
- **Frame format:** `7E [Length] [Command] [Payload...] [CRC-CCITT/0x1D0F checksum, 2 bytes]`.
  Start byte `0x7E` may appear inside the payload — disambiguated by checksum validation with a
  fallback/retry decoder (a Tokio-based codec).
- **Sensor MCU has two modes:** Bootloader (38400 baud — hardware info, config, firmware flashing;
  command `0x10` transitions to Firmware mode) and Firmware (115200 baud — capacitance stream at
  2Hz, on-demand bed temperature probing, piezo data after gain/frequency configuration).
- Response packets = request command ID **+ 0x80**. Config commands use retry logic at ~800ms
  intervals. A 20Hz tick scheduler manages retries, command precedence, and intervals.
- I2C: PCAL6416A GPIO expander to enable/reset both MCUs and the front LED; IS31FL3194 LED driver
  with custom breathing/effect rendering.

### Source layout (build-on map)

- `common/` — shared serial/protocol handling for both subsystems (checksum, codec, shared packets)
- `sensor/` — Sensor subsystem comms · `frozen/` — Frozen subsystem comms (TEC/pump/priming)
- `reset.rs` — subsystem enable/reset via PCAL6416A · `led/` — IS31FL3194 controller/model
- `mqtt.rs` — MQTT client + event loop · `config/` — config model + MQTT publish · `main.rs`

### Features

MQTT interface (root topic `opensleep/`) for full monitoring + control. Presence detection
(capacitance, solo/couples), unlimited-waypoint temperature profiles, vibration alarms with
offsets, LED effects, daily priming, RON (`config.ron`) config. Home Assistant via MQTT
(`HASS.md`). MQTT spec in `MQTT.md` — e.g. `opensleep/state/sensor/bed_temp` (6-element array,
centidegrees C), `.../state/frozen/left_temp|right_temp|heatsink_temp`, `.../state/presence/*`,
`.../actions/{calibrate,set_away_mode,set_prime,set_profile,set_presence_config}`.

### Install & limitations

Requires SSH root on the SoM (hardware disassembly + SD/Yocto rootfs modification + SSH key inject,
then factory-reset to boot the modified system). Pod 3 fully supported; Pod 4/5 untested; Pod 1/2
impossible. No mobile-app compatibility (by design — the cloud layer is gone). MCU firmware mode
persists >10 min without reboot (a documented quirk).

---

## 2. free-sleep — throwaway31265/free-sleep  (what the user runs)

- **URL:** https://github.com/throwaway31265/free-sleep (install: INSTALLATION.md)
- **What it is:** local LAN control app for jailbroken Pods. Installs a lightweight server on the
  Pod's Linux and serves a React web UI.
- **License:** **MIT.**
- **Architecture:** Backend Node.js + Express + TypeScript (REST API); Frontend React + Material-UI
  + Zustand + React Query; storage LowDB (JSON) + SQLite for biometrics at
  `/persistent/free-sleep-data/free-sleep.db`.
- **Hardware interface:** does **NOT** replace Frank. It replaces only the `dac` cloud bridge and
  talks to the still-running Frank via the **`dac.sock`** Unix socket (credited to bobobo1618's
  ninesleep research). This is exactly the "bolted on top of stock firmware" design the user
  dislikes.
- **Features:** temperature control, schedules (power on/off, temp, priming, alarms), timezone,
  away mode, LED brightness, biometrics (HR validated, HRV, breathing). Survives internet loss
  ("your pod WILL NOT turn off"). Home Assistant community integrations exist.
- **Install:** jailbreak via SD-card swap or OTA hijack; **reversible** — a firmware reset returns
  the Pod to the stock Eight Sleep app. Pod 3/4/5 supported; Pod 1/2 not.

---

## 3. ninesleep — bobobo1618/ninesleep  (foundational dac.sock research)

- **URL:** https://github.com/bobobo1618/ninesleep
- **What it is:** a drop-in replacement for the stock `dac` process (`systemctl stop dac` first).
  Keeps Frank; exposes a local REST API on **port 8000** (JSON).
- **Documented API:** `POST /temperature/<left|right>` (units = **tenths of a degree C**, so 40 =
  4°C), `POST /temperature-duration/<left|right>` (seconds until shutoff), alarms (intensity %,
  duration s, Unix trigger time, pattern `"double"` strong / `"rise"` gentle).
- **License:** not stated in repo README (treat as all-rights-reserved until confirmed on GitHub).
- **Value:** first to document `dac.sock` and Frank's local control surface; free-sleep is built on
  it. But it still keeps Frank, so it is a partial (cloud-only) replacement.

---

## 4. Supporting reverse-engineering / root writeups

- **8rp — Eight Sleep Research Project** (github.com/Schluggi/8rp, **GPL-3.0**): documentation hub —
  "how to hack your Pod / get root", "how to communicate with the Pod", "hardware overview".
  Work-in-progress but the community's central RE doc. Not endorsed by Eight Sleep.
- **ZeroSleep — Bo Lopker** (https://blopker.com/writing/04-zerosleep-1/): root via SD-card rootfs
  read/modify (EXT4, `rootfs.tar.gz`), SSH key inject, factory-reset to load. Names the stack
  components, `swupdate`/`.swu`, and the `update-api.8slp.net` endpoint.
- **Adam Schaal — "Rooting My Eight Sleep Pod 3"** (https://blog.adamschaal.com/posts/2025-12-16-rooting-eight-sleep/):
  root via **JTAG / 14-pin Plug-of-Nails** ribbon, FT232RL FTDI at 921600 baud, interrupt U-Boot,
  `setenv bootargs 'root=PARTLABEL=rootfs_a rootwait init=/bin/bash'`, then `systemctl mask`
  swupdate, defibrillator, eight-kernel, telegraf, vector, frankenfirmware, dac. Good boot-chain +
  service-disable reference.
- Also seen: v3rm0n/freesleep (Pod **2** temp UI, unrelated arch) and lukas-clarke/eight_sleep
  (Home Assistant cloud-API integration, not on-device).

---

## What is already reverse-engineered (do NOT redo)

1. **Full MCU USART protocol** (frame format, CRC-CCITT 0x1D0F, baud rates, bootloader/firmware
   modes, cmd+0x80 responses, retry timing) — opensleep `BACKGROUND.md` + `common/` source.
2. **Sensor data formats** — capacitance @2Hz, 8 bed-temp sensors (centidegrees C arrays), ambient
   temp/humidity, piezo HR/breathing — opensleep sensor module + MQTT spec.
3. **Frozen/TEC/pump/priming/solenoid control** — opensleep frozen module.
4. **I2C map** — PCAL6416A @0x20 on /dev/i2c-1 (subsystem enable), IS31FL3194 LED — opensleep.
5. **dac.sock local control surface** (temperature tenths-°C, duration, alarms) — ninesleep +
   free-sleep.
6. **Root + boot chain** (SD-swap and JTAG/U-Boot paths, services to mask, swupdate/.swu, cloud
   endpoints) — blopker, Schaal, 8rp.

Still proprietary / not fully open: the **STM32 MCU firmware itself** (`.bbin` blobs) — every
project reuses Eight Sleep's compiled MCU firmware. A truly 100%-blob-free stack would require
reimplementing the two STM32F030 firmwares (TEC PID + safety, sensor DSP), which no project has
done. Also not fully documented: exact per-command byte tables (read them from opensleep source).

## Licensing & reuse assessment

| Project | License | Reusable for clean FOSS replacement? |
|---|---|---|
| opensleep | GPL-3.0 | **Yes — best base.** Copyleft is compatible with a FOSS goal. Already replaces the whole SoM stack and talks UART directly. Fork it. |
| free-sleep | MIT | UX/React frontend and HA patterns are cleanly reusable (permissive), but its *architecture* (keep-Frank + dac.sock) is what to move away from. |
| ninesleep | unspecified | Research value high; code reuse risky without a stated license. Confirm on GitHub before copying code. |
| 8rp | GPL-3.0 | Docs reference only. |
| blog writeups | n/a (prose) | Root/boot procedure reference only. |

**Recommendation:** Build on **opensleep** (GPL-3.0 Rust, direct-UART, full-stack replacement). It
already embodies the "replace, don't bolt on" architecture the user wants and carries the most
complete protocol reverse engineering. Borrow free-sleep's MIT React UI + Home Assistant ergonomics
on top. Treat ninesleep/8rp/blog posts as protocol/root references. The only remaining frontier for
a strictly blob-free stack is open MCU firmware for the two STM32F030CCT6 chips — greenfield work no
existing project has attempted.

## Key URLs
- https://github.com/LiamSnow/opensleep  (+ BACKGROUND.md, MQTT.md, HASS.md)
- https://liamsnow.com/projects/opensleep/
- https://github.com/throwaway31265/free-sleep
- https://github.com/bobobo1618/ninesleep
- https://github.com/Schluggi/8rp
- https://blopker.com/writing/04-zerosleep-1/
- https://blog.adamschaal.com/posts/2025-12-16-rooting-eight-sleep/
