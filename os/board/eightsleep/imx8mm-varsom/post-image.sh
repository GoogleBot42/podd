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
# The boot container (SPL + ATF + U-Boot + NXP DDR firmware) is stitched by
# Buildroot's stock i.MX8MM assembler, board/freescale/common/imx/
# imx8-bootloader-prepare.sh, which runs as the FIRST post-image script (see the
# defconfig's BR2_ROOTFS_POST_IMAGE_SCRIPT) and emits `imx8-boot-sd.bin`. On this
# SoC there is NO secure boot, so the container is unsigned. genimage expects the
# file named `imx-boot` (see genimage.cfg), so normalize the name here.
if [ -f "$BINARIES_DIR/imx8-boot-sd.bin" ]; then
	cp "$BINARIES_DIR/imx8-boot-sd.bin" "$BINARIES_DIR/imx-boot"
elif [ ! -f "$BINARIES_DIR/imx-boot" ]; then
	echo "post-image: neither imx8-boot-sd.bin nor imx-boot found in $BINARIES_DIR" >&2
	echo "  the imx8-bootloader-prepare.sh post-image step must run before this one" >&2
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
