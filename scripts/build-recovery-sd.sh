#!/usr/bin/env bash
#
# build-recovery-sd.sh - assemble the podd recovery SD image
# (flashing-method.md §5 / §6b item 4).
#
# WHAT IT PRODUCES
#   dist/podd-recovery-sd.img(.gz) - the ordinary clean-room SD image with an
#   eMMC-install payload baked into its data partition. `dd` it to a spare
#   microSD: the Pod boots podd from the card exactly like a normal podd SD
#   (stock eMMC untouched, stock card = instant revert), and the payload is
#   there for a deliberate, rollback-safe eMMC slot install afterwards.
#
# HOW IT IS BUILT (and why it is a derivative, not a second image)
#   Everything the card boots from - imx-boot @0x8400, the U-Boot env @0x400000
#   (already mmcdev=1 mmcpart=1, i.e. "boot my own p1"), and the A/B rootfs
#   slots - is byte-for-byte the podd-sd.img the L2 Buildroot build produced and
#   that has been proven to boot. This script copies that image and rewrites
#   exactly ONE region: the p3 data partition, replaced by an ext4 carrying
#   /podd-recovery/. Both the untouched prefix and the rewritten partition are
#   readback-verified with `cmp` before the image is published.
#
# CLEAN-ROOM NOTE (supersedes flashing-method.md §6b items 2-3)
#   That section predates the from-source L2 boot chain and told you to vendor
#   Eight's/Variscite's stock `imx-boot-sd.bin` and stock DTB. We do NOT: the
#   boot container in podd-sd.img is built from source (Variscite U-Boot + ATF +
#   NXP DDR training blobs) and has booted the hardware - see docs/CLEANROOM-OS.md.
#   No Eight Sleep binary is an input to, or an output of, this script.
#
# SAFETY NOTE - why there is no auto-installer
#   The §5 sketch had the card rewrite the eMMC unattended at power-on. It does
#   not, and will not: eMMC is only ever written through install/podd-slot-
#   install.sh, into the INACTIVE slot, with the rollback state machine armed
#   (.claude/rules/media-writes.md). This script itself only ever writes a
#   regular file and refuses a block-device output.
#
#   Note also that podd-slot-install.sh cannot run FROM this card: the card's
#   env has mmcdev=1, so its mmcpart selects a slot on the card, not on eMMC.
#   The script detects that and refuses. Making an SD-booted eMMC install work
#   needs mmcdev/mmcblk flipped as part of the same env write and needs a way
#   to tell which eMMC slot still holds stock - open work, needs hardware
#   (#155). The payload rides along so it is available on the rooted stock
#   system.
#
# Inputs (all defaulted from a finished `os/scripts/build.sh` run):
#   --sd-img PATH      podd-sd.img to derive from   ($PODD_SD_IMG)
#   --rootfs PATH      podd-rootfs.tar.gz payload   ($PODD_ROOTFS_TARGZ)
#   --images-dir DIR   Buildroot output/images to look in  ($PODD_IMAGES_DIR)
#   --out PATH         output image, default dist/podd-recovery-sd.img ($OUT_IMG)
#   --no-gzip          skip the .img.gz (the raw .img is always written)
#   --plan             print the assembly plan and exit 0
#   -h, --help         this help
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"

SD_IMG="${PODD_SD_IMG:-}"
ROOTFS_TGZ="${PODD_ROOTFS_TARGZ:-}"
IMAGES_DIR="${PODD_IMAGES_DIR:-}"
OUT="${OUT_IMG:-}"
DO_GZIP=1
MODE="build"

die() { printf 'build-recovery-sd.sh: %s\n' "$*" >&2; exit 1; }
log() { printf '==> %s\n' "$*"; }

usage() { sed -n '3,51p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'; }

