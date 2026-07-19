#!/bin/sh
#
# podd-install.sh - userland install of podd onto a rooted Pod (flashing-method
# §3a / §3b). This is the PRIMARY, easiest, safest path: it drops the podd
# bundle in /opt/podd, installs podd.service, masks Eight's OTA/control stack,
# and starts podd. It touches NEITHER the bootloader NOR the eMMC block devices,
# so it cannot brick the unit and is fully reversible.
#
# It is POSIX sh + busybox friendly (sha256sum, wget/curl, mount/unsquashfs,
# systemctl, fw_printenv). Idempotent and safe to re-run.
#
# Usage:
#   podd-install.sh --source github:owner/repo[@tag]
#   podd-install.sh --source gitea:https://git.example.org/owner/repo[@tag]
#   podd-install.sh --url https://host/path/to/release   # dir with manifest.json
#   podd-install.sh --dir /mnt/usb/podd-release          # offline / local dir
#
# Options (also settable via the PODD_* environment variables):
#   --source SPEC     github:owner/repo[@tag] | gitea:URL[@tag]   (PODD_RELEASE_SOURCE)
#   --url BASE        explicit release base URL holding manifest.json (PODD_RELEASE_URL)
#   --dir PATH        local directory holding manifest.json + artifact (PODD_RELEASE_DIR)
#   --channel NAME    expected channel; warns on mismatch (PODD_CHANNEL, default stable)
#   --pubkey PATH     ed25519 verifying key to REQUIRE a valid signature (PODD_PUBKEY)
#   --variant NAME    pod4 | pod3 - which default config to seed (PODD_VARIANT, default pod4)
#   --prefix DIR      install root, default /opt/podd (PODD_PREFIX)
#   --no-mask         do NOT mask the vendor OTA/control units
#   --no-start        install but do not enable/start podd.service
#   -h | --help       this help
#
# Integrity (SHA-256) is ALWAYS enforced. A signature is verified only if you
# pass --pubkey (and a verifier is available); otherwise you get a loud warning.
set -eu

# ---- defaults ------------------------------------------------------------
PREFIX="${PODD_PREFIX:-/opt/podd}"
CHANNEL="${PODD_CHANNEL:-stable}"
VARIANT="${PODD_VARIANT:-pod4}"
PUBKEY="${PODD_PUBKEY:-}"
SRC_SPEC="${PODD_RELEASE_SOURCE:-}"
SRC_URL="${PODD_RELEASE_URL:-}"
SRC_DIR="${PODD_RELEASE_DIR:-}"
DO_MASK=1
DO_START=1

MANIFEST_NAME="manifest.json"
# Vendor OTA + control units to mask so they cannot fight podd or auto-revert to
# stock (flashing-method §2a). NOTE: we deliberately never touch `cage`.
VENDOR_UNITS="swupdate swupdate.socket swupdate-progress defibrillator dac frank capybara telegraf vector frankenfirmware eight-kernel"

log()  { printf '==> %s\n' "$*"; }
warn() { printf '!!  %s\n' "$*" >&2; }
die()  { printf '!!  %s\n' "$*" >&2; exit 1; }

usage() { sed -n '2,31p' "$0" | sed 's/^# \{0,1\}//'; exit "${1:-0}"; }

# ---- args ----------------------------------------------------------------
while [ $# -gt 0 ]; do
  case "$1" in
    --source) SRC_SPEC="$2"; shift 2 ;;
    --url)    SRC_URL="$2"; shift 2 ;;
    --dir)    SRC_DIR="$2"; shift 2 ;;
    --channel) CHANNEL="$2"; shift 2 ;;
    --pubkey) PUBKEY="$2"; shift 2 ;;
    --variant) VARIANT="$2"; shift 2 ;;
    --prefix) PREFIX="$2"; shift 2 ;;
    --no-mask) DO_MASK=0; shift ;;
    --no-start) DO_START=0; shift ;;
    -h|--help) usage 0 ;;
    *) die "unknown argument: $1 (try --help)" ;;
  esac
