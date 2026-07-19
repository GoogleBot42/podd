#!/usr/bin/env bash
#
# Buildroot post-build hook (runs after the rootfs is assembled, before images).
# Board: Eight Sleep i.MX8M-Mini Variscite "SD" hub.
#
# Wires the rootfs bits that aren't a package: the persistent-data mount, the
# RAUC system config, and the read-only-rootfs expectations. $1 = $TARGET_DIR.
set -euo pipefail

TARGET_DIR="$1"
BOARD_DIR="$(cd "$(dirname "$0")" && pwd)"
# Buildroot exports BINARIES_DIR (output/images) to post-build scripts.
BINARIES_DIR="${BINARIES_DIR:-$TARGET_DIR/../images}"

# --- kernel + DTB into the slot's /boot --------------------------------------
# Each RAUC rootfs slot is self-contained: U-Boot reads the kernel and DTB from
# the active slot's ext4 /boot (see uboot-env.txt: `load mmc ... /boot/Image.gz`
# and `/boot/podd.dtb`). Buildroot leaves them in output/images, so stage them
# into the rootfs here, renaming the DTB to the name the env loads.
mkdir -p "$TARGET_DIR/boot"
if [ -f "$BINARIES_DIR/Image.gz" ]; then
	install -D -m 0644 "$BINARIES_DIR/Image.gz" "$TARGET_DIR/boot/Image.gz"
else
	echo "post-build: $BINARIES_DIR/Image.gz not found (kernel not built yet?)" >&2
	exit 1
fi
if [ -f "$BINARIES_DIR/imx8mm-podd.dtb" ]; then
	install -D -m 0644 "$BINARIES_DIR/imx8mm-podd.dtb" "$TARGET_DIR/boot/podd.dtb"
else
	echo "post-build: $BINARIES_DIR/imx8mm-podd.dtb not found (DTS not built?)" >&2
	exit 1
fi

# --- persistent data partition (survives A/B slot swaps) ---------------------
# Mutable state lives here; the rootfs slots stay effectively read-only.
mkdir -p "$TARGET_DIR/data"
if ! grep -q '/data' "$TARGET_DIR/etc/fstab" 2>/dev/null; then
	printf '%s\n' 'LABEL=podd_data\t/data\text4\tdefaults,noatime\t0\t2' \
		>> "$TARGET_DIR/etc/fstab"
fi
# podd expects /data/podd for its config + state.
mkdir -p "$TARGET_DIR/data/podd"

# --- RAUC --------------------------------------------------------------------
install -D -m 0644 "$BOARD_DIR/rauc-system.conf" \
	"$TARGET_DIR/etc/rauc/system.conf"
# The verification keyring (public cert) must be provisioned per-owner at
# release time; ship a placeholder path so a missing key fails loudly rather
# than silently accepting unsigned bundles.
# TODO(release): drop the real signing cert here (see docs/RELEASING.md).

echo "post-build: podd data mount + RAUC system.conf staged"
