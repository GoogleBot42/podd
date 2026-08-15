# podd SD-boot image, L1 stock-clone variant (LEGACY)

> **SUPERSEDED (2026-07-20).** `dist/podd-sd.img.gz` now refers to the
> **clean-room L2 image** built by `os/scripts/build.sh` — from-source
> bootloader/kernel/rootfs, zero Eight Sleep binaries, proven booting the Pod
> and driving the bed. Use [CLEANROOM-OS.md](CLEANROOM-OS.md). This document
> describes the earlier L1 image (a patched clone of the owner's stock SD,
> built by `scripts/build-podd-sd.sh`); it is kept because its boot-flow
> analysis (offsets, env, mmc numbering, revert model) remains accurate and
> the L1 clone remains a working fallback for an owner with their own backups.
> The L1 image cannot be published (it contains Eight's copyrighted OS).

**The SD-swap model (still true for both L1 and L2 images):** you write one
image to a microSD, swap it for the stock card, and the Pod boots a complete
podd system entirely from the SD. The eMMC is **never touched** — swapping the
original card back reverts to stock instantly.

> The L1 variant was validated on hardware 2026-07-18/19 (booted, joined WiFi,
> podd drove the bed) before being retired in favor of the clean-room image.

---

## Why this is the safest revert path

On these hubs the boot device selection lives on the **SD card**, not the eMMC:

| Fact (confirmed from the owner's dumps) | Consequence |
|---|---|
| The i.MX8MM boot ROM honours an **SD manufacturing override**: a valid image on the SD boots regardless of eMMC. | The inserted SD is effectively the primary boot device. |
| `imx-boot` (SPL + U-Boot + ATF + DDR fw) sits at **byte 0x8400 on the SD** *and* on the eMMC. | The SD boots standalone; we reuse the stock SD bootloader verbatim. |
| The **U-Boot environment is on the SD** at offset `0x400000`, size `0x1000` (`/etc/fw_env.config → /dev/mmcblk1 0x400000 0x1000`). | Whatever env is on the *inserted* card decides where root comes from. |
| `mmcargs` sets `root=/dev/mmcblk${mmcblk}p${mmcpart}`; `bootcmd` does `mmc dev ${mmcdev}` then loads `/boot/Image.gz` + DTB from `mmc ${mmcdev}:${mmcpart}`. | Change three env vars and the whole boot target moves. |

Stock SD env: `mmcdev=2 mmcblk=2 mmcpart=1` → **boot rootfs from the eMMC**.
podd SD env: `mmcdev=1 mmcblk=1 mmcpart=1` → **boot rootfs from the SD's own p1**.

So: **the SD card *is* the switch.** Insert the podd card → boots podd from the
SD. Re-insert the stock card → boots stock from the eMMC. The eMMC's contents
are identical either way because we never write to it.

---

## What's on the image

The image is a full-device clone of the owner's **stock SD** with exactly these
changes (everything else — `imx-boot@0x8400`, MBR, p2, p3/cage — is byte-identical
to stock):

1. **p1 rootfs → a "podd-ified" clone of the STOCK eMMC rootfs.**
   We carve the working Yocto rootfs out of `mmcblk2p1` (the eMMC gold master)
   and reuse **all** of it — the stock kernel `/boot/Image.gz`, the DTB
   `imx8mm-var-som-symphony-eight.dtb`, every driver, and every file's real
   ownership / setuid / capabilities. It is modified **in place** (never
   extracted and repacked, which as an unprivileged user would destroy
   ownership) to add:
   - `/opt/podd/releases/<version>/rootfs/{podd,ui,podd.service}`
   - `/opt/podd/config.ron` (seeded from `config.pod4.example.ron`)
   - `/opt/podd/current → releases/<version>`
   - `/etc/systemd/system/podd.service`, enabled in `multi-user.target.wants`
   - **masked** vendor / conflicting units (symlinked to `/dev/null`) — see below.
   The rootfs is resized from the eMMC's 6.79 GiB partition down to the SD's
   6.1 GiB p1 so it fits, then spliced into the SD image's p1 region.

2. **U-Boot env @ 0x400000 → boot from SD.** Rebuilt from the owner's **exact
   stock env blob**, changing only `mmcdev 2→1`, `mmcblk 2→1`, pinning
   `mmcpart=1` and `mmcautodetect=no`. Every other variable is byte-for-byte
   identical to stock (verified by a round-trip check in the builder).

### Units masked on the podd SD (and why)

Masking = a `/etc/systemd/system/<unit> → /dev/null` symlink, which makes systemd
treat the unit as non-existent. **We do not delete any vendor files** — the OS
stays intact; the units just don't start.

- **Vendor OTA / control stack** (same set as `install/podd-install.sh`):
  `swupdate(.service/.socket)`, `swupdate-progress`, `defibrillator`, `dac`,
  `frank`, `capybara`, `telegraf`, `vector`, `frankenfirmware`, `eight-kernel`.
  These would fight podd for the MCUs or try to OTA/revert the system.
- **`persistent-manager.service` + `persistent.mount`** — **critical for the
  "eMMC untouched" guarantee.** These mount `/dev/mmcblk2p3` (the eMMC *cage*)
  read-write with `discard`, run `e2fsck` on it, and `persistent-manager.sh`
  will even `mkfs.ext4` it if the fsck fails badly. Masked, so a podd SD boot
  **never mounts, fscks, trims, or reformats the eMMC.** podd stores everything
  under `/opt/podd` on the SD, so it doesn't need the cage.
  > **Gotcha (hand-built L1 images):** masking `persistent.mount` silently
  > breaks WiFi if the stock `NetworkManager.conf` is left as-is. Stock reads
  > connection profiles from `/persistent/system-connections/` (see
  > `docs/research/connectivity-and-diff.md`), which lives on the now-masked
  > eMMC cage — so a profile written anywhere else (e.g.
  > `/etc/NetworkManager/system-connections/`) is silently ignored.
  > `scripts/build-podd-sd.sh` does not carry a fix for this (it masks the unit
  > but doesn't repoint NetworkManager). The clean-room L2 image ships the
  > fix — an overridden `[keyfile] path=/etc/NetworkManager/system-connections/`
  > (`os/board/eightsleep/imx8mm-varsom/rootfs-overlay/etc/NetworkManager/NetworkManager.conf`)
  > plus masking `systemd-networkd-wait-online.service` (`post-build.sh`), which
  > otherwise blocks on a network that will never come up and stalls the boot.
  > If you hand-patch an L1 image, carry both changes yourself.
- **`free-sleep*`** — found pre-installed on this eMMC image. It binds port
  `3000` (which podd's API also uses) and drives the same MCUs, so it must not
  run alongside podd.

> `cage` is **never** touched (the mount that would touch the eMMC cage is
> masked). The SD's own p3 is left as-is and is available for podd's persistent
> data if you later want it.

---

## Build it

Prerequisites: the owner's backups in `../backup/` (`mmcblk2.img.gz`,
`mmcblk1-sd.img.gz`, `sd-uboot-env-0x400000.bin.gz`, `mmcblk2-parttable.txt`),
`nix`, and the podd payload built:

```sh
export PATH="$HOME/.nix-profile/bin:$PATH"
nix build .#podd-aarch64 -o result-podd   # aarch64 static podd binary
nix build .#ui           -o result-ui      # the SPA
./scripts/build-podd-sd.sh                 # -> dist/podd-sd.img.gz (+ .manifest.txt)
```

The builder is fully unprivileged (no root, no loop mounts): it carves ext4 with
`dd`, edits it with `debugfs`, sizes it with `resize2fs`, and builds the env with
`mkenvimage`. It prints the output path, raw + gz size, sha256, and a manifest of
every change vs the stock SD. Override inputs/outputs with the `PODD_SD_*`
environment variables documented at the top of the script.

---

## Write it to a microSD

You need a card **at least as large as the stock card** (the stock SD is a
~14.8 GiB device; a 16 GB card is the practical minimum). The `.img.gz` expands
to a full-device image — write the **whole** thing.

**`dd` (Linux/macOS)** — double-check `of=` is the SD, not your disk:

```sh
# find the device (e.g. /dev/sdX on Linux, /dev/diskN on macOS)
gzip -dc dist/podd-sd.img.gz | sudo dd of=/dev/sdX bs=4M conv=fsync status=progress
sync
```

**Raspberry Pi Imager / balenaEtcher** — choose "Use custom image", select
`dist/podd-sd.img.gz` (both tools read `.gz` directly), pick the SD, write.

---

## Swap it in

1. Power the Pod **off** and unplug it.
2. Open the hub, note the orientation, and **remove the stock microSD**. Keep it
   safe — **it is your one-step revert.**
3. Insert the **podd** microSD the same way.
4. Reassemble and power on.

To revert at any time: power off, swap the **stock** card back in, power on. The
eMMC was never modified, so you are back to bone-stock.

---

## First-boot checklist

> **There is no attachable serial console on this board.** On the i.MX8M-Mini /
> Variscite "New-Rat 0.8" hub the `console=ttymxc3,115200` UART is **not broken
> out to any reachable header** — it exists only on the SoM edge-connector pins
> (83/85; 115200 8N1, 3.3 V logic, if you ever tap the SoM directly, which is
> impractical). The JTAG-footprint header on this board is **real JTAG**, not a
> UART — see [FLASHING.md](FLASHING.md#step-4--open-the-pod-and-find-the-debug-header).

Verify first boot with the **self-logging diagnostics** instead: patch them into
the image with `scripts/patch-podd-sd-diag.sh` before writing the card. It adds
`podd-bootlog-{early,mid,late}.service` (see `install/diag/`), which write boot
evidence — `timeline.txt`, staged `dmesg` dumps, the full journal,
`podd-status.txt`, and network state — to **`/opt/podd/bootlog/` on the SD's p1**
(persistent ext4; `/var/log` is a tmpfs on this image, so nothing survives
there). Boot the Pod, wait ~3 minutes, power off, and read the card in a host SD
reader:

```sh
sudo mount -o ro <sd-device>p1 /mnt
ls -la /mnt/opt/podd/bootlog/     # timeline.txt, dmesg.*, journal.txt, ...
```

If the Pod comes up on the network you can check everything over SSH instead;
the hardcore live-debug fallback is **JTAG via OpenOCD** (the i.MX8MM is well
supported) on the board's JTAG header.

Confirm, in order (in the captured bootlog, over SSH, or on a JTAG/tapped
console):

**1. U-Boot picks the SD as the boot target.** On a console you would see:
```
current mmcdev: 1
```
and the kernel/DTB loading from `mmc 1:1` (not `2:1`). Without a console, U-Boot
output isn't capturable — the equivalent check is the kernel cmdline in the
bootlog (`/proc/cmdline` in the snapshots, or `dmesg.early.txt`): it must say
`root=/dev/mmcblk1p1`. If it says `mmcblk2p*`, the env didn't take — re-check
that you wrote the podd card, not the stock one. *(Verifies: env change applied.)*

**2. Kernel root is the SD.** In the kernel log:
```
... root=/dev/mmcblk1p1 ...
VFS: Mounted root (ext4 filesystem) on device 179:...   # mmcblk1p1
```
Root must be **mmcblk1p1**, never `mmcblk2p*`. *(Verifies: booting the SD rootfs.)*

**3. podd is running.** In the bootlog this is `podd-status.txt` and
`journal.txt`; on a shell:
```sh
systemctl status podd            # active (running)
journalctl -u podd -b --no-pager # podd startup logs
```
podd serves its UI on `http://<pod-ip>:3000`. It ships with `PODD_DRY_RUN=true`
(MCU writes are logged, not sent) until you deliberately arm it. *(Verifies: podd
installed + enabled.)*

**4. The eMMC is NOT mounted (untouched guarantee).** Confirm nothing mounted the
eMMC cage:
```sh
mount | grep mmcblk2    # should print NOTHING
systemctl status persistent.mount persistent-manager.service   # masked / inactive
```
`mmcblk2*` must not appear in `mount`. If it does, the mask didn't apply — power
off and re-check the image. *(Verifies: eMMC stays untouched at runtime.)*

**5. Vendor stack is masked.**
```sh
systemctl status swupdate dac frank capybara free-sleep 2>&1 | grep -E 'masked|inactive'
```

**6. Configure and arm (when ready).** Edit `/opt/podd/config.ron` for your
hardware (the seeded config is a **Pod 4** profile — if this is a Pod 3, start
from `config.pod3.example.ron`), then flip `PODD_DRY_RUN=false` in the unit /
drop-in and `systemctl restart podd` once you're confident.

---

## Recover / revert

- **Doesn't boot, or misbehaves:** power off, swap the **stock microSD** back in,
  power on. Done — the eMMC is bone-stock, so this is a guaranteed, instant
  revert. Nothing on the eMMC was ever written.
- **Want to try again:** rebuild `podd-sd.img.gz`, re-write the podd card,
  re-insert. The stock card remains your golden restore point — keep a raw
  backup of it too (`gzip -dc` of `backup/mmcblk1-sd.img.gz` is that image).

---

## Assumptions to verify on hardware

All of these are consistent with the owner's confirmed dumps, but none has been
booted yet:

1. **SD-MFG override is active on this unit.** Confirmed active on the units the
   backups came from; *residual unknown:* whether it's fused off on some
   production units (cannot be read from an image). If it were, the SD wouldn't
   boot — swap back to stock, no harm.
2. **U-Boot env format.** The image uses a single (non-redundant) `0x1000`
   CRC-prefixed env with `0x00` padding, exactly matching the owner's dumped SD
   env (`/etc/fw_env.config` says `0x400000 0x1000`). The builder verifies the
   rebuilt env reads back with `fw_printenv`.
3. **Kernel boots from mmcblk1.** The stock kernel + DTB are reused unchanged and
   `/etc/fstab` mounts `/` via `/dev/root` (no hardcoded `mmcblk2`), so the same
   rootfs boots correctly as `mmcblk1p1`.
4. **No other unit writes the eMMC.** The only auto-start paths that touch
   `mmcblk2` (`persistent.mount` / `persistent-manager`) are masked; verify with
   checklist step 4 on first boot.