done

[ "$(id -u)" = "0" ] || die "must run as root (installs to $PREFIX, edits systemd units)"

# ---- pick a downloader ---------------------------------------------------
DL=""
if command -v curl >/dev/null 2>&1; then DL="curl"; fi
if [ -z "$DL" ] && command -v wget >/dev/null 2>&1; then DL="wget"; fi

# fetch URL DEST  (only used for remote sources)
fetch() {
  _u="$1"; _d="$2"
  case "$DL" in
    curl) curl -fsSL -o "$_d" "$_u" ;;
    wget) wget -q -O "$_d" "$_u" ;;
    *) die "need curl or wget to fetch $_u" ;;
  esac
}

# ---- resolve the release source into MANIFEST_URL + ARTIFACT_BASE (or DIR)-
# Mirrors pod_updater::config::ReleaseSourceUrl::resolve so the installer and
# the on-device agent agree on URL shapes.
MODE=""; MANIFEST_URL=""; ARTIFACT_BASE=""
if [ -n "$SRC_DIR" ]; then
  MODE="dir"
elif [ -n "$SRC_URL" ]; then
  MODE="http"
  ARTIFACT_BASE="${SRC_URL%/}"
  MANIFEST_URL="${ARTIFACT_BASE}/${MANIFEST_NAME}"
elif [ -n "$SRC_SPEC" ]; then
  MODE="http"
  case "$SRC_SPEC" in
    github:*)
      _rest="${SRC_SPEC#github:}"
      case "$_rest" in
        *@*) _or="${_rest%@*}"; _tag="${_rest##*@}"
             ARTIFACT_BASE="https://github.com/${_or}/releases/download/${_tag}" ;;
        *)   ARTIFACT_BASE="https://github.com/${_rest}/releases/latest/download" ;;
      esac ;;
    gitea:*)
      _rest="${SRC_SPEC#gitea:}"
      case "$_rest" in
        *@*) _url="${_rest%@*}"; _tag="${_rest##*@}" ;;
        *)   _url="$_rest"; _tag="latest" ;;
      esac
      ARTIFACT_BASE="${_url%/}/releases/download/${_tag}" ;;
    *) die "unknown --source scheme: $SRC_SPEC (use github:owner/repo or gitea:URL)" ;;
  esac
  MANIFEST_URL="${ARTIFACT_BASE}/${MANIFEST_NAME}"
else
  die "no release source: pass --source, --url, or --dir (see --help)"
fi

WORK="$(mktemp -d "${TMPDIR:-/tmp}/podd-install.XXXXXX")"
cleanup() { [ -n "${MNT:-}" ] && umount "$MNT" 2>/dev/null || true; rm -rf "$WORK"; }
trap cleanup EXIT INT TERM

# ---- STEP A: back up FIRST (flashing-method §6a "always back up first") ----
# All best-effort: a stock-userland install writes no block devices, but we
# still capture the U-Boot env + partition table + active slot so recovery is
# trivial if you later move to a slot/OS install.
BACKUP="${PREFIX}/backup/$(date +%Y%m%d-%H%M%S)"
mkdir -p "$BACKUP"
log "backing up to $BACKUP"
if command -v fw_printenv >/dev/null 2>&1; then
  fw_printenv > "$BACKUP/fw_printenv.txt" 2>/dev/null || warn "fw_printenv failed (ok on non-i.MX)"
  # Record the active A/B slot pointer for reference.
  { echo "mmcpart=$(fw_printenv -n mmcpart 2>/dev/null || echo '?')"
    echo "current_slot=$(fw_printenv -n current_slot 2>/dev/null || echo '?')"
  } > "$BACKUP/active-slot.txt" 2>/dev/null || true
