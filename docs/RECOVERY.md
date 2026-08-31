# Recovery — unbrick and restore

For anyone whose Pod won't boot, boots into a broken state, or who wants to go back
to stock. Depending on how deep you go: an SD card reader (i.MX SD hub), the serial
adapter from [FLASHING.md](FLASHING.md) (MediaTek), or a PC with reflashing tools.

> **Recovery is layered.** The stock system stays on the inactive A/B slot, the
> installer keeps backups in `/opt/podd/backup/`, and (on the i.MX SD hub) the
> internal SD holds a factory recovery image. If you were running the
> [SD-swap boot](SD-BOOT.md), swapping the stock card back is a complete, instant
> revert. Work the nets in order, cheapest first; most failures are resolved at
> net 1 with a single env change.

Related: [FLASHING.md](FLASHING.md) (boards, debug headers, wiring) ·
[INSTALL.md](INSTALL.md) · [UPDATING.md](UPDATING.md).

---

## Which Pod?

- **i.MX family** (Pod 3 with SD, Pod 4) → four nets, [below](#imx-pod-3-sd--pod-4).
- **MediaTek** (Pod 3 no-SD, FCC `2AYXT61100001`) → serial + `mtkclient`,
  [below](#mediatek-pod-3-no-sd).

To identify the hub, see
[FLASHING.md → Identify your Pod](FLASHING.md#step-1--identify-your-pod).

---

## i.MX (Pod 3-SD / Pod 4)

Four nested safety nets, cheapest first. Try them in order.

> **Before any net below: verify your SD card.** A card that reports full capacity
> but throws `No space left on device` early into a `dd` write is counterfeit or
> failing, not a script bug — and the failure looks like a boot problem, not a media
> problem. Check the claimed size (`lsblk -b`) and, if you doubt it, confirm with
> `f3probe --destructive --time-ops /dev/sdX` (destructive — spare cards only, it
> writes and reads back the whole card).

### Net 1 — Revert the boot slot

Resolves most failures: any bad-slot, bad-env, or failed-update situation. This is
also how you go back to stock.

> **How you reach U-Boot/the env depends on your board.** The i.MX "New-Rat 0.8"
> (Pod 3 SD) hub has no reachable console UART — its JTAG-footprint header is real
> JTAG, not serial (see
> [FLASHING.md](FLASHING.md#step-4--open-the-pod-and-find-the-debug-header)). On
> that board, flip the env from a root shell on any slot that still boots
> (`fw_setenv mmcpart 1`, then reboot), or via JTAG/OpenOCD if nothing boots, or
> skip straight to nets 2/3, which need no console. On MediaTek the J7/921600
> serial console works — see below.

At a U-Boot prompt (serial on MediaTek, JTAG on i.MX): power on, press Ctrl-C
repeatedly to stop autoboot, then repoint the boot pointer at the stock slot:

```
printenv                 # see current mmcpart / current_slot
setenv mmcpart 1         # 1 = slot A (usually stock); use 2 for slot B
run bootcmd
```

To make it stick across reboots, `saveenv` after `setenv`. From a running root
shell the equivalent is `fw_setenv mmcpart 1` + reboot. When recovering from a
failed slot install, set `mmcpart` to the other slot from the one that failed. On
newer builds you can also set `current_slot a` (or `b`).

> **Caution: don't trust `fw_printenv`'s exit code.** On this board `fw_printenv`
> false-negatives its own CRC verification — it fails even against a known-good,
> byte-for-byte-correct stock env. Don't gate a script's "success" on its return
> code; to verify an env, read the raw env region back and parse the NUL-separated
> `key=value` pairs directly, past the 4-byte CRC header.

The Pod then boots the known-good system. No data is touched.

### Net 2 — Rear button: factory reset from the internal SD

If the internal microSD still has its factory recovery image intact, the Pod can
reflash eMMC from it without a serial adapter: with the Pod powered off, hold the
rear button (next to the power cable) while applying power. The factory-reset flow
re-extracts the stock rootfs (`install_yocto.sh -u`) onto the eMMC A/B slots and
rewrites the bootloader from the SD.

> **Keep a clean recovery SD.** If you use the SD-backdoor root method
> ([FLASHING.md Path B](FLASHING.md#path-b-imx-only-solder-free-sd-backdoor-aka-zerosleep)),
> keep an un-backdoored, golden `rootfs.tar.gz` on the SD so this button reset
> restores clean stock, not your modified image.

### Net 2b — podd's own recovery SD

Releases ship `podd-recovery-sd-<tag>.img.gz`: an ordinary podd SD image (see
[CLEANROOM-OS.md](CLEANROOM-OS.md)) carrying `/data/podd-recovery/` — the rootfs
tarball, its `.sha256`, and `podd-slot-install.sh`. Write it to a spare card and
verify the write: `cmp -n <byte-count> image.img /dev/sdX`.

It boots a working podd with the eMMC untouched, which on its own recovers a Pod
whose eMMC is in a bad state. Putting the stock card back undoes it.

The payload is carried, not executed: it does not run from this card.
`podd-slot-install.sh` is the stock-U-Boot path (env `mmcdev=2`, so `mmcpart`
selects an eMMC slot); booted from the podd card `mmcdev=1` and `mmcpart` selects a
slot on the *card*, so the script detects the mismatch and refuses rather than
repointing U-Boot at the wrong device. Run it from the rooted stock system — see
[INSTALL.md](INSTALL.md) for what it does and does not touch.

Maintainers: `scripts/build-recovery-sd.sh --plan` prints the assembly plan.

### Net 3 — Full-disk restore from a backup image (`dd`)

Write a whole-disk image back over eMMC (or the SD). Needs a golden image captured
earlier, plus either a root shell or the SD pulled out into a PC.

**Capture a golden backup while healthy** — stream the whole eMMC to your
workstation:

```sh
ssh -p 8822 rewt@<pod-ip> 'dd if=/dev/mmcblk2 bs=4M' | gzip > pod-emmc-backup.img.gz
```

**Restore it later**, from a root shell. This overwrites the entire disk:

```sh
gzip -dc pod-emmc-backup.img.gz | dd of=/dev/mmcblk2 bs=4M
sync
```

> `dd` to the wrong device wipes it. Double-check `/dev/mmcblk2` is the Pod's eMMC
> (`cat /proc/partitions`). Reference stock images (`mmcblk2.img.gz`,
> `mmcblk1-sd.img.gz`) are intended to be published as release assets, providing a
> known-good restore point.

### Net 4 — `uuu` / SDP over USB-OTG (last resort)

If the bootloader itself is dead, the i.MX boot ROM drops to Serial Download
Protocol (SDP) over USB-OTG, and `uuu` can reflash from scratch using Variscite's
public bootloader/image.

> **Not usable on the Pod yet.** The Pod's USB-OTG pads have not been located on a
> connector, so this net is untested on real hardware; use nets 1–3. Locating and
> exposing OTG1 is the open task.

---

## MediaTek (Pod 3 no-SD)

MediaTek has no SD card, so there is no rear-button SD reset and no SD `dd`
restore; fewer nets are available. Stage recovery tools before doing any
bootloader-level work.

### Primary — Serial U-Boot

The serial console at J7 works (921600 baud — see
[FLASHING.md](FLASHING.md#step-4--open-the-pod-and-find-the-debug-header)):
attach serial, interrupt U-Boot, and revert the slot:

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
over the control board's USB-C port (J13):

1. With an invalid/erased preloader, the SoC's BROM auto-enters USB download mode.
   Otherwise you force BROM (hold a key / short a BROM test point at power-on).
2. `mtkclient` can read and write eMMC partitions and repair the preloader.

> **This path is documented from the chip side but unverified on a real Pod.**
> Before any bootloader-level work: (1) confirm J13 is wired to the MT8365 USB, (2)
> capture a full stock eMMC image and the partition scatter while the unit is
> healthy, and (3) have `mtkclient` working — there is no SD fallback. Whether
> secure-boot fuses require a signed loader (needing `--auth` or a patched DA) is
> also unknown. Treat MediaTek bootloader recovery as experimental.

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

  (Optionally `rm -rf /opt/podd` to remove podd entirely; backups live under
  `/opt/podd/backup/` — copy them off first to keep them.)

- **Undo a slot install:** use net 1 to point `mmcpart` back at the stock slot (the
  installer leaves it unmodified), or nets 2/3 to reflash stock.
