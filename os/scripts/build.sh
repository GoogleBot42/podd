#!/usr/bin/env bash
#
# build.sh - one-command reproducible build of the podd clean-room OS image.
#
# Buildroot needs a real FHS host environment (it hardcodes paths like
# /usr/bin/file), while the podd/UI cross-build and a working mkimage come from
# Nix. This orchestrates the split: it does the Nix builds OUTSIDE the FHS, then
# runs Buildroot INSIDE the `.#buildrootEnv` FHS sandbox, passing through a
# known-good mkimage to dodge Buildroot's broken host mkimage (see build-image.sh
# and os/README.md). Run from anywhere; args after `--` pass to build-image.sh.
#
# Usage:
#   os/scripts/build.sh [--output-dir dist/] [-- <extra build-image.sh args>]
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/../.." && pwd)"
cd "${REPO_ROOT}"

command -v nix >/dev/null 2>&1 || { echo "build.sh: nix is required" >&2; exit 1; }

echo "==> nix: podd binary, web UI, Buildroot FHS env"
nix build .#podd-aarch64 -o result-podd
nix build .#ui           -o result-ui
nix build .#buildrootEnv -o result-brenv

echo "==> nix: known-good mkimage (Buildroot's own host mkimage is broken)"
# ubootTools has multiple outputs (out, man); pick the one that has bin/mkimage.
GOODMK=""
while read -r _p; do
	if [ -x "${_p}/bin/mkimage" ]; then GOODMK="${_p}/bin/mkimage"; break; fi
done < <(nix build nixpkgs#ubootTools --no-link --print-out-paths)
[ -n "${GOODMK}" ] || { echo "build.sh: could not find mkimage in nixpkgs#ubootTools" >&2; exit 1; }
SSL_LIBDIR="$(ldd "${GOODMK}" | awk '/libssl/{print $3}' | xargs -r dirname)"

echo "==> Buildroot build inside the FHS sandbox"
PODD_FIT_MKIMAGE="${GOODMK}" \
PODD_FIT_MKIMAGE_LIBS="${SSL_LIBDIR}" \
	./result-brenv/bin/podd-buildroot-env -c \
	"cd '${REPO_ROOT}' && PODD_FIT_MKIMAGE='${GOODMK}' PODD_FIT_MKIMAGE_LIBS='${SSL_LIBDIR}' \
	 os/scripts/build-image.sh --no-nix \
	   --podd-bin '${REPO_ROOT}/result-podd/bin/podd' \
	   --ui-dir '${REPO_ROOT}/result-ui' \
	   ${*:-} --output-dir dist/"

echo "==> done: dist/podd-sd.img.gz"
