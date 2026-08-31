#!/usr/bin/env bash
#
# build-podd-sd.sh - build a self-contained bootable "podd SD card" image for the
# Eight Sleep i.MX8M-Mini / Variscite SD-variant hub (Pod 3-SD / Pod 4).
#
# WHAT IT PRODUCES
#   dist/podd-sd.img.gz AND the raw dist/podd-sd.img alongside it - a
#   full-device image you `dd` to a microSD (gunzip the .gz first, or use the
#   raw .img directly). Inserting that card (swapping the stock one) makes
#   the Pod boot a COMPLETE podd system entirely from the SD. The eMMC is
#   NEVER written by this build, and the running system is configured so it
#   is not written at runtime either, so swapping the original card back
#   reverts to stock instantly. The raw .img is also what
#   scripts/patch-podd-sd-diag.sh and scripts/slim-podd-sd.sh expect as input.
#
# HOW IT WORKS (see docs/SD-BOOT.md for the full rationale)
#   The U-Boot environment lives on the SD at offset 0x400000
#   (/etc/fw_env.config -> /dev/mmcblk1 0x400000 0x1000). Stock env has
#   mmcdev=2/mmcblk=2 -> boot rootfs from eMMC. We flip it to mmcdev=1/mmcblk=1
#   -> boot rootfs from the SD's own p1. Everything else in the env is kept
#   byte-for-byte identical to the owner's stock SD env.
#
#   The SD's p1 is replaced with a "podd-ified" clone of the STOCK eMMC rootfs
#   (mmcblk2p1): the exact working Yocto OS - stock kernel /boot/Image.gz, DTB,
#   drivers, ownership, setuid/capabilities - modified IN PLACE (never extracted
#   as an unprivileged user, which would destroy ownership) to add podd under
#   /opt/podd and to mask the vendor OTA/control stack. imx-boot @0x8400, the
#   partition table, p2 and p3 (cage) of the stock SD are left untouched.
#
# UNPRIVILEGED: no root, no loop mounts. ext4 images are carved with dd,
# modified with debugfs, and sized with resize2fs - all on plain files.
#
# Tools come from nix on demand:
#   e2fsprogs (debugfs/mke2fs/resize2fs/e2fsck/dumpe2fs), ubootTools
#   (mkenvimage), util-linux (sfdisk), coreutils, gzip.
#
# Usage:
#   scripts/build-podd-sd.sh [--out FILE] [--work DIR] [--keep-work]
#
# Inputs are resolved from the repo + the sibling backup/ dir by default and can
# be overridden via the PODD_SD_* environment variables below.
set -euo pipefail

# ---------------------------------------------------------------------------
# Paths / parameters
# ---------------------------------------------------------------------------
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO="$(cd "$SCRIPT_DIR/.." && pwd)"
BACKUP_DIR="${PODD_SD_BACKUP:-$REPO/../backup}"

# Owner's real backups (templates / OS source).
EMMC_GZ="${PODD_SD_EMMC_GZ:-$BACKUP_DIR/mmcblk2.img.gz}"       # eMMC gold master
SD_GZ="${PODD_SD_SD_GZ:-$BACKUP_DIR/mmcblk1-sd.img.gz}"        # stock SD (template)
ENV_GZ="${PODD_SD_ENV_GZ:-$BACKUP_DIR/sd-uboot-env-0x400000.bin.gz}" # stock env blob
EMMC_PT="${PODD_SD_EMMC_PT:-$BACKUP_DIR/mmcblk2-parttable.txt}" # sfdisk dump of eMMC

# podd payload (built via `nix build .#podd-aarch64` and `nix build .#ui`).
PODD_BIN="${PODD_SD_PODD_BIN:-$REPO/result-podd/bin/podd}"
UI_DIR="${PODD_SD_UI_DIR:-$REPO/result-ui}"
CONFIG_RON="${PODD_SD_CONFIG:-$REPO/config.pod4.example.ron}"  # owner's backups are a Pod 4 SD hub
CONFIG_VARIANT="${PODD_SD_VARIANT:-pod4}"
PODD_SERVICE="${PODD_SD_SERVICE:-$REPO/install/podd.service}"
VERSION="${PODD_SD_VERSION:-$(git -C "$REPO" describe --tags --always --dirty 2>/dev/null || echo 0.0.1)-sd}"

