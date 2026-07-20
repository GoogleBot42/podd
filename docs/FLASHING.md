# Flashing podd onto your Pod

**Who this is for / what you'll need:** Anyone who owns an Eight Sleep Pod and
wants to run the open-source `podd` firmware on it. You should be comfortable
running commands in a terminal and opening a piece of consumer hardware with a
screwdriver and a heat gun. You do **not** need to be an embedded-systems
engineer — this guide walks you through every step. Depending on your hub you'll
need either a spare microSD card and a card reader (i.MX "SD" hub) or a small
USB-serial adapter (~$13, MediaTek hub), plus about an hour for the one-time
unlock step.

> **The one honest, irreducible catch:** the very first time you unlock a Pod you
> have to *open it up* physically. There is no software-only remote jailbreak (and
> podd deliberately doesn't rely on one). After that first unlock, everything —
> installing podd, updating it, even recovering a bricked unit — can be done over
> the network. See [Safety first](#safety-first) before you start.

---

## TL;DR — the quickest path for your situation

| Your situation | Do this |
|---|---|
| **Already rooted** (you run free-sleep / opensleep, have SSH) | Skip straight to [INSTALL.md](INSTALL.md) — it's literally one command. |
| **Fresh i.MX hub with an SD card inside** (Pod 3 "SD" hub) | **No serial console exists on this board** — the JTAG-footprint header is real JTAG, not a UART. Use the SD paths: boot podd entirely from a swapped SD card (the validated method, eMMC untouched — write the from-source clean-room image, [CLEANROOM-OS.md](CLEANROOM-OS.md); boot-flow details in [SD-BOOT.md](SD-BOOT.md)), or [root the stock system via the SD backdoor](#path-b-imx-only-solder-free-sd-backdoor-aka-zerosleep) → [INSTALL.md](INSTALL.md). |
| **Fresh i.MX hub with NO SD** (Pod 4 hub) | Not yet analyzed. No SD slot, so no SD paths; whether it has a reachable console UART is **unknown** (if its carrier matches the analyzed board, the header is JTAG, not UART). Deep unbrick is USB (uuu/SDP), also unverified. |
| **Fresh MediaTek "no-SD" hub (FCC 2AYXT61100001)** | [Buy a serial adapter](#what-to-buy) → [get root over serial at J7](#path-a-serial--u-boot--root-mediatek-boards) → [INSTALL.md](INSTALL.md). Deep recovery differs and is less-tested — read [the MediaTek notes](#mediatek-pod-3-no-sd-specifics). |

Related guides: **[INSTALL.md](INSTALL.md)** (installing once you have root) ·
**[UPDATING.md](UPDATING.md)** (keeping it current) ·
**[RECOVERY.md](RECOVERY.md)** (unbricking / going back to stock).

---

## Step 1 — Identify your Pod (by the HUB, not the app)

> **Important:** what matters for flashing is the **Hub** (the bedside control
> unit), *not* the mattress **Cover**. The Eight Sleep app's `coverVersion`
> ("Pod 3" / "Pod 4") describes the **cover** — and covers and hubs can be mixed.
> For example, the reference device this project was built against reports a **Pod 4
> cover** but runs on a **Pod 3 (SD) hub**. Identify your hub by its board, below —
> don't trust the app's generation label.

There are three meaningfully different **hubs**. Getting this right decides which
recovery nets you have.

| Hub | Chip / board | Storage | How to recognize it |
|---|---|---|---|
| **Pod 3 "SD" hub** | NXP i.MX8M Mini, Variscite module ("New-Rat") | eMMC **plus a bootable microSD** inside | A microSD card sits on the Variscite module (often glued in). Best supported — has the extra recovery-SD net. **[CONFIRMED — this is the analyzed hardware.]** |
| **Pod 4 hub** | i.MX8M Mini (inferred; not yet dumped) | eMMC only, **no SD** | Newer units; **no SD card** inside. U-Boot env lives on eMMC, not an SD. Debug access unverified (no confirmed console UART); deep unbrick is USB (uuu/SDP). **[INFERRED — no dump yet.]** |
| **MediaTek "no-SD" hub** | MediaTek MT8365 "Genio 350" | eMMC only, **no SD** | **FCC ID `2AYXT61100001`**; an extra USB-C port (J13) on the control board. Deep unbrick via mtkclient. **[INFERRED — no dump yet.]** |

How to tell which hub you have, cheapest checks first:

1. **FCC ID on the sticker.** `2AYXT61100001` → the **MediaTek no-SD** hub.
2. **The app's cover version is only a hint, not proof** — a "Pod 4" cover may be on
   a Pod 3 hub. Use it only to guess, then confirm by opening.
3. **Open it and look** (Step 4): an **i.MX** hub uses a Variscite module; if it has
   a **microSD card** inside, it's the **Pod 3 SD hub** (recovery-SD net available);
   if it's i.MX with **no SD**, treat it as a **Pod 4 hub** (no recovery-SD; USB
   unbrick). A **MediaTek** hub has no SD and an extra **USB-C (J13)** port.

> **Honesty note:** only the **Pod 3 SD hub** has been directly analyzed (it's the
> reference device). The **Pod 4** and **MediaTek** hub details are inferred from
> research + community reports, not a firmware dump — treat their specifics
> (especially deep-recovery) as unverified, and stage recovery tools before you start.
> If unsure, assume you have the fewest safety nets and prepare accordingly.

---

## Step 2 — Safety first

Read this section fully before you open anything. None of it is scary, but a few
minutes here saves you from the two ways people get into trouble.

**You can always go back to stock.** podd's whole design is built around this:

- The **"get root" step is non-destructive.** It only changes a runtime boot
  argument and (optionally) sets a root password. It does **not** touch the
  bootloader or wipe anything, so *this step alone cannot brick your Pod.*
- The **userland install** ([INSTALL.md](INSTALL.md)) writes no disk blocks at all
  — it drops files under `/opt/podd` and disables (masks) Eight's services. Undo it
  by unmasking those services.
- The **installer backs you up automatically.** Both `podd-install.sh` and
  `podd-slot-install.sh` snapshot your U-Boot environment, partition table, and
  active-slot pointer into `/opt/podd/backup/<timestamp>/` before doing anything.
- On i.MX Pods the **stock system stays on the inactive A/B slot** during a slot
  install, so the original firmware is one U-Boot command away.

Full recovery procedures live in **[RECOVERY.md](RECOVERY.md)**.

**The real risks, stated plainly:**

> - **Opening the Pod voids your warranty** and involves prying a glued/clipped
>   enclosure and possibly a heat gun. Go slow; that's the part that can physically
>   damage the unit.
> - **Water + mains + electronics.** The Pod pumps water. Keep it dry and
>   unplugged while it's open.
> - **The A/B slot install (`podd-slot-install.sh`) writes to eMMC** and *can*
>   require bootloader-level recovery if it goes wrong (serial U-Boot on
>   MediaTek; JTAG or the SD nets on the i.MX SD hub). The plain userland
>   install cannot. Prefer the userland install unless you specifically want
>   podd's own OS image.

**Back up first (the installer does it, but do it yourself too).** If you have a
root shell open, it costs nothing to also copy off the U-Boot environment
and partition table so recovery is trivial later:

```sh
fw_printenv > /tmp/fw_printenv.txt          # your boot configuration
cat /proc/partitions > /tmp/partitions.txt  # the disk layout
```

Copy those off the device (e.g. `scp`) and keep them. For a full "golden image"
backup, see [RECOVERY.md](RECOVERY.md).

---

## Step 3 — What to buy

**For an i.MX "SD" hub (the SD paths — no electronics needed):**

- A **microSD card ≥ 16 GB** and a **USB card reader**. That's it for both the
  [SD-swap podd boot](SD-BOOT.md) and the [SD backdoor](#path-b-imx-only-solder-free-sd-backdoor-aka-zerosleep).
- *(Optional, for low-level debug/unbrick only)* a **JTAG probe supported by
  OpenOCD** — the i.MX8MM is well supported. A **Tag-Connect TC2070-IDC**
  (~$50) clips onto the board's JTAG footprint with **no soldering**.

**For a MediaTek hub (the serial path):**

| Item | Cost | Notes |
|---|---|---|
| **FTDI FT232RL USB-UART adapter** | ~$13 | The cheap, solder-a-few-wires option. If its logic level is jumper-selectable, try the lower setting first; community reports 3.3 V working on this board, but the UART's native level is unverified — see [residual unknowns](#residual-unknowns--when-to-stop-and-ask). |
| **Tag-Connect TC2070-IDC** | ~$50 | Clips onto the J7 footprint with **no soldering**. Pricier, but the tidiest option if you'd rather not solder. |

For the **MediaTek deep-recovery path** you'll additionally want:

- A **USB-C cable** to connect a PC to the control board's USB-C port (J13), plus
  [`mtkclient`](https://github.com/bkerler/mtkclient) on that PC. Only needed if
  you're doing bootloader-level recovery on a MediaTek unit — not for a normal
  install.

You'll also want a Phillips screwdriver, a heat gun or hair dryer (to soften glue
around the enclosure / the microSD), and patience.

---

## Step 4 — Open the Pod and find the debug header

> **The debug header is NOT the same on every board.** The famous
> "J7 = UART, GND/RX/TX on pins 1/6/8" pinout that circulates in the community
> (free-sleep's reference photo) is from the **MediaTek MT8365 / "i350" board
> (625-00022)**. On the analyzed **i.MX8M Mini / Variscite "New-Rat 0.8"** board
> the same-looking JTAG-footprint header is **real JTAG** — wiring an FTDI to
> its pins 6/8 puts you on TDO/TDI and gets you nothing (harmless, but a dead
> end).

Opening the unit is the same everywhere:

1. **Unplug the Pod and drain/disconnect the water side.** Work dry.
2. Remove the outer enclosure screws and gently pry the case. Glue is common —
   warm it with a heat gun to soften, don't force it.
3. Locate the control board, then identify which board you have (Step 1) before
   touching the header.

### MediaTek "i350" board (625-00022): J7 is a UART console

You only need three of J7's pins:

| J7 pin | Signal | Connect to your FTDI |
|---|---|---|
| **pin 1** | **GND** | FTDI GND |
| **pin 6** | **RX** (Pod receives) | FTDI **TX** |
| **pin 8** | **TX** (Pod transmits) | FTDI **RX** |

> **Serial wiring is always crossed:** the Pod's TX goes to your adapter's RX,
> and vice versa. If you see no output, swap RX/TX — that's the #1 mistake and
> it can't hurt anything.

Either clip a **Tag-Connect TC2070** onto J7, or solder three thin wires to
pins 1/6/8. Then continue with [Path A](#path-a-serial--u-boot--root-mediatek-boards).

### i.MX "New-Rat 0.8" board (Pod 3 SD hub): the header is real JTAG

Confirmed by probing the analyzed board against the SoM vendor's documentation
("Table 48: JTAG Header Signals"):

| Pin | Signal | Pin | Signal |
|---|---|---|---|
| 1 | **JTAG_VREF** (3.3 V via 150 Ω) | 2 | **TMS** |
| 3 | GND | 4 | **TCK** (8.2 kΩ pulldown) |
| 5 | GND | 6 | **TDO** |
| 7 | GND (0 Ω on SoM) | 8 | **TDI** |
| 9 | **TRST_B** | 10 | **POR_B** |

- **There is no reachable console UART on this board.** The Linux console
  `ttymxc3` exists only on the SoM edge-connector pins (83/85); if you ever tap
  it at the SoM — impractical — it is 115200 8N1 at 3.3 V logic, for **both**
  U-Boot and the kernel (the 921600 figure is MediaTek-only).
- The board *does* have a separate UART pad group — but it is an **STM32 MCU
  UART speaking the binary `0x7E` LSP protocol**. It prints garbage in a
  terminal; it is not a console. Don't chase it.
- The header is useful for **JTAG via OpenOCD** (low-level debug/unbrick; the
  i.MX8MM is well supported). For a normal install you don't need it at all —
  use the SD paths in Step 5.
- **No console? No problem for diagnostics.** The podd SD image can carry
  **self-logging diag units** (`scripts/patch-podd-sd-diag.sh`, from
  `install/diag/`) that write each boot's dmesg/journal/network state to
  `/opt/podd/bootlog/` on the SD's first partition — power off, pull the card,
  and read the logs in a host SD reader. See the
  [SD-BOOT first-boot checklist](SD-BOOT.md#first-boot-checklist).

---

## Step 5 — Get in (the one-time hard part)

Pick the path for your **hub**:

- **MediaTek hub** → [Path A](#path-a-serial--u-boot--root-mediatek-boards)
  (serial over J7). This is the proven community method (free-sleep) — on that
  board.
- **i.MX "SD" hub** → no serial console exists (Step 4). Two SD options:
  - **The SD-swap podd boot ([SD-BOOT.md](SD-BOOT.md))** — this project's
    validated install method. You never need root on the stock system at all:
    write the podd SD image to a spare card, swap it in, and the Pod boots podd
    from the SD with the **eMMC untouched**. Swap the stock card back to revert.
  - **[Path B](#path-b-imx-only-solder-free-sd-backdoor-aka-zerosleep)** (SD
    backdoor) — if you specifically want root on the *stock* system, then a
    classic [INSTALL.md](INSTALL.md) userland install.
- **i.MX no-SD (Pod 4) hub** → not yet analyzed; no verified path. Don't assume
  the serial instructions below apply to it.

### Path A: serial → U-Boot → root (MediaTek boards)

This is the proven method used across the community (free-sleep) on the
**MediaTek "i350" board** — and on any board that turns out to have a reachable
console UART. It does **not** work on the i.MX "New-Rat 0.8" board (its header
is JTAG; see Step 4).

1. **Open a serial terminal at 921600 baud** and power on the Pod:

   ```sh
   minicom -b 921600 -o -D /dev/ttyUSB0
   # or:  screen /dev/ttyUSB0 921600
   ```

   > On this board the baud is **921600**, not 115200 — the console runs fast
   > even though U-Boot's own `baudrate` variable says 115200. Use 921600.

2. **Interrupt the bootloader.** As soon as you see `Hit any key to stop
   autoboot`, **hammer Ctrl-C** repeatedly. The window is short (about one
   second), so start spamming it right after you power on.

3. **At the U-Boot prompt**, boot straight into a root shell by overriding the
   init process. First confirm your slot, then boot:

   ```
   printenv
   setenv bootargs "root=PARTLABEL=rootfs_a rootwait init=/bin/bash"
   run bootcmd
   ```

   - On **i.MX** you can use either `root=PARTLABEL=rootfs_a` or `root=/dev/mmcblk2p1`.
   - On **MediaTek** use `root=PARTLABEL=rootfs_a` (its root device is `mmcblk0`,
     e.g. `/dev/mmcblk0p1`).
   - `printenv` shows whether the active slot is A or B (look at `mmcpart` — `1`=A,
     `2`=B — or `current_slot`). Pick the matching `rootfs_a`/`rootfs_b`.

4. **In the root shell, mount the essentials and set passwords** so you can log in
   normally afterward:

   ```sh
   mount -t proc proc /proc
   mount -t sysfs sysfs /sys
   mount -t devtmpfs devtmpfs /dev
   mount -t tmpfs tmpfs /run
   mount -o remount,rw /
   passwd root
   passwd rewt
   sync
   ```

5. **Reboot normally** (this time do **not** interrupt autoboot) and let the Pod
   boot into its full OS. Then log in — historically SSH is **`rewt@<pod-ip>` on
   port 8822**:

   ```sh
   ssh -p 8822 rewt@<pod-ip>
   ```

6. **Disable Eight's OTA/control stack** so it can't fight podd or silently update
   you back to stock. (The podd installer will also mask these, but doing it now
   keeps things quiet in the meantime.)

   ```sh
   systemctl disable --now swupdate swupdate-progress defibrillator \
       dac frank capybara telegraf vector frankenfirmware eight-kernel 2>/dev/null
   ```

That's it — you have root. Nothing in the bootloader was touched; only a rootfs
password persisted. **Go to [INSTALL.md](INSTALL.md).**

### Path B: i.MX only, solder-free "SD backdoor" (a.k.a. ZeroSleep)

On i.MX Pods the internal microSD carries the factory recovery image, and the
rear button forces a reflash from it. So you can get root **without any serial
adapter** — at the cost of physically freeing the glued microSD:

1. Open the Pod, gently heat and free the glued microSD, and read it in a card
   reader on your PC.
2. Add your SSH key (or edit `/etc/shadow`) inside the recovery payload on the SD,
   e.g. append `authorized_keys` into `/opt/images/Yocto/rootfs.tar.gz`.
3. Reinsert the SD, then **hold the rear button (next to the power cable) while
   applying power.** The factory-reset flow re-extracts the now-backdoored rootfs
   onto eMMC. SSH in as `rewt` on port 8822.

Trade-off: no electronics or soldering, but freeing the glued SD takes patience
with the heat gun. (On this board there is no serial alternative anyway — the
header is JTAG.) If all you want is to *run podd*, consider the
[SD-swap boot](SD-BOOT.md) instead: it uses the same freed SD slot but never
modifies the eMMC at all. With Path B you end up with root on stock; then go to
[INSTALL.md](INSTALL.md).

---

## MediaTek (Pod 3 no-SD) specifics

**Path A above is the MediaTek path** — serial over J7 → U-Boot →
`init=/bin/bash` → set password → install. That's the recommended path on this
hub (and the J7 UART pinout + 921600 baud are *this board's* facts). What's
different from the i.MX hubs:

- **There is no SD card, no SD backdoor, and no SD-swap boot.** Path B and
  [SD-BOOT.md](SD-BOOT.md) do not apply.
- **The deep-recovery net is `mtkclient` over USB-C (J13), and it is less-tested.**
  If you ever brick the bootloader, recovery means putting the MediaTek SoC into
  BROM download mode and reflashing with `mtkclient` from a PC. This path is
  documented from the chip side but **not yet verified on an actual Pod**, so:

  > **Before doing any bootloader-level work on a MediaTek Pod, stage a full stock
  > eMMC image and a working `mtkclient` setup first** — there is no SD fallback to
  > save you. For a plain userland install (the recommended path), you never touch
  > the bootloader, so this doesn't come up.

See [RECOVERY.md](RECOVERY.md#mediatek-pod-3-no-sd) for the full MediaTek recovery
detail.

---

## Residual unknowns / when to stop and ask

Be honest with yourself about these. If you hit one, stop and ask in the project
before forcing anything:

- **MediaTek UART logic level.** Unverified. Community 3.3 V FTDI setups have
  worked in practice on this board, but if your adapter's level is selectable,
  starting lower is the cautious choice.
- **The Pod 4 (i.MX no-SD) hub.** Not yet dumped or probed. No confirmed console
  UART, no SD paths, and the uuu/SDP unbrick is theoretical. If you have one and
  can open it, identifying its debug header (UART vs JTAG) is an open task.
- **i.MX "SD manufacturing override" fuse.** The SD-boot/recovery path relies on a
  boot-ROM override that is provably active on the units we've dumped, but *could*
  be fused off on some production runs. If the SD path doesn't work on your unit,
  the worst case is "it doesn't boot" — swap the stock card back, then ask; the
  remaining in is JTAG.
- **MediaTek below userland.** The exact partition layout, whether J13 is wired to
  the SoC's USB, and whether secure-boot fuses require a signed loader are all
  **unverified on real hardware.** Treat MediaTek bootloader work as experimental.
- **USB-OTG deep-unbrick on i.MX.** The absolute-last-resort `uuu`/SDP reflash
  needs a USB-OTG pad that nobody has located on the Pod yet. It's theoretical for
  now; the SD and JTAG nets (i.MX) / serial net (MediaTek) cover you in practice.

When in doubt, take the path that can't write anything: on an i.MX SD hub the
**SD-swap boot leaves the eMMC untouched** (swap the stock card back to revert);
on MediaTek the **serial U-Boot method never bricks anything**. Then do the safe
**userland install**.

---

Next: **[SD-BOOT.md](SD-BOOT.md)** (i.MX SD hub — boot podd from a swapped card)
or **[INSTALL.md](INSTALL.md)** (install podd once you have root).
