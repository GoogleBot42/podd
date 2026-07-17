# Eight Sleep Pod 3 "no-SD-card" variant (FCC ID 2AYXT61100001) — firmware/boot research

**Date:** 2026-07-17
**Scope:** Web + FCC-document research only. No firmware dump of this variant was available.
**Confidence tags:** [CONFIRMED] primary evidence; [INFERRED] strong deduction from evidence/platform norms; [UNKNOWN] not established.

> **HEADLINE CORRECTION:** The no-SD Pod 3 variant is **NOT** the NXP i.MX8M Mini / Variscite platform used by the SD-card Pod 3. FCC internal photos show it is a **custom MediaTek "i350" (MT8365 / Genio 350) System-on-Module**. This changes the recovery story: the generic unbrick is **MediaTek BROM/Preloader USB download (mtkclient / SP Flash Tool)**, *not* NXP UUU/SDP/imx-loader.

---

## 0. Primary evidence: FCC internal photos (exhibit 6388828)

Source: https://fccid.io/2AYXT61100001 and https://fcc.report/FCC-ID/2AYXT61100001 ; internal photos PDF: https://fcc.report/FCC-ID/2AYXT61100001/6388828.pdf
Filing: Eight Sleep Inc, Model 61100001 / product model 10503; internal photos dated 2023-08-29.

What the photos show (read directly from the rendered pages):

- **The SoM** carries a printed label: **"i350 SOM Module — Model: 625-00024 — HW: D04 — Mac id: 70B651000058 — EIGHT SLEEP"**. Silkscreen "OLogic" on the module bottom => the module was designed/built by OLogic for Eight Sleep. It is a card-edge (SO-DIMM-style) module, not a Variscite VAR-SOM. [CONFIRMED]
- **PMIC (module bottom, legible at 300 dpi): "MEDIATEK MT6357ARV 2225-AJHKH"**. MT6357 is the companion PMIC for the MediaTek MT8365. [CONFIRMED]
- **eMMC/eMCP (module bottom): "Kingston … 001,A00G-A 899274 TX29"** — a Kingston managed-NAND (eMMC) package. Onboard storage lives on the SoM. [CONFIRMED]
- Two additional large packages on the module bottom are smeared with thermal compound and **not legible** — [INFERRED] these are the MT8365 application processor + LPDDR. [INFERRED]
- **Antennas:** dual Wi‑Fi u.FL (WF0/WF1) and an on-board BT/BLE PCB antenna trace, consistent with the MT8365's integrated Wi‑Fi5/BT5 baseband. [CONFIRMED photos / INFERRED role]
- **Carrier: "CONTROL BOARD 625-00022 REV 14 — EIGHT SLEEP"**, with a SO-DIMM socket for the SoM and several STM32-class driver MCUs (silkscreen LEFT A/B, RIGHT C/D) for the thermoelectric/pump/sensor subsystems. [CONFIRMED]
- **Debug header J7** on the control board: a small ~8-way pad header adjacent to the SoM socket — [INFERRED] this is the JTAG/UART "Tag-Connect" landing the community guides attach to. [INFERRED]
- **J13 on the control-board bottom is a USB Type‑C receptacle** (legible label "J13"). Other bottom connectors: J5, J24 (small IDC headers), J2 LEFT PUMP, J8 RIGHT PUMP, SOLENOID 2, FAN 1/2, bulk caps. J13 is the only obvious USB port. [CONFIRMED it is USB‑C; INFERRED it is a SoC USB‑OTG / download port]
- **A metal microSD-style push connector is visible near the SoM socket** in the "Left Wi‑Fi antenna" photo (top-right of the control board, near "EIGHT SLEEP"). [INFERRED it is a microSD/uSD footprint] — even the "no-SD" board may carry an SD footprint, but no card is populated; boot storage is the SoM eMMC. [UNKNOWN whether populated/wired for boot]

