# SD boot on the i.MX8MM hub

The SD-swap model: write one image to a microSD, swap it for the stock card, and
the Pod boots a complete podd system entirely from the SD. The eMMC is never
touched, so swapping the original card back reverts to stock.

This document is the boot-flow reference behind that model and applies to any
SD-boot image on this hub. To build and write the current image, see
[CLEANROOM-OS.md](CLEANROOM-OS.md).

> A legacy L1 image, which bolted podd onto a clone of the owner's stock SD
> rather than building the OS from source, used the same boot flow. It was
> superseded by the clean-room image and its builder is no longer in the tree.

---

## Boot device selection

On these hubs the boot device selection lives on the SD card, not the eMMC:

| Fact (confirmed from the owner's dumps) | Consequence |
|---|---|
| The i.MX8MM boot ROM honours an SD manufacturing override: a valid image on the SD boots regardless of eMMC. | The inserted SD is effectively the primary boot device. |
| `imx-boot` (SPL + U-Boot + ATF + DDR fw) sits at byte `0x8400` on the SD *and* on the eMMC. | The SD boots standalone. |
| The U-Boot environment is on the SD at offset `0x400000`, size `0x1000` (`/etc/fw_env.config → /dev/mmcblk1 0x400000 0x1000`). | Whatever env is on the *inserted* card decides where root comes from. |
| `mmcargs` sets `root=/dev/mmcblk${mmcblk}p${mmcpart}`; `bootcmd` does `mmc dev ${mmcdev}` then loads `/boot/Image.gz` + DTB from `mmc ${mmcdev}:${mmcpart}`. | Change three env vars and the whole boot target moves. |

Stock SD env: `mmcdev=2 mmcblk=2 mmcpart=1` → boot rootfs from the eMMC.
podd SD env: `mmcdev=1 mmcblk=1 mmcpart=1` → boot rootfs from the SD's own p1.

The inserted card therefore selects the boot target. The podd card boots podd
from the SD; the stock card boots stock from the eMMC. The eMMC's contents are
identical either way because it is never written.

The env is a single (non-redundant) `0x1000` CRC-prefixed blob with `0x00`
padding, and reads back with `fw_printenv`. A rootfs cloned from the eMMC boots
correctly as `mmcblk1p1` because `/etc/fstab` mounts `/` via `/dev/root`, with no
hardcoded `mmcblk2`.

Caveat: whether the SD manufacturing override is fused off on some production
units cannot be read from an image. It is active on the units the backups came
from. If it were fused off, the SD would not boot, and swapping the stock card
back restores the previous state.

---

## Swap the card in

1. Power the Pod off and unplug it.
2. Open the hub, note the orientation, and remove the stock microSD. Keep it
   safe — it is the one-step revert.
3. Insert the podd microSD the same way.
4. Reassemble and power on.

To revert at any time, or if the podd card does not boot: power off, swap the
stock card back in, power on. Nothing on the eMMC was ever written, so the unit
returns to stock. Keep a raw backup of the stock card as well (`gzip -dc` of
`backup/mmcblk1-sd.img.gz`).

---

## First-boot checklist

> There is no attachable serial console on this board. On the i.MX8M-Mini /
> Variscite "New-Rat 0.8" hub the `console=ttymxc3,115200` UART is not broken out
> to any reachable header; it exists only on the SoM edge-connector pins (83/85;
> 115200 8N1, 3.3 V logic), which is impractical to tap. The JTAG-footprint
> header is real JTAG, not a UART — see
> [FLASHING.md](FLASHING.md#step-4--open-the-pod-and-find-the-debug-header).

Verify with the image's self-logging diag units instead
([CLEANROOM-OS.md](CLEANROOM-OS.md)): boot the Pod, wait ~3 minutes, power off,
and read the card in a host SD reader. If the Pod reaches the network, check over
SSH instead. The live-debug fallback is JTAG via OpenOCD; the i.MX8MM is well
supported.

Confirm, in order:

1. **U-Boot picked the SD.** Check the kernel cmdline in the bootlog
   (`/proc/cmdline`, or the early `dmesg` dump): it must say
   `root=/dev/mmcblk1p1`. If it says `mmcblk2p*`, the env did not take — check
   that you wrote the podd card, not the stock one.
2. **Kernel root is the SD.** `VFS: Mounted root (ext4 filesystem)` on
   `mmcblk1p1`, never `mmcblk2p*`.
3. **The eMMC is not mounted.**
   ```sh
   mount | grep mmcblk2    # must print nothing
   ```
   If `mmcblk2*` appears, something mounted the eMMC — power off and re-check the
   image.
4. **podd is running.**
   ```sh
   systemctl status podd            # active (running)
   journalctl -u podd -b --no-pager
   ```
   podd serves its UI on `http://<pod-ip>:3000` and ships with
   `PODD_DRY_RUN=true` (MCU writes are logged, not sent) until it is armed.
5. **Configure and arm, when ready.** Edit the config for your hardware, then set
   `PODD_DRY_RUN=false` in the unit or drop-in and `systemctl restart podd`.
