# Clean-room OS image (L2) — architecture

> **Status: proven on hardware.** The clean-room image (`os/scripts/build.sh` →
> `dist/podd-sd.img.gz`) boots the Pod 3-SD hub with **zero Eight Sleep
> binaries**: from-source `imx-boot` (Variscite SPL/U-Boot + ATF + NXP DDR fw),
> the 5.4 kernel + our DTB, and the Buildroot rootfs. Verified live: WiFi joins,
> sshd answers on 8822, podd drives both bed sides to their configured
> setpoints through the frozen MCU, sensor telemetry streams, and the
> free-sleep-compat API/UI serves on :3000. A/B slot switching + auto-rollback
> are wired (hand-rolled U-Boot env state machine + pod-update-agent — see
> "Install = OTA" below; **the full update cycle is not yet verified on
> hardware**).
> This supersedes the L1 "bolt podd onto Eight's Yocto rootfs" image; its boot
> flow is still the reference for the SD-swap model ([SD-BOOT.md](SD-BOOT.md)).
> See [ARCHITECTURE.md](ARCHITECTURE.md) for the userland (podd) and "Bring-up
> field notes" below for the pitfalls.

## Why this exists

The L1 image is a clone of the stock eMMC rootfs — Eight Sleep's `imx-boot`,
U-Boot, Variscite BSP kernel/DTB, and entire Yocto userland — with podd dropped
into `/opt/podd` and the vendor services masked. That costs two things:

- **Updates can only reach `/opt/podd`.** Everything else is opaque vendor
  binaries we can't rebuild, so a kernel/rootfs update is impossible and OTA is
  confined to swapping a squashfs under `/opt/podd`.
- **The image can't be published.** It's Eight's copyrighted OS, assembled from
  a specific owner's device dump (personal WiFi creds, the unit's serial). CI
  cannot legitimately attach it to a release.

Building the whole bootable system from source fixes both: every byte is ours to
rebuild, updates can replace the whole image, and CI can publish it.

## Decisions

| Question | Decision | Rationale |
|---|---|---|
| Clean-room scope | **OS image only** | Build bootloader + kernel + rootfs + podd from source. Eight's STM32 firmware keeps running *on the MCU chips* (it's on their own flash, not in the image; podd talks to it over UART). Rewriting the safety-critical STM32 thermal/sensor firmware is a separate, months-long, out-of-scope project. |
| Rootfs builder | **Buildroot** (`BR2_EXTERNAL` tree under `os/`) | Minimal, reproducible, from-source appliance rootfs. Smallest surface for a headless device; strong i.MX8MM support. Nix stays a build tool for the `podd` binary only, not the on-device runtime. |
| OTA engine | **pod-update-agent** (hand-rolled A/B) | RAUC was evaluated and dropped: it could not resolve its slot devices (by-partlabel on MBR), had no keyring, and would have added a second (X.509) signing system beside podup's ed25519 manifests. pod-update-agent streams the release's OS image onto the inactive slot with readback verification, and a bootcount state machine scripted in the U-Boot env (`uboot-env.txt` owns the semantics) provides trial boots + auto-rollback; podd marks-good after a healthy boot. One updater, one trust policy, for app and OS alike. |

## The clean-room boundary

The bar is "no Eight Sleep binaries," which is achievable. "Zero binary blobs of
any kind" is not achievable on this SoC:

- **The i.MX8M boot chain needs NXP's firmware blobs** — DDR PHY training
  (`lpddr4_pmu_train_*.bin`) and HDMI/DP firmware, fetched by Buildroot from
  NXP's `firmware-imx` release. They are NXP's, not Eight's, and every i.MX8M
  board (including fully mainline ones) ships them — the SoC cannot boot
  without the DDR training firmware.
  - **License terms** (`LA_OPT_NXP_Software_License`): redistribution is
    permitted *only embedded within an "Authorized System"* — hardware built
    around an NXP part. A firmware image for the i.MX8MM Pod qualifies, so
    publishing it from CI is within the license (the same basis Variscite,
    Yocto/meta-freescale, and Buildroot ship i.MX images on). NXP's copyright
    notice must ship with it (`make legal-info` emits this), and it stays a
    proprietary component — it cannot be relicensed GPL.
