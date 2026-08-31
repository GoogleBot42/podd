#!/usr/bin/env bash
#
# build-release.sh - shared CI logic for GitHub Actions and Gitea Actions.
#
# Builds the podd userland bundle (the primary deliverable, flashing-method.md
# §6b item 1) and produces the release `dist/` that pod-update-agent consumes:
#
#     dist/manifest.json            (signed if a key is available, else unsigned)
#     dist/app-<version>.squashfs   (podd + UI + configs, packed by `podup`)
#     dist/signing.pub              (public key, ONLY when signing)
#     dist/os-<version>.ext4.zst    (OS slot image; ONLY when OS_IMAGE is set)
#
# These names are exactly what pod-update-agent's GitHub/Gitea sources resolve:
#   GitHub latest:  https://github.com/<o>/<r>/releases/latest/download/manifest.json
#                   https://github.com/<o>/<r>/releases/latest/download/app-<v>.squashfs
#   Gitea:          <host>/<o>/<r>/releases/download/<tag>/manifest.json  (+ artifact)
# (manifest_name defaults to "manifest.json" in pod-update-agent's config.)
#
# Signing is OPTIONAL and honours pod-update's owner-controlled trust model:
#   - PODD_SIGNING_KEY present (base64 ed25519 seed) -> signed manifest + signing.pub
#   - PODD_SIGNING_KEY absent                        -> unsigned manifest (digests
#                                                       are STILL enforced by every
#                                                       consumer)
#
# Inputs (environment):
#   VERSION            release tag, e.g. "v0.1.0" (defaults to `git describe`)
#   CHANNEL            release channel, default "stable"
#   PODD_SIGNING_KEY   base64 ed25519 signing seed (CI secret; optional)
#   PODD_SIGNING_PUB   base64 ed25519 verifying key (optional; see key resolution)
#   OUT_DIR            output dir, default "dist"
#   VARIANTS           space-separated config variants to bundle, default "pod4 pod3"
#   OS_IMAGE           path to the OS slot artifact to include (optional):
#                      either the zstd-compressed podd-os.ext4.zst that
#                      os/scripts/build.sh emits, or a raw rootfs.ext2 (it is
#                      zstd-compressed here). Absent => app-only release,
#                      byte-identical behavior to before this input existed.
#   OS_VERSION         version for the OS component (default: APP_VERSION)
#
# Requires on PATH: nix (flakes), plus coreutils/openssl/base64 for key handling.
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$REPO_ROOT"

CHANNEL="${CHANNEL:-stable}"
OUT_DIR="${OUT_DIR:-dist}"
VARIANTS="${VARIANTS:-pod4 pod3}"

# ---------------------------------------------------------------------------
# Version: prefer an explicit tag; fall back to `git describe`. The app version
# drops any leading "v" (so the artifact is app-0.1.0.squashfs, not
# app-v0.1.0.squashfs); the manifest carries it verbatim, and the device reads
# the filename back out of the manifest, so it never needs to be predictable.
# ---------------------------------------------------------------------------
VERSION="${VERSION:-}"
if [ -z "$VERSION" ]; then
  VERSION="$(git describe --tags --always --dirty 2>/dev/null || echo "0.0.0-dev")"
fi
APP_VERSION="${VERSION#v}"

echo "==> podd release build"
echo "    version : $VERSION (app-version $APP_VERSION)"
echo "    channel : $CHANNEL"
echo "    out dir : $OUT_DIR"

# ---------------------------------------------------------------------------
# 1. Build the aarch64 podd binary, the web UI, and the host `podup` tool.
# ---------------------------------------------------------------------------
echo "==> nix build .#podd-aarch64"
nix build ".#podd-aarch64" --print-build-logs --out-link result-podd
echo "==> nix build .#ui"
nix build ".#ui" --print-build-logs --out-link result-ui
echo "==> nix build .#podup"
nix build ".#podup" --print-build-logs --out-link result-podup

PODD_BIN="result-podd/bin/podd"
UI_DIR="result-ui"
PODUP="result-podup/bin/podup"

