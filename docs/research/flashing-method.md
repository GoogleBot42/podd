# podd — Definitive Firmware Flashing Method (all Pod variants)

**Date:** 2026-07-17. **Author:** analysis + web-research + design pass for the `podd` project.
**Scope:** how to install `podd` onto every Pod variant, get root/access, and recover/unbrick — with a
concrete spec for the installer and CI to build.

> **CORRECTION (2026-07-18):** This document originally grouped "Pod3-SD / Pod4" as one i.MX family
> that both have an SD card. That is wrong. What matters is the **HUB**, not the app's `coverVersion`
> (which describes the *cover*). The reference device analyzed here is a **Pod 3 "SD" hub** (i.MX8MM
> Variscite "New-Rat", `mmcblk1` = a real SD on uSDHC2, `mmcblk2` = eMMC, U-Boot env on the SD) that
> happens to have a **Pod 4 *cover*** attached. A genuine **Pod 4 *hub* has NO SD card** (all-eMMC,
> env on eMMC), so the **recovery-SD auto-installer (§5) does NOT apply to a Pod 4 hub** — it's
> specific to the Pod 3 SD hub. For a Pod 4 hub use the serial-root + in-band eMMC A/B install, with
> USB `uuu`/SDP as the deep unbrick. Everything below that is labeled "CONFIRMED-DUMP" is the **Pod 3
> SD hub**; Pod 4 hub and MediaTek specifics remain INFERRED (no dump).

Confidence tags: **[CONFIRMED-DUMP]** proven from the owner's live backups this session; **[CONFIRMED-WEB]**
primary/technical source; **[INFERRED]**; **[UNKNOWN]**.

Variants covered:
- **Pod3-SD / Pod4 (i.MX)** — NXP i.MX8M Mini on Variscite VAR-SOM-MX8M-MINI. *This is the owner's unit and the
  only one we have full dumps for.* Note: the owner's "Pod 4" backup **has a bootable microSD** (`mmcblk1`) and
  is architecturally identical to the SD Pod 3; treat Pod3-SD and this Pod 4 as one i.MX family.
- **Pod3-noSD (MediaTek)** — MT8365 "Genio 350" (OLogic i350 SOM), all-eMMC. No SD. Web/FCC research only.

---

## 0. The single most important discovery this session

**The owner's backups prove that Eight Sleep's stock recovery mechanism *is* a Variscite-style recovery SD that
auto-installs to eMMC.** The full flow was recovered from the SD image (`mmcblk1-sd.img.gz`) and eMMC gold
master (`mmcblk2.img.gz`). This means the "dream one-step flasher" for the i.MX variants is **not speculative —
it already exists in the field and we can clone it with our own payload.** Everything below builds on that.

Evidence chain (all [CONFIRMED-DUMP]):
- The SD (`mmcblk1`) is a full bootable device: **imx-boot container at byte 0x8400** (IVT `d1 00 20 41`),
  second-stage U-Boot IVT at 0x61000, **redundant U-Boot env at 0x400000** (CRC-prefixed, matches
  `fw_printenv`). SD `/etc/issue` banner = `Version:rec-6c57c03-…` → a dedicated **recovery rootfs** on p1.
- The SD p1 recovery rootfs contains **`/opt/images/Yocto/rootfs.tar.gz`** (204 MB eMMC payload) +
  **`/opt/images/Yocto/imx-boot-sd.bin`** (1.17 MB bootloader) + **`/usr/bin/install_yocto.sh`** (the stock
  Variscite eMMC installer) + Eight's **`/opt/eight/bin/factory-reset.sh`** driver, auto-started by
  **`factory-reset.service`** (enabled in `multi-user.target.wants`).
- The eMMC's imx-boot at 0x8400 is **byte-identical (md5 `330e9548…`) to the SD's `imx-boot-sd.bin` payload** —
  proving `install_yocto.sh` really did `dd` the bootloader onto eMMC. eMMC therefore boots standalone too.
