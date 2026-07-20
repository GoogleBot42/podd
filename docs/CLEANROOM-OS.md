# Clean-room OS image (L2) — architecture

> **Status: PROVEN ON HARDWARE (2026-07-20).** The clean-room image
> (`os/scripts/build.sh` → `dist/podd-sd.img.gz`) boots the Pod 3-SD hub with
> **zero Eight Sleep binaries**: from-source `imx-boot` (Variscite SPL/U-Boot +
> ATF + NXP DDR fw), the 5.4 kernel + our DTB, and the Buildroot rootfs.
> Verified live: WiFi joins, sshd answers on 8822, podd drives **both** bed
> sides to their configured setpoints through the frozen MCU, sensor telemetry
> streams, and the free-sleep-compat API/UI serves on :3000. This retires both
> the L1 "bolt podd onto Eight's Yocto rootfs" model (`scripts/build-podd-sd.sh`,
> kept as documentation — see [SD-BOOT.md](SD-BOOT.md)) and the interim
> hand-derived "stockboot" image that borrowed Eight's bootloader region.
> Remaining: RAUC A/B slot-device wiring (MBR has no partlabels — the
> `rauc-system.conf` device paths need reconciling) and CI publishing. See
> [ARCHITECTURE.md](ARCHITECTURE.md) for the userland (podd) and "Bring-up
> field notes" below for the pitfalls hit on first boot.

## Why this exists

The L1 image is a clone of the stock eMMC rootfs — Eight Sleep's `imx-boot`,
U-Boot, Variscite BSP kernel/DTB, and entire Yocto userland — with podd dropped
into `/opt/podd` and the vendor services masked. Two consequences fall straight
out of that:

- **Updates can only reach `/opt/podd`.** Everything else is opaque vendor
  binaries we can't rebuild, so a kernel/rootfs update is impossible and OTA is
  confined to swapping a squashfs under `/opt/podd`.
- **The image can't be published.** It's Eight's copyrighted OS and it was
  assembled from a specific owner's device dump (personal WiFi creds, the unit's
  serial). CI cannot legitimately attach it to a release.

The clean-room image fixes both by building the whole bootable system **from
source**, so we own every byte in it, updates can replace the whole image, and CI
can publish it.

## Decisions

| Question | Decision | Rationale |
|---|---|---|
| Clean-room scope | **OS image only** | Build bootloader + kernel + rootfs + podd from source. Eight's STM32 firmware keeps running *on the MCU chips* (it's on their own flash, not in the image; podd talks to it over UART). Rewriting the safety-critical STM32 thermal/sensor firmware is a separate, months-long, out-of-scope project. |
| Rootfs builder | **Buildroot** (`BR2_EXTERNAL` tree under `os/`) | Minimal, reproducible, from-source appliance rootfs. Smallest surface for a headless device; strong i.MX8MM support. Nix stays a build tool for the `podd` binary only, not the on-device runtime. |
| OTA engine | **RAUC** | Mature signed-bundle A/B framework with U-Boot bootcount integration and auto-rollback. Replaces the homegrown `/opt/podd` symlink-swap as the primary update path. |

## The clean-room boundary (read this)

"No Eight Sleep binaries" is achievable and is the bar we hold. "Zero binary
blobs of any kind" is **not** possible on this SoC and never has been:

- **The i.MX8M boot chain needs NXP's firmware blobs** — DDR PHY training
  (`lpddr4_pmu_train_*.bin`) and HDMI/DP firmware, fetched by Buildroot from
  NXP's `firmware-imx` release. They are **NXP's**, not Eight's, and *every*
  i.MX8M board (including fully mainline ones) ships them — the SoC physically
  cannot boot without the DDR training firmware.
  - **License terms** (`LA_OPT_NXP_Software_License`): redistribution is
    permitted *only embedded within an "Authorized System"* — hardware built
    around an NXP part. A firmware image for the i.MX8MM Pod qualifies, so
    publishing it from CI is within the license (the same basis Variscite,
    Yocto/meta-freescale, and Buildroot ship i.MX images on). NXP's copyright
    notice must ship with it (`make legal-info` emits this), and it stays a
    **proprietary** component — it cannot be relicensed GPL. So the published
    image is *our GPL code + one NXP proprietary blob we're licensed to ship for
    this hardware + zero Eight Sleep code*. This is "no Eight blobs," not
    "100% FOSS."
