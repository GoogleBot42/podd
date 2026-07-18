# Recovery — unbrick and restore

**Who this is for / what you'll need:** Anyone whose Pod won't boot, boots into a
broken state, or who just wants to go back to stock. Depending on how deep you need
to go you'll need the same serial adapter from [FLASHING.md](FLASHING.md), and — in
the rare worst case — a PC with reflashing tools. **Read the reassurance below
first: on the well-supported (i.MX) Pods you have four nested safety nets, and the
cheapest one fixes almost everything.**

> **You can almost always get back.** The stock system stays on the inactive A/B
> slot, the installer keeps backups in `/opt/podd/backup/`, and (on i.MX) the
> internal SD holds a factory recovery image. Work through the nets **cheapest
> first** — most problems are solved at net #1 with a single U-Boot command.

Related: **[FLASHING.md](FLASHING.md)** (serial setup, wiring, J7) ·
**[INSTALL.md](INSTALL.md)** · **[UPDATING.md](UPDATING.md)**.

---

## First, which Pod?

- **i.MX family** (Pod 3 with SD, Pod 4) → four nets, [below](#imx-pod-3-sd--pod-4).
- **MediaTek** (Pod 3 no-SD, FCC `2AYXT61100001`) → serial + `mtkclient`,
  [below](#mediatek-pod-3-no-sd).

Not sure? See [FLASHING.md → Identify your Pod](FLASHING.md#step-1--identify-your-pod).

---

## i.MX (Pod 3-SD / Pod 4)

Four nested safety nets, cheapest and least invasive first. Try them in order.

### Net 1 — Serial U-Boot: revert the boot slot (fixes almost everything)

This is the primary net and it's almost always enough. It fixes any bad-slot,
bad-env, or failed-update situation, and it's how you "go back to stock."

1. Attach serial to **J7** and open a terminal at **921600** baud (see
   [FLASHING.md](FLASHING.md#step-4--open-the-pod-and-find-header-j7)).
2. Power on and **spam Ctrl-C** to stop autoboot.
3. At the U-Boot prompt, point the boot pointer back at the stock slot and boot:

   ```
   printenv                 # see current mmcpart / current_slot
   setenv mmcpart 1         # 1 = slot A (usually stock); use 2 for slot B
   run bootcmd
   ```

   To make it stick across reboots, `saveenv` after `setenv`. If you're recovering
   from a failed slot install, set `mmcpart` to the **other** slot from the one that
   failed. On newer builds you can also set `current_slot a` (or `b`).

That's it — you're booted back into a known-good system. No data is touched.

### Net 2 — Rear button: factory reset from the internal SD

If the internal microSD still has its factory recovery image intact, the Pod can
reflash eMMC from it with **no serial adapter**:

- With the Pod powered off, **hold the rear button (next to the power cable) while
  applying power.** The factory-reset flow re-extracts the stock rootfs
  (`install_yocto.sh -u`) onto the eMMC A/B slots and rewrites the bootloader from
  the SD.

> **Keep a clean recovery SD.** If you use the SD-backdoor root method
> ([FLASHING.md Path B](FLASHING.md#path-b-imx-only-solder-free-sd-backdoor-aka-zerosleep)),
> keep an **un-backdoored, golden `rootfs.tar.gz`** on the SD so this button reset
> restores clean stock, not your modified image.

### Net 3 — Full-disk restore from a backup image (`dd`)

The nuclear-but-simple option: write a whole-disk image back over eMMC (or the SD).
You need a golden image you (or the project) captured earlier, and either the SD
pulled out into a PC, or a root shell.

**Capture a golden backup while healthy** (do this once, ideally now):

```sh
# From a root shell on the Pod, stream the whole eMMC to your workstation:
ssh -p 8822 rewt@<pod-ip> 'dd if=/dev/mmcblk2 bs=4M' | gzip > pod-emmc-backup.img.gz
```

**Restore it later:**

```sh
# From a root shell, writing the image back (DANGER: overwrites the whole disk):
gzip -dc pod-emmc-backup.img.gz | dd of=/dev/mmcblk2 bs=4M
sync
```

> `dd` to the wrong device wipes it. Double-check `/dev/mmcblk2` is the Pod's eMMC
> (`cat /proc/partitions`). Reference stock images (`mmcblk2.img.gz`,
> `mmcblk1-sd.img.gz`) are intended to be published as release assets so you always
> have a known-good restore point.

### Net 4 — Deepest net: `uuu` / SDP over USB-OTG (last resort)

If the bootloader itself is truly dead, the i.MX boot ROM drops to Serial Download
Protocol (SDP) over USB-OTG, and `uuu` can reflash from scratch using Variscite's
public bootloader/image.

> **Not usable on the Pod yet.** Nobody has located the Pod's USB-OTG pads on a
> connector, so this net is currently theoretical on real hardware. In practice
> nets 1–3 cover you. If you're comfortable with hardware and want to help, finding
> and exposing OTG1 is the open task here.

---

## MediaTek (Pod 3 no-SD)

MediaTek has **no SD card**, so there's no rear-button SD reset and no SD `dd`
restore — the nets are thinner. Stage recovery tools *before* doing any
bootloader-level work.

### Primary — Serial U-Boot

Same as i.MX net 1: attach serial to J7, interrupt U-Boot, and revert the slot:

```
printenv
setenv current_slot a     # or:  setenv mmcpart 1
saveenv
run bootcmd
```

The root device on MediaTek is `mmcblk0` (e.g. `root=/dev/mmcblk0p1` /
`root=PARTLABEL=rootfs_a`).

### Deep unbrick — `mtkclient` over USB-C (J13)

If the bootloader/preloader is corrupted, recover with
[`mtkclient`](https://github.com/bkerler/mtkclient) (or SP Flash Tool) from a PC
over the control board's **USB-C port (J13)**:

1. With an invalid/erased preloader, the SoC's BROM auto-enters USB download mode.
   Otherwise you force BROM (hold a key / short a BROM test point at power-on).
2. `mtkclient` can read and write eMMC partitions and repair the preloader.

> **This path is documented from the chip side but UNVERIFIED on a real Pod.**
> Before any bootloader-level work: (1) confirm J13 is wired to the MT8365 USB, (2)
> capture a **full stock eMMC image** and the partition scatter *while the unit is
> healthy*, and (3) have `mtkclient` working — there is **no SD fallback** to save
> you. Whether secure-boot fuses require a signed loader (needing `--auth` or a
> patched DA) is also unknown. Treat MediaTek bootloader recovery as experimental.

---

## Going back to fully stock

- **Undo a userland install (no reboot needed):** stop and disable podd, then
  unmask Eight's services so the vendor stack runs again:

  ```sh
  systemctl disable --now podd
  systemctl unmask swupdate swupdate.socket swupdate-progress defibrillator \
      dac frank capybara telegraf vector frankenfirmware eight-kernel
  systemctl enable --now swupdate defibrillator frankenfirmware 2>/dev/null
  ```

  (Optionally `rm -rf /opt/podd` to remove podd entirely; your backups live under
  `/opt/podd/backup/` — copy them off first if you want to keep them.)

- **Undo a slot install:** use **Net 1** to point `mmcpart` back at the stock slot
  (it was left pristine on purpose), or **Net 2/3** to reflash stock.

Because the stock slot and the backups are always preserved, going back to stock is
a routine operation, not a rescue.