[ -x "$PODD_BIN" ] || { echo "!! podd binary not found at $PODD_BIN" >&2; exit 1; }
[ -e "$UI_DIR/index.html" ] || echo "!! warning: $UI_DIR/index.html missing (UI build may be empty)" >&2
[ -x "$PODUP" ] || { echo "!! podup not found at $PODUP" >&2; exit 1; }

# ---------------------------------------------------------------------------
# 2. Assemble the app payload directory, laid out as it appears on-device under
#    /opt/podd/current/. pod-update-agent mounts the squashfs and flips `current`;
#    the installer scripts extract it into /opt/podd/releases/<version>/.
#
#      podd               the aarch64 (static musl) daemon
#      ui/                the built SPA (PODD_SPA_DIR)
#      config.pod4.ron    default configs the installer can seed from
#      config.pod3.ron
#      podd.service       the systemd unit the installer drops in
#      VERSION            provenance breadcrumb
# ---------------------------------------------------------------------------
PAYLOAD="$(mktemp -d)"
trap 'rm -rf "$PAYLOAD"' EXIT

install -m 0755 "$PODD_BIN" "$PAYLOAD/podd"
mkdir -p "$PAYLOAD/ui"
cp -rL "$UI_DIR/." "$PAYLOAD/ui/"
install -m 0644 "install/podd.service" "$PAYLOAD/podd.service"
for v in $VARIANTS; do
  src="config.${v}.example.ron"
  if [ -f "$src" ]; then
    install -m 0644 "$src" "$PAYLOAD/config.${v}.ron"
  else
    echo "!! warning: $src not found; skipping config.${v}.ron" >&2
  fi
done
printf '%s\n' "$VERSION" > "$PAYLOAD/VERSION"
# Make the tree writable so mksquashfs' reproducible mode is deterministic.
chmod -R u+w "$PAYLOAD"

# ---------------------------------------------------------------------------
# 3. Resolve signing. If PODD_SIGNING_KEY is present we sign and MUST publish a
#    matching signing.pub. The public key is resolved from, in order:
#      a) PODD_SIGNING_PUB   (base64 verifying key, inline)
#      b) a committed signing.pub in the repo root or install/
#      c) derived from the private seed via openssl (Ed25519 SPKI)
#    so a maintainer only strictly needs the ONE secret PODD_SIGNING_KEY.
# ---------------------------------------------------------------------------
rm -rf "$OUT_DIR"
mkdir -p "$OUT_DIR"

KEY_ARGS=()
SIGN_MODE="unsigned"
KEYFILE=""
if [ -n "${PODD_SIGNING_KEY:-}" ]; then
  SIGN_MODE="signed"
  KEYFILE="$(mktemp)"
  printf '%s' "$PODD_SIGNING_KEY" | tr -d '[:space:]' > "$KEYFILE"
  KEY_ARGS=(--key "$KEYFILE")

  PUBFILE="$OUT_DIR/signing.pub"
  if [ -n "${PODD_SIGNING_PUB:-}" ]; then
    printf '%s\n' "$(printf '%s' "$PODD_SIGNING_PUB" | tr -d '[:space:]')" > "$PUBFILE"
    echo "==> signing.pub from PODD_SIGNING_PUB"
  elif [ -f signing.pub ]; then
    cp signing.pub "$PUBFILE"
    echo "==> signing.pub from repo root"
  elif [ -f install/signing.pub ]; then
    cp install/signing.pub "$PUBFILE"
    echo "==> signing.pub from install/"
  elif command -v openssl >/dev/null 2>&1; then
    # Derive the verifying key from the 32-byte seed. Wrap the seed in a PKCS8
    # Ed25519 private-key DER, let openssl compute the public SPKI, and strip
    # the trailing 32 raw key bytes -> base64 == pod_update::encode_verifying_key.
    _der="$(mktemp)"; _pub="$(mktemp)"
    {
      printf '\060\056\002\001\000\060\005\006\003\053\145\160\004\042\004\040'
      printf '%s' "$PODD_SIGNING_KEY" | tr -d '[:space:]' | base64 -d
    } > "$_der"
    if openssl pkey -inform DER -in "$_der" -pubout -outform DER 2>/dev/null \
         | tail -c 32 | base64 | tr -d '\n' > "$_pub" && [ -s "$_pub" ]; then
      cat "$_pub" > "$PUBFILE"; printf '\n' >> "$PUBFILE"
      echo "==> signing.pub derived from PODD_SIGNING_KEY via openssl"
    else
      echo "!! warning: could not derive signing.pub; release will be signed but ship NO public key asset" >&2
      echo "!! provide PODD_SIGNING_PUB or commit signing.pub to fix" >&2
    fi
    rm -f "$_der" "$_pub"
  else
    echo "!! warning: signing but no signing.pub source and no openssl; NO public key asset published" >&2
  fi