- **The STM32 MCU firmware stays Eight's** — but it lives on the microcontrollers'
  own flash, **not in this image**. The clean-room OS image contains none of it;
  podd merely speaks the (reverse-engineered) UART protocol to it. This only
  becomes a question if you want to *reflash* the MCUs from clean-room source,
  which is the out-of-scope STM32-rewrite project.

Net: the OS **image** is 100% free of **Eight-authored** code and is publishable
(one NXP proprietary boot blob remains, shipped under NXP's license as above).

## Component stack

```
  boot ROM (SoC, immutable)
    └─ SPL            ← U-Boot, from source
        └─ ATF (BL31) ← ARM Trusted Firmware, from source
            └─ U-Boot ← from source; RAUC bootcount/BOOT_ORDER env
                └─ Linux kernel + our DTB ← from source (Variscite BSP 5.4.x or mainline i.MX8MM)
                    └─ Buildroot rootfs (systemd or minimal init)
                        ├─ podd  (static aarch64-musl, from the Cargo workspace)
                        ├─ podd web UI (from ui/)
                        ├─ rauc  (update client)
                        └─ NetworkManager, sshd, iptables muzzle, …
```

- **`imx-boot`** is assembled by `imx-mkimage` (open) from SPL + U-Boot + ATF +
  the NXP DDR/HDMI blobs. Written raw at offset `0x8400`. No secure boot is
  enforced on these units (unsigned SPL runs), so a from-source boot chain works.
- **U-Boot env** at `0x400000` carries the RAUC A/B selection logic
  (`BOOT_ORDER` + `BOOT_x_LEFT` bootcount) and `mmcdev` (SD=1 / eMMC=2).
- **SoM = Variscite VAR-SOM-MX8M-MINI (DDR4), *not* a DART** — despite the live
  DTB's `compatible = "variscite,dart-mx8mm"` (Variscite's single Yocto MACHINE
  covers both SoMs; the U-Boot env's `board_name=VAR-SOM-MX8M-MINI` and the SOM
  EEPROM are authoritative). This matters: the DART is LPDDR4, the VAR-SOM is
  **DDR4**, and Variscite's single U-Boot build runtime-selects the DDR timing
  and control DTB from the SOM EEPROM — so `imx-boot` must append **both** DDR
  training firmware sets and pack **both** control DTBs in the FIT (see
  `os/board/eightsleep/imx8mm-varsom/post-image.sh`). Console is UART4
  (`ttymxc3`, not broken out). MCU UARTs: UART1 `ttymxc0` = Sensor, UART3
  `ttymxc2` = Frozen.
- **podd** is cross-compiled by the existing `nix build .#podd-aarch64` and
  installed into the rootfs as a Buildroot package.

## Slot & partition layout

RAUC drives two rootfs slots plus a persistent data partition. Kernel + DTB live
inside each rootfs (`/boot`), so each slot is self-contained and an update swaps
one atomic image.

**SD image (the publishable, first-class install):**

| Region | Contents |
|---|---|
| `0x8400` (raw) | `imx-boot` |
| `0x400000` (raw) | U-Boot env (`mmcdev=1`, RAUC `BOOT_ORDER`) |
| p1 | rootfs **A** (ext4, incl. `/boot`) |
| p2 | rootfs **B** (ext4, incl. `/boot`) |
| p3 | persistent data (config, schedules, logs) |

Boot from this SD leaves the eMMC **untouched** — swapping the stock card back is
still an instant, total revert. RAUC updates the inactive SD slot.

**eMMC install (later):** same layout mapped onto the eMMC A/B slots
(`rootfs_a`=p1 / `rootfs_b`=p2 / `cage`=p3), written to the inactive slot from a
running system, boot pointer flipped with `fw_setenv`. The stock slot survives as
the revert until you choose to overwrite it.

## Install = OTA (converged)

There is no longer a separate "installer that modifies an existing rootfs." Both
flows write a complete signed image to an inactive slot:

- **First install:** `dd` the published SD image to a card and boot it, **or**
  write the eMMC inactive slot from a running system and flip the pointer.
- **Update:** RAUC fetches a signed bundle, writes it to the inactive slot,
  verifies it, marks it primary with one boot attempt, reboots. On a failed
  canary the U-Boot bootcount logic auto-falls-back to the previous slot.

The `pod-update` crate's signing/manifest concepts still apply (offline Ed25519
owner key, integrity-always / signature-optional trust policy); RAUC's bundle
signing is the mechanism. The old `pod-updater` `/opt/podd` symlink-swap becomes
obsolete for the OS, though it may survive as an optional fast app-only dev loop.

## Build system & publishing

- The Buildroot external tree lives under **`os/`** (`BR2_EXTERNAL`): board
  defconfig, U-Boot/ATF/kernel config fragments, our DTB, the RAUC system config,
  a `genimage` layout for the A/B partitions, and post-build/post-image scripts
  that assemble `imx-boot` and the final `.img`.
- **CI** (`.github/workflows/release.yml`): a new job builds the Buildroot SD
  image and attaches `podd-sd-<version>.img.gz` (+ the slim variant) and the RAUC
  update bundle to the tag's release. This replaces the currently-`if: false`
  `recovery-sd` stub. The image is our own build, so publishing it is clean.

## Debug channels (no JTAG, no serial console)

This board has **no reachable serial console** (ttymxc3 is only on SoM edge pins
83/85), **no JTAG adapter**, and — confirmed by the owner (2026-07-19) — **no USB
port** (so NXP USB-SDP / `uuu` is not available either). Probing the live device
also confirmed **wired Ethernet is dead** (FEC MAC present but no PHY populated —
`Unable to connect to phy`). So bring-up leans on the channels that *do* exist,
none of which need a debug adapter or a port this board lacks:

- **WiFi + SSH** — the primary feedback channel once Linux is up (as on L1).
- **SD-card iteration with the stock medium as recovery** — the SD-boot path is
  non-destructive: the stock eMMC is never written, so a broken clean-room image
  on a *spare* SD is recovered by swapping the stock card back. This makes even
  blind bootloader bring-up safe: the worst case is "the spare SD doesn't boot."
- **Self-logging diag partition** — boot logs (dmesg/journal/status) written to
  the persistent partition, read post-mortem in a host card reader. Covers the
  pre-network window (`install/diag/`).
- **LED boot-progress codes** — the IS31FL3194 LED is on I²C and drivable from
  both U-Boot and Linux. Patch coarse "reached stage N" blink codes into U-Boot /
  early init to localize a blind bootloader failure without a console.
- **U-Boot-to-SD post-mortem** — once U-Boot proper is running (DRAM + MMC up) it
  can write a marker/log to a FAT partition on the SD before it fails, read back
  in a card reader. Covers the U-Boot-proper window that LED codes can only hint at.

With no console, JTAG, or USB, the bootloader phase has a **single recovery — the
spare-SD swap** — so it is designed to be un-brickable rather than debuggable:

- **The fatal, invisible zone (SPL + DDR training) is exactly what we do NOT
  change.** It is Variscite's DDR config for this exact SoM, used unmodified. All
  our changes live in U-Boot proper / device tree / env, which fail *visibly*
  (LED / SD-log) or merely *don't boot the spare SD*.
- **eMMC is never written during bring-up.** Every iteration is on a spare SD;
  the stock card (and untouched stock eMMC) is the instant, total revert.
- **The from-source bootloader is validated by A/B comparison**, not
  introspection: build `imx-boot` from Variscite DART source with minimal changes,
  `dd` to a spare SD, and check whether it boots the *same* kernel the stock
  bootloader does. It either comes up on WiFi/writes logs, or it doesn't and you
  swap back.

