# Clean-room OS image (L2) — architecture

> **Status: decided, bring-up not complete.** This supersedes the L1 "bolt podd
> onto Eight's Yocto rootfs" model (`scripts/build-podd-sd.sh`) as the target
> architecture. The L1 SD image still exists and boots today; it stays as a
> working fallback until the L2 image below boots on hardware. See
> [ARCHITECTURE.md](ARCHITECTURE.md) for the userland (podd) and
> [UPDATING.md](UPDATING.md) for the current (L1) update agent this replaces.

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

## Bring-up phases

Because **there is no reachable serial console on the New-Rat 0.8 board** (ttymxc3
is only on SoM edge pins 83/85), each phase is validated via the self-logging
diag (boot logs to the data partition, read post-mortem in a host card reader)
and JTAG/OpenOCD — the same constraint the L1 SD-boot path already lives with.

1. **Boot chain**: SPL + ATF + U-Boot from source → `imx-boot` that reaches a
   U-Boot prompt / autoboots. Validate over JTAG.
2. **Kernel + DTB**: our device tree brings up eMMC/SD, the two MCU UARTs
   (`ttymxc0`/`ttymxc2`), I²C (LED, GPIO expander, RTC), WiFi (brcmfmac). Boots
   to a Buildroot shell; diag logs land on the data partition.
3. **Rootfs**: podd + UI + NetworkManager + sshd + muzzle running; podd drives
   the MCUs (dry-run first, then live) exactly as it does on L1 today.
4. **RAUC A/B**: two slots, signed bundle, install-to-inactive + boot flip +
   bootcount rollback proven by deliberately shipping a broken slot.
5. **CI publish**: reproducible SD image + update bundle attached to a release.

## Parked / out of scope

- STM32 MCU firmware rewrite (kept as Eight's on-chip; tier-3 reflash still uses
  the `.bbin` blobs and is not clean-room).
- The MediaTek and i.MX-no-SD (Pod 4) hubs — different boot chains; this targets
  the analyzed i.MX8MM Variscite "SD" hub first.
- Bootloader OTA (tier 0) stays manual — too brick-prone to auto-update.