fi
cat /proc/partitions > "$BACKUP/partitions.txt" 2>/dev/null || true
# Dump the eMMC MBR (read-only) if the device node exists - never write to it.
for dev in /dev/mmcblk2 /dev/mmcblk0; do
  if [ -b "$dev" ]; then
    dd if="$dev" of="$BACKUP/$(basename "$dev")-mbr.bin" bs=512 count=1 2>/dev/null || true
    { command -v fdisk >/dev/null 2>&1 && fdisk -l "$dev"; } > "$BACKUP/$(basename "$dev")-parttable.txt" 2>/dev/null || true
    break
  fi
done

# ---- STEP B: get manifest + artifact -------------------------------------
if [ "$MODE" = "dir" ]; then
  [ -f "$SRC_DIR/$MANIFEST_NAME" ] || die "no $MANIFEST_NAME in $SRC_DIR"
  cp "$SRC_DIR/$MANIFEST_NAME" "$WORK/$MANIFEST_NAME"
else
  log "fetching manifest: $MANIFEST_URL"
  fetch "$MANIFEST_URL" "$WORK/$MANIFEST_NAME" || die "failed to download manifest"
fi

# Parse the App artifact out of the (pretty) manifest with busybox-safe tools.
# The app squashfs is the only artifact named app-*.squashfs.
ART_FILE="$(grep -oE 'app-[^"]*\.squashfs' "$WORK/$MANIFEST_NAME" | head -n1)"
[ -n "$ART_FILE" ] || die "no app-*.squashfs artifact in manifest"
# sha256 / size are the fields of that artifact object; in the struct's field
# order they immediately follow the filename line (grep -A context).
ART_SHA="$(grep -F -A2 "$ART_FILE" "$WORK/$MANIFEST_NAME" | grep -oE '[0-9a-f]{64}' | head -n1)"
ART_SIZE="$(grep -F -A3 "$ART_FILE" "$WORK/$MANIFEST_NAME" | sed -n 's/.*"size":[[:space:]]*\([0-9][0-9]*\).*/\1/p' | head -n1)"
MAN_CHANNEL="$(grep -o '"channel": *"[^"]*"' "$WORK/$MANIFEST_NAME" | head -n1 | sed 's/.*"\([^"]*\)"$/\1/')"
# Derive the version from the artifact name: app-<version>.squashfs.
VERSION="${ART_FILE#app-}"; VERSION="${VERSION%.squashfs}"

[ -n "$ART_SHA" ]  || die "could not read artifact sha256 from manifest"
[ -n "$ART_SIZE" ] || die "could not read artifact size from manifest"
log "release: version=$VERSION channel=${MAN_CHANNEL:-?} artifact=$ART_FILE"
if [ -n "$MAN_CHANNEL" ] && [ "$MAN_CHANNEL" != "$CHANNEL" ]; then
  warn "manifest channel '$MAN_CHANNEL' != requested '$CHANNEL' (continuing)"
fi

if [ "$MODE" = "dir" ]; then
  [ -f "$SRC_DIR/$ART_FILE" ] || die "artifact $ART_FILE missing in $SRC_DIR"
  cp "$SRC_DIR/$ART_FILE" "$WORK/$ART_FILE"
else
  log "fetching artifact: $ARTIFACT_BASE/$ART_FILE"
  fetch "$ARTIFACT_BASE/$ART_FILE" "$WORK/$ART_FILE" || die "failed to download $ART_FILE"
fi

# ---- STEP C: verify (integrity ALWAYS, signature if --pubkey) -------------
log "verifying size + SHA-256 (always enforced)"
ACT_SIZE="$(wc -c < "$WORK/$ART_FILE" | tr -d ' ')"
[ "$ACT_SIZE" = "$ART_SIZE" ] || die "size mismatch: expected $ART_SIZE, got $ACT_SIZE"
if command -v sha256sum >/dev/null 2>&1; then
  printf '%s  %s\n' "$ART_SHA" "$WORK/$ART_FILE" | sha256sum -c - >/dev/null \
    || die "SHA-256 MISMATCH - refusing to install $ART_FILE"