### Cross-reference confirming the SoC identity
- MediaTek's own docs use **"i350" as the alias for MT8365 / Genio 350**: "MT8365 (i350)" (https://baylibre.pages.baylibre.com/mediatek/rita/device/mediatek/mtk-android-12/docs/overview/i350.html ; https://genio.mediatek.com/doc/android/hw/mt8365-soc.html).
- Genio 350 = quad Cortex‑A53 @ up to 2.0 GHz, 14nm, integrated Wi‑Fi5/BT5, **companion PMIC = MT6357**, typical BSP storage = eMMC (SanDisk/WD) + LPDDR (Micron) — matches the module exactly. (https://genio.mediatek.com/genio-350 ; https://www.manualslib.com/manual/2590354/Mediatek-Genio-350.html?page=8 ; IoT-Yocto docs https://mediatek.gitlab.io/aiot/doc/aiot-dev-guide/master/hw/mt8365-soc.html ).

**=> Q1 answer: DIFFERENT SoC and SoM.** SD variant = NXP i.MX8MM on Variscite VAR‑SOM‑MX8M‑MINI. No‑SD variant = custom Eight Sleep/OLogic **"i350 SOM Module" 625‑00024 (HW D04)** built on **MediaTek MT8365 / Genio 350 (a.k.a. "i350")** with MT6357 PMIC and onboard Kingston eMMC. [CONFIRMED for module ID + PMIC + eMMC; SoC die INFERRED with high confidence from the "i350" name + MT6357 pairing.]

---

## 1. Same SoC/SoM as SD variant? (Q1)
**No — different.** See §0. Notable that both variants still run the same Eight Sleep software stack (Yocto Linux, U‑Boot, A/B `rootfs_a`/`rootfs_b`, swupdate `.swu`), so from the *userland/boot-script* level they look similar, but the silicon and bootloader/recovery tooling are different families. Board IDs: SoM 625‑00024 HW D04; carrier 625‑00022 Rev 14. [CONFIRMED]

## 2. Boot process without an SD card; where SPL/U-Boot/env live (Q2)
- MediaTek boot chain (Genio/MT8365): **BootROM (BROM) → Preloader (MediaTek's SPL analog, in eMMC boot0) → U‑Boot (or LK/U‑Boot) → Linux kernel**. [INFERRED from MediaTek platform norms + the U‑Boot prompt the guides interrupt]
- Everything lives on the **SoM's onboard eMMC** (Kingston). Preloader in the eMMC boot partition; U‑Boot + its environment in eMMC (dedicated partition/offset). There is **no removable SD** holding the bootloader/env. [INFERRED/CONFIRMED-by-absence]
- **U-Boot environment editing:** From a running root shell, `fw_setenv` (or interactive `setenv` at the serial U‑Boot prompt) writes the eMMC-resident env. **There is NO offline SD env-edit path** like the Variscite variant had. The only offline route to the env/eMMC is MediaTek BROM download or physical eMMC chip-off. [INFERRED]
- Practically, the community root method never persists an env change — it does a one-shot interactive `setenv bootargs …; run bootcmd` (see §4). [CONFIRMED from guides]

## 3. Partition / rootfs layout (Q3)
- **Same A/B scheme conceptually:** PARTLABEL `rootfs_a` / `rootfs_b`, selected by U‑Boot env `current_slot` (`current_slot = a`), plus a persistent partition (community DB path `/persistent/free-sleep-data/free-sleep.db`). [CONFIRMED from free-sleep INSTALLATION.md + adamschaal blog]
- **Device node numbering differs.** On the MediaTek SoM the onboard eMMC is the boot/only MMC device, so it is `/dev/mmcblk0` (adamschaal references `mmcblk0`), versus `/dev/mmcblk2` on the Variscite SD variant (where the removable SD was `mmcblk1`). [INFERRED / partially CONFIRMED]
- Exact partition table/offsets for the MediaTek variant: [UNKNOWN] (no dump available).

## 4. How root is obtained on the no-SD variant (Q4)
**Method: physical JTAG/UART → interrupt U‑Boot → boot a root shell → set a password. It is NOT a software exploit and NOT NXP SDP.** [CONFIRMED]

Source: free-sleep `INSTALLATION.md` (https://github.com/throwaway31265/free-sleep/blob/main/INSTALLATION.md) and adamschaal "Rooting My Eight Sleep Pod 3" (https://blog.adamschaal.com/posts/2025-12-16-rooting-eight-sleep/). free-sleep image docs: `docs/jtag/` (`pinout.jpeg`, `1_connection_layout.jpeg`, `2_ground.jpeg`, `3_tx_and_rx.jpeg`, `4_module_connection.png`) and `docs/installation/` (`0_minicom.png` … `4_web_app.png`).

Hardware: **TC2070‑IDC Tag‑Connect (~$50)** onto the debug header (J7), wired via **Dupont wires to an FTDI FT232RL (~$13)** USB‑UART. (Alt: solder 3 wires TX/RX/GND directly.) Serial at **921600 baud**, e.g. `minicom -b 921600 -o -D /dev/tty.usbserial-XXXX`.

Steps:
1. Power on; when U‑Boot prints **"Hit any key to stop autoboot"**, interrupt with **CTRL+C** to reach the U‑Boot prompt.
2. At U‑Boot:
   ```
   printenv current_slot                 # verify current_slot = a
   setenv bootargs "root=PARTLABEL=rootfs_a rootwait init=/bin/bash"
   run bootcmd                            # boots straight into a root shell
   ```
3. In the root shell, bring up mounts and set passwords:
   ```
   mount -t proc proc /proc
   mount -t sysfs sysfs /sys
   mount -t devtmpfs devtmpfs /dev
   mount -t tmpfs tmpfs /run
   mount -o remount,rw /
   passwd root
   passwd rewt
   sync
   ```
4. Reboot normally (do NOT interrupt this time), log in, disable Eight Sleep update services, configure networking with `nmcli`, then install free-sleep:
   ```
   /bin/bash -c "$(curl -fsSL https://raw.githubusercontent.com/throwaway31265/free-sleep/main/scripts/install.sh)"
   ```
   (Historically SSH is `rewt`@pod on port 8822.)

**"Impossible to brick" claim.** free-sleep/adamschaal state Pod 3 (no SD), Pod 4, Pod 5 are "impossible to brick as long as you follow the directions," fully reversible, no permanent hardware change. [CONFIRMED claim]
- *Documented reason:* the procedure only sets **bootargs at runtime (non-persistent)** and a rootfs password — it never rewrites the preloader/U‑Boot/partition table, so there is nothing to brick. [CONFIRMED reasoning]
- *Deeper hardware reason (the task hypothesized i.MX ROM→SDP fallback):* the correct analog here is the **MediaTek BROM**: if the eMMC preloader is absent/invalid (or the device is forced into download mode), the BootROM enters **BROM/Preloader USB download mode**, which lets tools reflash eMMC — the MediaTek equivalent of i.MX SDP. This is **[INFERRED]**, not stated in the Eight Sleep sources. (mtkclient: https://github.com/bkerler/mtkclient ; force-BROM: https://www.hovatek.com/blog/how-to-force-a-mediatek-device-into-brom-mode/ )

## 5. Serial/JTAG — does it reach U-Boot, is autoboot interruptible? (Q5)
**Yes — CONFIRMED.** The serial console reaches the **U‑Boot prompt** and **autoboot IS interruptible** (the whole root method depends on catching "Hit any key to stop autoboot" and pressing CTRL+C). This directly resolves the owner's doubt that "serial may only give the Linux console / not U‑Boot command access." Both free-sleep and adamschaal show live `printenv`/`setenv`/`run bootcmd` at the U‑Boot shell over the FTDI at 921600 baud. Header = J7 (Tag‑Connect TC2070). [CONFIRMED]
Caveat: there is a real timing window — miss the interrupt and it boots on to Linux (and can pull an OTA that may re-lock). [CONFIRMED warning]

## 6. USB recovery / generic unbrick (Q6)
**Correction to the premise:** because the SoC is MediaTek, **NXP UUU / imx-loader / SDP does NOT apply.** [CONFIRMED — wrong silicon family]
The MediaTek analog:
- **BROM/Preloader USB download mode** over the SoC's USB‑OTG, driven by **mtkclient** (open-source, BROM/DA exploit + read/write/repair) or **SP Flash Tool**. Can read/write eMMC partitions and recover a bad preloader. (https://github.com/bkerler/mtkclient , https://mtkclient.org/ , https://xdaforums.com/t/fixing-bricked-preloader-on-mediatek-mtk-devices.4670984/ )
- **What forces download mode:** invalid/erased preloader in eMMC boot0 causes BROM to fall through to USB download automatically; otherwise a hardware trigger (holding a key / shorting a specific KCOL0/BROM test point at power-on) forces it. This is analogous to i.MX BOOT_MODE straps but MediaTek-specific. [INFERRED]
- **The physical port:** the control board exposes a **USB‑C connector (J13)**. [CONFIRMED it is USB‑C; INFERRED it is the SoC USB‑OTG usable for BROM download.] Whether J13 is actually wired to the MT8365 USB and whether BROM download is enabled on this specific board is **[UNKNOWN / UNVERIFIED]** — no source demonstrates a MediaTek USB reflash of a Pod. Secure-boot/DA-auth status of the MT8365 fuses on this board is also **[UNKNOWN]** (if fuses lock the DA, mtkclient may need a signed/patched loader).
- The SD variant's recovery (hold button → boot SD p1 → copy `rootfs.tar.gz` to eMMC → reboot) is **gone** on the no‑SD board; the MediaTek BROM path is its logical replacement. [INFERRED]

## 7. Implications for installing a custom OS and reverting (Q7)
Because there is **no removable-SD env/edit escape hatch**, the safety strategy shifts to the A/B slots + the always-available U‑Boot serial console, with MediaTek BROM as the last-resort net:

**Safest install path**
1. Get root via §4 (JTAG/UART → U‑Boot → root shell).
2. **Before touching anything, take a full backup** from the root shell: `dd` the eMMC boot partitions (`/dev/mmcblk0boot0`, `boot1`) and every partition (or the whole `/dev/mmcblk0`) off-box. This is your restore image since there is no SD fallback. [recommended]
3. Keep the **factory slot pristine** (e.g. leave `rootfs_a` as stock). Write your custom rootfs to the **inactive slot** (`rootfs_b`).
4. Flip `current_slot` (via `fw_setenv current_slot b`, or interactively at U‑Boot) and boot. If the custom image fails, revert by setting `current_slot=a` at the U‑Boot serial prompt — always reachable. [recommended, mirrors community A/B usage]
5. **Do NOT overwrite the preloader / U‑Boot / env region** unless you have first proven MediaTek BROM recovery works on your unit (mtkclient over J13). Treat the bootloader as sacred; keep U‑Boot serial as your primary recovery.

**Reverting to stock**
- Soft revert: set `current_slot` back to the untouched stock slot at U‑Boot; or restore `rootfs_b` from backup.
- Full revert: Eight Sleep's documented factory reset (remove Pod from the app account, factory firmware reset per their docs, re-add as a new Pod) restores stock firmware; or restore your full eMMC backup via the root shell / BROM. [CONFIRMED the app-side reset flow exists per free-sleep docs]

**Net-below-the-net:** if you ever corrupt the preloader/U‑Boot and lose the serial prompt, recovery requires MediaTek BROM download (mtkclient/SP Flash Tool) over USB‑C (J13) — feasible in principle but **unproven on this hardware and gated by unknown DA/secure-boot fuse state**. Plan for it (have mtkclient + a stock eMMC image ready) before doing any bootloader-level work. [INFERRED risk]

---

## Open items / unknowns
- Exact MT8365 die marking (thermal-paste-obscured) — inferred, not read. [UNKNOWN]
- eMMC partition table/offsets, preloader/U‑Boot partition locations. [UNKNOWN]
- Whether J13 USB‑C is wired to MT8365 USB‑OTG and whether BROM download is enabled / DA-authenticated. [UNKNOWN]
- Whether the visible SD-style slot on the carrier is populated/bootable. [UNKNOWN]
- Secure-boot / eFuse lock state of the MT8365. [UNKNOWN]

## Key source URLs
- FCC listing + internal photos: https://fccid.io/2AYXT61100001 ; https://fcc.report/FCC-ID/2AYXT61100001 ; https://fcc.report/FCC-ID/2AYXT61100001/6388828.pdf
- free-sleep install guide (no-SD path): https://github.com/throwaway31265/free-sleep/blob/main/INSTALLATION.md ; JTAG images: https://github.com/throwaway31265/free-sleep/tree/main/docs/jtag
- adamschaal "Rooting My Eight Sleep Pod 3": https://blog.adamschaal.com/posts/2025-12-16-rooting-eight-sleep/
- blopker "ZeroSleep" (SD-variant background): https://blopker.com/writing/04-zerosleep-1/
- opensleep: https://github.com/LiamSnow/opensleep ; https://liamsnow.com/projects/opensleep/
- MediaTek i350/MT8365/Genio 350: https://baylibre.pages.baylibre.com/mediatek/rita/device/mediatek/mtk-android-12/docs/overview/i350.html ; https://genio.mediatek.com/genio-350 ; https://mediatek.gitlab.io/aiot/doc/aiot-dev-guide/master/hw/mt8365-soc.html ; https://www.manualslib.com/manual/2590354/Mediatek-Genio-350.html?page=8
- MediaTek recovery tooling: https://github.com/bkerler/mtkclient ; https://mtkclient.org/ ; https://www.hovatek.com/blog/how-to-force-a-mediatek-device-into-brom-mode/
