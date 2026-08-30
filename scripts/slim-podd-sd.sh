#!/usr/bin/env bash
#
# slim-podd-sd.sh - produce a SLIMMED podd SD image from a full one.
#
# The full podd-sd.img is a clone of the 16 GB stock SD: imx-boot + U-Boot env +
# p1 (podd rootfs) + the stock p2/p3 that podd never boots. That's ~14.8 GiB and
# needs a genuine 16 GB card. This produces a ~6.6 GiB image containing only what
# actually boots podd:
#     imx-boot @0x8400  +  U-Boot env @0x400000  +  p1 (rootfs)
# It fits any >=8 GB card, writes in ~1 minute, and the partition table is
# trimmed to p1 alone so there are no dangling p2/p3 entries.
#
# It also HARDENS the A/B rollback: the stock env's altbootcmd flips mmcpart 1<->2
# after bootlimit boot failures. With p2 dropped, that would jump to an absent
# partition. We rewrite altbootcmd to just reset bootcount and retry p1.
#
# Everything else (imx-boot, the whole rootfs, mmcdev=1/mmcblk=1/mmcpart=1) is
# copied BYTE-FOR-BYTE from the (already verified) full image and re-verified here
# by comparing SHA-256 of the imx-boot and p1 regions.
#
# Unprivileged: dd + sfdisk + mkenvimage on plain files, no root, no loop mounts.
#
# Diagnostics: if scripts/patch-podd-sd-diag.sh was already run on FULL_IMAGE,
# its /opt/podd/bootlog.sh survives the slim untouched (it's part of p1, which
# is copied byte-for-byte). Running patch-diag BEFORE slim is recommended but
# not required - an un-patched source image slims fine, it just won't
# self-log its boot (this script warns rather than failing in that case).
#
# Usage: scripts/slim-podd-sd.sh [FULL_IMAGE]      # default dist/podd-sd.img
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO="$(cd "$SCRIPT_DIR/.." && pwd)"
case "${1:-}" in
  -h|--help) sed -n '2,23p' "$0" | sed 's/^#\{0,1\} \{0,1\}//'; exit 0 ;;
esac
SRC="${1:-$REPO/dist/podd-sd.img}"
OUT="${PODD_SLIM_OUT:-$REPO/dist/podd-sd-slim.img}"

log()  { printf '==> %s\n' "$*"; }
warn() { printf '!!  %s\n' "$*" >&2; }
die()  { printf '!!  %s\n' "$*" >&2; exit 1; }

if [ ! -f "$SRC" ]; then
  [ -f "$SRC.gz" ] && die "source image not found: $SRC (found $SRC.gz - gunzip it first)"
  die "source image not found: $SRC"
fi
command -v nix >/dev/null 2>&1 || die "nix not found"
log "resolving tools via nix (e2fsprogs, util-linux, ubootTools, gzip)"
# shellcheck disable=SC2016  # $PATH must expand inside the nix shell, not here.
TOOL_PATH="$(nix shell nixpkgs#e2fsprogs nixpkgs#util-linux nixpkgs#ubootTools nixpkgs#coreutils nixpkgs#gzip \
  --command sh -c 'printf %s "$PATH"')" || die "nix tool resolution failed"
export PATH="$TOOL_PATH:$PATH"
for t in dd sfdisk mkenvimage debugfs e2fsck sha256sum gzip; do
  command -v "$t" >/dev/null 2>&1 || die "tool '$t' missing"
done

WORK="$(mktemp -d "${TMPDIR:-/tmp}/podd-slim.XXXXXX")"; trap 'rm -rf "$WORK"' EXIT

