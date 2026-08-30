#!/usr/bin/env bash
#
# patch-podd-sd-diag.sh - inject boot-time self-diagnostics into a podd SD image.
#
# WHY
#   On the i.MX8M-Mini / Variscite New-Rat 0.8 board there is NO exposed UART
#   console header (only JTAG on J7 + the STM32 MCU UART; the SoC console lives
#   on the SoM edge pins only). So when a podd SD "does not connect", there is no
#   serial log to look at. This patch makes the SD log its OWN boot so you can
#   read it back by mounting the card on a host - no serial, JTAG, or USB needed.
#
# WHAT IT ADDS (to the rootfs on p1, in place, unprivileged via debugfs)
#   /opt/podd/bootlog.sh                       - the logger (writes to
#                                                /opt/podd/bootlog, which is
#                                                PERSISTENT ext4; /var/log is a
#                                                tmpfs symlink on this image!)
#   podd-bootlog-early.service  (sysinit)      - proves it reached userspace
#   podd-bootlog-mid.service    (multi-user)   - snapshot after basic bringup
#   podd-bootlog-late.service   (multi-user)   - full dmesg/journal/network dump,
#                                                watches the network for ~60s
#
# AFTERWARD
#   dd the raw image to the microSD, boot the Pod, wait ~3 min, power off,
#   then on a host:
#       sudo mount -o ro <sd-device>p1 /mnt
#       ls -la /mnt/opt/podd/bootlog/     # timeline.txt, dmesg.*, journal.txt,
#       cp -r /mnt/opt/podd/bootlog /somewhere ; sudo umount /mnt
#
# Unprivileged: no root, no loop mounts (debugfs + dd on a plain file), mirrors
# scripts/build-podd-sd.sh.
#
# Usage:
#   scripts/patch-podd-sd-diag.sh [IMAGE]        # default: dist/podd-sd.img
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO="$(cd "$SCRIPT_DIR/.." && pwd)"
case "${1:-}" in
  -h|--help) sed -n '2,33p' "$0" | sed 's/^#\{0,1\} \{0,1\}//'; exit 0 ;;
esac
IMG="${1:-$REPO/dist/podd-sd.img}"
DIAG="$REPO/install/diag"

log()  { printf '==> %s\n' "$*"; }
die()  { printf '!!  %s\n' "$*" >&2; exit 1; }

if [ ! -f "$IMG" ]; then
  [ -f "$IMG.gz" ] && die "image not found: $IMG (found $IMG.gz - gunzip it first)"
  die "image not found: $IMG (pass the raw .img, not the .gz)"
fi
for f in bootlog.sh podd-bootlog-early.service podd-bootlog-mid.service podd-bootlog-late.service; do
  [ -f "$DIAG/$f" ] || die "missing diag artifact: $DIAG/$f"
done

# Tools from nix on demand.
command -v nix >/dev/null 2>&1 || die "nix not found on PATH"
log "resolving tools via nix (e2fsprogs, util-linux)"
# shellcheck disable=SC2016  # $PATH must expand inside the nix shell, not here.
TOOL_PATH="$(nix shell nixpkgs#e2fsprogs nixpkgs#util-linux --command sh -c 'printf %s "$PATH"')" \
  || die "failed to realise tools via nix"
export PATH="$TOOL_PATH:$PATH"
for t in debugfs e2fsck sfdisk dd; do command -v "$t" >/dev/null 2>&1 || die "tool '$t' missing"; done

WORK="$(mktemp -d "${TMPDIR:-/tmp}/podd-sd-diag.XXXXXX")"
trap 'rm -rf "$WORK"' EXIT
P1="$WORK/rootfs_p1.img"

# --- locate p1 (type 83, first partition) -----------------------------------
p1_line="$(sfdisk -d "$IMG" | grep -E 'img1[[:space:]]*:|1[[:space:]]*:[[:space:]]*start' | head -1)"
START="$(echo "$p1_line" | sed -n 's/.*start=[[:space:]]*\([0-9]*\).*/\1/p')"
SIZE="$(echo "$p1_line"  | sed -n 's/.*size=[[:space:]]*\([0-9]*\).*/\1/p')"
[ -n "$START" ] && [ -n "$SIZE" ] || die "could not parse p1 geometry from sfdisk -d"
log "p1: start=$START size=$SIZE sectors"

log "carving p1"
dd if="$IMG" of="$P1" bs=512 skip="$START" count="$SIZE" status=none
e2fsck -fy "$P1" >/dev/null 2>&1 || true
debugfs -R "stat /opt/podd" "$P1" >/dev/null 2>&1 || die "p1 has no /opt/podd - not a podd SD image?"

# --- build the debugfs command stream ---------------------------------------
DBG="$WORK/diag.cmds"
{
  echo "mkdir /opt/podd/bootlog"
  echo "write $DIAG/bootlog.sh /opt/podd/bootlog.sh"
  echo "sif /opt/podd/bootlog.sh mode 0100755"
  echo "write $DIAG/podd-bootlog-early.service /etc/systemd/system/podd-bootlog-early.service"
  echo "write $DIAG/podd-bootlog-mid.service /etc/systemd/system/podd-bootlog-mid.service"
  echo "write $DIAG/podd-bootlog-late.service /etc/systemd/system/podd-bootlog-late.service"
  echo "symlink /etc/systemd/system/sysinit.target.wants/podd-bootlog-early.service /etc/systemd/system/podd-bootlog-early.service"
  echo "symlink /etc/systemd/system/multi-user.target.wants/podd-bootlog-mid.service /etc/systemd/system/podd-bootlog-mid.service"
  echo "symlink /etc/systemd/system/multi-user.target.wants/podd-bootlog-late.service /etc/systemd/system/podd-bootlog-late.service"
} > "$DBG"

log "applying diagnostics via debugfs"
debugfs -w -f "$DBG" "$P1" > "$WORK/debugfs.log" 2>&1 || { cat "$WORK/debugfs.log" >&2; die "debugfs failed"; }

# verify
debugfs -R "stat /opt/podd/bootlog.sh" "$P1" 2>/dev/null | grep -q 'Mode:  0755' \
  || die "bootlog.sh not installed/executable"
for u in early mid late; do
  case "$u" in early) d=sysinit ;; *) d=multi-user ;; esac
  debugfs -R "ls /etc/systemd/system/$d.target.wants" "$P1" 2>/dev/null | tr ' ' '\n' \
    | grep -qx "podd-bootlog-$u.service" || die "unit podd-bootlog-$u not enabled"
done
e2fsck -fy "$P1" >/dev/null 2>&1 || true
log "diagnostics injected + verified"

# --- splice p1 back ---------------------------------------------------------
log "splicing p1 back into image"
dd if="$P1" of="$IMG" bs=512 seek="$START" count="$SIZE" conv=notrunc status=none

log "DONE - $IMG now self-logs its boot to /opt/podd/bootlog"
echo "Next: dd this raw image to the microSD, boot ~3 min, power off, then on a host:"
echo "  sudo mount -o ro <sd>p1 /mnt && ls -la /mnt/opt/podd/bootlog && sudo umount /mnt"