OUT="${PODD_SD_OUT:-$REPO/dist/podd-sd.img.gz}"
if [ -n "${PODD_SD_WORK:-}" ]; then
  WORK="$PODD_SD_WORK"
else
  WORK="$(mktemp -d "${TMPDIR:-/tmp}/podd-sd.XXXXXX")"
fi
KEEP_WORK=0

# U-Boot env geometry (from /etc/fw_env.config -> /dev/mmcblk1 0x400000 0x1000).
ENV_OFFSET=$((0x400000))
ENV_SIZE=$((0x1000))

# Vendor units to MASK (symlink -> /dev/null) so they cannot fight podd, drive
# the hardware, auto-revert to stock, or - crucially - TOUCH THE eMMC. Mirrors
# install/podd-install.sh VENDOR_UNITS, plus:
#   persistent-manager.service / persistent.mount: these mount+fsck (and can
#     reformat) /dev/mmcblk2p3 (the eMMC cage). Masking them is what keeps the
#     eMMC 100% untouched at RUNTIME, not just at build time.
#   free-sleep*: found pre-installed on this eMMC image; it binds port 3000
#     (which podd also uses) and drives the same MCUs, so it must not run.
MASK_UNITS_DEFAULT="swupdate.service swupdate.socket swupdate-progress.service \
defibrillator.service dac.service frank.service capybara.service telegraf.service \
vector.service frankenfirmware.service eight-kernel.service \
persistent-manager.service persistent.mount \
free-sleep.service free-sleep-stream.service free-sleep-update.service"
MASK_UNITS="${PODD_SD_MASK_UNITS:-$MASK_UNITS_DEFAULT}"

log()  { printf '==> %s\n' "$*"; }
warn() { printf '!!  %s\n' "$*" >&2; }
die()  { printf '!!  %s\n' "$*" >&2; exit 1; }

while [ $# -gt 0 ]; do
  case "$1" in
    --out)       OUT="$2"; shift 2 ;;
    --work)      WORK="$2"; shift 2 ;;
    --keep-work) KEEP_WORK=1; shift ;;
    -h|--help)   sed -n '2,38p' "$0" | sed 's/^#\{0,1\} \{0,1\}//'; exit 0 ;;
    *) die "unknown argument: $1 (try --help)" ;;
  esac
done