- `factory_reset` U-Boot env fragment probes the **I²C button** (bus 1, GPIO expander @0x20, LED ctlr @0x53);
  if held it does `setenv mmcdev 1; setenv mmcblk 1; setenv mmcpart 1; saveenv` → U-Boot then boots the **SD's**
  p1 recovery rootfs instead of eMMC.

---

## 1. Boot / flash architecture (per variant)

### 1a. Pod3-SD / Pod4 (i.MX8M Mini) — [CONFIRMED-DUMP unless noted]

| Thing | Location |
|---|---|
| SPL + U-Boot + ATF + DDR fw (`imx-boot`) | **SD `mmcblk1` @ 0x8400** *and* **eMMC `mmcblk2` @ 0x8400** (both valid) |
| U-Boot environment (2 KB redundant) | **SD `mmcblk1` @ 0x400000**, size 0x1000 (per `/etc/fw_env.config → /dev/mmcblk1 0x400000 0x1000`) |
| eMMC boot0/boot1 hardware partitions | **empty** (all zero) — bootloader lives in the user area, not boot0 |
| rootfs A / rootfs B | eMMC `mmcblk2p1` (label `A`) / `mmcblk2p2` (label `B`), ext4, 6.1 GiB each — **both populated** |
| persistent data (`/cage`, live: `/persistent`) | eMMC `mmcblk2p3` (label `cage`), 444 MiB — holds real data (`alarm.cbr`, RAW/loopback logs): **must preserve** |
| kernel + DTB | `/boot/Image.gz` + `imx8mm-var-som-symphony-eight.dtb` *inside each rootfs slot* (no separate boot part) |
| Partition table (eMMC) | MBR: p1 @sector16384, p2 @14252032, p3 @28487680 (`create_emmc_swupdate_parts` layout) |

**Boot-device selection [CONFIRMED-WEB + INFERRED]:** the i.MX8MM boot ROM has an **SD/SD2 "manufacturing
override": a valid image on the SD is booted irrespective of the boot-mode pins/fuses, overriding eMMC**
(NXP/Kontron/community-documented). On these Pods the runtime env lives on the SD and the whole recovery flow is
SD-driven, so **the SD is effectively the primary boot device; the eMMC imx-boot is the fallback** used if the
SD is absent (then U-Boot finds no env → falls back to its compiled default env; `globals.sh`'s `get_fw_val`
explicitly handles the "Cannot read environment" case). This SD-override is the mechanism that makes a custom
recovery-SD auto-installer feasible. *Residual [UNKNOWN]: whether the SD-MFG override is fused-off on some
production units — cannot be read from an image; but it is provably active on the units we have.*

**A/B + rollback state machine [CONFIRMED-DUMP]** (`globals.sh` + env): slot picked by `mmcpart` (1/2) with
`next_mmcpart`; newer builds also use `current_slot`/`next_slot` (free-sleep reads `current_slot`). `ustate`
state machine: `OK=0, INSTALLED=1, TESTING=2, FAILED=3`. `bootcount`/`bootlimit=3` + `altbootcmd` auto-flip
`mmcpart` to the other slot on 3 failed boots. `set_ustate`: INSTALLED→TESTING on first boot of a new image,
TESTING→OK (`bootcount 0 upgrade_available 0`) once userspace confirms, FAILED→ sets `mmcpart=OTHER falling_back 1`.

**Secure boot: NOT enforced [CONFIRMED, prior `boot-chain.md`].** SPL carries a CSF pointer but the signature
region is all-zero → unsigned SPL runs → HAB is open → custom bootloader/kernel/rootfs all boot. OTA `.swu`
packages *are* RSA-verified (`swupdate -k /etc/swupdate_public.pem`), but that only gates **OTA**, not direct
flashing.

### 1b. Pod3-noSD (MediaTek MT8365) — [INFERRED/CONFIRMED-WEB; no dump]

- Boot chain: **BROM → preloader (in eMMC boot0) → U-Boot → Linux**. Everything on the SoM's onboard Kingston
  eMMC; **no removable SD**. Root dev `mmcblk0`. U-Boot env in eMMC (edited only via running `fw_setenv` or the
  live serial prompt — no offline SD escape hatch).