**How it was actually validated (2026-07-19/20):** U-Boot and the kernel are
built from Variscite's `imx_v2020.04_5.4.70_2.3.0_var01` tree — the exact
branch the stock bootloader banner names — and the stock SD dump was used as a
byte-level *reference* (never as an ingredient): the assembled `imx-boot`'s
IVT offset, SPL entry, DDR-firmware block, and dual-DTB FIT were compared
against the dump before ever powering hardware, then live-booted. The two bugs
that made the *first* from-source attempt completely dead (no console on this
board, so total silence) are exactly the kind this comparison catches: the
generic Buildroot helper appended only the **LPDDR4** training firmware while
this SoM's SPL reads the **DDR4** set at `SPL end + 73728`, and it packed only
the DART control DTB while SPL's board-match wants
`imx8mm-var-som-symphony`.

## Bring-up phases

Ordered so the risky blind step (from-source bootloader) came **last**, on top
of an already-proven upper stack:

1. ✅ **Kernel + rootfs on a known-good bootloader** (spare SD): our Buildroot
   kernel + DTS + rootfs, booted by a working bootloader, brings up eMMC/SD, the
   two MCU UARTs (`ttymxc0`/`ttymxc2`), I²C (PMIC, RTC, LED, GPIO expander), and
   WiFi. Validated over SSH + the diag partition (via the interim "stockboot"
   image, since retired).
2. ✅ **podd on the clean rootfs**: podd + UI + NetworkManager + sshd + muzzle;
   podd drives the MCUs live — both sides tested on/off + setpoints, water
   temps converge (2026-07-20).
3. ✅ **From-source boot chain** (spare SD, stock card = recovery): SPL + ATF +
   U-Boot from the Variscite tree → `imx-boot`, validated byte-level against
   the stock dump, then live-booted (2026-07-20).
4. **RAUC A/B**: two slots, signed bundle, install-to-inactive + boot flip +
   bootcount rollback, proven by deliberately shipping a broken slot.
5. **CI publish**: reproducible SD image + update bundle attached to a release.

## Bring-up field notes (pitfalls that cost real debugging time)

- **WiFi driver must be `=m`, not `=y`.** Built-in, brcmfmac probes the SDIO
  bus ~80ms *before* the rootfs (which holds the firmware) mounts; the firmware
  load fails with ENOENT, never retries, and wlan0 never exists. As a module,
  udev coldplug loads it post-mount. `post-build.sh` hard-fails the build if
  `brcmfmac.ko` is missing from the target (the stale-incremental-build trap
  that originally motivated `=y`).
- **Buildroot tzdata is `BR2_TARGET_TZ_INFO`, not `BR2_PACKAGE_TZDATA`** (the
  latter is a blind Kconfig symbol that is silently dropped) — and even with
  zoneinfo installed, **jiff won't follow Buildroot's symlinked top-level zone
  dirs** (`America -> posix/America`), nor fall back to its bundled tzdb while
  a system zoneinfo dir exists. Net effect: podd crash-loops parsing any
  `timezone:` in config.ron. `post-build.sh` hardlinks the trees into place.
- **systemd-networkd must be masked including its sockets**, or it fights
  NetworkManager for wlan0 / spams the journal.
- **`RequiresMountsFor` belongs in `[Unit]`**, not `[Service]` — systemd
  silently ignores it there and podd raced the `/data` mount.
- **The frozen MCU drops any command frame containing `0x7E`** (the LSP frame
  delimiter — no byte-stuffing exists). See `FrozenTarget::delimiter_safe` in
  `pod-proto`; this was the long-standing "left side won't heat at 88°F" bug.
- **The Pod 4 sensor MCU intermittently goes silent** (~60s after connect, not
  deterministic on host traffic) and, when hard-wedged, only the PCAL6416A
  reset pulse revives it. podd's sensor supervisor retries in-process (frozen
  keeps holding temperature) and escalates to a process restart — which pulses
  the reset — after 6 straight failures. Root cause is an open RE item
  (`docs/research/pod4-sensor-protocol.md`).

## Parked / out of scope

- STM32 MCU firmware rewrite (kept as Eight's on-chip; tier-3 reflash still uses
  the `.bbin` blobs and is not clean-room).
- The MediaTek and i.MX-no-SD (Pod 4) hubs — different boot chains; this targets
  the analyzed i.MX8MM Variscite "SD" hub first.
- Bootloader OTA (tier 0) stays manual — too brick-prone to auto-update.
