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
# Idempotent: drop any prior podd_data line (incl. a stale malformed one from an
# earlier build, since the target tree is reused across builds), then add the
# correct space-separated entry. nofail => a missing/bad data card still boots.
if [ -f "$TARGET_DIR/etc/fstab" ]; then
	sed -i '/podd_data/d' "$TARGET_DIR/etc/fstab"
fi
echo 'LABEL=podd_data /data ext4 defaults,noatime,nofail 0 2' >> "$TARGET_DIR/etc/fstab"
# podd expects /data/podd for its config + state.
mkdir -p "$TARGET_DIR/data/podd"

# Default podd config baked into the rootfs; podd.service seeds it onto /data on
# first boot (see ExecStartPre) so the persistent copy is user-editable.
install -D -m 0644 "$BOARD_DIR/../../../../config.pod4.example.ron" \
	"$TARGET_DIR/etc/podd/config.ron"

# --- RAUC --------------------------------------------------------------------
install -D -m 0644 "$BOARD_DIR/rauc-system.conf" \
	"$TARGET_DIR/etc/rauc/system.conf"
# The verification keyring (public cert) must be provisioned per-owner at
# release time; ship a placeholder path so a missing key fails loudly rather
# than silently accepting unsigned bundles.
# TODO(release): drop the real signing cert here (see docs/RELEASING.md).

# --- reachability: services, sshd port, networkd conflict --------------------
# The overlay dropped NetworkManager.conf, the muzzle rules + unit, and the sshd
# drop-in. Enable the services and neutralize systemd-networkd (it conflicts with
# NetworkManager for wlan0). NetworkManager itself is enabled by its package.
enable_unit() { # <unit> <target>
	mkdir -p "$TARGET_DIR/etc/systemd/system/$2.wants"
	ln -sf "/usr/lib/systemd/system/$1" \
		"$TARGET_DIR/etc/systemd/system/$2.wants/$1"
}
mask_unit() { ln -sf /dev/null "$TARGET_DIR/etc/systemd/system/$1"; }

[ -f "$TARGET_DIR/usr/lib/systemd/system/sshd.service" ] && enable_unit sshd.service multi-user.target
enable_unit podd-muzzle.service sysinit.target
mask_unit systemd-networkd.service
mask_unit systemd-networkd-wait-online.service
# Boot diagnostics -> /data/bootlog (no serial console; read the card post-mortem).
enable_unit podd-bootlog-early.service sysinit.target
enable_unit podd-bootlog.timer timers.target
# Make sure sshd reads /etc/ssh/sshd_config.d/*.conf (older configs may not).
if [ -f "$TARGET_DIR/etc/ssh/sshd_config" ] \
   && ! grep -q '^Include /etc/ssh/sshd_config.d' "$TARGET_DIR/etc/ssh/sshd_config"; then
	printf '\nInclude /etc/ssh/sshd_config.d/*.conf\n' >> "$TARGET_DIR/etc/ssh/sshd_config"
fi

# --- owner secrets (WiFi PSK + SSH key) — injected, never committed ----------
# Reuse the L1 patch-files by default; override with PODD_SECRETS_DIR. Absent =>
# a generic image with no creds (correct for CI / public builds); it boots but
# won't join WiFi or accept SSH until provisioned.
SECRETS_DIR="${PODD_SECRETS_DIR:-$BOARD_DIR/../../../../dist/scripts/patch-files}"
if [ -f "$SECRETS_DIR/network-manager/MOMCorp.nmconnection" ]; then
	install -D -m 0600 "$SECRETS_DIR/network-manager/MOMCorp.nmconnection" \
		"$TARGET_DIR/etc/NetworkManager/system-connections/MOMCorp.nmconnection"
	echo "post-build: injected WiFi profile (MOMCorp)"
else
	echo "post-build: WARNING no WiFi profile at $SECRETS_DIR — image won't join WiFi" >&2
fi
if [ -f "$SECRETS_DIR/authorized_keys" ]; then
	install -d -m 0700 "$TARGET_DIR/root/.ssh"
	install -m 0600 "$SECRETS_DIR/authorized_keys" "$TARGET_DIR/root/.ssh/authorized_keys"
	echo "post-build: injected root authorized_keys"
else
	echo "post-build: WARNING no authorized_keys at $SECRETS_DIR — no SSH access" >&2
fi

echo "post-build: reachability (sshd 8822, muzzle, NM) + data mount + RAUC staged"
