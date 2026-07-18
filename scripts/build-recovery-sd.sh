#!/usr/bin/env bash
#
# build-recovery-sd.sh - STUB for the i.MX recovery-SD image (flashing-method.md
# §5 + §6b items 2-4). This is the "insert SD, power on, done" auto-installer
# that clones Eight's own recovery SD with a podd payload.
#
# It is NOT wired into the primary release pipeline: it depends on the L2 rootfs
# (a full aarch64 Yocto/Buildroot/Nix image reusing the stock DTB + DDR/ATF +
# imx-boot), which is NOT built yet. The `recovery-sd` CI job is gated `if:false`
# / workflow_dispatch-only and calls this script, which documents the exact
# steps and exits 1 until the inputs exist. Run with --plan to print the plan
# without failing.
#
# Required inputs (none produced by CI today -> the TODOs below):
#   PODD_ROOTFS_TARGZ   podd eMMC rootfs.tar.gz            (§6b item 2, L2 - MISSING)
#   IMX_BOOT_SD_BIN     stock/Variscite imx-boot-sd.bin    (§6b item 3 - reuse verbatim)
#   RECOVERY_ROOTFS     minimal installer rootfs (fork of Eight's `rec-...`)
#   UBOOT_ENV_BIN       2 KB redundant env preset for 0x400000
#   OUT_IMG             output path, default podd-recovery-sd.img
set -euo pipefail

MODE="${1:-build}"

cat <<'PLAN'
podd recovery-SD build plan (flashing-method.md §5 / §6b) - NOT YET IMPLEMENTED
------------------------------------------------------------------------------
Prereqs (blocked on the L2 rootfs build):
  1. PODD_ROOTFS_TARGZ - podd eMMC rootfs.tar.gz. TODO: build the aarch64 L2
     rootfs (reuse stock imx8mm-var-som-symphony-eight.dtb + DDR/ATF blobs +
     imx-boot; podd preinstalled; vendor OTA stack removed). Not built yet.
  2. IMX_BOOT_SD_BIN   - reuse the stock/Variscite imx-boot-sd.bin verbatim
     (unsigned SPL boots fine; do NOT rebuild). TODO: vendor this blob.
  3. RECOVERY_ROOTFS   - minimal installer rootfs: a fork of Eight's `rec-...`
     recovery rootfs carrying install_yocto.sh + a factory-reset.service that
     auto-runs the recovery-mode podd-install.sh.

Image assembly (all offsets from the owner's confirmed SD dump):
  a. Create MBR image; partition p1 = installer rootfs (ext4).
  b. dd IMX_BOOT_SD_BIN     -> image @ 0x8400   (imx-boot container / IVT d1 00 20 41)
  c. dd UBOOT_ENV_BIN       -> image @ 0x400000 (redundant CRC-prefixed U-Boot env,
     preset: mmcdev=1 mmcpart=1 so the SD boots ITS OWN installer rootfs)
  d. Populate p1 with RECOVERY_ROOTFS, then drop under /opt/images/Yocto/:
       - PODD_ROOTFS_TARGZ  (the eMMC payload)
       - IMX_BOOT_SD_BIN    (bootloader to dd onto eMMC)
  e. Enable the factory-reset.service auto-runner in multi-user.target.wants.
  f. gzip -> podd-recovery-sd.img.gz (dd-writable by end users).

On-device flow the SD then performs (modeled on the recovered factory-reset.sh):
  install_yocto.sh -u : wipe eMMC first 8 MiB; create A/B+cage; mkfs.ext4;
    dd imx-boot-sd.bin -> /dev/mmcblk2 seek=33 (KiB); extract rootfs.tar.gz ->
    slot A (or the INACTIVE slot to keep stock rollback); e2fsck; PRESERVE cage;
    fw_setenv mmcdev 2 mmcblk 2 mmcpart 1 mmcautodetect no ustate 0 bootcount 0;
    reboot -> next boot runs eMMC.
------------------------------------------------------------------------------
PLAN

if [ "$MODE" = "--plan" ]; then
  exit 0
fi

echo "!! build-recovery-sd.sh is a stub: the L2 rootfs (PODD_ROOTFS_TARGZ) is not built yet." >&2
echo "!! See docs/research/flashing-method.md §5 and the TODOs above. Run with --plan for the plan only." >&2
exit 1
