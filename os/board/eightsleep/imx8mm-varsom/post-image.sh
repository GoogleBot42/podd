#!/usr/bin/env bash
#
# Buildroot post-image hook. Board: Eight Sleep i.MX8M-Mini Variscite "SD" hub.
#
# Assembles the final flashable SD image:
#   1. build imx-boot (SPL + ATF + U-Boot + NXP DDR/HDMI blobs) via imx-mkimage
#   2. bake the RAUC U-Boot env blob (mkenvimage from uboot-env.txt)
#   3. lay out the A/B + data partitions (genimage) into podd-sd.img
#   4. gzip + manifest
#
# $1 = $BINARIES_DIR (Buildroot output/images). $BR2_EXTERNAL_PODD_PATH and the
# host tools ($HOST_DIR/bin) are exported by Buildroot.
set -euo pipefail

BINARIES_DIR="$1"
BOARD_DIR="$(cd "$(dirname "$0")" && pwd)"

# --- 1. imx-boot -------------------------------------------------------------
# imx-mkimage stitches the from-source SPL/U-Boot/ATF together with NXP's
# redistributable DDR + HDMI firmware. On this SoC there is NO secure boot, so
# the container is unsigned.
#
# TODO(bring-up, phase 1): the exact imx-mkimage target + input filenames are
# board/SoC specific (iMX8MM, LPDDR4). Buildroot's U-Boot package can produce
# flash.bin directly when BR2_TARGET_UBOOT_NEEDS_IMX_MKIMAGE is set and the
# firmware-imx / imx-mkimage packages are enabled — prefer that path so this
# script just copies the result. Placeholder until the U-Boot build is wired:
if [ -f "$BINARIES_DIR/flash.bin" ]; then
	cp "$BINARIES_DIR/flash.bin" "$BINARIES_DIR/imx-boot"
elif [ ! -f "$BINARIES_DIR/imx-boot" ]; then
	echo "post-image: imx-boot/flash.bin not found — U-Boot imx-mkimage step not wired yet" >&2
	echo "  (phase-1 bring-up: enable firmware-imx + imx-mkimage in the defconfig)" >&2
	exit 1
fi

# --- 2. U-Boot env blob ------------------------------------------------------
mkenvimage -s 0x1000 -p 0x00 -o "$BINARIES_DIR/uboot-env.bin" \
	"$BOARD_DIR/uboot-env.txt"

# --- 3. partitioned image ----------------------------------------------------
genimage \
	--config "$BOARD_DIR/genimage.cfg" \
	--inputpath "$BINARIES_DIR" \
	--outputpath "$BINARIES_DIR" \
	--rootpath "$(mktemp -d)"

# --- 4. compress + manifest --------------------------------------------------
IMG="$BINARIES_DIR/podd-sd.img"
gzip -kf "$IMG"
{
	echo "podd clean-room SD image (Buildroot + RAUC A/B)"
	echo "raw sha256 : $(sha256sum "$IMG" | awk '{print $1}')"
	echo "gz  sha256 : $(sha256sum "$IMG.gz" | awk '{print $1}')"
	echo "layout     : imx-boot@0x8400 + env@0x400000 + rootfs_a + rootfs_b + data"
	echo "write      : sudo dd if=$(basename "$IMG") of=/dev/sdX bs=4M conv=fsync status=progress"
} > "${IMG%.img}.manifest.txt"

echo "post-image: $IMG(.gz) assembled"