# ---------------------------------------------------------------------------
# Bring the required tools onto PATH via a single nix shell resolution.
# ---------------------------------------------------------------------------
NIX_PKGS=(nixpkgs#e2fsprogs nixpkgs#ubootTools nixpkgs#util-linux
          nixpkgs#dosfstools nixpkgs#coreutils nixpkgs#gzip)
log "resolving build tools via nix (e2fsprogs, ubootTools, util-linux, ...)"
command -v nix >/dev/null 2>&1 || die "nix not found on PATH"
# shellcheck disable=SC2016  # $PATH must expand inside the nix shell, not here.
TOOL_PATH="$(nix shell "${NIX_PKGS[@]}" --command sh -c 'printf %s "$PATH"')" \
  || die "failed to realise build tools via nix"
export PATH="$TOOL_PATH"
for t in debugfs mke2fs resize2fs e2fsck dumpe2fs mkenvimage sfdisk gzip; do
  command -v "$t" >/dev/null 2>&1 || die "tool '$t' missing after nix resolution"
done

# ---------------------------------------------------------------------------
# Validate inputs.
# ---------------------------------------------------------------------------
for f in "$EMMC_GZ" "$SD_GZ" "$ENV_GZ" "$EMMC_PT" "$PODD_BIN" "$CONFIG_RON" "$PODD_SERVICE"; do
  [ -e "$f" ] || die "missing required input: $f"
done
[ -d "$UI_DIR" ] || die "missing UI dir: $UI_DIR (run: nix build .#ui)"
file "$PODD_BIN" 2>/dev/null | grep -q aarch64 || warn "podd binary does not look aarch64: $PODD_BIN"

mkdir -p "$WORK" "$(dirname "$OUT")"
cleanup() { [ "$KEEP_WORK" = 1 ] || rm -rf "$WORK"; }
trap cleanup EXIT

SD_IMG="$WORK/sd.img"                 # decompressed stock SD (mutated in place = our image)
ROOTFS="$WORK/rootfs_p1.img"          # carved eMMC p1, podd-ified
ENV_BIN_SRC="$WORK/sd-env.bin"        # stock env blob
ENV_TXT="$WORK/env.txt"               # editable env text
ENV_BIN_OUT="$WORK/env-podd.bin"      # rebuilt env

# ---------------------------------------------------------------------------
# STEP 1 - carve the STOCK rootfs out of the eMMC image (partition 1).
# Offsets come from the recorded eMMC partition table (sfdisk dump); we pipe the
# decompression through dd so we never store the full ~15 GiB eMMC image.
# ---------------------------------------------------------------------------
p1_line="$(grep -E 'mmcblk2p1[[:space:]]*:' "$EMMC_PT")" || die "no p1 in $EMMC_PT"
EMMC_P1_START="$(echo "$p1_line" | sed -n 's/.*start=[[:space:]]*\([0-9]*\).*/\1/p')"
EMMC_P1_SIZE="$(echo "$p1_line" | sed -n 's/.*size=[[:space:]]*\([0-9]*\).*/\1/p')"
[ -n "$EMMC_P1_START" ] && [ -n "$EMMC_P1_SIZE" ] || die "could not parse eMMC p1 geometry"
log "eMMC p1: start=$EMMC_P1_START size=$EMMC_P1_SIZE sectors -> carving stock rootfs"
# dd stops after count, so gzip is killed by SIGPIPE (nonzero) - that's expected;
# check dd's own exit status via PIPESTATUS rather than the pipeline's.
set +o pipefail
gzip -dc "$EMMC_GZ" \
  | dd bs=512 skip="$EMMC_P1_START" count="$EMMC_P1_SIZE" of="$ROOTFS" status=none
carve_rc="${PIPESTATUS[1]}"
set -o pipefail
[ "$carve_rc" -eq 0 ] || die "failed to carve eMMC p1 (dd rc=$carve_rc)"

# Replay the journal / clean the filesystem so debugfs+resize2fs can work on it.
log "e2fsck (replay journal) on carved rootfs"
e2fsck -fy "$ROOTFS" || true   # -y: rc 1/2 = fixed, still usable
# Sanity: this must actually be the Yocto rootfs.
debugfs -R "stat /boot/Image.gz" "$ROOTFS" >/dev/null 2>&1 || die "carved p1 has no /boot/Image.gz"
debugfs -R "ls /opt/eight" "$ROOTFS" >/dev/null 2>&1 || die "carved p1 has no /opt/eight - wrong partition?"
log "verified carved rootfs (has /boot/Image.gz and /opt/eight)"

# ---------------------------------------------------------------------------
# STEP 2 - decompress the stock SD image (our template + final image) and read
# its partition table to learn where p1 lives and how big it may be.
# ---------------------------------------------------------------------------
log "decompressing stock SD template"
gzip -dc "$SD_GZ" > "$SD_IMG"
sd_p1_line="$(sfdisk -d "$SD_IMG" | grep -E 'img1[[:space:]]*:' )" || die "no p1 in SD image"
SD_P1_START="$(echo "$sd_p1_line" | sed -n 's/.*start=[[:space:]]*\([0-9]*\).*/\1/p')"
SD_P1_SIZE="$(echo "$sd_p1_line" | sed -n 's/.*size=[[:space:]]*\([0-9]*\).*/\1/p')"
[ -n "$SD_P1_START" ] && [ -n "$SD_P1_SIZE" ] || die "could not parse SD p1 geometry"
[ $((SD_P1_SIZE % 8)) -eq 0 ] || die "SD p1 size ($SD_P1_SIZE sectors) not a multiple of 8 (4K blocks)"
SD_P1_BLOCKS4K=$((SD_P1_SIZE / 8))     # 512-byte sectors -> 4096-byte fs blocks
log "SD p1: start=$SD_P1_START size=$SD_P1_SIZE sectors ($SD_P1_BLOCKS4K x 4K blocks)"

# ---------------------------------------------------------------------------
# STEP 3 - shrink the rootfs to fit the SD's (smaller) p1, then podd-ify it.
# ---------------------------------------------------------------------------
USED_BLOCKS="$(dumpe2fs -h "$ROOTFS" 2>/dev/null \
  | awk -F: '/Block count/{tot=$2} /Free blocks/{free=$2} END{print tot-free}')"
[ -n "$USED_BLOCKS" ] || die "could not read rootfs usage"
[ "$USED_BLOCKS" -lt "$SD_P1_BLOCKS4K" ] \
  || die "rootfs uses $USED_BLOCKS blocks > SD p1 capacity $SD_P1_BLOCKS4K"
log "resizing rootfs to $SD_P1_BLOCKS4K blocks to fill SD p1 (used=$USED_BLOCKS)"
resize2fs "$ROOTFS" "${SD_P1_BLOCKS4K}" || die "resize2fs failed"

# ---- assemble the debugfs command stream (podd install + vendor mask) -------
DBG="$WORK/debugfs.cmds"
REL="/opt/podd/releases/$VERSION"
RFS="$REL/rootfs"
: > "$DBG"
emit() { printf '%s\n' "$*" >> "$DBG"; }

log "podd-ifying rootfs: install /opt/podd/$VERSION, config, service, masks"
# podd on-device layout (matches install/podd-install.sh + pod_update_agent config).
for d in /opt/podd /opt/podd/releases "$REL" "$RFS" "$RFS/ui"; do
  emit "mkdir $d"
done
# podd binary (make it executable via sif; debugfs write defaults to 0644 root:root).
emit "write $PODD_BIN $RFS/podd"
emit "sif $RFS/podd mode 0100755"
# UI tree (SPA served by podd). Create sub-dirs first, then files.
while IFS= read -r sub; do
  [ "$sub" = "." ] && continue
  emit "mkdir $RFS/ui/$sub"
done < <(cd "$UI_DIR" && find . -mindepth 1 -type d | sed 's|^\./||' | sort)
while IFS= read -r rel; do
  emit "write $UI_DIR/$rel $RFS/ui/$rel"
done < <(cd "$UI_DIR" && find . -type f | sed 's|^\./||' | sort)
# Config: persistent copy at /opt/podd/config.ron (outside `current`) + a
# variant copy inside the bundle for parity with the OTA layout.
emit "write $CONFIG_RON /opt/podd/config.ron"
emit "write $CONFIG_RON $RFS/config.${CONFIG_VARIANT}.ron"
# `current` symlink -> this release (relative, matches installer semantics).
emit "symlink /opt/podd/current releases/$VERSION"
# systemd unit + enable it in multi-user.target.wants.
emit "write $PODD_SERVICE /etc/systemd/system/podd.service"
emit "write $PODD_SERVICE $RFS/podd.service"
emit "symlink /etc/systemd/system/multi-user.target.wants/podd.service /etc/systemd/system/podd.service"

# ---- mask vendor / eMMC-touching / conflicting units ------------------------
# Snapshot what already exists directly in /etc/systemd/system so we only `rm`
# (unlink) real entries before re-pointing them at /dev/null (idempotent mask).
ETC_LIST="$WORK/etc-systemd.ls"
debugfs -R "ls -l /etc/systemd/system" "$ROOTFS" 2>/dev/null \
  | awk '{print $NF}' > "$ETC_LIST" || true
for u in $MASK_UNITS; do
  if grep -qx "$u" "$ETC_LIST"; then
    emit "rm /etc/systemd/system/$u"
  fi
  emit "symlink /etc/systemd/system/$u /dev/null"
done

# Apply all modifications in one pass.
log "applying $(wc -l < "$DBG") debugfs commands"
debugfs -w -f "$DBG" "$ROOTFS" > "$WORK/debugfs.log" 2>&1 \
  || { cat "$WORK/debugfs.log" >&2; die "debugfs modifications failed"; }
# debugfs reports per-command errors on stdout without failing; surface real ones.
if grep -iE 'file exists|no space|could not|read error|write error|invalid' "$WORK/debugfs.log" \
     | grep -viq 'File exists while' ; then
  grep -iE 'file exists|no space|could not|read error|write error|invalid' "$WORK/debugfs.log" >&2 || true
fi

# Verify the payload really landed, then final fsck.
debugfs -R "stat $RFS/podd" "$ROOTFS" 2>/dev/null | grep -q 'Mode:  0755' \
  || die "podd binary missing or not executable after install"
debugfs -R "stat /opt/podd/config.ron" "$ROOTFS" >/dev/null 2>&1 \
  || die "config.ron missing after install"
log "e2fsck (final) on podd-ified rootfs"
e2fsck -fy "$ROOTFS" || true

# ---------------------------------------------------------------------------
# STEP 4 - splice the podd rootfs into the SD template's p1 region.
# resize2fs shrank the FILESYSTEM but not the image file, so copy exactly the
# first SD_P1_SIZE sectors (= the resized fs) into p1. imx-boot@0x8400, the MBR,
# p2 and p3 are untouched.
# ---------------------------------------------------------------------------
log "writing podd rootfs into SD p1 (seek=$SD_P1_START count=$SD_P1_SIZE sectors)"
dd if="$ROOTFS" of="$SD_IMG" bs=512 seek="$SD_P1_START" count="$SD_P1_SIZE" \
   conv=notrunc status=none || die "failed to splice rootfs into SD image"

# ---------------------------------------------------------------------------
# STEP 5 - rewrite the U-Boot env at 0x400000 so the SD boots ITS OWN rootfs.
# Start from the owner's exact stock env blob; change only mmcdev/mmcblk (2->1),
# pin mmcpart=1 and mmcautodetect=no; rebuild with the SAME size + zero padding
# so every other byte is identical to stock.
# ---------------------------------------------------------------------------
log "rebuilding U-Boot env: mmcdev=1 mmcblk=1 mmcpart=1 (boot rootfs from SD)"
gzip -dc "$ENV_GZ" > "$ENV_BIN_SRC"
[ "$(wc -c < "$ENV_BIN_SRC")" -eq "$ENV_SIZE" ] || die "stock env is not $ENV_SIZE bytes"
# Drop the 4-byte CRC, split NUL-separated entries into lines, keep key=value.
tail -c +5 "$ENV_BIN_SRC" | tr '\0' '\n' | grep -a '=' > "$ENV_TXT"
sed -i -e 's/^mmcdev=.*/mmcdev=1/' \
       -e 's/^mmcblk=.*/mmcblk=1/' \
       -e 's/^mmcpart=.*/mmcpart=1/' \
       -e 's/^mmcautodetect=.*/mmcautodetect=no/' "$ENV_TXT"
if ! grep -q '^mmcdev=1$' "$ENV_TXT" || ! grep -q '^mmcblk=1$' "$ENV_TXT"; then
  die "env edit did not take"
fi
mkenvimage -s "$ENV_SIZE" -p 0x00 -o "$ENV_BIN_OUT" "$ENV_TXT" \
  || die "mkenvimage failed"
[ "$(wc -c < "$ENV_BIN_OUT")" -eq "$ENV_SIZE" ] || die "rebuilt env wrong size"
ENV_SEEK=$((ENV_OFFSET / ENV_SIZE))
dd if="$ENV_BIN_OUT" of="$SD_IMG" bs="$ENV_SIZE" seek="$ENV_SEEK" count=1 \
   conv=notrunc status=none || die "failed to write env into SD image"

# Verify the env reads back correctly from the image. NOTE: fw_printenv is
# unreliable on a plain file with this single-env format (it fails even on the
# known-good stock env), so we read the region back and parse it directly the
# same way U-Boot does: skip the 4-byte CRC header, split NUL-separated
# key=value pairs. The CRC itself was written correctly by mkenvimage (verified
# out-of-band: U-Boot's crc32 over env[4:] matches the stored value).
ENV_READBACK="$WORK/env-readback.bin"
dd if="$SD_IMG" of="$ENV_READBACK" bs="$ENV_SIZE" skip="$ENV_SEEK" count=1 status=none
ENV_CHECK="$(tail -c +5 "$ENV_READBACK" | tr '\0' '\n' | grep -aE '^mmc(dev|blk|part|autodetect)=' || true)"
echo "$ENV_CHECK" | grep -qx 'mmcdev=1'  || die "env verify: mmcdev!=1 (got: $(echo "$ENV_CHECK" | tr '\n' ' '))"
echo "$ENV_CHECK" | grep -qx 'mmcblk=1'  || die "env verify: mmcblk!=1"
echo "$ENV_CHECK" | grep -qx 'mmcpart=1' || die "env verify: mmcpart!=1"
log "env verified on image: $(echo "$ENV_CHECK" | tr '\n' ' ')"

# ---------------------------------------------------------------------------
# STEP 6 - keep the raw .img + compress + checksum + manifest.
#
# The raw image otherwise only lives in $WORK, which cleanup() deletes on
# exit (unless --keep-work). scripts/slim-podd-sd.sh and
# scripts/patch-podd-sd-diag.sh both default to the raw dist/podd-sd.img, so
# copy it out to dist/ alongside the .gz (mirrors what the L2 build's
# os/scripts/build-image.sh already does) instead of letting it evaporate
# with $WORK (podd#49).
# ---------------------------------------------------------------------------
case "$OUT" in
  *.gz) RAW_OUT="${OUT%.gz}" ;;
  *)    RAW_OUT="$OUT.img" ;;
