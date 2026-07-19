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