else
  die "sha256sum not found - cannot verify integrity, refusing to install"
fi
log "integrity OK ($ART_SHA)"

verify_signature() {
  # Returns 0 if verified, 1 if a verifier ran and FAILED, 2 if no verifier.
  _pub="$1"; _man="$2"
  # (a) prefer a real pod-update verifier if one was side-loaded onto the device.
  if command -v podup >/dev/null 2>&1; then
    podup verify --pubkey "$_pub" --manifest "$_man" >/dev/null 2>&1 && return 0 || return 1
  fi
  if command -v podd-verify >/dev/null 2>&1; then
    podd-verify --pubkey "$_pub" --manifest "$_man" >/dev/null 2>&1 && return 0 || return 1
  fi
  # (b) jq + openssl (>=3.0): reconstruct the canonical manifest bytes and check
  # the raw Ed25519 signature against the SubjectPublicKeyInfo built from the key.
  if command -v jq >/dev/null 2>&1 && command -v openssl >/dev/null 2>&1; then
    jq -jc '.manifest' "$_man" > "$WORK/canonical.bin" 2>/dev/null || return 2
    jq -r '.signature // empty' "$_man" 2>/dev/null | base64 -d > "$WORK/sig.bin" 2>/dev/null || return 2
    [ -s "$WORK/sig.bin" ] || return 1   # manifest is unsigned but a key was required
    {
      printf '\060\052\060\005\006\003\053\145\160\003\041\000'
      tr -d '[:space:]' < "$_pub" | base64 -d
    } | base64 > "$WORK/pub.b64"
    { echo "-----BEGIN PUBLIC KEY-----"; cat "$WORK/pub.b64"; echo "-----END PUBLIC KEY-----"; } > "$WORK/pub.pem"
    openssl pkeyutl -verify -pubin -inkey "$WORK/pub.pem" -rawin \
      -in "$WORK/canonical.bin" -sigfile "$WORK/sig.bin" >/dev/null 2>&1 && return 0 || return 1
  fi
  # (c) minisign, if the owner produced a detached .minisig alongside the artifact.
  if command -v minisign >/dev/null 2>&1 && [ -f "$WORK/${ART_FILE}.minisig" ]; then
    minisign -Vm "$WORK/$ART_FILE" -p "$_pub" >/dev/null 2>&1 && return 0 || return 1
  fi
  return 2
}

if [ -n "$PUBKEY" ]; then
  [ -f "$PUBKEY" ] || die "--pubkey $PUBKEY not found"
  if verify_signature "$PUBKEY" "$WORK/$MANIFEST_NAME"; then
    rc=0
  else
    rc=$?
  fi
  case "$rc" in
    0) log "signature OK (authenticity verified against $PUBKEY)" ;;
    2) warn "SIGNATURE NOT VERIFIED: no on-device verifier (need podup / jq+openssl>=3 / minisign)."
       warn "Integrity (SHA-256) IS verified. To ENFORCE signatures on all future"
       warn "updates, set PODD_UPDATER_TRUST=$PUBKEY for podd's update agent." ;;
    *) die "SIGNATURE INVALID for manifest - refusing to install" ;;
  esac
else
  warn "no --pubkey given: installing on integrity (SHA-256) only, signature NOT checked."
  warn "This is fine for your own unsigned builds. For authenticity, pass --pubkey."
fi

