#!/usr/bin/env bash
#
# Buildroot post-image hook. Board: Eight Sleep i.MX8M-Mini Variscite "SD" hub
# (VAR-SOM-MX8M-MINI, DDR4 — see the boot-container notes below).
#
# Assembles the final flashable SD image:
#   1. build imx-boot (SPL + dual DDR fw + ATF + U-Boot FIT) Variscite-style
#   2. bake the U-Boot env blob (mkenvimage from uboot-env.txt, incl. the A/B
#      rollback state machine)
#   3. lay out the A/B + data partitions (genimage) into podd-sd.img
#   4. gzip + manifest, plus the OTA slot artifact (podd-os.ext4.zst) and the
#      slot-install tarball (podd-rootfs.tar.gz)
#
# $1 = $BINARIES_DIR (Buildroot output/images). $BR2_EXTERNAL_PODD_PATH,
# $BUILD_DIR and the host tools ($HOST_DIR/bin) are exported by Buildroot.
set -euo pipefail

BINARIES_DIR="$1"
BOARD_DIR="$(cd "$(dirname "$0")" && pwd)"

# --- 1. imx-boot (from-source boot container) --------------------------------
# We assemble the i.MX8MM boot container OURSELVES instead of using Buildroot's
# generic board/freescale/common/imx/imx8-bootloader-prepare.sh, because the
# generic script is wrong for Variscite's unified DART/VAR-SOM U-Boot in two
# fatal ways (both verified against the stock Pod 3 SD dump, and both were why
# the previous from-source boot chain was dead on arrival):
#
#   a) DDR PHY training firmware. The live device's SOM is a VAR-SOM-MX8M-MINI
#      (stock env: board_name=VAR-SOM-MX8M-MINI, som_rev13) which uses *DDR4*;
#      the DART-MX8M-MINI variant uses LPDDR4. Variscite's SPL supports both in
#      one binary, runtime-selecting timing by SOM EEPROM, and expects BOTH fw
#      sets appended after the SPL:
#          +0      lpddr4 1d imem/dmem + 2d imem/dmem  (slots 32K/4K each dim)
#          +73728  ddr4   1d imem/dmem + 2d imem/dmem  (same slot layout)
#      73728 = CONFIG_IMX8M_DDRPHY_FW_OFFSET in imx8mm_var_dart_defconfig, and
#      ddr4_timing.c reads its fw from that offset. The generic Buildroot script
#      appends ONLY the lpddr4 set (ddr_fw.bin -> lpddr4_pmu_train_fw.bin), so
#      on the VAR-SOM the SPL trained DDR4 with garbage read past the image end
#      -> dead board, no console. This layout below reproduces the stock boot's
#      firmware section BYTE-IDENTICALLY (verified by cmp against the dump).
#
#   b) U-Boot FIT device trees. SPL picks the u-boot.itb config whose
#      *description* matches the EEPROM-detected board ("imx8mm-var-dart-
#      customboard" or "imx8mm-var-som-symphony", board/variscite/
#      imx8mm_var_dart/spl.c board_fit_config_name_match). The previous build
#      packed only the DART control DTB, so even with good DDR the VAR-SOM
#      would find no matching config. Pack BOTH, like the stock image does.
#
# All inputs are built from source (U-Boot/ATF from Variscite's public trees)
# except the NXP DDR PHY training blobs from firmware-imx (NXP-EULA
# redistributable, required by every i.MX8M boot chain — not Eight Sleep code).

UBOOT_DIR="$(ls -d "$BUILD_DIR"/uboot-*/ 2>/dev/null | head -n1)"
FWDDR_DIR="$(ls -d "$BUILD_DIR"/firmware-imx-*/firmware/ddr/synopsys 2>/dev/null | head -n1)"
[ -n "$UBOOT_DIR" ] || { echo "post-image: no uboot-* dir under BUILD_DIR=$BUILD_DIR" >&2; exit 1; }
[ -n "$FWDDR_DIR" ] || { echo "post-image: no firmware-imx ddr/synopsys dir under BUILD_DIR=$BUILD_DIR" >&2; exit 1; }

WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

# Pad-or-truncate a blob into its fixed slot (imem slots are 32768 bytes, dmem
# slots 4096 — helper.c IMEM_LEN/DMEM_LEN; ddr4_dmem_1d.bin ships larger than
# its slot in newer firmware-imx, and the stock image carries exactly the first
# 4096 bytes of it, which is all the SPL ever copies).
slot() { # <src> <dst> <slot_size>
	dd if="$1" of="$2" bs="$3" count=1 conv=sync status=none
}
slot "$FWDDR_DIR/lpddr4_pmu_train_1d_imem.bin" "$WORK/l1i" 32768
slot "$FWDDR_DIR/lpddr4_pmu_train_1d_dmem.bin" "$WORK/l1d" 4096
slot "$FWDDR_DIR/lpddr4_pmu_train_2d_imem.bin" "$WORK/l2i" 32768
slot "$FWDDR_DIR/lpddr4_pmu_train_2d_dmem.bin" "$WORK/l2d" 4096
slot "$FWDDR_DIR/ddr4_imem_1d.bin"             "$WORK/d1i" 32768
slot "$FWDDR_DIR/ddr4_dmem_1d.bin"             "$WORK/d1d" 4096
slot "$FWDDR_DIR/ddr4_imem_2d.bin"             "$WORK/d2i" 32768
slot "$FWDDR_DIR/ddr4_dmem_2d.bin"             "$WORK/d2d" 4096
cat "$WORK"/l1i "$WORK"/l1d "$WORK"/l2i "$WORK"/l2d > "$WORK/lpddr4_half.bin"
cat "$WORK"/d1i "$WORK"/d1d "$WORK"/d2i "$WORK"/d2d > "$WORK/ddr4_half.bin"
# The lpddr4 half MUST be exactly 73728 bytes: that constant is baked into the
# U-Boot defconfig (CONFIG_IMX8M_DDRPHY_FW_OFFSET) as the ddr4 fw offset.
lp_size="$(stat -c%s "$WORK/lpddr4_half.bin")"
if [ "$lp_size" -ne 73728 ]; then
	echo "post-image: lpddr4 fw half is $lp_size bytes, expected 73728 (CONFIG_IMX8M_DDRPHY_FW_OFFSET)" >&2
	exit 1
