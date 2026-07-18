#!/bin/sh
#
# install.sh - the tiny bootstrap behind podd's docs one-liner. It is NOT a
# blind `curl | bash`: it downloads podd-install.sh, prints that script's
# SHA-256 so you can compare it against the value published in the docs, and
# only then runs it. And regardless of this script, the bundle podd-install.sh
# installs is itself SHA-256 (and optionally signature) verified against the
# release manifest - so trust never rests on this fetch.
#
# Usage (example docs one-liner):
#   curl -fsSL https://raw.githubusercontent.com/eightsleep/podd/main/install/install.sh | sh -s -- \
#       --source github:eightsleep/podd
#
# Any arguments after `--` are passed straight through to podd-install.sh.
#
# Pin the installer script's digest to fail closed:
#   PODD_INSTALL_SHA256=<expected-sha256>   compared to the downloaded script;
#                                            mismatch => abort before running.
#   PODD_INSTALLER_URL=<url>                 override where podd-install.sh is
#                                            fetched from.
set -eu

INSTALLER_URL="${PODD_INSTALLER_URL:-https://raw.githubusercontent.com/eightsleep/podd/main/install/podd-install.sh}"
EXPECT_SHA="${PODD_INSTALL_SHA256:-}"

log()  { printf '==> %s\n' "$*"; }
warn() { printf '!!  %s\n' "$*" >&2; }
die()  { printf '!!  %s\n' "$*" >&2; exit 1; }

WORK="$(mktemp -d "${TMPDIR:-/tmp}/podd-bootstrap.XXXXXX")"
trap 'rm -rf "$WORK"' EXIT INT TERM
SCRIPT="$WORK/podd-install.sh"

log "downloading installer: $INSTALLER_URL"
if command -v curl >/dev/null 2>&1; then
  curl -fsSL -o "$SCRIPT" "$INSTALLER_URL"
elif command -v wget >/dev/null 2>&1; then
  wget -q -O "$SCRIPT" "$INSTALLER_URL"
else
  die "need curl or wget"
fi
[ -s "$SCRIPT" ] || die "downloaded installer is empty"

if command -v sha256sum >/dev/null 2>&1; then
  GOT_SHA="$(sha256sum "$SCRIPT" | cut -d' ' -f1)"
else
  GOT_SHA="(sha256sum unavailable - cannot show digest)"
fi

cat <<EOF
--------------------------------------------------------------------------
 podd installer downloaded.
   sha256: $GOT_SHA
 Compare this against the value published in the podd docs/release notes
 BEFORE trusting it. (The bundle it installs is separately SHA-256 / signature
 verified against the release manifest regardless.)
--------------------------------------------------------------------------
EOF

if [ -n "$EXPECT_SHA" ]; then
  if [ "$EXPECT_SHA" = "${GOT_SHA:-}" ]; then
    log "digest matches PODD_INSTALL_SHA256 - proceeding"
  else
    die "digest MISMATCH: expected $EXPECT_SHA, got $GOT_SHA - aborting"
  fi
else
  warn "PODD_INSTALL_SHA256 not set: running without pinning the installer digest."
  warn "For a fail-closed install, set PODD_INSTALL_SHA256 to the docs value."
fi

log "running podd-install.sh $*"
exec sh "$SCRIPT" "$@"