while [ "$#" -gt 0 ]; do
	case "$1" in
		--sd-img)     SD_IMG="${2:?--sd-img needs a PATH}"; shift 2 ;;
		--rootfs)     ROOTFS_TGZ="${2:?--rootfs needs a PATH}"; shift 2 ;;
		--images-dir) IMAGES_DIR="${2:?--images-dir needs a DIR}"; shift 2 ;;
		--out)        OUT="${2:?--out needs a PATH}"; shift 2 ;;
		--no-gzip)    DO_GZIP=0; shift ;;
		--plan)       MODE="plan"; shift ;;
		-h|--help)    usage; exit 0 ;;
		*)            die "unknown argument: $1 (try --help)" ;;
	esac
done

# --- input resolution ---------------------------------------------------------
# dist/ first (what os/scripts/build.sh copies out), then the raw Buildroot
# images dir.
[ -n "${IMAGES_DIR}" ] || IMAGES_DIR="${REPO_ROOT}/build/buildroot/output/images"
pick() { # <explicit> <basename> -> first existing candidate, or empty
	if [ -n "$1" ]; then printf '%s' "$1"; return; fi
	for c in "${REPO_ROOT}/dist/$2" "${IMAGES_DIR}/$2"; do
		if [ -f "$c" ]; then printf '%s' "$c"; return; fi
	done
}
SD_IMG="$(pick "${SD_IMG}" podd-sd.img)"
ROOTFS_TGZ="$(pick "${ROOTFS_TGZ}" podd-rootfs.tar.gz)"
[ -n "${OUT}" ] || OUT="${REPO_ROOT}/dist/podd-recovery-sd.img"

INSTALLER="${REPO_ROOT}/install/podd-slot-install.sh"

if [ "${MODE}" = "plan" ]; then
	cat <<PLAN
podd recovery-SD build plan (flashing-method.md §5 / §6b item 4)
------------------------------------------------------------------------------
Inputs (from a finished os/scripts/build.sh run):
  podd-sd.img        ${SD_IMG:-<NOT FOUND - run os/scripts/build.sh>}
  podd-rootfs.tar.gz ${ROOTFS_TGZ:-<NOT FOUND - run os/scripts/build.sh, or os/scripts/package-rootfs.sh against an existing tree>}
  installer          ${INSTALLER}
Output:
  ${OUT}(.gz)

Assembly (no root, no mounting, nothing raw written outside the output file):
  a. Read the MBR of podd-sd.img; locate p3 and confirm its ext4 label is
     "podd_data" (refuse otherwise - never guess which partition to overwrite).
  b. Stage the payload tree /podd-recovery/ = podd-rootfs.tar.gz + its .sha256
     + podd-slot-install.sh + README.txt.
  c. mke2fs -d that tree into an ext4 sized EXACTLY to p3, label podd_data.
  d. Copy podd-sd.img -> the output, then dd the payload fs over p3 only.
  e. Verify: everything before p3 is byte-identical to podd-sd.img (cmp -n),
     p3 reads back byte-identical to the payload fs (cmp -i), sizes match.
  f. gzip -k -> podd-recovery-sd.img.gz + a manifest with both sha256s.

What the card does when the user boots it:
  It IS a podd SD. imx-boot @0x8400 + env @0x400000 (mmcdev=1 mmcpart=1) boot
  slot A from the card; podd comes up, WiFi provisioning included. The stock
  eMMC is untouched and the stock card remains a total, instant revert.

The eMMC payload it carries (/data/podd-recovery/ once booted):
  podd-rootfs.tar.gz + .sha256 + podd-slot-install.sh + README.txt.
  podd-slot-install.sh is the STOCK-U-Boot path (env mmcdev=2, so mmcpart means
  an eMMC slot). Booted from this card mmcdev=1 and mmcpart means a slot on the
  card, so the script detects the mismatch and refuses rather than repointing
  U-Boot at the wrong device. Run it on the rooted stock system instead.

Deliberately NOT done (see the header): no stock imx-boot is vendored, no
bootloader is written to eMMC, and nothing installs itself unattended.
------------------------------------------------------------------------------
PLAN
	exit 0