- Same *userland* A/B scheme (`rootfs_a`/`rootfs_b`, `current_slot`) and Yocto/systemd/SWUpdate stack as i.MX.
- Secure boot / DA-auth eFuse state: **[UNKNOWN]**.

---

## 2. Getting access / root — the one-time hard part

All variants share the **universal serial/U-Boot method** (the only path that works on every variant and is
already proven in the wild by free-sleep). The i.MX variants additionally have the **SD/recovery route**.

### 2a. Universal: serial → U-Boot → root shell [CONFIRMED, free-sleep + owner]

> **CORRECTION (2026-07-19, owner probing of the analyzed board + Variscite SoM
> docs "Table 48: JTAG Header Signals"):** the J7 pinout and 921600 console below
> were later confirmed to apply **only to the MediaTek MT8365 / "i350" board
> (625-00022)** — free-sleep's reference photo is of that board. On the analyzed
> **i.MX8M Mini / Variscite "New-Rat 0.8"** hub the JTAG-footprint header is
> **real JTAG** (pin 1 = JTAG_VREF 3.3 V via 150 Ω, 2 = TMS, 4 = TCK, 6 = TDO,
> 8 = TDI, 9 = TRST_B, 10 = POR_B; 3/5/7 = GND), and the `ttymxc3` console is
> **not broken out to any reachable header** — it exists only on SoM edge pins
> 83/85 (115200 8N1, 3.3 V, both U-Boot and kernel; **not** 921600). So this
> serial method is NOT universal: on that board use the SD-swap path
> (`docs/SD-BOOT.md`) or JTAG. See `docs/FLASHING.md` for the corrected guide.

**Hardware to buy:**
- USB-UART adapter: **FTDI FT232RL** (~$13). *Verify/词 set logic level to **1.8 V*** — the i.MX8MM UART is
  natively 1.8 V [INFERRED]; a 3.3 V FTDI may work but 1.8 V is correct. (Owner captured console traffic fine at
  3.3 V in practice, but 1.8 V is the safe spec.)
- Connection to header **J7** (JTAG-footprint): either a **Tag-Connect TC2070-IDC** (~$50, no soldering) or
  solder 3 wires.
- **Pinout (J7):** **pin 1 = GND, pin 6 = RX, pin 8 = TX**. Console baud = **921600** (note: U-Boot's *own*
  `baudrate` env is 115200 for `ttymxc3`, but the community/owner console runs 921600 — use 921600).

**Steps:**
1. `minicom -b 921600 -o -D /dev/ttyUSB0` (or `screen … 921600`). Power on the Pod.
2. When `Hit any key to stop autoboot` appears, **spam Ctrl-C** (window is short: `bootdelay=1`).
3. At the U-Boot prompt:
   ```
   printenv                                  # confirm mmcdev/mmcpart (or current_slot)
   setenv bootargs "root=PARTLABEL=rootfs_a rootwait init=/bin/bash"   # or root=/dev/mmcblk2p1
   run bootcmd                               # boots straight to a root shell
   ```
   (On MediaTek use `root=PARTLABEL=rootfs_a` / `mmcblk0p1`; on i.MX either PARTLABEL or `/dev/mmcblk2p1`.)
4. In the shell:
   ```
   mount -t proc proc /proc; mount -t sysfs sysfs /sys; mount -t devtmpfs devtmpfs /dev
   mount -t tmpfs tmpfs /run; mount -o remount,rw /
   passwd root; passwd rewt; sync
   ```
5. Reboot normally (do **not** interrupt), log in, then disable Eight's OTA/control stack (so it can't fight
   podd or auto-update you back to stock):
   ```
   systemctl disable --now swupdate swupdate-progress defibrillator \
       dac frank capybara telegraf vector frankenfirmware eight-kernel 2>/dev/null
   ```
   SSH is historically `rewt@<pod>` on **port 8822**.