- **The STM32 MCU firmware stays Eight's** — but it lives on the microcontrollers'
  own flash, not in this image. podd speaks the (reverse-engineered) UART
  protocol to it. This only matters if you want to reflash the MCUs from
  clean-room source, which is the out-of-scope STM32-rewrite project.

Net: the published image is *our GPL code + one NXP proprietary boot blob we're
licensed to ship for this hardware + zero Eight Sleep code*. That is "no Eight
blobs," not "100% FOSS."

## Component stack

```
  boot ROM (SoC, immutable)
    └─ SPL            ← U-Boot, from source
        └─ ATF (BL31) ← ARM Trusted Firmware, from source
            └─ U-Boot ← from source; A/B bootcount state machine in the env
                └─ Linux kernel + our DTB ← from source (Variscite BSP 5.4.x or mainline i.MX8MM)
                    └─ Buildroot rootfs (systemd or minimal init)
                        ├─ podd  (static aarch64-musl, from the Cargo workspace)
                        ├─ podd web UI (from ui/)
                        ├─ fw_printenv/fw_setenv (libubootenv; A/B env access)
                        └─ NetworkManager, sshd, LAN-only inbound firewall, …
```

- **`imx-boot`** is assembled by `imx-mkimage` (open) from SPL + U-Boot + ATF +
  the NXP DDR/HDMI blobs. Written raw at offset `0x8400`. No secure boot is
  enforced on these units (unsigned SPL runs), so a from-source boot chain works.
- **U-Boot env** at `0x400000` carries the A/B slot selection (`mmcpart`) and
  the hand-rolled bootcount/rollback state machine — semantics owned by the
  comment block in `os/board/eightsleep/imx8mm-varsom/uboot-env.txt`.
- **SoM = Variscite VAR-SOM-MX8M-MINI (DDR4), *not* a DART** — despite the live
  DTB's `compatible = "variscite,dart-mx8mm"` (Variscite's single Yocto MACHINE
  covers both SoMs; the U-Boot env's `board_name=VAR-SOM-MX8M-MINI` and the SOM
  EEPROM are authoritative). The DART is LPDDR4, the VAR-SOM is **DDR4**, and
  Variscite's single U-Boot build runtime-selects the DDR timing and control
  DTB from the SOM EEPROM — so `imx-boot` must append both DDR training
  firmware sets and pack both control DTBs in the FIT (see
  `os/board/eightsleep/imx8mm-varsom/post-image.sh`). Console is UART4
  (`ttymxc3`, not broken out). MCU UARTs: UART1 `ttymxc0` = Sensor, UART3
  `ttymxc2` = Frozen.
- **podd** is cross-compiled by the existing `nix build .#podd-aarch64` and
  installed into the rootfs as a Buildroot package.

## Hardware watchdog

The SoC watchdog (`imx2-wdt` @ `0x30280000`, `CONFIG_IMX2_WDT=y`) probes on
every boot and exposes `/dev/watchdog`, but nothing opened it until issue #172:
`Restart=always` on `podd.service` covers a podd crash and does nothing for a
kernel or PID-1 hang, which rode out — heaters still holding their last
setpoint — until the Pod was physically power-cycled.