# --- geometry (parse p1 from the source) ------------------------------------
LABEL_ID="$(sfdisk -d "$SRC" | sed -n 's/^label-id:[[:space:]]*//p')"
p1="$(sfdisk -d "$SRC" | grep -E 'img1[[:space:]]*:')" || die "no p1 in source"
P1_START="$(echo "$p1" | sed -n 's/.*start=[[:space:]]*\([0-9]*\).*/\1/p')"
P1_SIZE="$(echo "$p1"  | sed -n 's/.*size=[[:space:]]*\([0-9]*\).*/\1/p')"
[ -n "$P1_START" ] && [ -n "$P1_SIZE" ] || die "cannot parse p1 geometry"
P1_END=$((P1_START + P1_SIZE))                     # first sector past p1 = new image length
log "p1: start=$P1_START size=$P1_SIZE end=$P1_END sectors ($((P1_END*512)) bytes); label-id=$LABEL_ID"

ENV_SIZE=$((0x1000)); ENV_SEEK4K=$((0x400000 / 0x1000))   # env @0x400000, 4 KiB blocks

# --- 1. copy [0 .. end of p1) : imx-boot + env + rootfs ---------------------
log "copying head+env+rootfs into $OUT ($((P1_END*512)) bytes)"
dd if="$SRC" of="$OUT" bs=512 count="$P1_END" status=none

# --- 2. trim the partition table to p1 only ---------------------------------
log "rewriting partition table -> p1 only"
{ echo "label: dos"
  echo "label-id: $LABEL_ID"
  echo "unit: sectors"
  echo "start=$P1_START, size=$P1_SIZE, type=83"
} | sfdisk "$OUT" >/dev/null 2>&1 || die "sfdisk repartition failed"

# --- 3. harden the env: altbootcmd = reset bootcount + retry p1 -------------
log "hardening U-Boot env (altbootcmd retries p1 instead of absent p2)"
dd if="$OUT" of="$WORK/env.bin" bs="$ENV_SIZE" skip="$ENV_SEEK4K" count=1 status=none
[ "$(wc -c < "$WORK/env.bin")" -eq "$ENV_SIZE" ] || die "env read wrong size"
tail -c +5 "$WORK/env.bin" | tr '\0' '\n' | grep -a '=' > "$WORK/env.txt"
grep -q '^mmcdev=1$' "$WORK/env.txt" || die "source env is not the podd env (mmcdev!=1) - refusing"
sed -i 's|^altbootcmd=.*|altbootcmd=setenv bootcount 0; saveenv; run bootcmd|' "$WORK/env.txt"
mkenvimage -s "$ENV_SIZE" -p 0x00 -o "$WORK/env-new.bin" "$WORK/env.txt" || die "mkenvimage failed"
[ "$(wc -c < "$WORK/env-new.bin")" -eq "$ENV_SIZE" ] || die "rebuilt env wrong size"
dd if="$WORK/env-new.bin" of="$OUT" bs="$ENV_SIZE" seek="$ENV_SEEK4K" count=1 conv=notrunc status=none

# --- 4. verify: imx-boot + p1 byte-identical to source; env/ptable as intended
log "verifying imx-boot + rootfs are byte-identical to the source"
rng_sha() { dd if="$1" bs=512 skip="$2" count="$3" status=none | sha256sum | awk '{print $1}'; }
# imx-boot region: sector 66 (0x8400) .. just before env (sector 8192)
IB_SRC="$(rng_sha "$SRC" 66 8126)"; IB_OUT="$(rng_sha "$OUT" 66 8126)"
[ "$IB_SRC" = "$IB_OUT" ] || die "imx-boot region MISMATCH ($IB_SRC != $IB_OUT)"
# rootfs p1
P1_SRC="$(rng_sha "$SRC" "$P1_START" "$P1_SIZE")"; P1_OUT="$(rng_sha "$OUT" "$P1_START" "$P1_SIZE")"
[ "$P1_SRC" = "$P1_OUT" ] || die "rootfs p1 MISMATCH ($P1_SRC != $P1_OUT)"
log "imx-boot + rootfs verified identical (p1 sha256=$P1_OUT)"