esac
log "moving raw image -> $RAW_OUT"
mv "$SD_IMG" "$RAW_OUT"
log "compressing -> $OUT (this can take a few minutes; image is a full device)"
gzip -c "$RAW_OUT" > "$OUT"
RAW_BYTES="$(wc -c < "$RAW_OUT")"
GZ_BYTES="$(wc -c < "$OUT")"
SHA="$(sha256sum "$OUT" | awk '{print $1}')"
RAW_SHA="$(sha256sum "$RAW_OUT" | awk '{print $1}')"

MANIFEST="${OUT%.gz}.manifest.txt"
{
  echo "podd SD image manifest"
  echo "======================"
  echo "generated : $(date -u +%Y-%m-%dT%H:%M:%SZ)"
  echo "version   : $VERSION"
  echo "output    : $OUT"
  echo "  raw image   : $RAW_OUT (write this full image to the microSD)"
  echo "  gz size : $GZ_BYTES bytes"
  echo "  gz sha256   : $SHA"
  echo "  raw size    : $RAW_BYTES bytes"
  echo "  raw sha256  : $RAW_SHA"
  echo
  echo "Inputs"
  echo "  eMMC gold master : $EMMC_GZ (rootfs carved from p1 @sector $EMMC_P1_START)"
  echo "  SD template      : $SD_GZ"
  echo "  stock U-Boot env : $ENV_GZ"
  echo "  podd binary      : $PODD_BIN"
  echo "  UI dir           : $UI_DIR"
  echo "  config seeded    : $CONFIG_RON -> /opt/podd/config.ron"
  echo
  echo "What changed vs the stock SD (everything else byte-identical)"
  echo "  [p1] REPLACED : SD p1 now holds a clone of the STOCK eMMC rootfs,"
  echo "                  resized to $SD_P1_BLOCKS4K x 4K blocks, podd-ified:"
  echo "                    + /opt/podd/releases/$VERSION/rootfs/{podd,ui,podd.service}"
  echo "                    + /opt/podd/config.ron  (from $(basename "$CONFIG_RON"))"
  echo "                    + /opt/podd/current -> releases/$VERSION"
  echo "                    + /etc/systemd/system/podd.service (+ enabled in multi-user.target.wants)"
  echo "                    ~ masked units (-> /dev/null):"
  for u in $MASK_UNITS; do echo "                        $u"; done
  echo "                  Stock kernel /boot/Image.gz, DTB, /opt/eight kept intact."
  echo "  [env] 0x400000 : mmcdev 2->1, mmcblk 2->1, mmcpart=1, mmcautodetect=no."
  echo "                   All other env vars identical to stock."
  echo "  [unchanged] imx-boot @0x8400, MBR partition table, p2 (slot B), p3 (cage)."
  echo
  echo "eMMC: NEVER written by this build. persistent.mount / persistent-manager"
  echo "      (which mount+fsck+possibly reformat /dev/mmcblk2p3) are MASKED, so the"
  echo "      running podd system does not write the eMMC either. Swap the stock SD"
  echo "      back to revert instantly."
} > "$MANIFEST"

log "DONE"
echo
cat "$MANIFEST"
echo
echo "image : $OUT (gz)  /  $RAW_OUT (raw)"
echo "size  : $GZ_BYTES bytes (gz)  /  $RAW_BYTES bytes (raw)"
echo "sha256: $SHA  (gz)"