fi

# --- validate -----------------------------------------------------------------
[ -n "${SD_IMG}" ] && [ -f "${SD_IMG}" ] \
	|| die "no podd-sd.img found (looked in dist/ and ${IMAGES_DIR}). Build it first: os/scripts/build.sh"
[ -n "${ROOTFS_TGZ}" ] && [ -f "${ROOTFS_TGZ}" ] \
	|| die "no podd-rootfs.tar.gz found (looked in dist/ and ${IMAGES_DIR}). Build it: os/scripts/build.sh, or os/scripts/package-rootfs.sh --output-dir dist/ against an existing Buildroot tree"
[ -f "${INSTALLER}" ] || die "installer not found: ${INSTALLER}"
[ ! -b "${OUT}" ] || die "refusing: output ${OUT} is a block device. This script builds an IMAGE FILE; write it to a card yourself (and verify with cmp -n)."

# mke2fs: prefer the host PATH, fall back to the one Buildroot already built.
MKE2FS="$(command -v mke2fs || true)"
if [ -z "${MKE2FS}" ]; then
	for c in "${IMAGES_DIR}/../host/sbin/mke2fs" "${REPO_ROOT}/build/buildroot/output/host/sbin/mke2fs"; do
		if [ -x "$c" ]; then MKE2FS="$c"; break; fi
	done
fi
[ -n "${MKE2FS}" ] || die "mke2fs not found (install e2fsprogs, or run inside 'nix shell nixpkgs#e2fsprogs')"

log "sd image  : ${SD_IMG}"
log "payload   : ${ROOTFS_TGZ}"
log "mke2fs    : ${MKE2FS}"
log "output    : ${OUT}"

WORK="$(mktemp -d "${TMPDIR:-/tmp}/podd-recovery.XXXXXX")"
trap 'rm -rf "${WORK}"' EXIT INT TERM

# --- a. locate the data partition in the MBR ---------------------------------
# 4 x 16-byte entries at 0x1BE: type at +4, LBA start at +8, sector count at +12
# (little-endian). Read them straight out of the image - no util-linux needed,
# and no chance of a tool "helpfully" picking a different partition than the one
# we checked.
SECTOR=512
u8()   { od -An -tu1 -j "$2" -N1 "$1" | tr -d ' \n'; }
le32() { od -An -tu4 --endian=little -j "$2" -N4 "$1" | tr -d ' \n'; }
pent() { printf '%d' $((446 + 16 * ($1 - 1))); }

[ "$(u8 "${SD_IMG}" 510)" = "85" ] && [ "$(u8 "${SD_IMG}" 511)" = "170" ] \
	|| die "${SD_IMG} has no MBR signature (0x55AA) - is that really a podd SD image?"
for n in 1 2 3; do
	[ "$(u8 "${SD_IMG}" "$(( $(pent "$n") + 4 ))")" != "0" ] \
		|| die "${SD_IMG} partition $n is empty; expected the 3-partition podd layout (rootfs_a, rootfs_b, data)"
done
[ "$(u8 "${SD_IMG}" "$(( $(pent 4) + 4 ))")" = "0" ] \
	|| die "${SD_IMG} has a 4th partition; this is not the podd SD layout, refusing to guess"

P3_START="$(le32 "${SD_IMG}" "$(( $(pent 3) + 8 ))")"
P3_COUNT="$(le32 "${SD_IMG}" "$(( $(pent 3) + 12 ))")"
P3_OFF=$((P3_START * SECTOR))
P3_BYTES=$((P3_COUNT * SECTOR))

# Confirm by label before overwriting anything: the ext4 superblock lives 1024 B
# into the partition, s_volume_name at +0x78, 16 bytes.
P3_LABEL="$(dd if="${SD_IMG}" bs=16 count=1 skip=$((P3_OFF + 1024 + 0x78)) \
	iflag=skip_bytes status=none | tr -d '\0')"