This is entirely **runtime/non-persistent** (only a rootfs password persists) — nothing in the bootloader is
touched, so this step alone **cannot brick** any variant.

### 2b. i.MX only, solder-free: SD factory-image edit (a.k.a. "ZeroSleep") [CONFIRMED-WEB + DUMP]
Because the SD carries `/opt/images/Yocto/rootfs.tar.gz` (the eMMC payload) and the rear **I²C button** forces a
reflash-from-SD, you can root **without any serial adapter**:
1. Open the Pod, free the glued microSD (heat gun), read it in a host.
2. Append your key to the recovery payload:
   `tar --numeric-owner -rf rootfs.tar ./etc/ssh/authorized_keys` (then re-gzip in place), *or* edit
   `/etc/shadow`/`/etc/passwd` in the payload.
3. Reinsert SD, **hold the rear button next to the power cable while applying power** → factory-reset re-extracts
   the (now back-doored) rootfs onto eMMC slot A. SSH in as `rewt` on 8822.

Trade-off: no electronics, but requires physically freeing the glued SD (more invasive than clipping onto J7).

### 2c. MediaTek only, last resort: mtkclient/BROM (see §4b).

---

## 3. Installing podd — recommended path + fallbacks (ordered by ease)

podd already ships a **signed, atomic, reproducible update system** (`crates/pod-update` + host CLI
`crates/podup`). The OS-level installer should layer on top of that. Three install modes:

### (a) Already-rooted (free-sleep/opensleep users) — *easiest, no new hardware*
They already have SSH + a disabled OTA stack. Just:
```
curl -fsSL https://.../podd-install.sh | sh      # fetch podd bundle, verify signature, install
```
podd runs as a userland payload on Eight's stock Yocto (opensleep/free-sleep deployment model): drop the `podd`
binary + `config.ron` in `/opt/podd`, install `podd.service`, `systemctl enable --now podd`, and (idempotently)
mask the vendor OTA/control units from §2a. **This is the recommended default for existing users** and touches
neither the bootloader nor the inactive slot — instant, reversible, no eMMC re-partition.