fi
cat "$WORK/lpddr4_half.bin" "$WORK/ddr4_half.bin" > "$BINARIES_DIR/ddr_fw_var.bin"

# SPL + fw, then the U-Boot FIT (ATF BL31 @ 0x920000 = imx8mm BL31_BASE, U-Boot
# proper @ 0x40200000, both control DTBs), then the i.MX8MM boot container.
dd if="$BINARIES_DIR/u-boot-spl.bin" of="$WORK/u-boot-spl-padded.bin" \
	bs=4 conv=sync status=none
cat "$WORK/u-boot-spl-padded.bin" "$BINARIES_DIR/ddr_fw_var.bin" \
	> "$BINARIES_DIR/u-boot-spl-ddr.bin"

cp "$UBOOT_DIR/arch/arm/dts/imx8mm-var-dart-customboard.dtb" \
   "$UBOOT_DIR/arch/arm/dts/imx8mm-var-som-symphony.dtb" "$WORK/"
cp "$BINARIES_DIR/u-boot-nodtb.bin" "$BINARIES_DIR/bl31.bin" \
   "$BINARIES_DIR/u-boot-spl-ddr.bin" "$WORK/"
(
	cd "$WORK"
	BL31=bl31.bin BL33=u-boot-nodtb.bin ATF_LOAD_ADDR=0x00920000 \
		mkimage_fit_atf.sh imx8mm-var-dart-customboard.dtb \
		imx8mm-var-som-symphony.dtb > u-boot.its
	mkimage -E -p 0x5000 -f u-boot.its u-boot.itb
	# SPL load addr 0x7E1000 = CONFIG_SPL_TEXT_BASE; FIT at +0x60000 in the
	# container. Matches the stock boot container's IVT exactly.
	mkimage_imx8 -fit -loader u-boot-spl-ddr.bin 0x7E1000 \
		-second_loader u-boot.itb 0x40200000 0x60000 -out imx-boot
)
cp "$WORK/u-boot.itb" "$WORK/imx-boot" "$BINARIES_DIR/"

# --- 2. U-Boot env blob ------------------------------------------------------
mkenvimage -s 0x1000 -p 0x00 -o "$BINARIES_DIR/uboot-env.bin" \
	"$BOARD_DIR/uboot-env.txt"

# --- 3. partitioned image ----------------------------------------------------
genimage \
	--config "$BOARD_DIR/genimage.cfg" \
	--inputpath "$BINARIES_DIR" \
	--outputpath "$BINARIES_DIR" \
	--rootpath "$(mktemp -d)"

# --- 4. compress + manifest + OTA slot artifact ------------------------------
# The OTA artifact is the bare slot filesystem (kernel+dtb in /boot included):
# pod-updater streams it onto the inactive A/B partition. zstd's frame checksum
# (on by default) gives decompressed-integrity verification for free; the
# release pipeline renames this to os-<version>.ext4.zst (see
# scripts/build-release.sh).
zstd -T0 -19 -kf "$BINARIES_DIR/rootfs.ext2" -o "$BINARIES_DIR/podd-os.ext4.zst"

# The slot-install artifact: the same rootfs as a verified tarball, for the
# consumers that populate a slot by extracting rather than by dd'ing an fs image
# (install/podd-slot-install.sh, scripts/build-recovery-sd.sh). Buildroot already
# tarred $TARGET_DIR under fakeroot (BR2_TARGET_ROOTFS_TAR_GZIP); this only
# renames, content-checks and checksums it. Re-runnable standalone — see #52.
EXT_PATH="${BR2_EXTERNAL_PODD_PATH:-$BOARD_DIR/../../..}"
"$EXT_PATH/scripts/package-rootfs.sh" --images-dir "$BINARIES_DIR"

IMG="$BINARIES_DIR/podd-sd.img"
gzip -kf "$IMG"
{
	echo "podd clean-room SD image (Buildroot, A/B slots)"
	echo "raw sha256 : $(sha256sum "$IMG" | awk '{print $1}')"
	echo "gz  sha256 : $(sha256sum "$IMG.gz" | awk '{print $1}')"
	echo "layout     : imx-boot@0x8400 + env@0x400000 + rootfs_a + rootfs_b + data"
	echo "boot chain : from-source SPL/U-Boot (Variscite imx_v2020.04_5.4.70_2.3.0_var01) + ATF + dual LPDDR4/DDR4 fw"
	echo "write      : sudo dd if=$(basename "$IMG") of=/dev/sdX bs=4M conv=fsync status=progress"
	echo "rootfs.tgz : $(cut -d' ' -f1 < "$BINARIES_DIR/podd-rootfs.tar.gz.sha256")  podd-rootfs.tar.gz"
} > "${IMG%.img}.manifest.txt"

echo "post-image: $IMG(.gz) + podd-os.ext4.zst + podd-rootfs.tar.gz assembled (from-source imx-boot)"