# ---- STEP D: install the payload -----------------------------------------
RELEASES="$PREFIX/releases"
RELEASE_DIR="$RELEASES/$VERSION"
log "installing to $RELEASE_DIR"
mkdir -p "$RELEASES"
rm -rf "$RELEASE_DIR"
mkdir -p "$RELEASE_DIR"
# Payload content lives under <release>/rootfs/ so that the on-device layout is
# IDENTICAL whether a release was placed by this installer or by the on-device
# OTA agent (pod-updater mounts the squashfs at <release>/rootfs). `current`
# points at <release>, so the binary is always at current/rootfs/podd.
ROOTFS="$RELEASE_DIR/rootfs"
mkdir -p "$ROOTFS"
# Keep the artifact next to the extracted tree (parity with the OTA layout).
cp "$WORK/$ART_FILE" "$RELEASE_DIR/app.squashfs" 2>/dev/null || true

if command -v unsquashfs >/dev/null 2>&1; then
  unsquashfs -f -d "$ROOTFS" "$WORK/$ART_FILE" >/dev/null
else
  # Fallback: loop-mount the read-only squashfs and copy it out.
  MNT="$WORK/mnt"; mkdir -p "$MNT"
  mount -t squashfs -o ro,loop "$WORK/$ART_FILE" "$MNT" \
    || die "cannot unpack squashfs (no unsquashfs and mount failed)"
  cp -a "$MNT/." "$ROOTFS/"
  umount "$MNT"; MNT=""
fi
[ -f "$ROOTFS/podd" ] || die "payload missing podd binary"
chmod +x "$ROOTFS/podd"

# Atomically flip the `current` symlink to the new release.
ln -sfn "$RELEASE_DIR" "$PREFIX/current.tmp"
mv "$PREFIX/current.tmp" "$PREFIX/current" 2>/dev/null || ln -sfn "$RELEASE_DIR" "$PREFIX/current"

# Seed a default config the FIRST time only - never clobber an owner's edits.
if [ ! -f "$PREFIX/config.ron" ]; then
  if [ -f "$ROOTFS/config.${VARIANT}.ron" ]; then
    cp "$ROOTFS/config.${VARIANT}.ron" "$PREFIX/config.ron"
    log "seeded default config from config.${VARIANT}.ron (edit $PREFIX/config.ron)"
  else
    warn "no bundled config.${VARIANT}.ron; create $PREFIX/config.ron before first run"
  fi
else
  log "keeping existing $PREFIX/config.ron"
fi

# Install the systemd unit.
if [ -f "$ROOTFS/podd.service" ]; then
  cp "$ROOTFS/podd.service" /etc/systemd/system/podd.service
else
  warn "bundle has no podd.service; leaving any existing unit in place"
fi

# ---- STEP E: mask vendor stack + enable podd -----------------------------
if command -v systemctl >/dev/null 2>&1; then
  if [ "$DO_MASK" = "1" ]; then
    log "masking vendor OTA/control units (idempotent; never touches cage)"
    for u in $VENDOR_UNITS; do
      systemctl disable --now "$u" >/dev/null 2>&1 || true
      systemctl mask "$u" >/dev/null 2>&1 || true
    done
  fi
  systemctl daemon-reload || true
  if [ "$DO_START" = "1" ]; then
    log "enabling + starting podd.service"
    systemctl enable --now podd.service || die "systemctl enable --now podd failed"
  fi
else
  warn "no systemctl found; installed files but did not enable a service"
fi

# ---- done ----------------------------------------------------------------
IP="$(ip route get 1 2>/dev/null | sed -n 's/.*src \([0-9.]*\).*/\1/p' | head -n1)"
[ -n "$IP" ] || IP="<pod-ip>"
cat <<EOF

==========================================================================
 podd $VERSION installed.

   binary : $PREFIX/current/rootfs/podd
   config : $PREFIX/config.ron   (PODD_DRY_RUN=true until you arm real writes)
   backup : $BACKUP
   UI      : http://$IP:3000

 Next:
   - edit $PREFIX/config.ron, then: systemctl restart podd
   - check it:  systemctl status podd   /   journalctl -u podd -f
   - re-run this script any time to update; it is idempotent.
==========================================================================
EOF