### (b) Fresh unit — root first (§2a serial), then run the installer
Same installer as (a), preceded by the one-time serial root. This is the **recommended path for a brand-new
unit** on all variants (it's the only one that works on MediaTek too).

### (c) In-band A/B slot install (the robust, rollback-safe upgrade) — *recommended for L2 full-OS installs*
For users who want podd's own OS image (not just a userland payload), install into the **inactive** eMMC slot and
flip the pointer, keeping stock as instant rollback. This mirrors Eight's own `swupdate`/`globals.sh` flow:
1. From the root shell, detect active slot: `fw_printenv mmcpart` (or `current_slot`). Target = the other slot.
2. Write podd's rootfs to the inactive slot: `mkfs.ext4 -L B /dev/mmcblk2p2` then extract `podd-rootfs.tar.gz`
   (or `dd` a filesystem image) into it, including `/boot/Image.gz` + DTB.
3. Flip the pointer + arm rollback (i.MX, writes to SD env):
   ```
   fw_setenv mmcpart 2 next_mmcpart 1 ustate 1 upgrade_available 1 bootcount 0   # ustate=INSTALLED
   ```
   On first boot podd's boot-confirm sets `ustate=OK` (`bootcount 0 upgrade_available 0`); if it fails to boot 3×,
   `altbootcmd` auto-reverts `mmcpart` to the stock slot → **instant, automatic rollback**. (MediaTek: same, but
   env is on eMMC — use `fw_setenv current_slot b`.)
4. Preserve `mmcblk2p3` (`cage`/`persistent`) untouched.

### (d) Recovery-SD one-step auto-installer (i.MX only — the dream method) — see §5.

---

## 4. Recovery / unbrick (per variant)

### 4a. i.MX (Pod3-SD / Pod4) — four nested safety nets, cheapest first
1. **Serial U-Boot** (always available): re-enter U-Boot, `setenv mmcpart 1` (stock slot), `run bootcmd`. Fixes
   any bad-slot/env situation. This is the primary net.
2. **Rear-button factory reset from the SD** [CONFIRMED-DUMP]: if the SD's recovery rootfs + payload are intact,
   hold the button at power-on → `install_yocto.sh -u` rewrites eMMC (bootloader + A/B + cage) from the SD's
   `rootfs.tar.gz`. Keep an **un-backdoored golden `rootfs.tar.gz`** on the SD as a built-in restore.
3. **Full-disk gold-master restore** (nuclear, needs the SD out or a root shell): `dd` the whole `mmcblk2.img`
   (or SD `mmcblk1.img`) back. Keep the owner's `mmcblk2.img.gz` / `mmcblk1-sd.img.gz` as the reference images.
4. **UUU / SDP over USB-OTG1** [CONFIRMED-WEB at chip level; UNVERIFIED on Pod]: a truly dead bootloader drops
   the ROM to Serial Download Protocol on USB-OTG1; `uuu spl` + `uuu emmc_burn_all` reflash from Variscite's
   public `flash.bin`/`.wic`. **Blocker:** nobody has located the Pod's OTG1 pads on a connector. *Action item
   for a hardware session: find/expose OTG1.* Until then this net is theoretical on the Pod.

### 4b. MediaTek (Pod3-noSD) — [CONFIRMED-WEB tooling; UNVERIFIED on Pod]
- **Primary:** serial U-Boot (as 4a.1), `fw_setenv current_slot a`.
- **Unbrick net:** **mtkclient** (or SP Flash Tool) over the control-board **USB-C (J13)**. Invalid/erased
  preloader → BROM auto-enters USB download; else force BROM (hold key / short a BROM test point at power-on).
  mtkclient can read/write eMMC partitions and repair the preloader. **Prereqs to prove on hardware:** (i) J13 is
  wired to the MT8365 USB, (ii) DA/secure-boot fuses don't require a signed loader (may need `--auth`), (iii)
  capture the partition scatter. Have mtkclient + a full stock eMMC image staged **before** any bootloader-level
  work, since there is no SD fallback.

---

## 5. Recovery-SD one-step auto-installer — design (i.MX only) ✅ FEASIBLE

**This is the definitive answer to the project's big question: YES, an auto-installing recovery SD is feasible
for the i.MX variants — because it is exactly what Eight already ships, and we recovered the entire flow.**

**What the SD contains** (clone of Eight's recovery SD with podd payload):
- `imx-boot` at **0x8400** (reuse Variscite/Eight's `imx-boot-sd.bin`; DDR/ATF blobs are board-specific — reuse
  them verbatim).
- A **redundant U-Boot env at 0x400000**, pre-set so the SD boots its *own* installer rootfs by default
  (`mmcdev=1 mmcpart=1`), inheriting Eight's `factory_reset`/`altbootcmd`/`bootcount` logic.
- **p1 = a minimal installer rootfs** (fork of the `rec-…` recovery rootfs) containing `install_yocto.sh` +
  an Eight-style `factory-reset.service`→`podd-install.sh` auto-runner.
- **p1:/opt/images/Yocto/** = podd's **`rootfs.tar.gz`** (the eMMC image payload) + **`imx-boot-sd.bin`**.

**How it flashes + sets the boot pointer** (podd-install.sh, modeled on the recovered `factory-reset.sh`):
1. Boot the SD installer rootfs (either default env, or the user holds the button — either works).
2. `install_yocto.sh -u`: umount + wipe eMMC first 8 MiB; create A/B+cage; `mkfs.ext4 -L A/-L B/-L cage`;
   **`dd imx-boot-sd.bin → /dev/mmcblk2 seek=33 (KiB)`**; extract podd `rootfs.tar.gz` → eMMC slot A. *(Option to
   write podd only to the **inactive** slot and preserve the stock other slot for rollback — see §3c — instead of
   the default full A/B wipe.)*
3. `e2fsck` A + cage; preserve/keep `cage`.
4. **Flip the boot pointer:** `fw_setenv mmcdev 2 mmcblk 2 mmcpart 1 mmcautodetect no ustate 0 bootcount 0`
   (writes to SD env @ 0x400000) → next boot runs eMMC.
5. `reboot`. User may **leave the SD in** (it remains the bootloader + env + permanent recovery rootfs — strictly
   better than stock) or leave the golden stock SD.

**The one honest catch:** reaching the microSD slot requires **opening the Pod and freeing the glued SD** (heat
gun), same disassembly cost as attaching serial. So "insert SD, power on, done" is true *once you can physically
reach the slot*. It is not a no-disassembly consumer flow. For units where the slot is accessible, this is the
easiest and most robust method; for everyone else, serial (§2a) is less invasive.

*There is no SD path on MediaTek Pod3-noSD or on any i.MX unit whose SD-MFG override is fused off — those fall
back to serial + mtkclient/UUU.*

---

## 6. What the installer must do & what CI must build

### 6a. Installer scripts (implement these)
- **`podd-install.sh` (userland mode, §3a/b):** verify signed podd bundle (reuse `pod-update` verification) →
  stop/mask vendor units → install binary + `config.ron` + `podd.service` → enable. Idempotent; re-runnable.
- **`podd-slot-install.sh` (in-band A/B, §3c):** detect active slot → mkfs + extract podd rootfs to inactive
  slot → `fw_setenv` flip with `ustate=INSTALLED` → reboot; includes a boot-confirm hook that sets `ustate=OK`.
- **`podd-install.sh` on the recovery SD (§5):** the `factory-reset.sh` clone driving `install_yocto.sh -u` +
  env flip + reboot.
- **Common:** always back up first (`dd` eMMC boot region + partition table off-box; export `fw_printenv`),
  never touch `mmcblk2p3`/`cage` unless asked, keep the stock slot pristine when doing A/B.

### 6b. CI artifacts (build all of these)
1. **podd userland bundle** — cross-compiled `aarch64-unknown-linux-musl` `podd` binary + `podd.service` +
   default `config.pod3/pod4.ron`, wrapped in a **signed `pod-update` manifest** (podup already does this). This
   is the primary deliverable and covers §3a/b.
2. **podd eMMC `rootfs.tar.gz`** (for §3c and §5) — a Yocto/Buildroot/Nix aarch64 rootfs that **reuses the stock
   DTB `imx8mm-var-som-symphony-eight.dtb`, DDR/ATF blobs, and `imx-boot`** (board bring-up is non-negotiable),
   with podd preinstalled and the vendor OTA stack removed.
3. **`imx-boot-sd.bin`** — reuse the stock/Variscite bootloader (do not rebuild unless needed; it's unsigned and
   boots fine).
4. **Recovery-SD image `podd-recovery-sd.img.gz`** (§5) — a full bootable `.img`: MBR + imx-boot@0x8400 +
   env@0x400000 (pre-set) + p1 installer rootfs carrying (2)+(3). `dd`-writable by end users.
5. **MediaTek: full stock+podd eMMC image + an `mtkclient` flash script + scatter** (once captured on hardware) —
   for §4b unbrick and fresh flash of the no-SD variant.
6. **Reference stock images** checked into release assets: the owner's `mmcblk2.img.gz` (eMMC) and
   `mmcblk1-sd.img.gz` (SD) as golden restore points.

---

## 7. Recommendation — single easiest supported path per variant

| Variant | Recommended primary | Fallback / unbrick |
|---|---|---|
| **Pod3-SD / Pod4 (i.MX), already rooted** | §3a userland bundle over SSH (no hardware) | serial U-Boot slot revert |
| **Pod3-SD / Pod4 (i.MX), fresh** | §2a serial root → §3a/b installer | rear-button SD factory reset; full-disk `dd`; UUU/SDP |
| **Pod3-SD / Pod4 (i.MX), max robustness** | §5 recovery SD (own the bootloader+env+recovery) | everything above |
| **Pod3-noSD (MediaTek), fresh** | §2a serial root → §3a/b installer | mtkclient/SP Flash Tool over J13 (stage first) |

**One-line verdict:** ship (1) the signed userland bundle as the default for everyone, (2) the serial-root guide
as the universal one-time unlock, and (3) the podd recovery-SD as the robust/unbrick option for i.MX — it is
proven feasible because it clones Eight's own recovery SD.

---

## 8. What still can't be made easy + residual UNKNOWNS

- **The first unlock always needs physical entry** — open the Pod and either clip serial onto J7 or free the
  glued microSD. No purely-software remote jailbreak exists (nor should podd rely on one). This is the
  irreducible hard step.
- **UART logic level** — spec is 1.8 V; confirm before wiring a 3.3 V FTDI. [minor UNKNOWN]
- **i.MX SD-MFG override fuse state** — provably active on the units we have, but could be fused-off on some
  production runs; a fused-off unit loses the recovery-SD path (serial still works). [UNKNOWN, needs hardware]
- **USB-OTG1 pads on the i.MX carrier** — needed for the UUU/SDP bottom-of-stack unbrick; location undocumented.
  [UNKNOWN, needs hardware — high-value to find]
- **MediaTek (Pod3-noSD) everything below userland** — exact partition/scatter, whether J13 is wired to MT8365
  USB, DA/secure-boot fuse lock (may need `--auth` or a patched DA). No dump exists; all [UNKNOWN] — needs a
  physical MediaTek unit + mtkclient session.
- **Pod 4/5 control-plane RE** — a *shell* is solved and podd's OS install works; but opensleep-style direct MCU
  control on Pod 4/5 needs new sensor/thermal MCU protocol RE (out of scope for flashing, flagged in `pod4.md`).

---

## Sources
- **Owner backups (primary, this session):** `mmcblk1-sd.img.gz`, `mmcblk2.img.gz`, `fw_printenv.txt`,
  `mmcblk2-parttable.txt`, `mmcblk2boot0.img.gz`, `sd-uboot-env-0x400000.bin.gz` — analyzed via `debugfs`/`dd`.
  Recovered files: `install_yocto.sh`, `/opt/eight/bin/{factory-reset.sh,factory-reset-i2c.sh,globals.sh,
  swupdate.sh,defibrillator.sh}`, `factory-reset.service`, `/etc/fw_env.config`, `/etc/swupdate.cfg`.
- Prior session reports: `boot-chain.md`, `pod3-nosd.md`, `pod4.md`, `generic-flash-recovery.md`,
  `connectivity-and-diff.md`.
- free-sleep: https://github.com/throwaway31265/free-sleep ; INSTALLATION.md ; docs/jtag/
- opensleep: https://github.com/LiamSnow/opensleep ; ninesleep: https://github.com/bobobo1618/ninesleep
- ZeroSleep: https://blopker.com/writing/04-zerosleep-1/ ; Adam Schaal: https://blog.adamschaal.com/posts/2025-12-16-rooting-eight-sleep/
- i.MX8MM boot / SD-MFG override / SDP: https://community.nxp.com/t5/i-MX-Processors-Knowledge-Base/i-MX8-Boot-process-and-creating-a-bootable-image/ta-p/1101253 ;
  https://docs.kontron-electronics.de/sw/yocto/build-ktn-imx/usage-mx8mm.html ; https://trac.gateworks.com/wiki/venice/SDP ;
  https://github.com/nxp-imx/mfgtools
- Variscite recovery SD / install_yocto.sh: https://variwiki.com/index.php?title=Yocto_Recovery_SD_card ;
  https://dev.variscite.com/dart-mx8m-mini/…/imx-uuu/ ; https://github.com/varigit/uboot-imx
- MediaTek MT8365/Genio 350 + mtkclient: https://github.com/bkerler/mtkclient ;
  https://mediatek.gitlab.io/aiot/doc/aiot-dev-guide/master/hw/mt8365-soc.html ; https://fccid.io/2AYXT61100001