[ "${P3_LABEL}" = "podd_data" ] \
	|| die "partition 3 of ${SD_IMG} is labelled '${P3_LABEL}', expected 'podd_data' - refusing to overwrite a partition I cannot identify"

log "data part : p3 @ ${P3_OFF} bytes, ${P3_BYTES} bytes, label '${P3_LABEL}'"

# --- b. stage the payload -----------------------------------------------------
PAY="${WORK}/payload/podd-recovery"
mkdir -p "${PAY}"
cp "${ROOTFS_TGZ}" "${PAY}/podd-rootfs.tar.gz"
if [ -f "${ROOTFS_TGZ}.sha256" ]; then
	cp "${ROOTFS_TGZ}.sha256" "${PAY}/podd-rootfs.tar.gz.sha256"
else
	( cd "${PAY}" && sha256sum podd-rootfs.tar.gz > podd-rootfs.tar.gz.sha256 )
fi
# Normalise the sidecar to the payload's own basename so podd-slot-install.sh's
# `<tarball>.sha256` lookup matches regardless of what the source was called.
awk '{ printf "%s  podd-rootfs.tar.gz\n", $1 }' "${PAY}/podd-rootfs.tar.gz.sha256" \
	> "${PAY}/podd-rootfs.tar.gz.sha256.tmp"
mv "${PAY}/podd-rootfs.tar.gz.sha256.tmp" "${PAY}/podd-rootfs.tar.gz.sha256"
install -m 0755 "${INSTALLER}" "${PAY}/podd-slot-install.sh"

cat > "${PAY}/README.txt" <<'README'
podd recovery payload
=====================

This card already IS a podd system: it boots podd from its own A/B rootfs
slots and leaves the Pod's internal eMMC completely untouched. That is the
recovery - keep the stock card somewhere safe and swapping it back is an
instant, total revert at any time.

This directory carries the eMMC slot-install artifact so it travels with the
card. Nothing here runs by itself.

  podd-rootfs.tar.gz         the aarch64 rootfs (kernel + DTB under /boot)
  podd-rootfs.tar.gz.sha256  its digest, checked before any eMMC write
  podd-slot-install.sh       the eMMC A/B slot installer

IMPORTANT - where the installer runs
------------------------------------
podd-slot-install.sh is the STOCK-U-Boot eMMC path: it belongs on a rooted
stock system, whose U-Boot env has mmcdev=2 so that mmcpart selects an eMMC
slot. Booted from THIS card the env has mmcdev=1, mmcpart selects a slot on
the card itself, and flipping it after writing eMMC would repoint U-Boot at
the wrong device - so the script detects that and refuses. Copy the tarball to
the stock system and run it there:

  sh podd-slot-install.sh --rootfs podd-rootfs.tar.gz
  sh podd-slot-install.sh --confirm-good     # once the new slot boots healthy

It writes the INACTIVE eMMC slot only, verifies the .sha256 first, never
touches the persistent data partition, and arms U-Boot's automatic revert
after 3 failed boots. No bootloader is ever written to the eMMC.

See docs/RECOVERY.md and docs/INSTALL.md in the podd repo.
README

PAY_BYTES="$(du -sb "${WORK}/payload" | cut -f1)"
[ "${PAY_BYTES}" -lt "${P3_BYTES}" ] \
	|| die "payload is ${PAY_BYTES} bytes but the data partition is only ${P3_BYTES} - grow the data partition in os/board/eightsleep/imx8mm-varsom/genimage.cfg"

# --- c. build the payload filesystem, sized exactly to p3 --------------------
PAYFS="${WORK}/data.ext4"
truncate -s "${P3_BYTES}" "${PAYFS}"
"${MKE2FS}" -q -F -t ext4 -L podd_data -E root_owner=0:0 \
	-d "${WORK}/payload" "${PAYFS}"

