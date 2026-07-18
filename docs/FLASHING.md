# Flashing podd onto your Pod

**Who this is for / what you'll need:** Anyone who owns an Eight Sleep Pod and
wants to run the open-source `podd` firmware on it. You should be comfortable
running commands in a terminal and opening a piece of consumer hardware with a
screwdriver and a heat gun. You do **not** need to be an embedded-systems
engineer — this guide walks you through every step. Depending on your Pod you'll
need either a small USB-serial adapter (~$13) or just a USB-C cable, plus about
an hour for the one-time "get root" step.

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
| **Fresh Pod 3 or Pod 4 (i.MX, has an SD card inside)** | [Buy a serial adapter](#what-to-buy) → [get root over serial](#path-a-serial--u-boot--root-universal) → [INSTALL.md](INSTALL.md). |
| **Fresh Pod 3 "no-SD" (MediaTek, FCC 2AYXT61100001)** | Same serial path, but the deep-recovery net is different and less-tested — read [the MediaTek notes](#mediatek-pod-3-no-sd-specifics) carefully. |
| **You want maximum robustness on an i.MX Pod** | The recovery-SD auto-installer is designed but **not built yet** — use the serial path today. See [RECOVERY.md](RECOVERY.md). |

Related guides: **[INSTALL.md](INSTALL.md)** (installing once you have root) ·
**[UPDATING.md](UPDATING.md)** (keeping it current) ·
**[RECOVERY.md](RECOVERY.md)** (unbricking / going back to stock).

---

## Step 1 — Identify your Pod

There are three meaningfully different Pods. Getting this right decides which
recovery nets you have, so check before you buy anything.

| Variant | Chip / board | Storage | How to recognize it |
|---|---|---|---|
| **Pod 3 with SD** / **Pod 4** (the "i.MX family") | NXP i.MX8M Mini on a Variscite module | eMMC **plus a bootable microSD** inside | Best supported. Has all four recovery nets. |
| **Pod 3 "no-SD"** | MediaTek MT8365 "Genio 350" | eMMC only, **no SD** | **FCC ID `2AYXT61100001`** on the label. Fewer recovery nets. |
| **Pod 4** | i.MX8M Mini (like Pod 3-SD) with a different sensor MCU | eMMC + microSD | Treated as part of the i.MX family for flashing; only its `config.ron` differs. |

How to tell which one you have, cheapest checks first:

1. **The Eight Sleep app / cover version.** The in-app `coverVersion` (and the
   generation you bought it as — "Pod 3" vs "Pod 4") is the first clue. Pod 4 is
   the newer hardware; Pod 3 shipped in both an SD and a no-SD board revision.
2. **The FCC ID on the sticker.** If the label reads **`2AYXT61100001`**, it's the
   **MediaTek no-SD Pod 3**. (You can look FCC IDs up at fccid.io.)
3. **Opening it and looking.** Once open (Step 4), an i.MX unit has a microSD card
   slot on the Variscite module (often with a glued-in card). The MediaTek unit has
   no SD slot and an extra USB-C port (labelled J13) on the control board.

> If you're unsure, treat it as **MediaTek** until proven otherwise — that's the
> more conservative assumption because it has fewer safety nets, so you'll stage
> the right recovery tools before touching anything.

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
>   require serial recovery if it goes wrong. The plain userland install cannot.
>   Prefer the userland install unless you specifically want podd's own OS image.

**Back up first (the installer does it, but do it yourself too).** If you have a
serial root shell open, it costs nothing to also copy off the U-Boot environment
and partition table so recovery is trivial later:

```sh
fw_printenv > /tmp/fw_printenv.txt          # your boot configuration
cat /proc/partitions > /tmp/partitions.txt  # the disk layout
```

Copy those off the device (e.g. `scp`) and keep them. For a full "golden image"
backup, see [RECOVERY.md](RECOVERY.md).

---

## Step 3 — What to buy

For the **universal serial path** (works on every Pod) you need one of:

| Item | Cost | Notes |
|---|---|---|
| **FTDI FT232RL USB-UART adapter** | ~$13 | The cheap, solder-a-few-wires option. **Set its logic level to 1.8 V if it has a jumper.** The Pod's UART is natively 1.8 V. A 3.3 V adapter has been observed to work in practice, but 1.8 V is the correct/safe spec — see [residual unknowns](#residual-unknowns--when-to-stop-and-ask). |
| **Tag-Connect TC2070-IDC** | ~$50 | Clips onto the J7 footprint with **no soldering**. Pricier, but the tidiest option if you'd rather not solder. |

For the **MediaTek deep-recovery path** you'll additionally want:

- A **USB-C cable** to connect a PC to the control board's USB-C port (J13), plus
  [`mtkclient`](https://github.com/bkerler/mtkclient) on that PC. Only needed if
  you're doing bootloader-level recovery on a MediaTek unit — not for a normal
  install.

You'll also want a Phillips screwdriver, a heat gun or hair dryer (to soften glue
around the enclosure / the microSD), and patience.

---

## Step 4 — Open the Pod and find header J7

This is the same for all i.MX variants; MediaTek is similar but the recovery
board differs.

1. **Unplug the Pod and drain/disconnect the water side.** Work dry.
2. Remove the outer enclosure screws and gently pry the case. Glue is common —
   warm it with a heat gun to soften, don't force it.
3. Locate the control board. Find header **J7** — it uses a standard JTAG-style
   footprint. You only need three of its pins:

   | J7 pin | Signal | Connect to your FTDI |
   |---|---|---|
   | **pin 1** | **GND** | FTDI GND |
   | **pin 6** | **RX** (Pod receives) | FTDI **TX** |
   | **pin 8** | **TX** (Pod transmits) | FTDI **RX** |

   > **Serial wiring is always crossed:** the Pod's TX goes to your adapter's RX,
   > and vice versa. If you see no output, swap RX/TX — that's the #1 mistake and
   > it can't hurt anything.

4. Either clip a **Tag-Connect TC2070** onto J7, or solder three thin wires to
   pins 1/6/8.

---

## Step 5 — Get root (the one-time hard part)

Pick your path. **Path A (serial → U-Boot) works on every Pod** and is the
recommended universal method. Path B is a solder-free alternative for i.MX Pods
only.

### Path A: serial → U-Boot → root (universal)

This is the proven method used across the community (free-sleep, opensleep) and
it works on **all** variants.

1. **Open a serial terminal at 921600 baud** and power on the Pod:

   ```sh
   minicom -b 921600 -o -D /dev/ttyUSB0
   # or:  screen /dev/ttyUSB0 921600
   ```

   > Baud is **921600**, not 115200 — the Pod's console runs fast even though
   > U-Boot's own `baudrate` variable says 115200. Use 921600.

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

Trade-off: no electronics or soldering, but freeing the glued SD is more invasive
than clipping onto J7. Either way you end up with root; then go to
[INSTALL.md](INSTALL.md).

---

## MediaTek (Pod 3 no-SD) specifics

Everything in **Path A above works on MediaTek** — serial → U-Boot → `init=/bin/bash`
→ set password → install. That's the recommended path. What's different:

- **There is no SD card and no SD backdoor.** Path B does not apply.
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

- **UART logic level.** The spec is **1.8 V**; a 3.3 V FTDI has worked in practice
  but isn't guaranteed. If you have a 1.8 V-capable adapter, use it.
- **i.MX "SD manufacturing override" fuse.** The SD-boot/recovery path relies on a
  boot-ROM override that is provably active on the units we've dumped, but *could*
  be fused off on some production runs. If the SD recovery path doesn't work on
  your unit, fall back to serial (which always works).
- **MediaTek below userland.** The exact partition layout, whether J13 is wired to
  the SoC's USB, and whether secure-boot fuses require a signed loader are all
  **unverified on real hardware.** Treat MediaTek bootloader work as experimental.
- **USB-OTG deep-unbrick on i.MX.** The absolute-last-resort `uuu`/SDP reflash
  needs a USB-OTG pad that nobody has located on the Pod yet. It's theoretical for
  now; the serial and SD nets cover you in practice.

When in doubt: the **serial U-Boot method never bricks anything** and is always
available. Use it, get root, and do the safe **userland install**.

---

Next: **[INSTALL.md](INSTALL.md) — install podd now that you have root.**
