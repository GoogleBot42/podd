# Eight Sleep Pod 3 — Full FOSS Firmware Replacement Plan

> **Status: original planning document (historical).** This is the plan the
> project was built from and is kept as-is for design rationale. For the current
> state of the code — crate layout, what is implemented vs. still planned — see
> [`ARCHITECTURE.md`](ARCHITECTURE.md) and the top-level [`README.md`](../README.md).

*Author: reverse-engineering pass over the four firmware dumps in `./firmware/`
(original SD image, stock rootfs, OTA rootfs, and the freesleep-modded rootfs).
Detailed evidence lives in `scratchpad/reports/{dac-protocol,hardware-frank,boot-chain,prior-art,connectivity-and-diff}.md`.*

---

## 1. Executive summary

The Pod 3 is a headless **NXP i.MX8M Mini** Linux computer (Variscite VAR-SOM-MX8M-MINI
on a custom "EightSleep New-Rat 0.8" carrier) that does **none of the thermal/sensor
work itself**. All the real-time control lives in **two external STM32F030 microcontrollers**
reached over UART. The Linux side is "just" an orchestrator: it drives the MCUs, runs a
thermostat/scheduler, reads sensors, blinks an LED ring, and phones home to Eight's cloud.

Three facts make a complete replacement realistic:

1. **No proprietary kernel drivers.** Every sleep-specific peripheral is driven from
   *userspace* over stock `/dev/ttymxc*`, `/dev/i2c-*`, and GPIO. The only non-mainline
   kernel module in the whole rootfs is `spi-nxp-fspi`. So the entire Linux userland is
   fair game to delete and rewrite.
2. **Secure boot is not enforced.** The shipped bootloader is unsigned and the SoC
   provably runs it, so we can flash our own SPL/U-Boot/kernel/rootfs with no signing keys.
   Only OTA `.swu` packages are RSA-gated — and we're bypassing OTA entirely.
3. **The hard part is already reverse-engineered.** `opensleep` (GPL-3.0, Rust) has fully
   documented the STM32 UART protocol and replaced the entire Eight userland. We do not
   need to re-derive the wire format from the stripped binaries.

**The one real fork in the road** is the STM32 firmware (Section 4). "No blobs at all"
means writing new safety-critical Peltier/pump control firmware from scratch — greenfield,
with real hardware-damage risk. Every existing project reuses Eight's extracted MCU blobs.
This is the decision that sets the project's scale, and I need your call on it.

### Decisions (from you) — updated
- **Blob scope:** *Reuse the two STM32 blobs* (Option A). The MCUs keep Eight's proven firmware,
  flashed by our own updater. STM32 rewrite (Option B) is out of scope for now.
- **Build vs runtime:** **Nix is the *build* system** (reproducible cross-compilation of the Rust
  `podd` and any image), **not** the on-device runtime. NixOS-on-device is rejected as unsuitable
  for a headless appliance. Runtime stays a conventional Linux (see the L1/L2 fork below).
- **Genericity goal:** the approach should be usable by others across Pod variants (untestable by
  us). This strongly favors replacing Eight's *software* on the shared Yocto base (L1) over a
  per-SoC full-OS-image swap (L2) — see §4.7 and §5.
- **Dev target & caution:** only your in-use Pod (Pod 3 SD variant). Never overwrite the active A/B
  slot or the SD; keep a full eMMC backup; revert paths in §4.6.