The base layer is systemd's own watchdog support, turned on by the overlay
drop-in `os/board/eightsleep/imx8mm-varsom/rootfs-overlay/etc/systemd/system.conf.d/10-watchdog.conf`
(that file owns the exact values and the reasoning for each). PID 1 opens
`/dev/watchdog`, programs a 30 s hardware timeout and kicks it every 15 s; a
hang longer than that resets the SoC. The drop-in is part of the rootfs, so it
ships to fresh SD images and to existing installs through the ordinary OS OTA
(`os-<version>.ext4.zst` is the whole slot filesystem — see
[Install = OTA](#install--ota)) with no migration step.

Why an unscheduled reboot is safe here — it lands inside invariants that
already exist:

- podd comes back through `podd.service` (`Restart=always`) and re-enters the
  ~60 s sensor-MCU zombie window exactly as it does after any restart, so the
  actuation path's retry-until-confirmed rule covers it (`.claude/rules/actuation-safety.md`).
- Alarms do not arm before NTP sync, and there is no RTC battery, so a reboot
  cannot resurrect a stale alarm time.
- The heat setpoint does not survive as a latch: podd hard-resets both STM32s
  through the PCAL6416A expander during startup (`crates/podd-core/src/reset.rs`,
  called from `podd_core::run`) and then drives the target from the schedule —
  the same sequence as any other restart.
- The U-Boot A/B state machine counts boot attempts, so a watchdog reboot loop
  on a bad slot reverts to the previous one instead of cycling forever.

Not part of this layer, deliberately: `WatchdogSec=` + `sd_notify` supervision
on `podd.service`. That would make podd's own liveness a reboot trigger, and a
spurious restart costs a 60 s actuation-deaf window — a separate decision from
"the kernel must not be allowed to hang".

## Slot & partition layout

Two rootfs slots plus a persistent data partition. Kernel + DTB live inside
each rootfs (`/boot`) and U-Boot loads them from the selected slot, so each
slot is self-contained and an update swaps one atomic image.

**SD image (the publishable, first-class install):**

| Region | Contents |
|---|---|
| `0x8400` (raw) | `imx-boot` |
| `0x400000` (raw) | U-Boot env (`mmcdev=1`, `mmcpart` slot select + rollback state) |
| p1 | rootfs **A** (ext4, incl. `/boot`) |
| p2 | rootfs **B** (ext4, incl. `/boot`) |
| p3 | persistent data (config, schedules, logs) |

Boot from this SD leaves the eMMC untouched — swapping the stock card back is
an instant, total revert. OS updates write the inactive SD slot.

**eMMC install (later):** same layout mapped onto the eMMC A/B slots
(`rootfs_a`=p1 / `rootfs_b`=p2 / `cage`=p3), written to the inactive slot from a
running system, boot pointer flipped with `fw_setenv`. The stock slot survives as
the revert until you choose to overwrite it.

## Install = OTA

There is no separate "installer that modifies an existing rootfs." Both flows
write a complete signed image to an inactive slot:

- **First install:** `dd` the published SD image to a card and boot it, or
  write the eMMC inactive slot from a running system and flip the pointer.
- **Update:** `pod-update-agent` fetches the release's `os-<version>.ext4.zst`
  (manifest sha256 + optional ed25519, like every artifact), streams it onto
  the inactive slot, verifies the write by readback, and arms the U-Boot trial
  (`upgrade_available=1 bootcount=0 ustate=1` + the `mmcpart` flip). The next
  reboot boots the new slot with U-Boot counting attempts — 3 failures
  auto-revert to the old slot — and a healthy podd disarms the env
  (mark-good). See [UPDATING.md](UPDATING.md) for the owner-facing flow.

One trust policy covers everything: the same `pod-update` manifest (offline
Ed25519 owner key, integrity-always / signature-optional) carries the app
squashfs and the OS image; there is no second bundle format or cert system.
The `/opt/podd` symlink-swap remains the app tier's no-reboot update path.

**Unverified:** the A/B cycle is code-complete but has never run on hardware.
Proving it means deliberately shipping a broken slot and confirming the
bootcount rollback fires.

## Build system & publishing

- The Buildroot external tree lives under **`os/`** (`BR2_EXTERNAL`): board
  defconfig, U-Boot/ATF/kernel config fragments, our DTB, the U-Boot env
  (incl. the A/B state machine), a `genimage` layout for the A/B partitions,
  and post-build/post-image scripts that assemble `imx-boot`, the final
  `.img`, the OTA slot artifact (`podd-os.ext4.zst`) and the slot-install
  tarball (`podd-rootfs.tar.gz` — same rootfs, for the consumers that extract
  instead of `dd`; see [`os/README.md`](../os/README.md)).
- **Publishing**: releases are cut by CI on a `v*` tag; the OS-level artifacts
  (`podd-sd-<tag>.img.gz`, `os-<version>.ext4.zst`, `podd-rootfs-<tag>.tar.gz`,
  `podd-recovery-sd-<tag>.img.gz`) come from the release workflow's `os-image`
  job — see [RELEASING.md](RELEASING.md#the-os-image-lane). Everything is our
  own build, so publishing it is clean.

## Debug channels (no JTAG, no serial console)

This board has no reachable serial console (ttymxc3 is only on SoM edge pins
83/85), no JTAG adapter, and no USB port, so NXP USB-SDP / `uuu` is unavailable.
Wired Ethernet is also dead — FEC MAC present, no PHY populated (`Unable to
connect to phy`). Bring-up uses the channels that remain:

- **WiFi + SSH** — the primary feedback channel once Linux is up.
- **SD-card iteration with the stock medium as recovery** — the SD-boot path is
  non-destructive: the stock eMMC is never written, so a broken clean-room image
  on a spare SD is recovered by swapping the stock card back. This makes blind
  bootloader bring-up safe — the worst case is a spare SD that does not boot.
- **Self-logging diag partition** — boot logs (dmesg/journal/status) written to
  the persistent partition, read post-mortem in a host card reader. Covers the
  pre-network window, and is baked into every clean-room build:
  `os/board/eightsleep/imx8mm-varsom/rootfs-overlay/usr/bin/podd-bootlog` plus
  its early/late units write to `/data/bootlog` (partition p3, label
  `podd_data`).
- **LED boot-progress codes** — the IS31FL3194 LED is on I²C and drivable from
  both U-Boot and Linux. Patch coarse "reached stage N" blink codes into U-Boot /
  early init to localize a blind bootloader failure without a console. The
  reference sequence for a healthy *stock* boot, observed on real hardware:
  steady LED at power-on → off → green → blue.
- **U-Boot-to-SD post-mortem** — once U-Boot proper is running (DRAM + MMC up) it
  can write a marker/log to a FAT partition on the SD before it fails, read back
  in a card reader. Covers the U-Boot-proper window that LED codes can only hint at.

With no console, JTAG, or USB, the bootloader phase has a single recovery — the
spare-SD swap. It is therefore designed to be un-brickable rather than debuggable:

- **The silent-failure zone (SPL + DDR training) is what we do not change.**
  It is Variscite's DDR config for this exact SoM, used unmodified. All our
  changes live in U-Boot proper / device tree / env, which fail visibly
  (LED / SD-log) or simply do not boot the spare SD.
- **eMMC is never written during bring-up.** Every iteration is on a spare SD;
  the stock card (and untouched stock eMMC) is the instant, total revert.
- **The from-source bootloader is validated by comparison, not introspection.**
  Build `imx-boot` from Variscite's `imx_v2020.04_5.4.70_2.3.0_var01` tree (the
  branch the stock bootloader banner names), then byte-check it against the
  stock SD dump *before* powering hardware — the dump is a reference, never an
  ingredient. Compare the IVT offset, SPL entry, DDR-firmware block, and
  dual-DTB FIT. That catches the two failure modes that leave the board
  completely silent: appending only the **LPDDR4** training firmware when this
  SoM's SPL reads the **DDR4** set at `SPL end + 73728`, and packing only the
  DART control DTB when SPL's board-match wants `imx8mm-var-som-symphony`.
  Then `dd` to a spare SD and check whether it boots the same kernel the stock
  bootloader does: it comes up on WiFi and writes logs, or you swap back.

## Bring-up field notes

- **WiFi driver must be `=m`, not `=y`.** Built-in, brcmfmac probes the SDIO
  bus ~80ms before the rootfs (which holds the firmware) mounts; the firmware
  load fails with ENOENT, never retries, and wlan0 never exists. As a module,
  udev coldplug loads it post-mount. `post-build.sh` hard-fails the build if
  `brcmfmac.ko` is missing from the target, since a stale incremental build can
  silently drop it. The module also needs power sequencing the
  base DART/VAR-SOM dtsi doesn't provide, and its firmware/NVRAM blobs aren't
  in upstream `linux-firmware` — see the WiFi power/firmware comment block in
  `os/board/eightsleep/imx8mm-varsom/imx8mm-podd.dts` (~lines 126-197).
- **Bootloader-region splice, for bisecting boot-chain vs. rootfs failures.**
  `imx-boot` occupies raw sectors 66–8191 (bytes `0x8400` up to, but not
  including, the U-Boot env at `0x400000`/sector 8192). Copying just that
  range from a known-good image into a candidate image isolates whether a dead
  board is the boot chain or something downstream (env/rootfs), without
  touching either:
  ```
  dd if=<known-good>.img of=<target>.img bs=512 skip=66 seek=66 count=8126 conv=notrunc
  ```
- **Buildroot tzdata is `BR2_TARGET_TZ_INFO`, not `BR2_PACKAGE_TZDATA`** (the
  latter is a blind Kconfig symbol that is silently dropped) — and even with
  zoneinfo installed, jiff will not follow Buildroot's symlinked top-level zone
  dirs (`America -> posix/America`), nor fall back to its bundled tzdb while
  a system zoneinfo dir exists. Net effect: podd crash-loops parsing any
  `timezone:` in config.ron. `post-build.sh` hardlinks the trees into place.
- **systemd-networkd must be masked including its sockets**, or it fights
  NetworkManager for wlan0 / spams the journal.
- **`RequiresMountsFor` belongs in `[Unit]`**, not `[Service]` — systemd
  silently ignores it there and podd races the `/data` mount.
- **The frozen MCU drops any command frame containing `0x7E`** (the LSP frame
  delimiter — no byte-stuffing exists). See `FrozenTarget::delimiter_safe` in
  `pod-proto`.
- **The Pod 4 sensor MCU intermittently goes silent** (~60s after connect, not
  deterministic on host traffic) and, when hard-wedged, only the PCAL6416A
  reset pulse revives it. podd's sensor supervisor retries in-process (frozen
  keeps holding temperature) and escalates to a process restart — which pulses
  the reset — after 6 straight failures. Root cause is an open RE item
  (`docs/research/pod4-sensor-protocol.md`).
- **Sensor-MCU zombie window on every (re)connect, not just hard-wedges.**
  After podd's startup reset pulse, the G0 sensor MCU streams telemetry and
  answers Ping normally but silently discards alarm/actuation writes (`SetAlarm`)
  for tens of seconds — observed live: fires attempted <25s after connect never
  start, while fires attempted >60s in do work. One-shot writes are never
  sufficient for anything alarm-related. Alarm-critical commands (manual test fires,
  dismissals) become a pending write the scheduler resends every 2s, up to 60s,
  until the firmware confirms — see `PendingFire`/the resend loop in
  `crates/podd-core/src/sensor/manager.rs`.
- **The G0 firmware dismisses alarms on its own double-tap detection**
  (accelerometer in the puck; logs `FW: <millis> [lisR] dismissing alarm (2
  taps)`). podd parses that message and marks the side dismissed for the rest
  of the window — without it, podd re-arms the alarm ~5s later.

## Parked / out of scope

- STM32 MCU firmware rewrite (kept as Eight's on-chip; tier-3 reflash still uses
  the `.bbin` blobs and is not clean-room).
- The MediaTek and i.MX-no-SD (Pod 4) hubs — different boot chains; this targets
  the analyzed i.MX8MM Variscite "SD" hub first.
- Bootloader OTA (tier 0) stays manual — too brick-prone to auto-update.
