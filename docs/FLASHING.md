# Flashing podd onto your Pod

For anyone who owns an Eight Sleep Pod and wants to run the open-source `podd`
firmware on it. You should be comfortable in a terminal and with opening consumer
hardware (screwdriver, heat gun). Depending on your hub you'll need either a
spare microSD card and a card reader (i.MX "SD" hub) or a USB-serial adapter
(~$13, MediaTek hub), plus about an hour for the one-time unlock.

> The first unlock requires physically opening the Pod. There is no software-only
> remote jailbreak, and podd does not rely on one. After that, everything —
> installing podd, updating it, recovering a bricked unit — is done over the
> network. Read [Safety first](#step-2--safety-first) before you start.

---

## TL;DR — the quickest path for your situation

| Your situation | Do this |
|---|---|
| Already rooted (free-sleep / opensleep, SSH access) | Go to [INSTALL.md](INSTALL.md) — one command. |
| Fresh i.MX hub with an SD card inside (Pod 3 "SD" hub) | No serial console exists on this board; the JTAG-footprint header is real JTAG, not a UART. Use an SD path: boot podd from a swapped SD card (validated, eMMC untouched — write the from-source clean-room image, [CLEANROOM-OS.md](CLEANROOM-OS.md); boot-flow details in [SD-BOOT.md](SD-BOOT.md)), or [root the stock system via the SD backdoor](#path-b-imx-only-solder-free-sd-backdoor-aka-zerosleep) → [INSTALL.md](INSTALL.md). |
| Fresh i.MX hub with no SD (Pod 4 hub) | Not yet analyzed. No SD slot, so no SD paths; whether it has a reachable console UART is unknown (if its carrier matches the analyzed board, the header is JTAG, not UART). Deep unbrick is USB (uuu/SDP), also unverified. |
| Fresh MediaTek "no-SD" hub (FCC 2AYXT61100001) | [Buy a serial adapter](#step-3--what-to-buy) → [get root over serial at J7](#path-a-serial--u-boot--root-mediatek-boards) → [INSTALL.md](INSTALL.md). Deep recovery differs and is less-tested — read [the MediaTek notes](#mediatek-pod-3-no-sd-specifics). |

Related guides: [INSTALL.md](INSTALL.md) (installing once you have root) ·
[UPDATING.md](UPDATING.md) (keeping it current) ·
[RECOVERY.md](RECOVERY.md) (unbricking / going back to stock).

---

## Step 1 — Identify your Pod

What matters for flashing is the **hub** (the bedside control unit), not the
mattress **cover**. The Eight Sleep app's `coverVersion` ("Pod 3" / "Pod 4")
describes the cover, and covers and hubs can be mixed — the reference device this
project was built against reports a Pod 4 cover on a Pod 3 (SD) hub. Identify
your hub by its board. Which of the three hubs you have determines the recovery
nets available to you.

| Hub | Chip / board | Storage | How to recognize it |
|---|---|---|---|
| Pod 3 "SD" hub | NXP i.MX8M Mini, Variscite module ("New-Rat") | eMMC plus a bootable microSD inside | A microSD card sits on the Variscite module (often glued in). Best supported — has the extra recovery-SD net. **[CONFIRMED — this is the analyzed hardware.]** |
| Pod 4 hub | i.MX8M Mini (inferred; not yet dumped) | eMMC only, no SD | Newer units, no SD card inside. U-Boot env lives on eMMC. Debug access unverified (no confirmed console UART); deep unbrick is USB (uuu/SDP). **[INFERRED — no dump yet.]** |
| MediaTek "no-SD" hub | MediaTek MT8365 "Genio 350" | eMMC only, no SD | FCC ID `2AYXT61100001`; an extra USB-C port (J13) on the control board. Deep unbrick via mtkclient. **[INFERRED — no dump yet.]** |

Cheapest check first: the FCC ID on the sticker — `2AYXT61100001` means the
MediaTek no-SD hub. Otherwise open it (Step 4) and look for a Variscite module:
with a microSD card inside it is the Pod 3 SD hub, without one treat it as a
Pod 4 hub.

> Only the Pod 3 SD hub has been directly analyzed. The Pod 4 and MediaTek
> details are inferred from research and community reports, not a firmware dump;
> treat their specifics, especially deep-recovery, as unverified and stage
> recovery tools before you start. If unsure, assume the fewest safety nets.

---

## Step 2 — Safety first

You can always go back to stock:

- The "get root" step is non-destructive. It changes a runtime boot argument and
  optionally sets a root password. It does not touch the bootloader or wipe
  anything, so this step alone cannot brick your Pod.
- The userland install ([INSTALL.md](INSTALL.md)) writes no disk blocks: it drops
  files under `/opt/podd` and masks Eight's services. Undo it by unmasking them.
- The installer backs you up automatically. Both `podd-install.sh` and
  `podd-slot-install.sh` snapshot your U-Boot environment, partition table, and
  active-slot pointer into `/opt/podd/backup/<timestamp>/` first.
- On i.MX Pods the stock system stays on the inactive A/B slot during a slot
  install, so the original firmware is one U-Boot command away.

Full recovery procedures live in [RECOVERY.md](RECOVERY.md).

The real risks:

> - **Opening the Pod voids your warranty** and involves prying a glued/clipped
>   enclosure and possibly a heat gun. Go slow; this is the step that can
>   physically damage the unit.
> - **Water, mains, and electronics.** The Pod pumps water. Keep it dry and
>   unplugged while it is open.
> - **The A/B slot install (`podd-slot-install.sh`) writes to eMMC** and can
>   require bootloader-level recovery if it goes wrong (serial U-Boot on
>   MediaTek; JTAG or the SD nets on the i.MX SD hub). The userland install
>   cannot. Prefer the userland install unless you specifically want podd's own
>   OS image.

Back up first. The installer does this, but if you have a root shell open, copy
off the U-Boot environment and partition table yourself too, then `scp` them
somewhere safe. For a full golden-image backup, see [RECOVERY.md](RECOVERY.md).

```sh
fw_printenv > /tmp/fw_printenv.txt          # your boot configuration
cat /proc/partitions > /tmp/partitions.txt  # the disk layout
```

---

## Step 3 — What to buy

For an i.MX "SD" hub (the SD paths — no electronics needed):

- A microSD card ≥ 16 GB and a USB card reader, for both the
  [SD-swap podd boot](SD-BOOT.md) and the
  [SD backdoor](#path-b-imx-only-solder-free-sd-backdoor-aka-zerosleep).
- Optional, for low-level debug/unbrick only: a JTAG probe supported by OpenOCD
  (the i.MX8MM is well supported). A Tag-Connect TC2070-IDC (~$50) clips onto the
  board's JTAG footprint with no soldering.

For a MediaTek hub (the serial path):

| Item | Cost | Notes |
|---|---|---|
| FTDI FT232RL USB-UART adapter | ~$13 | Solder a few wires. If its logic level is jumper-selectable, try the lower setting first; community reports 3.3 V working on this board, but the UART's native level is unverified — see [residual unknowns](#residual-unknowns--when-to-stop-and-ask). |
| Tag-Connect TC2070-IDC | ~$50 | Clips onto the J7 footprint with no soldering. |

For MediaTek deep recovery you also need a USB-C cable to the control board's
USB-C port (J13) and [`mtkclient`](https://github.com/bkerler/mtkclient) on the
PC. Not needed for a normal install.

You will also want a Phillips screwdriver and a heat gun or hair dryer to soften
glue around the enclosure and the microSD.

---

## Step 4 — Open the Pod and find the debug header

> **The debug header is not the same on every board.** The widely-circulated
> "J7 = UART, GND/RX/TX on pins 1/6/8" pinout (free-sleep's reference photo) is
> from the MediaTek MT8365 / "i350" board (625-00022). On the analyzed i.MX8M
> Mini / Variscite "New-Rat 0.8" board the same-looking header is real JTAG —
> wiring an FTDI to its pins 6/8 puts you on TDO/TDI and achieves nothing
> (harmless, but a dead end).

Opening the unit is the same everywhere:

1. Unplug the Pod and drain/disconnect the water side. Work dry.
2. Remove the outer enclosure screws and gently pry the case. Glue is common —
   warm it with a heat gun to soften; do not force it.
3. Locate the control board, then identify which board you have (Step 1) before
   touching the header.

### MediaTek "i350" board (625-00022): J7 is a UART console

Three of J7's pins:

| J7 pin | Signal | Connect to your FTDI |
|---|---|---|
| pin 1 | GND | FTDI GND |
| pin 6 | RX (Pod receives) | FTDI TX |
| pin 8 | TX (Pod transmits) | FTDI RX |

> Serial wiring is crossed: the Pod's TX goes to your adapter's RX, and vice
> versa. If you see no output, swap RX/TX; this is the most common mistake and
> swapping is harmless.

Either clip a Tag-Connect TC2070 onto J7, or solder three thin wires to pins
1/6/8. Then continue with [Path A](#path-a-serial--u-boot--root-mediatek-boards).

### i.MX "New-Rat 0.8" board (Pod 3 SD hub): the header is real JTAG

Confirmed by probing the analyzed board against the SoM vendor's documentation
("Table 48: JTAG Header Signals"):

| Pin | Signal | Pin | Signal |
|---|---|---|---|
| 1 | JTAG_VREF (3.3 V via 150 Ω) | 2 | TMS |
| 3 | GND | 4 | TCK (8.2 kΩ pulldown) |
| 5 | GND | 6 | TDO |
| 7 | GND (0 Ω on SoM) | 8 | TDI |
| 9 | TRST_B | 10 | POR_B |

- There is no reachable console UART on this board. The Linux console `ttymxc3`
  exists only on the SoM edge-connector pins (83/85); tapping it there is
  impractical, and it is 115200 8N1 at 3.3 V logic for both U-Boot and the kernel
  (the 921600 figure is MediaTek-only).
- The board does have a separate UART pad group, but it is an STM32 MCU UART
  speaking the binary `0x7E` LSP protocol. It prints garbage in a terminal; it is
  not a console.
- The header is useful for JTAG via OpenOCD (low-level debug/unbrick). A normal
  install does not need it — use the SD paths in Step 5.
- Boot problems are diagnosed without a console. The clean-room image ships
  self-logging diag units (`podd-bootlog` plus its early/late services) that write
  each boot's dmesg, journal, and network state to `/data/bootlog` on the SD's
  `podd_data` partition (p3). Power off, pull the card, and read the logs in a
  host SD reader. See [CLEANROOM-OS.md](CLEANROOM-OS.md).

---

## Step 5 — Get in (the one-time hard part)

Pick the path for your hub:

- **MediaTek hub** → [Path A](#path-a-serial--u-boot--root-mediatek-boards),
  serial over J7.
- **i.MX "SD" hub** → no serial console exists (Step 4). Either boot podd from a
  swapped card ([SD-BOOT.md](SD-BOOT.md)) — the validated method, needs no root
  on stock, leaves the eMMC untouched, swap the stock card back to revert — or
  use [Path B](#path-b-imx-only-solder-free-sd-backdoor-aka-zerosleep) to root
  the stock system and then run an [INSTALL.md](INSTALL.md) userland install.
- **i.MX no-SD (Pod 4) hub** → not yet analyzed; no verified path. Do not assume
  the serial instructions below apply to it.

### Path A: serial → U-Boot → root (MediaTek boards)

Works on the MediaTek "i350" board and any board with a reachable console UART.
It does not work on the i.MX "New-Rat 0.8" board (its header is JTAG; see
Step 4).

1. Open a serial terminal at 921600 baud and power on the Pod:

   ```sh
   minicom -b 921600 -o -D /dev/ttyUSB0
   # or:  screen /dev/ttyUSB0 921600
   ```

   > On this board the baud is 921600, not 115200 — the console runs fast even
   > though U-Boot's own `baudrate` variable says 115200. Use 921600.

2. Interrupt the bootloader. As soon as you see `Hit any key to stop autoboot`,
   press Ctrl-C repeatedly. The window is about one second, so start right after
   you power on.

3. At the U-Boot prompt, override the init process to boot into a root shell.
   Confirm your slot first:

   ```
   printenv
   setenv bootargs "root=PARTLABEL=rootfs_a rootwait init=/bin/bash"
   run bootcmd
   ```

   - On i.MX use either `root=PARTLABEL=rootfs_a` or `root=/dev/mmcblk2p1`.
   - On MediaTek use `root=PARTLABEL=rootfs_a` (its root device is `mmcblk0`,
     e.g. `/dev/mmcblk0p1`).
   - `printenv` shows whether the active slot is A or B (look at `mmcpart` —
     `1`=A, `2`=B — or `current_slot`). Pick the matching `rootfs_a`/`rootfs_b`.

4. In the root shell, mount the essentials and set passwords so you can log in
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

5. Reboot normally — do not interrupt autoboot this time. SSH is `rewt@<pod-ip>`
   on port 8822:

   ```sh
   ssh -p 8822 rewt@<pod-ip>
   ```

6. Disable Eight's OTA/control stack so it cannot fight podd or update you back
   to stock. The podd installer masks these too.

   ```sh
   systemctl disable --now swupdate swupdate-progress defibrillator \
       dac frank capybara telegraf vector frankenfirmware eight-kernel 2>/dev/null
   ```

You now have root. Nothing in the bootloader was touched; only a rootfs password
persisted. Go to [INSTALL.md](INSTALL.md).

### Path B: i.MX only, solder-free "SD backdoor" (a.k.a. ZeroSleep)

On i.MX Pods the internal microSD carries the factory recovery image, and the
rear button forces a reflash from it. This yields root without a serial adapter,
at the cost of physically freeing the glued microSD:

1. Open the Pod, gently heat and free the glued microSD, and read it in a card
   reader on your PC.
2. Add your SSH key (or edit `/etc/shadow`) inside the recovery payload on the
   SD, e.g. append `authorized_keys` into `/opt/images/Yocto/rootfs.tar.gz`.
3. Reinsert the SD, then hold the rear button (next to the power cable) while
   applying power. The factory-reset flow re-extracts the now-backdoored rootfs
   onto eMMC. SSH in as `rewt` on port 8822.

Freeing the glued SD takes patience with the heat gun. If you only want to run
podd, the [SD-swap boot](SD-BOOT.md) uses the same freed slot and never modifies
the eMMC. Path B leaves you with root on stock; go to [INSTALL.md](INSTALL.md).

---

## MediaTek (Pod 3 no-SD) specifics

Path A above is the MediaTek path; the J7 UART pinout and 921600 baud are this
board's facts. There is no SD card, no SD backdoor, and no SD-swap boot, so
Path B and [SD-BOOT.md](SD-BOOT.md) do not apply.

The deep-recovery net is `mtkclient` over USB-C (J13): put the MediaTek SoC into
BROM download mode and reflash from a PC. This is documented from the chip side
but not yet verified on an actual Pod.

> **Before any bootloader-level work on a MediaTek Pod, stage a full stock eMMC
> image and a working `mtkclient` setup.** There is no SD fallback. A userland
> install never touches the bootloader.

See [RECOVERY.md](RECOVERY.md#mediatek-pod-3-no-sd) for full MediaTek recovery
detail.

---

## Residual unknowns / when to stop and ask

If you hit one of these, stop and ask in the project before forcing anything:

- **MediaTek UART logic level.** Unverified. Community 3.3 V FTDI setups have
  worked on this board, but if your adapter's level is selectable, start lower.
- **The Pod 4 (i.MX no-SD) hub.** Not yet dumped or probed. No confirmed console
  UART, no SD paths, and the uuu/SDP unbrick is theoretical. If you have one and
  can open it, identifying its debug header (UART vs JTAG) is an open task.
- **i.MX "SD manufacturing override" fuse.** The SD-boot/recovery path relies on
  a boot-ROM override that is active on the units dumped so far, but it could be
  fused off on some production runs. If the SD path does not work on your unit,
  the worst case is that it does not boot — swap the stock card back, then ask.
  The remaining way in is JTAG.
- **MediaTek below userland.** The partition layout, whether J13 is wired to the
  SoC's USB, and whether secure-boot fuses require a signed loader are all
  unverified on real hardware. Treat MediaTek bootloader work as experimental.
- **USB-OTG deep-unbrick on i.MX.** The last-resort `uuu`/SDP reflash needs a
  USB-OTG pad that has not been located on the Pod. It is theoretical; the SD and
  JTAG nets (i.MX) and the serial net (MediaTek) cover you in practice.

When in doubt, take the path that cannot write anything: on an i.MX SD hub the
SD-swap boot leaves the eMMC untouched (swap the stock card back to revert); on
MediaTek the serial U-Boot method never bricks anything. Then do the userland
install.
