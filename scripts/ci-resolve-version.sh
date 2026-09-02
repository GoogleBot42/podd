#!/usr/bin/env bash
#
# ci-resolve-version.sh - decide what version the release workflow is building
# and whether it may publish. Writes VERSION, TAG and PUBLISH to $GITHUB_ENV.
#
# Called by both jobs in .github/workflows/release.yml so they can never
# disagree about the answer.
#
#   tag push / dispatch with `version`  -> VERSION=<tag>, PUBLISH=true
#   dispatch with no `version`          -> VERSION=0.0.0-<short sha>, PUBLISH=false
#
# The smoke-test version is NOT the branch name on purpose: a branch name may
# contain '/', and it lands in artifact filenames (app-<version>.squashfs,
# podd-sd-<tag>.img.gz), which would try to write into a directory that isn't
# there.
#
# Inputs (environment): INPUT_VERSION, REF_TYPE, REF_NAME, GITHUB_ENV.
set -euo pipefail

INPUT_VERSION="${INPUT_VERSION:-}"
REF_TYPE="${REF_TYPE:-}"
REF_NAME="${REF_NAME:-}"

if [ -n "${INPUT_VERSION}" ]; then
	VERSION="${INPUT_VERSION}"
	PUBLISH=true
elif [ "${REF_TYPE}" = "tag" ]; then
	VERSION="${REF_NAME}"
	PUBLISH=true
else
	VERSION="0.0.0-$(git rev-parse --short HEAD)"
	PUBLISH=false
fi

case "${VERSION}" in
	*/*|*' '*) echo "ci-resolve-version.sh: refusing unusable version '${VERSION}'" >&2; exit 1 ;;
esac

echo "==> version ${VERSION} (publish: ${PUBLISH})"

: "${GITHUB_ENV:?ci-resolve-version.sh must run inside GitHub Actions}"
{
	echo "VERSION=${VERSION}"
	echo "TAG=${VERSION}"
	echo "PUBLISH=${PUBLISH}"
} >> "${GITHUB_ENV}"
