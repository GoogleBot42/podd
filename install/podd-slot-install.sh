#!/bin/sh
#
# podd-slot-install.sh - in-band A/B slot install (flashing-method.md §3c).
#
# SCOPE: this is the STOCK-U-Boot eMMC path (installing podd's rootfs onto a
# rooted stock unit's inactive eMMC slot). The clean-room SD image uses the
# same env-var scheme (mmcpart/next_mmcpart/upgrade_available/bootcount/
# ustate, 1=A 2=B) but implements the bootcount logic in its own env script
# (os/board/eightsleep/imx8mm-varsom/uboot-env.txt) and applies OS updates
# through pod-update-agent, which also runs the --confirm-good step automatically
# after a healthy boot.
#
# ****************************  DANGER: THIS WRITES eMMC  *******************
# This is the robust, rollback-safe way to install podd's OWN rootfs: it
# writes to the INACTIVE eMMC slot, keeping the currently-running (stock or
# podd) slot pristine as an instant rollback, then flips the U-Boot pointer with
# the rollback state machine armed (ustate=INSTALLED, bootcount=0). If the new
# slot fails to boot 3x, U-Boot's altbootcmd auto-reverts to the old slot.
#
# It NEVER touches the active slot and NEVER touches mmcblk2p3 (`cage` /
# persistent data). It refuses to run if it cannot positively identify the
# inactive slot. Unlike podd-install.sh (userland, unbrickable), a mistake here
# CAN require serial-U-Boot recovery - read flashing-method.md §3c and §4 first.
# **************************************************************************
#
# The payload is podd-rootfs.tar.gz, the L2 rootfs tarball built by the
# clean-room OS build (os/scripts/build.sh -> dist/podd-rootfs.tar.gz; see
# os/scripts/package-rootfs.sh). It ships as a release asset and on the recovery
# SD. With no --rootfs the script looks in the usual places (see ROOTFS_SEARCH
# below) and, if it finds one, picks up an adjacent .sha256 automatically.
#
# Usage:
#   podd-slot-install.sh                                          # auto-locate
#   podd-slot-install.sh --rootfs /path/podd-rootfs.tar.gz        # install
#   podd-slot-install.sh --rootfs https://host/podd-rootfs.tar.gz # download+install
#   podd-slot-install.sh --confirm-good                           # mark boot healthy
#
# Options:
#   --rootfs PATH|URL   the podd eMMC rootfs.tar.gz (default: auto-locate)
#   --sha256 HEX        expected SHA-256 (default: adjacent <tarball>.sha256)
#   --disk DEV          eMMC whole-disk device, default auto (/dev/mmcblk2|0)
#   --confirm-good      set ustate=OK bootcount=0 (run this from a booted podd)
#   --no-reboot         do everything except the final reboot
#   --yes               skip the interactive confirmation
#   -h | --help         this help
set -eu

# Where an unqualified run looks for the payload, in order: the recovery-SD
# payload dir (its data partition mounts at /data), the stock recovery-SD
# convention, an installed podd tree, next to this script, and the cwd.
ROOTFS_SEARCH="/data/podd-recovery/podd-rootfs.tar.gz
/opt/images/Yocto/podd-rootfs.tar.gz
/opt/podd/podd-rootfs.tar.gz
$(dirname "$0")/podd-rootfs.tar.gz
./podd-rootfs.tar.gz"

ROOTFS=""
EXPECT_SHA=""
DISK=""
CONFIRM_GOOD=0
DO_REBOOT=1
ASSUME_YES=0

log()  { printf '==> %s\n' "$*"; }
warn() { printf '!!  %s\n' "$*" >&2; }
die()  { printf '!!  %s\n' "$*" >&2; exit 1; }
usage() { sed -n '2,45p' "$0" | sed 's/^# \{0,1\}//'; exit "${1:-0}"; }

