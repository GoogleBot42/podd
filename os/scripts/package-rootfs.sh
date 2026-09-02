#!/usr/bin/env bash
#
# package-rootfs.sh - package the L2 rootfs tarball (podd-rootfs.tar.gz).
#
# WHAT IT PRODUCES
#   podd-rootfs.tar.gz(+.sha256) - the aarch64 clean-room rootfs as a tarball:
#   the SAME tree that seeds the SD image's A/B slots (podd + web UI + kernel
#   and DTB under /boot), packaged for the consumers that install a slot by
#   *extracting* rather than by dd'ing a filesystem image:
#     * install/podd-slot-install.sh  - in-band eMMC A/B slot install
#     * scripts/build-recovery-sd.sh  - the recovery-SD payload
#   (pod-updater's OTA path uses podd-os.ext4.zst instead - a bare filesystem
#   image it streams onto the inactive partition - and is unaffected by this.)
#
# WHERE THE INPUT COMES FROM
#   Buildroot emits output/images/rootfs.tar.gz from the very same $TARGET_DIR
#   it builds rootfs.ext2 from (BR2_TARGET_ROOTFS_TAR{,_GZIP} in
#   os/configs/podd_imx8mm_varsom_sd_defconfig), under fakeroot, so ownership
#   and modes are the real on-target ones. There is no second build system and
#   no second rootfs: this script only renames, verifies and checksums it.
#
#   It is invoked automatically by the board post-image.sh at the end of a
#   normal `os/scripts/build.sh` run. Run it by hand to (re)package from an
#   existing Buildroot tree without rebuilding anything - it takes seconds:
#
#     os/scripts/package-rootfs.sh --output-dir dist/
#
# Usage:
#   os/scripts/package-rootfs.sh [options]
#
#   --buildroot DIR    Buildroot checkout (uses DIR/output/images).
#                      Default: <repo>/build/buildroot.
#   --images-dir DIR   Buildroot output/images directly (wins over --buildroot).
#   --output-dir DIR   Also copy the tarball + .sha256 here.
#   --out FILE         Write the tarball to FILE instead of
#                      <images-dir>/podd-rootfs.tar.gz.
#   --no-verify        Skip the content checks (NOT recommended).
#   -h, --help         Show this help and exit.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/../.." && pwd)"

BUILDROOT_DIR=""
IMAGES_DIR=""
OUTPUT_DIR=""
OUT=""
VERIFY=1

die() { printf 'package-rootfs.sh: %s\n' "$*" >&2; exit 1; }
log() { printf '==> %s\n' "$*"; }

usage() { sed -n '3,38p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'; }

while [ "$#" -gt 0 ]; do
	case "$1" in
		--buildroot)  BUILDROOT_DIR="${2:?--buildroot needs a DIR}"; shift 2 ;;
		--images-dir) IMAGES_DIR="${2:?--images-dir needs a DIR}"; shift 2 ;;
		--output-dir) OUTPUT_DIR="${2:?--output-dir needs a DIR}"; shift 2 ;;
		--out)        OUT="${2:?--out needs a FILE}"; shift 2 ;;
		--no-verify)  VERIFY=0; shift ;;
		-h|--help)    usage; exit 0 ;;
		*)            die "unknown argument: $1 (try --help)" ;;
	esac
done

if [ -z "${IMAGES_DIR}" ]; then
	[ -n "${BUILDROOT_DIR}" ] || BUILDROOT_DIR="${REPO_ROOT}/build/buildroot"
	IMAGES_DIR="${BUILDROOT_DIR}/output/images"
fi
[ -d "${IMAGES_DIR}" ] || die "no Buildroot images dir: ${IMAGES_DIR} (build the OS image first: os/scripts/build.sh)"
[ -n "${OUT}" ] || OUT="${IMAGES_DIR}/podd-rootfs.tar.gz"