# mke2fs -d copies the *build host* uid/gid onto every file, which shows up as a
# bare numeric owner on the Pod. Cosmetic (only root reads this), so best-effort:
# debugfs ships with mke2fs, but don't fail the build if it is missing.
DEBUGFS="$(command -v debugfs || true)"
[ -n "${DEBUGFS}" ] || { [ -x "$(dirname "${MKE2FS}")/debugfs" ] && DEBUGFS="$(dirname "${MKE2FS}")/debugfs"; } || true
if [ -n "${DEBUGFS}" ]; then
	for f in /podd-recovery /podd-recovery/README.txt \
	         /podd-recovery/podd-rootfs.tar.gz \
	         /podd-recovery/podd-rootfs.tar.gz.sha256 \
	         /podd-recovery/podd-slot-install.sh; do
		"${DEBUGFS}" -w -R "sif ${f} uid 0" "${PAYFS}" >/dev/null 2>&1 || true
		"${DEBUGFS}" -w -R "sif ${f} gid 0" "${PAYFS}" >/dev/null 2>&1 || true
	done
fi
log "payload fs: ${P3_BYTES} bytes, ${PAY_BYTES} bytes of content"

# --- d. copy + splice ---------------------------------------------------------
mkdir -p "$(dirname "${OUT}")"
cp --reflink=auto "${SD_IMG}" "${OUT}"
dd if="${PAYFS}" of="${OUT}" bs=1M seek="${P3_OFF}" oflag=seek_bytes \
	conv=notrunc status=none
sync

# --- e. readback-verify -------------------------------------------------------
# Two halves that together cover the whole file: the prefix must be untouched,
# and p3 must be exactly what we meant to put there. (p3 is the last partition
# in genimage.cfg, so prefix + p3 is the entire image; the size check below
# makes that explicit rather than assumed.)
[ "$(stat -c%s "${OUT}")" = "$(stat -c%s "${SD_IMG}")" ] \
	|| die "output size changed - the splice went outside the data partition"
cmp -n "${P3_OFF}" "${SD_IMG}" "${OUT}" \
	|| die "readback FAILED: bytes before the data partition differ from ${SD_IMG}"
cmp -i "0:${P3_OFF}" -n "${P3_BYTES}" "${PAYFS}" "${OUT}" \
	|| die "readback FAILED: the data partition does not match the payload filesystem"
[ $((P3_OFF + P3_BYTES)) -le "$(stat -c%s "${OUT}")" ] \
	|| die "data partition extends past the end of the image"
log "verified  : prefix identical to podd-sd.img, p3 identical to the payload fs"

# --- f. compress + manifest ---------------------------------------------------
RAW_SHA="$(sha256sum "${OUT}" | cut -d' ' -f1)"
if [ "${DO_GZIP}" -eq 1 ]; then
	gzip -kf "${OUT}"
	GZ_SHA="$(sha256sum "${OUT}.gz" | cut -d' ' -f1)"
else
	GZ_SHA="(not built: --no-gzip)"
fi
{
	echo "podd recovery SD image (clean-room podd-sd.img + eMMC install payload)"
	echo "derived from : $(basename "${SD_IMG}")"
	echo "payload      : $(basename "${ROOTFS_TGZ}") -> /podd-recovery/ on p3"
	echo "raw sha256   : ${RAW_SHA}"
	echo "gz  sha256   : ${GZ_SHA}"
	echo "write        : sudo dd if=$(basename "${OUT}") of=/dev/sdX bs=4M conv=fsync status=progress"
	echo "verify       : sudo cmp -n $(stat -c%s "${OUT}") $(basename "${OUT}") /dev/sdX"
	echo "eMMC install : boot the card, then 'sh /data/podd-recovery/podd-slot-install.sh' (never automatic)"
} > "${OUT%.img}.manifest.txt"

log "done: ${OUT}$( [ "${DO_GZIP}" -eq 1 ] && echo '(.gz)' ) + $(basename "${OUT%.img}").manifest.txt"