while [ $# -gt 0 ]; do
  case "$1" in
    --rootfs) ROOTFS="$2"; shift 2 ;;
    --sha256) EXPECT_SHA="$2"; shift 2 ;;
    --disk) DISK="$2"; shift 2 ;;
    --confirm-good) CONFIRM_GOOD=1; shift ;;
    --no-reboot) DO_REBOOT=0; shift ;;
    --yes) ASSUME_YES=1; shift ;;
    -h|--help) usage 0 ;;
    *) die "unknown argument: $1 (try --help)" ;;
  esac
done

[ "$(id -u)" = "0" ] || die "must run as root"
command -v fw_printenv >/dev/null 2>&1 || die "fw_printenv not found - this needs the U-Boot env tools"
command -v fw_setenv   >/dev/null 2>&1 || die "fw_setenv not found - this needs the U-Boot env tools"

# ---- read the active slot from the U-Boot env ----------------------------
# i.MX uses mmcpart (1=slot A, 2=slot B); newer builds also track current_slot.
ACTIVE="$(fw_printenv -n mmcpart 2>/dev/null || echo '')"

# ---- --confirm-good: the booted podd calls this to clear the rollback arm --
if [ "$CONFIRM_GOOD" = "1" ]; then
  log "marking current boot healthy: ustate=OK (0), bootcount=0, upgrade_available=0"
  fw_setenv ustate 0
  fw_setenv bootcount 0
  fw_setenv upgrade_available 0
  log "done - rollback disarmed, this slot is now confirmed good."
  exit 0
fi

# ================= install path (writes the INACTIVE slot) =================
# No --rootfs: look for the payload in the standard places before giving up, and
# say exactly where we looked (this used to be a hard "not built yet" error).
if [ -z "$ROOTFS" ]; then
  for cand in $ROOTFS_SEARCH; do
    if [ -f "$cand" ]; then ROOTFS="$cand"; break; fi
  done
  if [ -n "$ROOTFS" ]; then
    log "found rootfs payload: $ROOTFS"
  else
    warn "no --rootfs given and no podd-rootfs.tar.gz found. Looked in:"
    for cand in $ROOTFS_SEARCH; do warn "    $cand"; done
    die "pass --rootfs PATH|URL. Build one with os/scripts/build.sh (-> dist/podd-rootfs.tar.gz) or take it from a release asset / the recovery SD."
  fi
fi

# Resolve the whole-disk eMMC device.
if [ -z "$DISK" ]; then
  for d in /dev/mmcblk2 /dev/mmcblk0; do
    [ -b "$d" ] && { DISK="$d"; break; }
  done
fi
[ -n "$DISK" ] && [ -b "$DISK" ] || die "could not find eMMC whole-disk device (pass --disk)"

# The rollback story rests entirely on U-Boot's bootcount/altbootcmd
# auto-revert. Confirm it actually exists in THIS unit's env BEFORE writing
# anything: arming ustate=1 on a U-Boot without altbootcmd would leave a bad
# slot boot-looping with no fallback, needing serial-U-Boot recovery (#51).
ALTBOOTCMD="$(fw_printenv -n altbootcmd 2>/dev/null || echo '')"
[ -n "$ALTBOOTCMD" ] || die "this unit's U-Boot env has no 'altbootcmd' - the 3-failed-boots auto-revert this script relies on is absent, so a bad slot could NOT roll back. Refusing. Inspect 'fw_printenv' and see flashing-method.md §3c/§4."

# mmcpart only means "which slot" RELATIVE TO mmcdev. On a rooted stock unit
# mmcdev=2, so mmcpart selects an eMMC slot and everything below lines up. Boot
# the clean-room podd SD instead and mmcdev=1: mmcpart then selects a slot on
# the CARD, so reading the active slot from it and flipping it after writing
# eMMC would silently repoint U-Boot at the wrong device. Refuse rather than
# guess - the SD-booted eMMC install needs mmcdev/mmcblk flipped too, and needs
# some way to tell which eMMC slot still holds stock, neither of which is
# settled (#155). (Booted from the podd SD you already have podd; OS updates
# there go through pod-updater's SD slot writer.)
MMCDEV="$(fw_printenv -n mmcdev 2>/dev/null || echo '')"
DISK_INDEX="${DISK##*mmcblk}"
if [ -n "$MMCDEV" ] && [ "$MMCDEV" != "$DISK_INDEX" ]; then
  die "the U-Boot env in use has mmcdev=$MMCDEV, so its mmcpart selects a slot on /dev/mmcblk${MMCDEV}, not on the install target $DISK. Flipping mmcpart here would point U-Boot at the wrong device. This is the stock-U-Boot eMMC path (mmcdev=2); if you booted the podd SD (mmcdev=1) do not use this script - see docs/RECOVERY.md and flashing-method.md §3c."