# --- 1. locate Buildroot's tarball -------------------------------------------
# BR2_TARGET_ROOTFS_TAR_GZIP gives us rootfs.tar.gz directly; an older tree (or
# one configured for an uncompressed tar) leaves rootfs.tar, which we compress.
SRC_GZ="${IMAGES_DIR}/rootfs.tar.gz"
SRC_TAR="${IMAGES_DIR}/rootfs.tar"
TMP=""
# Not `[ -n ] && rm`: with TMP empty that list returns 1, and under set -e a
# failing command inside the EXIT trap turns a successful run into exit 1
# (this failed the first CI OS build after 2 h of successful Buildroot work).
cleanup() { if [ -n "${TMP}" ]; then rm -rf "${TMP}"; fi; }
trap cleanup EXIT INT TERM

if [ -f "${SRC_GZ}" ] && { [ ! -f "${SRC_TAR}" ] || [ "${SRC_GZ}" -nt "${SRC_TAR}" ]; }; then
	log "source: ${SRC_GZ}"
	STAGED="${SRC_GZ}"
elif [ -f "${SRC_TAR}" ]; then
	log "source: ${SRC_TAR} (compressing)"
	TMP="$(mktemp -d "${TMPDIR:-/tmp}/podd-rootfs.XXXXXX")"
	STAGED="${TMP}/podd-rootfs.tar.gz"
	gzip -9 -c "${SRC_TAR}" > "${STAGED}"
else
	die "no rootfs.tar.gz or rootfs.tar in ${IMAGES_DIR} - has the Buildroot build finished? (os/scripts/build.sh)"
fi

# --- 2. verify the payload BEFORE anyone installs it -------------------------
# podd-slot-install.sh re-checks the extracted tree, but failing here is far
# cheaper than failing with a half-written eMMC slot. These are exactly the
# properties the slot consumers depend on.
if [ "${VERIFY}" -eq 1 ]; then
	LIST="$(mktemp "${TMPDIR:-/tmp}/podd-rootfs-list.XXXXXX")"
	tar -tzf "${STAGED}" > "${LIST}" || die "cannot list ${STAGED} - truncated tarball?"
	trap 'cleanup; rm -f "${LIST}"' EXIT INT TERM

	need() { grep -qxF "./$1" "${LIST}" || die "rootfs tarball is missing $1 - refusing to publish it"; }
	need boot/Image.gz
	need usr/bin/podd
	grep -qE '^\./boot/.*\.dtb$' "${LIST}" \
		|| die "rootfs tarball has no device tree under /boot - refusing to publish it"

	# Clean-room gate: this artifact gets extracted onto eMMC, so make it loud
	# if a vendor OTA/control binary ever leaks into the rootfs. The list is the
	# same one install/podd-install.sh masks on a stock unit.
	for v in usr/bin/swupdate usr/bin/dac usr/bin/frank usr/bin/capybara \
	         usr/bin/defibrillator usr/bin/frankenfirmware opt/eight home/dac; do
		if grep -qxF "./${v}" "${LIST}"; then
			die "vendor artifact ${v} present in the rootfs - clean-room violation, refusing to publish"
		fi
	done
	log "verified: /boot/Image.gz + DTB + /usr/bin/podd present, no vendor OTA stack"
fi

# --- 3. emit ------------------------------------------------------------------
mkdir -p "$(dirname "${OUT}")"
[ "${STAGED}" = "${OUT}" ] || cp -f "${STAGED}" "${OUT}"
( cd "$(dirname "${OUT}")" && sha256sum "$(basename "${OUT}")" > "$(basename "${OUT}").sha256" )
log "rootfs tarball: ${OUT} ($(du -h "${OUT}" | cut -f1))"
log "sha256        : $(cut -d' ' -f1 < "${OUT}.sha256")"

if [ -n "${OUTPUT_DIR}" ]; then
	mkdir -p "${OUTPUT_DIR}"
	cp -f "${OUT}" "${OUT}.sha256" "${OUTPUT_DIR}/"
	log "copied to ${OUTPUT_DIR}/"
fi