- **THE OPEN FORK:** L1 (replace Eight's software, keep Yocto base — recommended, generic) vs L2
  (also replace the OS base image — purist, per-SoC, greenfield). Pending your call.

---

## 2. The system as it exists today

### 2.1 Hardware
| Part | Detail |
|---|---|
| SoC | i.MX8M Mini (4× Cortex-A53 + Cortex-M4), Variscite VAR-SOM-MX8M-MINI SoM |
| Carrier | custom "EightSleep New-Rat 0.8" |
| RAM/eMMC | eMMC ~14.8 GiB |
| WiFi/BT | Broadcom BCM4329 (SDIO) + UART1 (ttymxc1, has RTS/CTS) |
| RTC | Micro Crystal RV-3028 @ i2c `0x68` |
| PMIC | ROHM BD71847 @ i2c `0x4b` |
| Audio codec | WM8904 @ i2c `0x1a` (used for the chime/alarm speaker) |
| LED | **IS31FL3194** RGB LED ring (I²C) — the *only* user feedback surface |
| GPIO expander | **PCAL6416A** @ i2c `0x20` — MCU enable/reset lines + factory button |
| **Frozen MCU** | STM32F030CCT6 on **`/dev/ttymxc2`** (serial@30880000) — TECs, pumps, solenoid, water level |
| **Sensor MCU** | STM32F030CCT6 on **`/dev/ttymxc0`** (serial@30860000) — bed temp, presence, piezo |
| Console | `/dev/ttymxc3` @ 115200 (serial@30a60000) |

> **Errata (corrected):** this table originally had the two MCU UARTs swapped
> (Frozen on `ttymxc0`, Sensor on `ttymxc2`). The assignment above — Frozen on
> `ttymxc2`, Sensor on `ttymxc0` — is what runs and what was confirmed on Pod 4
> hardware; `crates/podd-core/src/config/device.rs` and
> [`ARCHITECTURE.md`](ARCHITECTURE.md) own this fact.

The two "subsystems":
- **Frozen** (thermal/fluid): 2× thermoelectric (Peltier) modules with PID temp control,
  2× water pumps (left/right), a tank solenoid valve, water-level sensing, priming logic,
  TEC/water temperature sensing. Requires a periodic heartbeat or it faults off.
- **Sensor**: 8× bed thermistors (centi-°C arrays), 6× capacitive presence channels (~2 Hz),
  ambient temp/humidity, 2× piezo sensors (heart rate / breathing), vibration motors (for
  the wake alarm). Auto-ranging ADC. Streams CBOR frames.

The Cortex-M4 `/boot/cm_*.bin` RPMsg files are **stock NXP SDK demos**, unrelated to the
product — ignore them.

### 2.2 Boot chain & partitions
- **imx-boot** (SPL + U-Boot **2020.04** Variscite BSP) at raw offset `0x8000`. Unsigned.
- MBR partition table, first 8 MiB reserved for the bootloader (no boot partition):
  - **p1** — rootfs slot **A**, ext4, 6.1 GiB (`/dev/mmcblk2p1`)
  - **p2** — rootfs slot **B**, ext4, 6.1 GiB (`/dev/mmcblk2p2`)
  - **p3** — persistent **cage**, ext4, 444 MiB (`/dev/mmcblk2p3`, bind-mounted to `/persistent`)
- Kernel `Image.gz-5.4.127` + DTB `imx8mm-var-som-symphony-eight.dtb` live in `/boot` of the
  *active* rootfs slot.
- **A/B update state machine** (U-Boot env + SWUpdate): `mmcpart` selects the slot,
  `bootcount`/`bootlimit=3` trips `altbootcmd` to flip slots (auto-failover), `ustate`
  is the "did the new image confirm itself?" flag that userspace (`defibrillator`) clears
  on a healthy boot. Holding the I²C button forces slot A (factory reset).

### 2.3 Software stack (Eight's codenames)
| Service | Binary | Role |
|---|---|---|
| `frank` | `/opt/eight/bin/frankenfirmware` (C++20/ASIO, stripped aarch64) | **The hardware controller.** Drives both STM32s over UART (framed "LSP" protocol), runs the thermostat + temperature scheduler, reads sensors, handles MCU firmware updates, exposes control up via `/deviceinfo/dac.sock`. |
| `dac` | Node/TS `@eight/device-api-client` | **Cloud bridge only** — a "dumb pipe." No local intelligence. Talks CoAP to Eight's cloud and forwards commands/telemetry to `frank` over the socket. |
| `capybara` | `Eight.Capybara` (.NET) | Onboarding: BLE WiFi setup (BlueZ GATT), WiFi via NetworkManager/D-Bus, LED ring control, self-tests, MCU reboot/heartbeat. |
| `burrow` | `burrow.sh` | First-boot provisioning: reads device-id/MAC/serial from EEPROM into `/deviceinfo`, sets up persistent partition, installs WireGuard + WiFi creds. |
| `defibrillator` | `defibrillator.sh` | Watchdog: pings the internet/VPN, reboots after ~5 min offline, self-heals WireGuard, drives the `ustate` OTA-confirm machine, falls back slots on failure. |
| `cagekeeper` | `cagekeeper.sh` | Mounts/manages the persistent partition. (There is **no display**; the "cage" is data, not a Wayland compositor.) |

### 2.4 Key protocols (all documented in the reports)
- **STM32 UART "LSP"**: frame `7E [len][cmd][payload][CRC-CCITT/0x1D0F]`, response = `cmd|0x80`,
  acks + retries + receive timeout, per-MCU state machine (unknown → bootloader → firmware →
  update). 38400 baud in bootloader; sensor MCU jumps to 115200 in firmware. `0x7E` in payload
  is escaped.
- **`dac.sock`** (local, unix socket, newline-delimited): opcode table —
  `1 SET_TEMP, 2 SET_ALARM, 9/10 HEAT_L/R, 11/12 LEVEL_L/R, 13 PRIME, 14 SEND_VARIABLES`, …
  Telemetry variables like `heatLevelL/R`, `tgHeatLevelL/R`, `waterLevel`, `priming`.
  Temperatures are integer tenths-of-°C; heat "levels" are discrete table entries.
- **Cloud** (what we're deleting): CoAP over TLS to `device-api.8slp.net:5684`; OTA over HTTPS
  from `update-api.8slp.net`; logs to `nrl.8slp.net`/AWS Kinesis; WireGuard tunnel to
  `100.64.0.1`. All of this goes away in a local-only build.

### 2.5 Safety faults the firmware enforces (a replacement MUST keep these)
Overcurrent (mA ceiling per TEC), "off but still drawing current," over-temperature (°C),
bad-thermistor → clamp the TEC trigger, "thermostat not allowed to turn on," sensor-power
gating, and a heat auto-off timeout. These are the difference between "cools the bed" and
"overheats a mains-powered Peltier next to someone sleeping." **Most of these live in the
STM32 firmware**, which is exactly why the blob decision matters.

---

## 3. Prior art (and what we reuse)

| Project | Lang / License | What it is | Reuse |
|---|---|---|---|
| **opensleep** (LiamSnow) | Rust / **GPL-3.0** | **Full replacement** of dac+frank+capybara; talks directly to both STM32s over UART. Documents the whole protocol in `BACKGROUND.md`. | **Foundation.** This is the "replace, don't bolt on" design you want. |
| **free-sleep** (throwaway31265) | TS+React / **MIT** | What you run now. A local web UI/API that drives the *still-running* `frankenfirmware` via `dac.sock`. Bolted on top of stock firmware. | Steal its **React UI + Home Assistant ergonomics** (MIT lets us relicense/adapt freely). |
| **ninesleep** (bobobo1618) | — | `dac.sock` control surface documentation. | Protocol reference. |
| **8rp** (Schluggi) | GPL-3.0 | Pod reverse-engineering notes / root method. | Reference. |

The important consequence: **opensleep already did the scary UART reverse-engineering.**
Starting there converts "reverse a stripped C++ binary's serial protocol" (weeks, error-prone)
into "read a documented Rust codebase and extend it."

**License posture for the combined project:** opensleep is GPL-3.0, so the resulting stack is
GPL-3.0. free-sleep's MIT UI is GPL-compatible to fold in. That satisfies "FOSS of course."

---

## 4. The pivotal decision — the STM32 firmware ("no blobs")

You said **"No firmware blobs, etc."** There are two very different projects hiding behind that
sentence, because there are *two* layers of firmware:

**Layer 1 — the Linux userland (frank/dac/capybara).** Replacing this with FOSS is
straightforward and is the bulk of the value. Zero blobs, no debate. Everyone agrees here.

**Layer 2 — the STM32F030 firmware (`firmware-frozen.bbin`, `firmware-sensor.bbin`).**
These run *on the microcontrollers*, not on Linux. They contain the real-time TEC PID loops,
the pump/solenoid sequencing, the overcurrent/overtemp safety cutoffs, and the sensor DSP.
**No project — including opensleep — has reimplemented these.** They all flash Eight's
extracted `.bbin` blobs (128-byte big-endian header, magic `0x88888888`, then a raw STM32
image at flash base `0x08000000`). opensleep is "blob-free on the Linux side only."

So the fork is:

- **Option A — Blob-free Linux, reuse the two STM32 blobs (pragmatic; what opensleep does).**
  100% FOSS on the SoC. The MCUs keep running Eight's firmware, flashed by *our* updater.
  Effort: weeks. Risk: low. Downside: two ~40–50 KB binary blobs remain on the MCUs, so it's
  not *literally* blob-free.

- **Option B — Truly 100% blob-free: rewrite the STM32 firmware too (greenfield).**
  We write new STM32F030 firmware for both MCUs. This is a real embedded project on its own:
  TEC bidirectional PID + current sensing + thermal safety, pump/solenoid control, capacitive
  + piezo + thermistor acquisition and filtering — all with **no prior art and no schematic**,
  reverse-engineered from behavior. Effort: months. Risk: **high and physical** — a bug can
  overheat a Peltier or dead-head a pump. Mandatory: a bench rig, a current-limited PSU, and
  ideally a spare Pod to brick.

My recommendation: **do Option A first** (get a fully local, cloud-free, hackable Pod running
your own code — that alone kills everything you dislike about free-sleep), and treat Option B
as a **separate, optional Phase 5** you can start later on a bench unit without risking your
bed. The plan below is structured so A is a complete, shippable product and B bolts on after.

---

## 4.5 [L2-ONLY] Custom-OS-image boot integration (verified against the dumps + community docs)

> **Scope note:** This section applies to **L2** (replacing the whole OS image). **L1 does not touch
> the bootloader, kernel, DTB, or boot flow at all** — it only swaps userland on the existing rootfs,
> so skip to §4.6/§5 for the L1 path. Kept here because it's researched and needed if/when you pursue
> L2. (Also: "NixOS" below is illustrative of *an* L2 image; per your decision the L2 runtime would be
> a conventional Nix-cross/Buildroot rootfs, not NixOS — the boot glue is identical either way.)

**The device boots and runs from internal eMMC — the SD is recovery-only.** This corrects an
earlier assumption. Evidence: the device's *persisted* U-Boot env (the copy at raw offset
`0x400000`, carrying the real `serial#=<device-serial>`, `ethaddr`, `board_name=VAR-SOM-MX8M-MINI`)
sets **`mmcdev=2 mmcblk=2 mmcpart=2`** → `root=/dev/mmcblk2p2`. The A/B slots are
**`rootfs_a`=`/dev/mmcblk2p1`** and **`rootfs_b`=`/dev/mmcblk2p2`** on **eMMC** (`mmcblk2`);
persistent `/cage` is `mmcblk2p3`. The `mmcdev=1`/`mmcblk=1` values are only in the *shipped
default-env file* inside the rootfs, not the running env. The `factory_reset` routine switches to
`mmcdev=1` (SD) **only when the I²C button is held** — that is the recovery/install path.
free-sleep's INSTALLATION.md confirms it: root is obtained via **JTAG/serial to the U-Boot prompt**
(`root=PARTLABEL=rootfs_a rootwait init=/bin/bash`) and the **eMMC is modified directly**; the SD
is never reflashed. (Two HW variants exist: with-SD and no-SD / FCC ID 2AYXT61100001.)

**Consequence:** we do **not** run NixOS from the SD. We install it to eMMC. Because you already
have root (that's what free-sleep needs), the safest path is an **in-place install to the inactive
eMMC A/B slot** from a root shell — no disassembly beyond what rooting already required, no SD swap.

**U-Boot boot flow (unmodified, we reuse it):**
```
bootcmd → mmc dev ${mmcdev}(=2, eMMC) → factory_reset(button? → mmcdev=1 SD) → mmc rescan →
  loadbootscript:  load mmc 2:${mmcpart} ${loadaddr} /boot/boot.scr → source   ← we hook here
  (else) loadimage: load /boot/Image.gz + loadfdt /boot/<dtb> → booti (no initrd)
altbootcmd (after bootlimit=3): flip mmcpart 1↔2, saveenv, reboot   ← free A/B auto-rollback
```
U-Boot tries **`/boot/boot.scr` first**, so we don't touch SPL/U-Boot — we drop our own `boot.scr`
that loads the NixOS kernel + initrd + DTB from our eMMC slot. Console `ttymxc3,115200`, no secure
boot. The env also documents the boot LED + button (I²C `0x53`=IS31FL3194, `0x20`=PCAL6416A) — a
ready-made reference for our own boot indicator.

**How NixOS slots in:**
- **Bootloader:** keep the existing imx-boot (SPL + U-Boot 2020.04) untouched. NixOS owns only an
  eMMC rootfs slot + its `/boot`.
- **Boot glue:** a small NixOS module generates `/boot/boot.scr` (via `mkimage`) for the current
  generation — loads `Image`, the NixOS `initrd`, and the stock
  `imx8mm-var-som-symphony-eight.dtb`; sets `root=/dev/mmcblk2pN init=…systemd console=ttymxc3,115200`.
  (Standard pattern for U-Boot boards without `extlinux`/`sysboot`.)
- **Kernel:** package the **Variscite BSP 5.4.127 kernel from source in Nix**, reuse the stock DTB —
  guarantees carrier pinmux, BCM4329 SDIO WiFi, and UART/I²C aliases work day one. Mainline later.
- **WiFi firmware:** BCM4329 brcmfmac firmware ships in `linux-firmware` (redistributable) — fine.
- **Rollback:** *three* nets — NixOS generations, U-Boot `bootcount`/`altbootcmd` slot flip, and the
  intact stock slot (revert = `fw_setenv mmcpart <stock>`). Eight's cloud OTA/`swupdate`/`ustate`
  machinery is deleted; updates become "build closure, write inactive slot, flip pointer."

**The cautious install workflow (matches "only my daily-driver Pod"):**
1. Phase 0: from your existing root shell, `dd` a full backup of **`/dev/mmcblk2`** (all of eMMC) to
   a file over the network, plus `/deviceinfo` and the `/cage` partition. This is the master revert.
2. Identify the **inactive** slot (you run from `mmcpart=2`/`rootfs_b`; target `rootfs_a`=`mmcblk2p1`).
   Write NixOS there; **never overwrite the running slot**. Stock stays bootable the whole time.
3. `fw_setenv` the boot pointer to the NixOS slot; reboot. If it fails: `bootlimit` auto-falls back,
   or hold the factory-reset button for the SD recovery path, or `fw_setenv` back from a rescue.
4. Build a **custom recovery SD** (mirrors Variscite/Eight's installer to reflash eMMC from scratch)
   as the ultimate backstop — a later task, not the primary path.

**Caveats to validate on hardware:** confirm U-Boot's `load mmc` + `booti` accept an initrd on this
board (expected yes); confirm where `fw_setenv` writes the env (`/etc/fw_env.config`) so the boot
pointer actually persists; first boot over the `ttymxc3` serial console so we can watch/fix live.
The SD-variant may load SPL from the SD — if so, keep the SD inserted and untouched (it stays the
recovery medium); we only change eMMC + env.

---

## 4.6 Is the inactive slot really unused? (verified) + exact revert procedure

> **Scope note:** The A/B-slot mechanics below matter for **L2** and for the *safer* L1 install
> variant (clone running slot → apply L1 changes to the inactive slot → flip). Plain L1 (in-place
> userland swap on the running slot, opensleep-style) is reverted simply by re-enabling Eight's
> services or a stock firmware-reset; keep the eMMC backup regardless. "NixOS" below = "your custom
> image/slot" generally.

**Where the env lives:** `/etc/fw_env.config` = `/dev/mmcblk1 0x400000 0x1000` — the U-Boot
environment is stored on the **SD card** (mmcblk1), single copy (no redundant env). So the real
topology is: **SPL + U-Boot + env boot from the SD; the A/B rootfs runs from eMMC (mmcblk2).** The
SD is required to boot but is never modified by us; the eMMC holds the running OS. `fw_setenv` from
the running eMMC OS edits the SD's env (keep the SD inserted).

**Is the inactive rootfs slot used? — No, except by the OTA updater.** Verified from the stock
scripts:
- Not mounted: there are **no `.mount` units**, and `fstab` mounts only `/cage`=`mmcblk2p3` + tmpfs.
  The inactive rootfs partition is never mounted or read in normal operation.
- The **only writer** of the inactive slot is **`swupdate` (OTA)**: `swupdate.sh` waits for
  `ustate==OK`, then `swupdate` writes the *other* partition, sets `ustate=INSTALLED` +
  `next_mmcpart`, and reboots. On reboot `defibrillator` sets `ustate=TESTING`, validates
  connectivity, then either `set_ustate OK` (`fw_setenv ustate 0 bootcount 0`) or
  `reboot_to_fallback` → `set_ustate FAILED` → `fw_setenv ustate FAILED mmcpart <other> falling_back 1`.
- **Therefore, before staging NixOS in the inactive slot, mask the stock writers/rebooters** so
  nothing overwrites the staged image or flips slots under us: `swupdate`, the suricatta OTA poller,
  and `defibrillator`. (We're deleting all of them anyway.)
- **Two fallback layers exist:** (1) userspace `defibrillator` (stock only), and (2) U-Boot
  `bootcount`/`bootlimit=3` → `altbootcmd` flips `mmcpart` to the other slot, `saveenv`, reboots —
  independent of userspace. On NixOS only layer (2) protects us, so: **NixOS must reset
  `bootcount=0` on a successful boot** (a tiny oneshot service), otherwise 3 good boots would trip a
  spurious fallback to stock.

**Must be checked on YOUR live device in Phase 0 (can't be read from the image):** which slot is
currently active (`fw_printenv mmcpart` — the image shows `2`/`rootfs_b`, but yours may have OTA'd
to `1`), current `ustate`/`bootcount`, and that no OTA is mid-flight. The stock copy that saves you
is the **currently-active slot**, which we never touch — we only ever write the *inactive* one.

**Zero-commitment test before committing (do this first):** at the U-Boot serial prompt, test-boot
NixOS **without** `saveenv`:
```
=> setenv mmcpart 1        # the slot where NixOS is staged
=> boot                    # or: run bootcmd
```
If it works, great. If not, just power-cycle — the *saved* env still says `mmcpart=2`, so you're
back on stock with nothing changed. Only after NixOS proves itself do you `saveenv` the pointer.

**Exact revert steps if the NixOS slot goes bad (least → most invasive):**
1. **NixOS boots to a shell but is broken:**
   `fw_setenv mmcpart <STOCK> ; fw_setenv bootcount 0 ; fw_setenv ustate 0 ; reboot`
   (`<STOCK>` = the slot you came from, e.g. `2`.) → boots stock.
2. **NixOS won't boot — automatic:** power-cycle a few times. U-Boot increments `bootcount` each
   attempt; after `bootlimit=3`, `altbootcmd` flips `mmcpart` to the stock slot, resets bootcount,
   `saveenv`, boots stock. (Verify `bootcount` increments on your unit; if unsure use step 3.)
3. **NixOS won't boot — guaranteed manual (serial console `ttymxc3@115200`, the header you rooted
   with):** power on, interrupt autoboot (`bootdelay=1` — hold a key immediately), then:
   `=> setenv mmcpart 2 ; setenv bootcount 0 ; setenv ustate 0 ; saveenv ; boot` → boots stock.
   Independent of bootcount; writes the SD env directly. **This is the reliable revert.**
4. **Deep recovery — factory-reset button:** hold the internal button during power-on. U-Boot's
   `factory_reset` sets `mmcdev=1/mmcblk=1/mmcpart=1/saveenv` → boots the SD's recovery rootfs,
   which can reflash eMMC to stock.
5. **Master restore:** boot any working slot (or the recovery SD) and `dd` your Phase-0 backup of
   `/dev/mmcblk2` back, or reflash via JTAG/UUU → exact factory eMMC.

Because we never touch the active stock slot or the SD (env + bootloader), **every one of these
lands on a working stock system.** The only ways to lose that guarantee are overwriting the active
slot or corrupting the SD — both explicitly forbidden by the plan.

---

## 4.7 Variant landscape (researched) + the replacement-level fork

Web research (no firmware for these) established that the Pod line is **not one platform**:

| | Pod 3 (SD) — your unit | Pod 3 (no-SD) | Pod 4 / Pod 5 |
|---|---|---|---|
| SoC / SoM | **NXP i.MX8M Mini**, Variscite VAR-SOM-MX8M-MINI | **MediaTek MT8365 "Genio 350"**, custom Eight/OLogic "i350 SOM" (FCC 2AYXT61100001) | i.MX8MM+Variscite (*inferred*; FCC photos sealed) |
| Boot | SPL+U-Boot+env on **removable microSD**; rootfs on eMMC | BROM→Preloader(eMMC boot0)→U-Boot, **all on eMMC** | U-Boot on eMMC |
| Root dev | `mmcblk2` | `mmcblk0` | eMMC |
| Unbrick | NXP `uuu`/SDP over USB-OTG (unproven on board) | MediaTek `mtkclient`/SP Flash Tool via USB-C **J13** (unproven) | NXP `uuu` (unproven) |
| Secure boot | off → **GO** | off → **GO** (fuses unverified) | off → **GO** |
| **Shared by all** | **Yocto + systemd · A/B rootfs `rootfs_a`/`rootfs_b` + `current_slot` env · STM32 MCUs over UART · serial → interruptible U-Boot (header J7, 921600)** | | |

**Root is the same everywhere** (serial J7 → interrupt autoboot → `init=/bin/bash` → set passwords →
disable OTA), which resolves the serial-access doubt: two independent sources + the no-SD FCC
teardown confirm the header reaches an **interruptible U-Boot console**. But **everything below the
userland differs per SoC** (bootloader, kernel, DTB, device nodes, unbrick tool).

**Key finding that sets the architecture:** *every* existing project — opensleep included — runs its
code **on the stock Yocto rootfs** and never replaces the OS image. opensleep is a cross-compiled
Rust binary under stock systemd that **deletes Eight's `dac`/`frank`/`capybara`/OTA and drives the
STM32s directly**; it never touches the A/B slots. So a full-OS replacement is greenfield relative
to all prior art, *and* it cannot be generic (i.MX8MM vs MT8365 are separate builds).

**The replacement-level spectrum:**
- **L1 — replace Eight's *software* stack (recommended, generic).** A clean FOSS Rust `podd` +
  local API/UI that removes every Eight service (dac/frank/capybara/burrow/defibrillator/OTA/cloud/
  WireGuard) and talks to the MCUs directly. Keeps the Yocto *base* (kernel/libc/systemd) and the
  STM32 blobs. Built with **Nix**; conventional runtime. Runs on all variants. Eliminates 100% of
  Eight's firmware logic and all cloud coupling — architecturally a clean replacement, not a
  free-sleep-style bolt-on (free-sleep leaves `frankenfirmware` running and pokes it via `dac.sock`;
  L1 deletes it).
- **L2 — also replace the OS base image (optional, purist, per-SoC).** Swap the Yocto rootfs for a
  minimal Nix-cross/Buildroot rootfs on the inactive A/B slot, reusing the vendor kernel/U-Boot/DTB.
  Greenfield, higher-risk, must be built separately per SoC → not the "generic for others" layer.
- **L3 — kernel/bootloader/STM32 firmware from scratch.** Research-grade, bench-only. Ruled out
  (we reuse STM32 blobs).

**Recommendation:** ship **L1** as the reusable foundation; pursue **L2** later on your own unit as
an opt-in deeper layer. The rest of this plan is written for L1, with L2 called out where it differs.

---

## 5. Target architecture (the replacement)

```
        ┌─────────────────────────────────────────────────────────┐
        │  Stock Yocto base (kernel/systemd) + Nix-built podd  [L1]  │
        │                                                           │
        │  ┌──────────────┐   local HTTP/REST + WS   ┌───────────┐  │
        │  │  Web UI (SPA)│◄────────────────────────►│  podd     │  │
        │  │ (from        │                          │ (Rust     │  │
        │  │  free-sleep) │                          │  daemon)  │  │
        │  └──────────────┘                          └─────┬─────┘  │
        │        ▲  Home Assistant / MQTT / REST           │        │
        │        │                                         │        │
        │   your LAN only — no cloud, no WireGuard    ┌─────┴─────┐  │
        │                                             │ hardware  │  │
        │                                             │ layer     │  │
        │                                             └──┬─────┬──┘  │
        └────────────────────────────────────────────────┼─────┼────┘
                       UART ttymxc2 (LSP) ────────────────┘     └──── UART ttymxc0 (LSP)
                              │                                        │
                        ┌─────┴─────┐                            ┌─────┴─────┐
                        │ Frozen MCU│  (STM32 runs Eight's       │ Sensor MCU│
                        │ TEC/pump  │   .bbin blob — Option A)    │ presence/ │
                        └───────────┘                            │ temp/HR   │
                                                                 └───────────┘
   also on I²C: IS31FL3194 LED ring · PCAL6416A (MCU enable/reset) · RV-3028 RTC · WM8904 audio
```

**`podd` (the one daemon that replaces frank+dac+capybara)** owns:
- The two LSP UART links (frame/escape/CRC, retries, per-MCU bootloader→firmware state machine).
- The MCU firmware updater (parse `.bbin`, drive the STM32 bootloader over LSP) — flashes Eight's
  archived blob (Option A); the same path would flash our own firmware under the optional L3.
- Thermostat + temperature scheduler (the logic Eight kept in the *cloud* — we bring it local).
- Alarm scheduling → vibration motors + WM8904 chime.
- Sensor ingestion → local time-series store (SQLite), presence detection, optional HR/HRV.
- Safety supervisor on the Linux side (heartbeat to Frozen, sanity-clamp setpoints, watchdog).
- LED ring status, PCAL6416A MCU enable/reset, RV-3028 RTC.
- A **local REST/WebSocket API** + optional **MQTT/Home Assistant** — no cloud, ever.
- Onboarding: bring up WiFi via NetworkManager/`wpa_supplicant` (keep the BLE setup only if
  you want phone-based provisioning; otherwise a config file is simpler).

Everything Eight built for *its* business — CoAP cloud client, OTA/`swupdate`, WireGuard
phone-home, Kinesis/journalbeat logging, remote provisioning — is **deleted**, not ported.

---

## 6. Staged roadmap (L1-first)

Working assumptions (you were away when I asked; override any of these): **L1 now, L2 optional
later**; **`podd` = fork & extend opensleep** (Rust, GPL-3.0); **Nix builds, conventional runtime**;
reuse STM32 blobs (Option A). Each phase leaves you with a working Pod.

**Phase 0 — Safety net, recovery, and hardware access.**
- From your existing root shell: `dd` a **full backup of the whole eMMC** (`/dev/mmcblk2` on your SD
  variant) over the network and verify it. Back up `/deviceinfo`, `/persistent`/`/cage`, and note the
  active slot (`fw_printenv mmcpart`/`current_slot`), `ustate`, `bootcount`.
- Archive Eight's `.bbin` MCU blobs (`/opt/eight/lib/subsystem_updates`) — needed to (re)flash the
  STM32s and as the Option-A firmware of record.
- Establish and TEST a recovery path *before* changing anything: the **serial → U-Boot** console
  (header J7, 921600 — confirm you can interrupt autoboot), and identify the SoC-specific unbrick
  (`uuu`/SDP for i.MX8MM; note it's the MediaTek `mtkclient` path on the no-SD variant). Prove you can
  get back to stock.

**Phase 1 — Stand up the FOSS control daemon `podd` (the heart of L1).**
- Fork **opensleep**; bring up its LSP UART layer against the live MCUs (still on Eight's firmware).
  Verify each command against telemetry before trusting it (protocol was reversed on *some* units).
- Prove the MCU updater by (re)flashing the archived `.bbin` blobs.
- Implement/verify: read all sensors, set TEC temperature, run pumps/prime, LED ring, RTC, and the
  mandatory **Frozen heartbeat + setpoint sanity-clamp safety supervisor** (honor every fault in §2.5).
- Milestone: *bed heats/cools and sensors read from our own daemon.*

**Phase 2 — Cut the cord: delete Eight's stack, run only `podd`.**
- Disable/mask every Eight service: `dac`, `frank`, `capybara`, `burrow`, `defibrillator`,
  `cagekeeper`, `swupdate`+suricatta poller, journalbeat/vector, WireGuard `wg0`. Install
  `podd.service`. (opensleep-style in-place swap on the running rootfs — reversible by re-enabling.)
- **Safer variant (recommended):** clone the running slot → apply these changes on the *inactive* A/B
  slot → flip `current_slot`, so stock stays pristine for one-command rollback (§4.6).
- Milestone: *nothing phones home; the Pod is fully local and runs only your code.*

**Phase 3 — Product features (what free-sleep gets right, done natively in `podd`).**
- Local thermostat + schedules + away mode + alarms (vibration motors + WM8904 chime) — the logic
  Eight kept in its *cloud*, now on-device.
- Local time-series store (SQLite); presence detection; optional HR/breathing from the piezo channels.
- Local **REST/WebSocket API** + **Home Assistant / MQTT**; a React UI adapted from free-sleep (MIT).
- Milestone: *feature parity with free-sleep, but it IS the firmware and is cleanly hackable.*

**Phase 4 — Robustness, reproducibility, and a Nix build.**
- Package `podd` (and its deploy artifacts) as a **Nix flake** (cross-compiled aarch64); reproducible
  releases + a documented, scripted install/rollback that others can run on their own units.
- Watchdog + safety supervisor hardening; keep the U-Boot `bootcount`/`altbootcmd` A/B failover as the
  low-level net; local-only updater (no Eight cloud).
- Milestone: *reproducible, recoverable, and installable by other people.*

**Phase 5 (optional) — L2: replace the OS base image.** On the inactive A/B slot, build a minimal
**Nix-cross/Buildroot** rootfs reusing the vendor kernel/U-Boot/DTB (see §4.5 for the boot glue),
carrying only `podd` + deps. Per-SoC (i.MX8MM here); not generic. Removes the last of Eight's OS
packaging.

**Phase 6 (optional) — L3: rewrite the STM32 firmware** for a literal zero-blob stack. Bench-only,
current-limited PSU, spare hardware. Research-grade; scope separately.

---

## 7. Risks & how the plan mitigates them

| Risk | Mitigation |
|---|---|
| Bricking your daily-driver Pod | Full eMMC backup + a **tested** recovery path (serial U-Boot; SoC-specific `uuu`/`mtkclient`) *before* any change (Phase 0). L1 in-place is reversible; the safer L1 variant keeps the stock slot pristine. |
| Overheating a Peltier / pump damage | Keep Eight's STM32 firmware (Option A) so hardware safety stays intact; add a Linux-side safety supervisor (heartbeat, setpoint clamp, honor all §2.5 faults) as belt-and-suspenders. |
| UART protocol surprises vs. opensleep | opensleep was reversed on *some* units; validate every command against telemetry, capture live UART early, fail safe on unknown responses. |
| Losing device identity (MAC/serial from EEPROM) | Preserve read-only EEPROM/`/deviceinfo` access; back them up in Phase 0; `podd` shouldn't need the cloud identity at all. |
| Approach not generalizing | L1 targets the shared Yocto+A/B+STM32 layer; keep SoC/board specifics (device nodes, unbrick) behind a thin config so others can port. L2/L3 are explicitly per-SoC and optional. |
| GPL-3.0 obligations (forking opensleep) | Fine for a public FOSS project; keep the repo public, preserve notices. |

---

## 8. Decisions

**Resolved with you:** Option A (reuse STM32 blobs) · **Nix = build only, conventional runtime**
(NixOS-on-device rejected) · dev on your daily-driver Pod 3 (SD variant) with backups + tested
recovery, never overwriting the active slot or SD · approach should be **generic/reusable**.

**Assumed (you were away; confirm or override):**
1. **Replacement level:** **L1 now, L2 optional later.**
2. **`podd` foundation:** **fork & extend opensleep** (Rust, GPL-3.0).

**Still genuinely open (I'll proceed on the rec):**
3. **Onboarding:** config-file / local-web WiFi (simpler, rec) vs BLE phone-based (reuses capybara's idea).
4. **How I help build:** produce artifacts + an exact runbook you execute, and/or drive interactively
   if you give me SSH to the Pod. (I can't touch your hardware directly.)

---

## 9. Update architecture (a first-class concern)

Free-sleep's scheme is the anti-pattern to avoid: install via `curl …install.sh | bash` off `main`,
update via **`git pull` on the device** running prebuilt (git-committed) JS. Flaws → our inverse
requirements: unpinned/unsigned `curl|bash` → **signed, pinned artifacts**; non-atomic `git pull` on a
live tree with no rollback → **atomic + auto-rollback**; on-device unverifiable build → **reproducible
Nix build, version = content hash, nothing built on device**; no health gate → **canary health check**;
no code/config/data split or version coherence → **coherent signed bundles + migrated config**.

**Charter:** atomic · **integrity always enforced** (content-addressed SHA-256; a corrupt/truncated
artifact is always rejected) · **authenticity is owner-controlled and optional** (see trust policy
below) · reproducible · version-coherent · **offline-installable** (LAN/USB/file; never depends on
GitHub/cloud being up) · observable + one-click reversible · fork-friendly.

**Trust policy (owner-controlled — no central authority).** Signing is *optional*; the device owner
sets which authors to trust, so anyone can update their own device or fork:
- **Unsigned / dev mode** — accept any bundle (digests still checked). For hacking on your own device
  or trusted local pushes.
- **Your own key(s)** — `podup keygen` is self-service; put your public key(s) on your device and push
  your own signed builds. Multiple trusted keys (yours + a friend's + optionally an upstream one).
- **Off entirely.**
Signatures add *authenticity* (proof of who built it), which matters for a remote auto-pull channel,
not for `podup release`-ing your own build onto your own Pod. Suggested default: allow unsigned from a
local file/USB, require a trusted signature only for a remote auto-pull channel. Implemented in
`pod-update` as `TrustPolicy::{AllowUnsigned, RequireSigned(keys)}`.

**Four tiers, matched to risk/cadence:**

| Tier | Component | Cadence | Mechanism |
|---|---|---|---|
| 2 (main loop) | **App: `podd` + UI + config schema** | frequent | atomic release-dir swap + health-gated rollback, **no reboot** |
| 1 | **OS image** (kernel+DTB+rootfs) [L2] | infrequent | **eMMC A/B + U-Boot bootcount/altbootcmd**, RAUC-style signed bundle |
| 3 | **STM32 MCU firmware** (`.bbin`) | rare | `podd` flashes on version-change only, verify readback, MCU bootloader = fallback |
| 0 | **Bootloader** (U-Boot/preloader) | ~never | excluded from auto-updates; manual, recovery net staged (serial/UUU/mtkclient) |

**Tier 2 (app).** Nix builds `podd-release-<ver>` = Rust binary + UI static assets + config
migrations + signed manifest (versions, min-OS/min-MCU, hashes). Device: fetch → **verify signature**
→ unpack to `/opt/podd/releases/<ver>` → run **canary** (API up, MCUs respond, no panic in N s) → only
then atomically flip `/opt/podd/current` symlink + `systemctl restart` → else discard, keep current.
Retain last K releases for instant `podd rollback`. Config lives outside the release dir
(`/persistent`), forward-migrated, never clobbered. Seconds, no reboot, reversible.

**Tier 1 (OS, L2).** Nix builds the full rootfs → signed **RAUC** bundle (RAUC = the correct version
of Eight's SWUpdate). Updater writes the **inactive** A/B slot, flips `current_slot`, reboots; U-Boot
`bootcount`/`bootlimit=3`/`altbootcmd` auto-reverts a slot that fails to boot-and-confirm; `podd`
marks-good on a healthy boot (resets bootcount). This is Eight's `ustate` machine minus the cloud, on
your key. (RAUC↔Nix is some integration; a minimal hand-rolled A/B writer is an acceptable start.)

**Tier 3 (MCU).** `.bbin` ships in a bundle with its own version; `podd` flashes only on version
change, only when idle, verifies readback/CRC; the STM32 bootloader is the built-in fallback (bad
flash → MCU stays in bootloader → retry). No MCU A/B → rare + gated + logged.

**Provenance/transport.** A tiny **signed manifest** (channel: stable/beta) lists current versions +
artifact URLs + hashes. Device polls it from a **self-hosted URL or GitHub *Releases assets*** (signed
tarballs, not a git checkout), or accepts a local file/USB. CI = `nix build → sign → publish`. Public
key baked into `podd` + the OS image; rotatable.

**Observability.** `podd` exposes `/version` (app/OS/MCU/bootloader + closure hashes + history) and
update/rollback controls in the UI; channels, "check now", "install file", "roll back".

**Updating the updater.** App-updater ships inside `podd` (updated by the app bundle); OS-updater
(RAUC) updated by the OS-image A/B; bootloader ~never. No component self-updates in a way that can
brick mid-write.

**Genericity.** Tiers 2–3 are SoC-agnostic; only Tier 1 touches variant specifics (device nodes,
U-Boot vs MediaTek env) — kept behind config so a fork on the no-SD MediaTek Pod changes only
`mmcblk2→mmcblk0` + the env backend.

---

## Appendix — where the evidence lives
Per-area reports are preserved durably in **`./research/`**: `dac-protocol.md`, `hardware-frank.md`,
`boot-chain.md`, `prior-art.md`, `connectivity-and-diff.md`, `pod3-nosd.md`, `pod4.md`,
`generic-flash-recovery.md`. The extracted rootfs trees and the decompressed `pod3.img` remain under
the session `scratchpad/work/` (temporary — regenerable from `./firmware/` if needed).