fi

# Compute inactive slot + its partition. Refuse if the active slot is unknown.
case "$ACTIVE" in
  1) INACTIVE=2 ;;
  2) INACTIVE=1 ;;
  *) die "cannot determine active slot from 'fw_printenv mmcpart' (got '${ACTIVE:-empty}'). Refusing to guess - identify it manually and set the target with care." ;;
esac
TARGET_PART="${DISK}p${INACTIVE}"
ACTIVE_PART="${DISK}p${ACTIVE}"
CAGE_PART="${DISK}p3"    # persistent data - MUST be preserved, never touched.

[ -b "$TARGET_PART" ] || die "inactive slot device $TARGET_PART not found"
[ "$TARGET_PART" != "$CAGE_PART" ] || die "refusing: target resolved to cage partition $CAGE_PART"
[ "$TARGET_PART" != "$ACTIVE_PART" ] || die "refusing: target equals active slot"

log "eMMC disk       : $DISK"
log "active slot     : $ACTIVE ($ACTIVE_PART)  <- stays pristine (rollback)"
log "target (inactive): $INACTIVE ($TARGET_PART)  <- will be WIPED + written"
log "preserved       : $CAGE_PART (cage/persistent) - untouched"

# ---- fetch the rootfs if it is a URL, then verify it ----------------------
WORK="$(mktemp -d "${TMPDIR:-/tmp}/podd-slot.XXXXXX")"
MNT="$WORK/mnt"
cleanup() { umount "$MNT" 2>/dev/null || true; rm -rf "$WORK"; }
trap cleanup EXIT INT TERM

fetch() { # <url> <dest>  -> 0 on success
  if command -v curl >/dev/null 2>&1; then curl -fsSL -o "$2" "$1"
  elif command -v wget >/dev/null 2>&1; then wget -q -O "$2" "$1"
  else die "need curl or wget to download $1"; fi
}

TARBALL="$ROOTFS"
SHAFILE="$ROOTFS.sha256"
case "$ROOTFS" in
  http://*|https://*)
    TARBALL="$WORK/podd-rootfs.tar.gz"
    SHAFILE="$WORK/podd-rootfs.tar.gz.sha256"
    log "downloading rootfs: $ROOTFS"
    fetch "$ROOTFS" "$TARBALL"
    # Best-effort: the release/recovery-SD layout ships <tarball>.sha256 beside
    # it. Absent is fine (we warn below); present means we verify for free.
    fetch "$ROOTFS.sha256" "$SHAFILE" 2>/dev/null || rm -f "$SHAFILE" ;;
esac
[ -f "$TARBALL" ] || die "rootfs tarball not found: $TARBALL"

# os/scripts/package-rootfs.sh emits `<sha>  podd-rootfs.tar.gz` next to the
# tarball; use it when the caller did not pass --sha256 explicitly.
if [ -z "$EXPECT_SHA" ] && [ -f "$SHAFILE" ]; then
  EXPECT_SHA="$(cut -d' ' -f1 < "$SHAFILE")"
  log "using digest from $SHAFILE"
fi

if [ -n "$EXPECT_SHA" ]; then
  command -v sha256sum >/dev/null 2>&1 || die "sha256sum missing; cannot verify --sha256"
  printf '%s  %s\n' "$EXPECT_SHA" "$TARBALL" | sha256sum -c - >/dev/null \
    || die "SHA-256 MISMATCH on rootfs - refusing to write eMMC"
  log "rootfs integrity OK ($EXPECT_SHA)"
else
  warn "no --sha256 given: writing rootfs WITHOUT integrity verification"
