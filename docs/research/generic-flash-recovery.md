# Generic FOSS Firmware Replacement for the Eight Sleep Pod 3 — Flashing & Recovery Landscape

Research date: 2026-07-17. Scope: web/community-tooling landscape to inform a **generic, reusable** FOSS
firmware replacement for the Pod 3 (i.MX8M Mini / Variscite VAR-SOM-MX8M-MINI; STM32 MCUs over UART;
A/B rootfs on eMMC; no secure boot). Every claim tagged **CONFIRMED / INFERRED / UNKNOWN** with a URL.

---

## Executive answer to the owner's skepticism

**The Pod 3 serial header DOES give real U-Boot command access — this is CONFIRMED by two independent
sources.** free-sleep's INSTALLATION.md and Adam Schaal's rooting write-up both interrupt an
*interruptible autoboot* ("Hit any key to stop autoboot", Ctrl+C) and then run genuine U-Boot commands
(`printenv`, `setenv bootargs …`, `run bootcmd`). It is not merely the post-boot Linux login console.
The one nuance: the connector is a JTAG-footprint header (Tag-Connect TC2070, board header `J7`), but the
community wires only its **3 UART pins** to an FTDI — they are not doing JTAG/SWD.

---

## 1. opensleep — real base OS / runtime + install method

**opensleep runs its Rust binary ON TOP of Eight Sleep's stock Yocto rootfs — it does NOT ship or flash
its own OS image.** (CONFIRMED)

