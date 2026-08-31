#!/usr/bin/env bash
#
# upload-release-gitea.sh - create/find a Gitea release for a tag and upload
# every file in the given dir as an asset, via the Gitea API (no external
# actions). Assets are uploaded with their bare filename so pod-update-agent's Gitea
# source resolves <host>/<owner>/<repo>/releases/download/<tag>/<filename>.
#
# Inputs (environment, all provided by Gitea Actions):
#   GITEA_SERVER_URL   e.g. https://git.example.org  (falls back to GITHUB_SERVER_URL)
#   GITEA_REPOSITORY   "owner/repo"                  (falls back to GITHUB_REPOSITORY)
#   GITEA_TOKEN        API token                     (falls back to GITHUB_TOKEN)
#   TAG                the release tag, e.g. v0.1.0
#   DIST_DIR           dir of assets to upload, default "dist"
set -euo pipefail

SERVER="${GITEA_SERVER_URL:-${GITHUB_SERVER_URL:-}}"
REPO="${GITEA_REPOSITORY:-${GITHUB_REPOSITORY:-}}"
TOKEN="${GITEA_TOKEN:-${GITHUB_TOKEN:-}}"
TAG="${TAG:-}"
DIST_DIR="${DIST_DIR:-dist}"

[ -n "$SERVER" ] || { echo "!! no GITEA_SERVER_URL/GITHUB_SERVER_URL" >&2; exit 1; }
[ -n "$REPO" ]   || { echo "!! no GITEA_REPOSITORY/GITHUB_REPOSITORY" >&2; exit 1; }
[ -n "$TOKEN" ]  || { echo "!! no GITEA_TOKEN/GITHUB_TOKEN" >&2; exit 1; }
[ -n "$TAG" ]    || { echo "!! no TAG" >&2; exit 1; }

API="${SERVER%/}/api/v1/repos/${REPO}"
AUTH="Authorization: token ${TOKEN}"

echo "==> resolving release for tag $TAG in $REPO"
release_json="$(curl -fsSL -H "$AUTH" "${API}/releases/tags/${TAG}" 2>/dev/null || true)"
release_id="$(printf '%s' "$release_json" | sed -n 's/.*"id"[[:space:]]*:[[:space:]]*\([0-9]\+\).*/\1/p' | head -n1)"

if [ -z "$release_id" ]; then
  echo "==> creating release $TAG"
  release_json="$(curl -fsSL -X POST -H "$AUTH" -H "Content-Type: application/json" \
    -d "{\"tag_name\":\"${TAG}\",\"name\":\"${TAG}\"}" \
    "${API}/releases")"
  release_id="$(printf '%s' "$release_json" | sed -n 's/.*"id"[[:space:]]*:[[:space:]]*\([0-9]\+\).*/\1/p' | head -n1)"
fi
[ -n "$release_id" ] || { echo "!! could not resolve release id" >&2; exit 1; }
echo "==> release id $release_id"

for f in "$DIST_DIR"/*; do
  [ -f "$f" ] || continue
  name="$(basename "$f")"
  echo "==> uploading $name"
  # Best-effort delete an existing asset of the same name so re-runs are idempotent.
  existing="$(curl -fsSL -H "$AUTH" "${API}/releases/${release_id}/assets" 2>/dev/null || true)"
  asset_id="$(printf '%s' "$existing" \
    | tr '}' '\n' \
    | grep -F "\"name\":\"${name}\"" \
    | sed -n 's/.*"id"[[:space:]]*:[[:space:]]*\([0-9]\+\).*/\1/p' | head -n1)"
  if [ -n "$asset_id" ]; then
    curl -fsSL -X DELETE -H "$AUTH" "${API}/releases/${release_id}/assets/${asset_id}" >/dev/null 2>&1 || true
  fi
  curl -fsSL -X POST -H "$AUTH" \
    -F "attachment=@${f};filename=${name}" \
    "${API}/releases/${release_id}/assets?name=${name}" >/dev/null
done

echo "==> uploaded $(find "$DIST_DIR" -maxdepth 1 -type f | wc -l) asset(s) to $TAG"