fi

# ---- back up the env + partition table BEFORE writing --------------------
BACKUP="/opt/podd/backup/slot-$(date +%Y%m%d-%H%M%S)"
mkdir -p "$BACKUP"
fw_printenv > "$BACKUP/fw_printenv.txt" 2>/dev/null || true
dd if="$DISK" of="$BACKUP/$(basename "$DISK")-mbr.bin" bs=512 count=1 2>/dev/null || true
{ command -v fdisk >/dev/null 2>&1 && fdisk -l "$DISK"; } > "$BACKUP/parttable.txt" 2>/dev/null || true
log "backed up env + MBR to $BACKUP"

# ---- final confirmation --------------------------------------------------
if [ "$ASSUME_YES" != "1" ]; then
  printf '\nAbout to mkfs.ext4 and overwrite %s (inactive slot %s).\nThe active slot %s and %s (cage) are NOT touched.\nType YES to proceed: ' \
    "$TARGET_PART" "$INACTIVE" "$ACTIVE_PART" "$CAGE_PART"
  read -r ans
  [ "$ans" = "YES" ] || die "aborted by user"
fi

# ---- write the inactive slot ---------------------------------------------
umount "$TARGET_PART" 2>/dev/null || true
LABEL="rootfs_$( [ "$INACTIVE" = "1" ] && echo a || echo b )"
log "mkfs.ext4 -L $LABEL $TARGET_PART"
mkfs.ext4 -F -L "$LABEL" "$TARGET_PART"
mkdir -p "$MNT"
mount "$TARGET_PART" "$MNT"
log "extracting rootfs into $TARGET_PART (must include /boot/Image.gz + DTB)"
tar -xzf "$TARBALL" -C "$MNT"
sync

# Sanity-check the extracted tree BEFORE flipping the boot pointer: a
# truncated, wrong, or kernel-less tarball must fail HERE, leaving the
# pointer on the known-good active slot, not at boot time (#51).
[ -s "$MNT/boot/Image.gz" ] \
  || die "extracted rootfs has no /boot/Image.gz - boot pointer NOT flipped, active slot untouched. Bad tarball?"
DTB_COUNT="$(find "$MNT/boot" -name '*.dtb' 2>/dev/null | wc -l)"
[ "$DTB_COUNT" -gt 0 ] \
  || die "extracted rootfs has no device tree (*.dtb) under /boot - boot pointer NOT flipped, active slot untouched. Bad tarball?"
log "slot sanity OK: /boot/Image.gz present, $DTB_COUNT DTB(s)"

umount "$MNT"
command -v e2fsck >/dev/null 2>&1 && e2fsck -pf "$TARGET_PART" >/dev/null 2>&1 || true

# ---- flip the pointer + ARM rollback (writes the U-Boot env) -------------
# On first boot podd should run `podd-slot-install.sh --confirm-good` to set
# ustate=OK; if it never does and the slot fails 3x, altbootcmd reverts mmcpart.
log "flipping boot pointer to slot $INACTIVE with rollback armed"
fw_setenv mmcpart "$INACTIVE"
fw_setenv next_mmcpart "$ACTIVE"
fw_setenv ustate 1              # INSTALLED
fw_setenv upgrade_available 1
fw_setenv bootcount 0
# Newer builds also read current_slot; keep it consistent (a=1, b=2).
fw_setenv current_slot "$( [ "$INACTIVE" = "1" ] && echo a || echo b )" 2>/dev/null || true

cat <<EOF

==========================================================================
 podd rootfs written to slot $INACTIVE ($TARGET_PART), boot pointer flipped.
   rollback : automatic to slot $ACTIVE after 3 failed boots (altbootcmd)
   confirm  : once booted into podd, run:
                podd-slot-install.sh --confirm-good
   backup   : $BACKUP
==========================================================================
EOF

if [ "$DO_REBOOT" = "1" ]; then
  log "rebooting into the new slot in 5s (Ctrl-C to cancel)"
  sleep 5
  reboot
else
  log "--no-reboot: reboot manually to boot the new slot"
fi