Repo file tree (CONFIRMED, https://github.com/LiamSnow/opensleep): `Cargo.toml`, `Cargo.lock`, `.cargo/`,
`src/`, `opensleep.service`, `SETUP.md`, `BACKGROUND.md`, `example_solo.ron`, `example_couples.ron`,
`.github/workflows/`. **No `flake.nix`, no Buildroot/Yocto config, no `Dockerfile`, no install script.**
=> It is a plain **cross-compiled aarch64 Rust binary** (the `.cargo/` dir holds the cross-compile
target config), not a Buildroot/Yocto/Nix system image. (CONFIRMED tree; INFERRED that build == `cargo build`
for aarch64.)

- **Init system: systemd** (Eight's stock init is systemd; opensleep ships `opensleep.service`). (CONFIRMED,
  https://raw.githubusercontent.com/LiamSnow/opensleep/main/SETUP.md)
- **What it controls:** it *replaces Eight's three stock userland control programs — DAC, Frank, Capybara —*
  and talks directly to the **two STM32 subsystems** ("Sensor" = temp/capacitance/piezo/vibration; "Frozen"
  = TEC/pumps/priming) over their **USART** protocols. Pure userland; the kernel/rootfs stay Eight's.
  (CONFIRMED, https://github.com/LiamSnow/opensleep README + BACKGROUND.md)
- **Install steps (CONFIRMED, SETUP.md):**
  1. Get root (see §3 — SD-card `rootfs.tar.gz` edit for the SD variant; SETUP.md defers Pod 3-no-SD/Pod 4/5
     to the free-sleep serial method).
  2. SSH in on **port 8822**.
  3. `systemctl disable --now dac frank capybara swupdate-progress swupdate defibrillator` (stop the stock
     control + OTA stack so it can't fight opensleep or auto-update you out).
  4. Copy the `opensleep` binary + `config.ron` (from `example_solo.ron`/`example_couples.ron`) to
     `/opt/opensleep`.
  5. Copy `opensleep.service` to `/lib/systemd/system`; `systemctl enable --now opensleep`.
- **Does it touch eMMC A/B slots?** No — install is **in-place userland** on the currently-booted rootfs.
  It does not write the inactive slot or reflash. (CONFIRMED by the absence of any such step in SETUP.md;
  the only "flash" event is Eight's own factory-reset used to *gain* root, not opensleep's install.)

**Architecture takeaway:** opensleep and free-sleep use the *same* deployment model — userland payload on
Eight's stock Yocto + systemd + disable the OTA/`dac` stack. ninesleep is the only project that goes further
by swapping the `dac` daemon binary itself (also userland). None of them build or flash a full custom OS.

---

## 2. Serial / JTAG reality on Pod 3

- **CONFIRMED — real U-Boot console, interruptible autoboot.** free-sleep INSTALLATION.md: connect minicom,
  power on, "Get ready to interrupt the boot when you see `Hit any key to stop autoboot` (I just hit CTRL+C)",
  then `printenv current_slot`, `setenv bootargs "root=PARTLABEL=rootfs_a rootwait init=/bin/bash"`,
  `run bootcmd`. Those are U-Boot commands. Independently corroborated by Adam Schaal (same phrases + same
  `init=/bin/bash` trick). Sources:
  https://raw.githubusercontent.com/throwaway31265/free-sleep/main/INSTALLATION.md ,
  https://blog.adamschaal.com/posts/2025-12-16-rooting-eight-sleep/
- **CONFIRMED — connector & pinout.** Board = "CONTROL BOARD 625-00022 REV 14", JTAG-footprint header **`J7`**.
  free-sleep's `docs/jtag/pinout.jpeg` annotates: **Pin 1 = GND, Pin 6 = RX, Pin 8 = TX**. Cable = Tag-Connect
  **TC2070-IDC** (~$50) or solder 3 wires; adapter = **FTDI FT232RL** (~$13). **Baud = 921600.**
  Source: https://github.com/throwaway31265/free-sleep/tree/main/docs/jtag
- **CONFIRMED — autoboot is not locked;** Ctrl+C drops to the U-Boot shell, so `bootdelay` is nonzero.
- **UNKNOWN — exact `bootdelay` window** (docs say "get ready … spam Ctrl+C") and **UNKNOWN — UART logic
  voltage** (not documented; i.MX8MM UARTs are natively **1.8 V** — INFERRED; verify level before wiring the
  FT232RL, which defaults to 3.3 V/5 V).

Bottom line: the serial header reaches U-Boot on **all** Pod 3 variants — this is the universal root/recovery
path (needed on the no-SD variant, Pod 4, Pod 5).

---

## 3. Serial-FREE control paths

**(a) SD variant — factory-image edit (this is "ZeroSleep"), NOT an offline U-Boot env edit.** (CONFIRMED,
https://blopker.com/writing/04-zerosleep-1/)
- The Pod's microSD is **not the boot device** — it is a *factory-image store* the daughterboard reads during
  a factory reset. The image lives at **`/opt/images/Yocto/rootfs.tar.gz`** on the SD's ext4 partition.
- Method: pull SD (glued/heat-gun to free it), append your key to the tar
  (`tar --numeric-owner -rf rootfs.tar ./etc/ssh/authorized_keys`), re-gzip, reinsert, then **hold the small
  rear button next to the power cable while applying power** → factory reset re-extracts the *backdoored*
  rootfs onto eMMC. SSH in as **`rewt`** on **port 8822**.
- ninesleep documents the identical SD+factory-reset route (editing `/etc/shadow`, NetworkManager, SSH key).
  Sources: https://github.com/bobobo1618/ninesleep , https://github.com/LiamSnow/opensleep/blob/main/SETUP.md

**IMPORTANT correction to the project premise:** The hypothesis that ZeroSleep roots via an **offline
`fw_setenv` / hex edit of the U-Boot env at `/dev/mmcblkX` @ `0x400000`** is **FALSE as a documented method.**
No source (blopker, Schluggi, Schaal, free-sleep, opensleep, ninesleep) edits the U-Boot env offline. Editing
the i.MX8M env at `0x400000` is *generically valid* for this SoC family (INFERRED — the i.MX8M env commonly
lives at `/dev/mmcblkX 0x400000 0x1000`), but it is **UNKNOWN / undocumented** as an actual Pod root path.

**(b) In-band A/B-slot flip (write inactive eMMC slot from a root shell, `fw_setenv` the active slot, reboot).**
- **CONFIRMED the platform is A/B + SWUpdate + U-Boot-env:** boot args reference `root=PARTLABEL=rootfs_a`;
  U-Boot has a `current_slot` env var; the OTA stack is `swupdate`/`swupdate-progress`/`defibrillator`
  (everyone disables these). Sources: INSTALLATION.md, Schaal.
- **UNKNOWN as a published technique** — no one documents doing the inactive-slot write + slot-flip. It is
  **INFERRED technically feasible** given the confirmed design, and is the cleanest *in-band* upgrade/rollback
  primitive for a generic installer, but there is **no community precedent** to copy.

---

## 4. Generic unbrick via NXP UUU / SDP

- **CONFIRMED (SoC level):** i.MX8MM supports recovery over **USB-OTG1** using NXP's **`uuu`** (mfgtool
  successor) and the **Serial Download Protocol (SDP)**: `uuu` loads SPL+U-Boot into RAM then programs eMMC.
  Variscite's own flow enumerates the SOM as "NXP … Blank" then runs `uuu spl_boot.lst` + `uuu
  emmc_burn_all.lst`. Sources:
  https://dev.variscite.com/dart-mx8m-mini/mx8mm-debian-bookworm-6.1.36_2.1.0-v1.1/imx-uuu/ ,
  https://github.com/nxp-imx/mfgtools
- **CONFIRMED — ROM auto-fallback to SDP** when the configured boot device holds no valid image:
  Kontron ("falls back to serial loader mode … via USB-OTG1"), Gateworks ("a corrupt or non-existent boot
  media will result in attempting to boot via SDP"). Sources:
  https://docs.kontron-electronics.de/sw/yocto/build-ktn-imx/usage-mx8mm.html ,
  https://trac.gateworks.com/wiki/venice/SDP
- **CONFIRMED caveat:** production boards are fused to boot eMMC, so fallback only fires if the **eMMC SPL is
  bad/erased** (Gateworks: `mmc dev 2 && mmc erase 0 8000` to force SDP). A *genuinely* bricked Pod (dead
  bootloader) should therefore drop to SDP on its own; a Pod with a still-valid SPL will keep booting eMMC.
- **UNKNOWN (the practical blocker):** whether the Pod's **custom carrier board exposes USB-OTG1 on a
  reachable connector/pads.** OTG1 exists at the SOM edge connector, but no source documents a Pod port, and
  **no public report exists of anyone running `uuu`/SDP against a Pod.** Community recovery is exclusively
  SD-edit or serial/U-Boot. Source: https://blog.adamschaal.com/posts/2025-12-16-rooting-eight-sleep/
- **CONFIRMED — files & BSP are public:** `uuu` needs `flash.bin`/`imx-boot` (SPL+U-Boot+ATF+DDR fw) and a
  `rootfs.wic`. Variscite BSP is open: https://github.com/varigit/uboot-imx ,
  https://github.com/varigit/meta-variscite-fslc , https://github.com/varigit/variscite-bsp-platform/releases .

**Verdict:** UUU/SDP is the *theoretically* generic unbrick net and is sound at the chip level, but it is
**unproven on the Pod carrier** and gated on locating/exposing OTG1. Reverse-engineering and documenting the
Pod's OTG1 pads should be a project goal — it would make the design genuinely "unbrickable."

---

## 5. Recovery SD

- **CONFIRMED (Variscite dev boards):** a Variscite "recovery SD" is a `dd`-written bootable `.img.gz`; the
  SOM boots Linux from SD and `/usr/bin/install_yocto.sh` reflashes eMMC. Source:
  https://dev.variscite.com/dart-mx6/RELEASE_THUD_V1.0_VAR-SOM-MX6/yocto-recovery-sd-card/ ,
  https://variwiki.com/index.php?title=Yocto_Recovery_SD_card
- **CONFIRMED (Pod difference):** the Pod's microSD is **not** a Variscite recovery SD. It stores
  `rootfs.tar.gz` that the *running OS's factory-reset routine* extracts to eMMC — the SD is data, not
  (necessarily) a ROM boot device.
- **INFERRED:** because the SOM is stock Variscite, a Variscite-style bootable recovery SD *could* reflash the
  Pod's eMMC **if** the Pod's SOM will boot from SD — but the Pod appears **fused/strapped to boot eMMC with
  no boot-mode switch**, so an inserted SD does **not** auto-run via the ROM. (BOOT_MODE pins + `BOOT_CFG`
  OTP fuses select the device; default `00` = SD-then-SDP, but production Pods are not on defaults.)
  Sources: Kontron (above), NXP i.MX8 boot KB
  https://community.nxp.com/t5/i-MX-Processors-Knowledge-Base/i-MX8-Boot-process-and-creating-a-bootable-image/ta-p/1101253

---

## 6. Most generic install + recovery recipe

**Deployment model (drives the architecture): ship a userland payload on Eight's stock Yocto, not a custom
OS.** This is what opensleep, free-sleep, and ninesleep all do and is the most portable across the SD and
no-SD variants (and forward to Pod 4/5). Cross-compile to **aarch64** (Rust `aarch64-unknown-linux-musl`
static, à la ninesleep, is the most self-contained), install a **systemd** unit, and on first run
**disable/mask the OTA + stock control stack**:
`systemctl disable --now swupdate-progress swupdate defibrillator eight-kernel telegraf vector frankenfirmware dac frank capybara`.

**Root acquisition — offer both, prefer serial for universality:**
- **Universal (all variants incl. no-SD, Pod 4/5): serial/U-Boot.** TC2070-IDC → FT232RL on header `J7`
  (GND=pin1, RX=pin6, TX=pin8), `minicom -b 921600`, Ctrl+C at "Hit any key to stop autoboot",
  `setenv bootargs "root=PARTLABEL=rootfs_a rootwait init=/bin/bash"; run bootcmd` → root shell → remount rw,
  `passwd root`/`passwd rewt`, then disable the OTA stack. (Verify UART is 1.8 V before wiring.)
- **No-electronics (SD variant only): factory-image edit.** Disassemble, free the microSD, append
  `authorized_keys` to `/opt/images/Yocto/rootfs.tar.gz` (`tar --numeric-owner`), reinsert, hold rear button
  while powering on → SSH `rewt@pod -p 8822`.

**Recovery / safety net, layered from cheapest to most generic:**
1. **Keep a golden `rootfs.tar.gz` on the SD** (SD variant): the button factory reset is a built-in,
   hardware-free restore — always keep an un-backdoored known-good copy alongside yours.
2. **Respect A/B slots:** install into the **inactive** eMMC slot and keep the factory slot pristine; use the
   U-Boot `current_slot` env / bootargs to roll back. (Feasible but undocumented — you'd be first; test on the
   bench with serial attached.)
3. **Serial/U-Boot** as the always-available manual recovery (re-set bootargs, re-flash from U-Boot).
4. **UUU/SDP over USB-OTG1** as the theoretical bottom-of-stack unbrick (dead-bootloader recovery via ROM SDP
   fallback + Variscite public `flash.bin`/`.wic`). **Action item:** locate and document the Pod's OTG1 pads —
   until then this net is unverified on the Pod.

**Net recommendation:** Build the firmware as a cross-compiled aarch64 userland payload + systemd unit that
disables Eight's OTA/control stack (portable across variants). Standardize on the **serial/U-Boot** path as
the universal, variant-independent install+recovery mechanism, with the **SD factory-image edit** as the
solder-free option for SD units. Treat A/B-slot install and **UUU/SDP-over-OTG1** as the two upgrade/unbrick
primitives worth pioneering — both are technically sound for this SoC but currently undocumented on the Pod.

---

## Source list
- opensleep: https://github.com/LiamSnow/opensleep ; https://raw.githubusercontent.com/LiamSnow/opensleep/main/SETUP.md ; BACKGROUND.md ; https://liamsnow.com/projects/opensleep
- free-sleep: https://github.com/throwaway31265/free-sleep ; https://raw.githubusercontent.com/throwaway31265/free-sleep/main/INSTALLATION.md ; https://github.com/throwaway31265/free-sleep/tree/main/docs/jtag ; scripts/install.sh
- ninesleep: https://github.com/bobobo1618/ninesleep
- Schluggi/8rp: https://github.com/Schluggi/8rp (API/protocol RE; root write-up is WIP/absent)
- ZeroSleep: https://blopker.com/writing/04-zerosleep-1/ (only Part 1 exists)
- Adam Schaal: https://blog.adamschaal.com/posts/2025-12-16-rooting-eight-sleep/
- Variscite: https://dev.variscite.com/dart-mx8m-mini/mx8mm-debian-bookworm-6.1.36_2.1.0-v1.1/imx-uuu/ ; https://variwiki.com/index.php?title=Yocto_Recovery_SD_card ; https://github.com/varigit/uboot-imx ; https://github.com/varigit/variscite-bsp-platform/releases
- NXP / boot ROM / SDP: https://github.com/nxp-imx/mfgtools ; https://docs.kontron-electronics.de/sw/yocto/build-ktn-imx/usage-mx8mm.html ; https://trac.gateworks.com/wiki/venice/SDP ; https://community.nxp.com/t5/i-MX-Processors-Knowledge-Base/i-MX8-Boot-process-and-creating-a-bootable-image/ta-p/1101253
- HN context: https://news.ycombinator.com/item?id=45715511