# partition table + env sanity
sfdisk -d "$OUT" | grep -qE 'img2[[:space:]]*:' && die "p2 still present after trim"
ENV_RB="$(dd if="$OUT" bs="$ENV_SIZE" skip="$ENV_SEEK4K" count=1 status=none | tail -c +5 | tr '\0' '\n')"
echo "$ENV_RB" | grep -qx 'mmcdev=1'  || die "env verify: mmcdev!=1"
echo "$ENV_RB" | grep -qx 'mmcblk=1'  || die "env verify: mmcblk!=1"
echo "$ENV_RB" | grep -qx 'mmcpart=1' || die "env verify: mmcpart!=1"
echo "$ENV_RB" | grep -q  '^altbootcmd=setenv bootcount 0; saveenv; run bootcmd$' || die "env verify: altbootcmd not hardened"

# rootfs contents sanity (podd + kernel present; diag logger is optional).
CARVE="$WORK/p1.img"
dd if="$OUT" of="$CARVE" bs=512 skip="$P1_START" count="$P1_SIZE" status=none
e2fsck -fy "$CARVE" >/dev/null 2>&1 || true
debugfs -R "stat /opt/podd/current/rootfs/podd" "$CARVE" >/dev/null 2>&1 \
  || debugfs -R "ls /opt/podd" "$CARVE" 2>/dev/null | grep -q current || die "podd payload missing"
debugfs -R "stat /boot/Image.gz" "$CARVE" >/dev/null 2>&1 || die "/boot/Image.gz missing"
# The diag self-logger (/opt/podd/bootlog.sh) is installed by the separate
# scripts/patch-podd-sd-diag.sh, not by build-podd-sd.sh, so a freshly built
# (un-patched) image legitimately lacks it. Warn rather than fail: slimming
# an un-patched image is valid, it just won't self-log its boot (podd#50).
DIAG_PRESENT=0
if debugfs -R "stat /opt/podd/bootlog.sh" "$CARVE" 2>/dev/null | grep -q 'Mode:  0755'; then
  DIAG_PRESENT=1
else
  warn "diag bootlog.sh not present in source image - slim image will not self-log its boot."
  warn "Run scripts/patch-podd-sd-diag.sh on the image BEFORE slimming if you need boot diagnostics."
fi
if [ "$DIAG_PRESENT" = 1 ]; then
  log "rootfs contents verified (podd payload, diag logger, kernel present)"
else
  log "rootfs contents verified (podd payload, kernel present; no diag logger)"
fi

# --- 5. compress + manifest -------------------------------------------------
log "compressing -> $OUT.gz"
gzip -c "$OUT" > "$OUT.gz"
RAW_BYTES="$(wc -c < "$OUT")"; GZ_BYTES="$(wc -c < "$OUT.gz")"
RAW_SHA="$(sha256sum "$OUT" | awk '{print $1}')"; GZ_SHA="$(sha256sum "$OUT.gz" | awk '{print $1}')"
{
  echo "podd SLIM SD image"
  echo "=================="
  echo "source     : $SRC"
  echo "raw bytes  : $RAW_BYTES ($(awk "BEGIN{printf \"%.2f\", $RAW_BYTES/1073741824}") GiB)"
  echo "raw sha256 : $RAW_SHA"
  echo "gz  bytes  : $GZ_BYTES"
  echo "gz  sha256 : $GZ_SHA"
  echo "layout     : imx-boot@0x8400 + U-Boot env@0x400000 + p1 (rootfs). p2/p3 dropped."
  echo "env        : mmcdev=1 mmcblk=1 mmcpart=1; altbootcmd hardened (retry p1)."
  if [ "$DIAG_PRESENT" = 1 ]; then
    echo "diag       : bootlog.sh present - image self-logs its boot to /opt/podd/bootlog"
  else
    echo "diag       : bootlog.sh NOT present - run scripts/patch-podd-sd-diag.sh before slim for boot diagnostics"
  fi
  echo "fits       : any card >= $RAW_BYTES bytes (an 8 GB card is plenty)."
  echo "write      : sudo dd if=$(basename "$OUT") of=/dev/sdX bs=4M conv=fsync status=progress; sync"
  echo "verify     : sudo cmp -n $RAW_BYTES $(basename "$OUT") /dev/sdX && echo OK"
} > "${OUT%.img}.manifest.txt"

log "DONE"
cat "${OUT%.img}.manifest.txt"
