#!/usr/bin/env bash
#
# build-image.sh - build the podd clean-room L2 OS image (Buildroot + RAUC A/B)
# for the Eight Sleep i.MX8M-Mini / Variscite DART-MX8M-MINI "SD" hub.
#
# WHAT IT PRODUCES
#   <buildroot>/output/images/podd-sd.img(.gz) - a complete, from-source bootable
#   SD image: imx-boot (SPL + ATF + U-Boot + NXP DDR/HDMI blobs) @0x8400, a RAUC
#   U-Boot env @0x400000, and A/B rootfs slots + a persistent data partition. The
#   whole thing is built from pinned upstream source (see os/README.md); `dd` it
#   to a spare microSD and it boots without touching the stock eMMC.
#
# HOW IT WORKS
#   1. podd + its web UI are cross-compiled OUTSIDE Buildroot via the repo's Nix
#      flake (reproducible static aarch64 build), then handed to Buildroot's
#      `podd` package via $PODD_BIN / $PODD_UI_DIR.
#   2. A pinned Buildroot tree is fetched (shallow git checkout of a tag) unless
#      an existing checkout is supplied with --buildroot.
#   3. Our BR2_EXTERNAL tree (os/) + defconfig configure the build; `make` fetches
#      and builds ATF, U-Boot, the Variscite 5.4 kernel, the rootfs, and stitches
#      the boot container + final image via the board post-image scripts.
#
#   This is a THIN, RE-RUNNABLE wrapper: everything version-specific lives in the
#   defconfig (upstream source pins) and in the BUILDROOT_* pins just below.
#
# Usage:
#   os/scripts/build-image.sh [options]
#
#   --buildroot DIR     Use an existing Buildroot checkout at DIR instead of
#                       fetching the pinned one (DIR is used as-is; its revision
#                       is NOT checked). Default: fetch into ./build/buildroot.
#   --podd-bin PATH     Prebuilt podd binary (skip `nix build .#podd-aarch64`).
#   --ui-dir DIR        Prebuilt UI asset dir (skip `nix build .#ui`).
#   --output-dir DIR    Copy the finished image(s) here after the build.
#   --jobs N            Parallelism for `make` (default: nproc).
#   --no-nix            Do not invoke nix; --podd-bin and --ui-dir are then
#                       REQUIRED (for CI runners / offline builds).
#   -h, --help          Show this help and exit.
#
# The build itself is long (hours) and large (GBs of downloads + objects); this
# script only orchestrates it.
set -euo pipefail

# --- pinned Buildroot ---------------------------------------------------------
# 2026.02 is the current Buildroot LTS; .3 is the latest point release. It ships
# the imx8mm boot flow (freescale-imx / firmware-imx 8.27 / host imx-mkimage),
# rauc, and the aarch64 toolchain the defconfig relies on. The canonical repo is
# https://git.buildroot.net/buildroot ; we clone the GitHub mirror over https for
# reliable CI fetches. Keep this in lockstep with os/README.md.
BUILDROOT_VERSION="2026.02.3"
BUILDROOT_REPO="https://github.com/buildroot/buildroot.git"

# --- locations ----------------------------------------------------------------
# This script lives in os/scripts/; the repo root is two levels up, the external
# tree (os/) is one level up.
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
BR2_EXTERNAL_DIR="$(cd "${SCRIPT_DIR}/.." && pwd)"
REPO_ROOT="$(cd "${BR2_EXTERNAL_DIR}/.." && pwd)"
DEFCONFIG="podd_imx8mm_varsom_sd_defconfig"

# --- defaults / CLI -----------------------------------------------------------
BUILDROOT_DIR=""
PODD_BIN=""
PODD_UI_DIR=""
OUTPUT_DIR=""
JOBS="$(nproc 2>/dev/null || echo 4)"
USE_NIX=1

die() { printf 'build-image.sh: %s\n' "$*" >&2; exit 1; }
log() { printf '==> %s\n' "$*"; }

usage() {
	# Print the leading comment block (up to the first blank comment terminator)
	# as the help text, so this stays the single source of truth.
	sed -n '3,41p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'
}

while [ "$#" -gt 0 ]; do
	case "$1" in
		--buildroot)  BUILDROOT_DIR="${2:?--buildroot needs a DIR}"; shift 2 ;;
		--podd-bin)   PODD_BIN="${2:?--podd-bin needs a PATH}"; shift 2 ;;
		--ui-dir)     PODD_UI_DIR="${2:?--ui-dir needs a DIR}"; shift 2 ;;
		--output-dir) OUTPUT_DIR="${2:?--output-dir needs a DIR}"; shift 2 ;;
		--jobs)       JOBS="${2:?--jobs needs a number}"; shift 2 ;;
		--no-nix)     USE_NIX=0; shift ;;
		-h|--help)    usage; exit 0 ;;
		*)            die "unknown argument: $1 (try --help)" ;;
	esac
done

# --- 1. podd + UI artifacts (Nix, outside Buildroot) --------------------------
# Resolve $PODD_BIN / $PODD_UI_DIR: build with Nix unless prebuilt paths were
# passed or --no-nix was given. Buildroot's podd.mk consumes these two vars.
build_with_nix() {
	command -v nix >/dev/null 2>&1 || die "nix not found; pass --podd-bin/--ui-dir or --no-nix"

	if [ -z "${PODD_BIN}" ]; then
		log "nix build .#podd-aarch64"
		nix build "${REPO_ROOT}#podd-aarch64" --out-link "${REPO_ROOT}/result-podd"
		PODD_BIN="${REPO_ROOT}/result-podd/bin/podd"
	fi
	if [ -z "${PODD_UI_DIR}" ]; then
		log "nix build .#ui"
		nix build "${REPO_ROOT}#ui" --out-link "${REPO_ROOT}/result-ui"
		PODD_UI_DIR="${REPO_ROOT}/result-ui"
	fi
}