fi
echo "==> signing mode: $SIGN_MODE"

# ---------------------------------------------------------------------------
# 4. Optional OS component. OS_IMAGE is the slot filesystem image the device's
#    AbSlotWriter streams onto the inactive A/B partition. podup records the
#    file under its basename, so stage it as os-<version>.ext4.zst first —
#    that basename is the URL the device fetches. A raw (uncompressed) input
#    is zstd-compressed here; the frame checksum (zstd default) is part of the
#    on-device integrity chain, so always compress with checksums enabled.
# ---------------------------------------------------------------------------
OS_ARGS=()
if [ -n "${OS_IMAGE:-}" ]; then
  [ -f "$OS_IMAGE" ] || { echo "!! OS_IMAGE=$OS_IMAGE not found" >&2; exit 1; }
  OS_VERSION="${OS_VERSION:-$APP_VERSION}"
  OS_STAGE_DIR="$(mktemp -d)"
  trap 'rm -rf "$PAYLOAD" "$OS_STAGE_DIR"' EXIT
  OS_STAGED="$OS_STAGE_DIR/os-${OS_VERSION#v}.ext4.zst"
  case "$OS_IMAGE" in
    *.zst) cp "$OS_IMAGE" "$OS_STAGED" ;;
    *)     zstd -T0 -19 -kf "$OS_IMAGE" -o "$OS_STAGED" ;;
  esac
  zstd -t "$OS_STAGED"   # decode test: catches a corrupt/checksum-less input
  OS_ARGS=(--os "$OS_STAGED" --os-version "${OS_VERSION#v}")
  echo "==> including OS component ${OS_VERSION#v} ($(basename "$OS_STAGED"))"
fi

# ---------------------------------------------------------------------------
# 5. Pack + (optionally) sign via podup. Produces dist/app-<v>.squashfs,
#    dist/os-<v>.ext4.zst (when OS_IMAGE is set) and dist/manifest.json.
# ---------------------------------------------------------------------------
echo "==> podup release"
"$PODUP" release \
  --channel "$CHANNEL" \
  --out-dir "$OUT_DIR" \
  --app-src "$PAYLOAD" \
  --app-version "$APP_VERSION" \
  "${OS_ARGS[@]}" \
  "${KEY_ARGS[@]}"

if [ -n "$KEYFILE" ]; then
  shred -u "$KEYFILE" 2>/dev/null || rm -f "$KEYFILE"
fi

# ---------------------------------------------------------------------------
# 6. Self-verify the release exactly as the device would.
# ---------------------------------------------------------------------------
echo "==> podup verify"
if [ "$SIGN_MODE" = "signed" ] && [ -f "$OUT_DIR/signing.pub" ]; then
  "$PODUP" verify --pubkey "$OUT_DIR/signing.pub" --manifest "$OUT_DIR/manifest.json" --dir "$OUT_DIR"
else
  "$PODUP" verify --manifest "$OUT_DIR/manifest.json" --dir "$OUT_DIR"
fi

echo "==> release assets in $OUT_DIR:"
ls -l "$OUT_DIR"
echo "==> done. Upload every file in $OUT_DIR/ to the release for tag $VERSION."
