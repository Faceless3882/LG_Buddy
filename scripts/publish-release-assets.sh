#!/bin/bash

set -euo pipefail

usage() {
    echo "Usage: $0 [--dist-dir <dir>] [--tag <release-tag>] [--commit <release-commit>]"
    exit 1
}

DIST_DIR="dist"
TAG="${GITHUB_REF_NAME:-}"
EXPECTED_COMMIT=""
DRY_RUN="${GH_RELEASE_DRY_RUN:-0}"

while [ "$#" -gt 0 ]; do
    case "$1" in
        --dist-dir)
            DIST_DIR="${2:-}"
            shift 2
            ;;
        --tag)
            TAG="${2:-}"
            shift 2
            ;;
        --commit)
            EXPECTED_COMMIT="${2:-}"
            shift 2
            ;;
        *)
            usage
            ;;
    esac
done

[ -n "$TAG" ] || {
    echo "Release tag must be provided via --tag or GITHUB_REF_NAME."
    exit 1
}

case "$TAG" in
    v[0-9]*) ;;
    *)
        echo "Release tag must start with v followed by a digit, for example v1.0.0."
        exit 1
        ;;
esac

[ -d "$DIST_DIR" ] || {
    echo "Distribution directory not found: $DIST_DIR"
    exit 1
}

mapfile -t ARCHIVES < <(find "$DIST_DIR" -maxdepth 1 -type f -name '*.tar.gz' | sort)
[ "${#ARCHIVES[@]}" -gt 0 ] || {
    echo "No release archives found in $DIST_DIR"
    exit 1
}

CHECKSUM_FILE="$DIST_DIR/sha256sums.txt"
[ -f "$CHECKSUM_FILE" ] || {
    echo "Checksum file not found: $CHECKSUM_FILE"
    exit 1
}

VERSION="${TAG#v}"
[ -n "$VERSION" ] || {
    echo "Release tag must include a version after v."
    exit 1
}

TITLE="LG Buddy ${VERSION}"
NOTES="Prebuilt LG Buddy release bundle for Linux. Extract the archive and run ./install.sh from inside the bundle."
RELEASE_FLAGS=()

if [[ "$VERSION" == *-* ]]; then
    RELEASE_FLAGS+=(--prerelease)
fi

if [ "$DRY_RUN" = "1" ]; then
    echo "Dry run: would publish tag $TAG"
    printf 'Archive: %s\n' "${ARCHIVES[@]}"
    echo "Checksum file: $CHECKSUM_FILE"
    echo "Title: $TITLE"
    if [ "${#RELEASE_FLAGS[@]}" -gt 0 ]; then
        echo "Release flags: ${RELEASE_FLAGS[*]}"
    fi
    exit 0
fi

[ -n "$EXPECTED_COMMIT" ] || {
    echo "Release commit must be provided via --commit."
    exit 1
}

TAG_COMMIT="$(git rev-list -n 1 "$TAG" 2>/dev/null || true)"
[ "$TAG_COMMIT" = "$EXPECTED_COMMIT" ] || {
    echo "Release tag $TAG points to ${TAG_COMMIT:-nothing}, expected $EXPECTED_COMMIT."
    exit 1
}

EXPECTED_PRERELEASE="false"
if [ "${#RELEASE_FLAGS[@]}" -gt 0 ]; then
    EXPECTED_PRERELEASE="true"
fi

if gh release view "$TAG" >/dev/null 2>&1; then
    RELEASE_STATE="$(gh release view "$TAG" --json isDraft,isPrerelease)"
    [ "$(printf '%s' "$RELEASE_STATE" | jq -r .isDraft)" = "false" ] || {
        echo "Existing release $TAG is still a draft."
        exit 1
    }
    [ "$(printf '%s' "$RELEASE_STATE" | jq -r .isPrerelease)" = "$EXPECTED_PRERELEASE" ] || {
        echo "Existing release $TAG has the wrong prerelease classification."
        exit 1
    }
else
    gh release create "$TAG" --verify-tag --title "$TITLE" --notes "$NOTES" "${RELEASE_FLAGS[@]}"
fi


for asset in "${ARCHIVES[@]}" "$CHECKSUM_FILE"; do
    asset_name="$(basename "$asset")"
    if gh release view "$TAG" --json assets --jq '.assets[].name' | grep -F -x -q "$asset_name"; then
        compare_dir="$(mktemp -d)"
        gh release download "$TAG" --pattern "$asset_name" --dir "$compare_dir"
        if ! cmp -s "$asset" "$compare_dir/$asset_name"; then
            echo "Existing release asset differs from the candidate: $asset_name"
            rm -rf "$compare_dir"
            exit 1
        fi
        rm -rf "$compare_dir"
    else
        gh release upload "$TAG" "$asset"
    fi
done