if [ "${USE_NIX}" -eq 1 ]; then
	build_with_nix
else
	[ -n "${PODD_BIN}" ]    || die "--no-nix requires --podd-bin"
	[ -n "${PODD_UI_DIR}" ] || die "--no-nix requires --ui-dir"
fi

[ -x "${PODD_BIN}" ]    || die "podd binary not executable: ${PODD_BIN}"
[ -d "${PODD_UI_DIR}" ] || die "UI dir not found: ${PODD_UI_DIR}"
# Absolutize: Buildroot runs the podd package (podd.mk) from the Buildroot dir,
# so a relative --podd-bin/--ui-dir would resolve against the wrong CWD there.
PODD_BIN="$(realpath "${PODD_BIN}")"
PODD_UI_DIR="$(realpath "${PODD_UI_DIR}")"
log "podd binary : ${PODD_BIN}"
log "UI assets   : ${PODD_UI_DIR}"

# --- 2. Buildroot tree --------------------------------------------------------
# Fetch the pinned tag unless the caller supplied a checkout. A shallow clone of
# the tag is enough and keeps CI fast.
if [ -z "${BUILDROOT_DIR}" ]; then
	BUILDROOT_DIR="${REPO_ROOT}/build/buildroot"
	if [ ! -d "${BUILDROOT_DIR}/.git" ]; then
		log "cloning Buildroot ${BUILDROOT_VERSION} -> ${BUILDROOT_DIR}"
		mkdir -p "$(dirname "${BUILDROOT_DIR}")"
		git clone --depth 1 --branch "${BUILDROOT_VERSION}" \
			"${BUILDROOT_REPO}" "${BUILDROOT_DIR}"
	else
		log "reusing Buildroot checkout at ${BUILDROOT_DIR}"
	fi
fi
[ -f "${BUILDROOT_DIR}/Makefile" ] || die "not a Buildroot tree: ${BUILDROOT_DIR}"

# --- 3. configure + build -----------------------------------------------------
# `make -C <buildroot> BR2_EXTERNAL=<os> <defconfig>` seeds .config; the second
# `make` runs the whole build. PODD_BIN / PODD_UI_DIR flow through to podd.mk.
log "applying ${DEFCONFIG} (BR2_EXTERNAL=${BR2_EXTERNAL_DIR})"
make -C "${BUILDROOT_DIR}" BR2_EXTERNAL="${BR2_EXTERNAL_DIR}" "${DEFCONFIG}"

# Work around a Buildroot bug: its host u-boot-tools (mkimage 2025.10) is built
# with an empty CONFIG_MKIMAGE_DTC_PATH, so the host mkimage cannot compile the
# boot FIT and dies with "-I: command not found". If the caller supplies a
# known-good mkimage via PODD_FIT_MKIMAGE (e.g. `nix build nixpkgs#ubootTools`),
# build host-uboot-tools first, then replace its mkimage with a thin wrapper
# around the good one (scoping LD_LIBRARY_PATH so other host tools are
# unaffected). See os/README.md → "Host mkimage workaround".
if [ -n "${PODD_FIT_MKIMAGE:-}" ]; then
	log "shimming Buildroot host mkimage with ${PODD_FIT_MKIMAGE}"
	make -C "${BUILDROOT_DIR}" host-uboot-tools
	_hostmk="${BUILDROOT_DIR}/output/host/bin/mkimage"
	cat > "${_hostmk}" <<EOF
#!/bin/sh
# Auto-generated by build-image.sh (Buildroot host-mkimage dtc-path bug).
exec env LD_LIBRARY_PATH="${PODD_FIT_MKIMAGE_LIBS:-}\${LD_LIBRARY_PATH:+:\$LD_LIBRARY_PATH}" "${PODD_FIT_MKIMAGE}" "\$@"
EOF
	chmod 0755 "${_hostmk}"
fi

# The podd package is install-only (artifacts built outside Buildroot), but
# Buildroot's stamp files don't content-track PODD_BIN/PODD_UI_DIR — without
# this, a rebuilt binary/UI silently never reaches the rootfs.
rm -f "${BUILDROOT_DIR}"/output/build/podd-*/.stamp_target_installed

log "building image (make -j${JOBS}) - this takes a while"
make -C "${BUILDROOT_DIR}" "-j${JOBS}" \
	PODD_BIN="${PODD_BIN}" \
	PODD_UI_DIR="${PODD_UI_DIR}"

# --- 4. collect ---------------------------------------------------------------
IMAGES_DIR="${BUILDROOT_DIR}/output/images"
IMG="${IMAGES_DIR}/podd-sd.img"
[ -f "${IMG}" ] || die "expected image not produced: ${IMG}"
log "image ready: ${IMG}(.gz)"

if [ -n "${OUTPUT_DIR}" ]; then
	mkdir -p "${OUTPUT_DIR}"
	# Copy the raw + gz images and the manifest emitted by the board post-image.
	for f in podd-sd.img podd-sd.img.gz podd-sd.manifest.txt; do
		[ -f "${IMAGES_DIR}/${f}" ] && cp -v "${IMAGES_DIR}/${f}" "${OUTPUT_DIR}/"
	done
	log "copied image(s) to ${OUTPUT_DIR}"
fi
