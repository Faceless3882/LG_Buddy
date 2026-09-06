#!/bin/bash

set -euo pipefail

SCRIPT_DIR="$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)"

usage() {
    echo "Usage: $0 <stage-draft|publish-draft> [--dist-dir <dir>] [--tag <release-tag>] [--commit <release-commit>]"
    exit 1
}

[ "$#" -gt 0 ] || usage
MODE="$1"
shift
case "$MODE" in
    stage-draft|publish-draft) ;;
    *) usage ;;
esac

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

(cd "$DIST_DIR" && sha256sum --strict -c "$(basename "$CHECKSUM_FILE")")

VERSION="${TAG#v}"
[ -n "$VERSION" ] || {
    echo "Release tag must include a version after v."
    exit 1
}

TITLE="LG Buddy ${VERSION}"
PLACEHOLDER_NOTES="Release notes pending maintainer review."
RELEASE_FLAGS=()
EXPECTED_CHANNEL="stable"

if [[ "$VERSION" == *-* ]]; then
    RELEASE_FLAGS+=(--prerelease)
    EXPECTED_CHANNEL="prerelease"
fi

for archive in "${ARCHIVES[@]}"; do
    manifest_expectations=(
        --expected-release-tag "$TAG"
        --expected-version "$VERSION"
        --expected-channel "$EXPECTED_CHANNEL"
    )
    if [ -n "$EXPECTED_COMMIT" ]; then
        manifest_expectations+=(--expected-commit "$EXPECTED_COMMIT")
    fi
    python3 "$SCRIPT_DIR/release_bundle_manifest.py" validate \
        --archive "$archive" \
        "${manifest_expectations[@]}"
done

if [ "$DRY_RUN" = "1" ]; then
    echo "Dry run: would run $MODE for tag $TAG"
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

RELEASE_EXISTS="false"
RELEASE_IS_DRAFT="true"
if gh release view "$TAG" >/dev/null 2>&1; then
    RELEASE_EXISTS="true"
    RELEASE_STATE="$(gh release view "$TAG" --json isDraft,isPrerelease,body,name)"
    RELEASE_IS_DRAFT="$(printf '%s' "$RELEASE_STATE" | jq -r .isDraft)"
    case "$RELEASE_IS_DRAFT" in
        true|false) ;;
        *)
            echo "Existing release $TAG returned an invalid draft state."
            exit 1
            ;;
    esac
    [ "$(printf '%s' "$RELEASE_STATE" | jq -r .isPrerelease)" = "$EXPECTED_PRERELEASE" ] || {
        echo "Existing release $TAG has the wrong prerelease classification."
        exit 1
    }
fi

if [ "$MODE" = "publish-draft" ] && [ "$RELEASE_EXISTS" = "false" ]; then
    echo "Release draft $TAG does not exist. Run stage-draft first."
    exit 1
fi

if [ "$MODE" = "stage-draft" ] && [ "$RELEASE_EXISTS" = "false" ]; then
    gh release create "$TAG" \
        --draft \
        --verify-tag \
        --title "$TITLE" \
        --notes "$PLACEHOLDER_NOTES" \
        "${RELEASE_FLAGS[@]}"
    RELEASE_EXISTS="true"
fi

EXPECTED_ASSETS=("${ARCHIVES[@]}" "$CHECKSUM_FILE")
EXPECTED_ASSET_NAMES=()

asset_name_is_expected() {
    local candidate="$1"
    local expected=""

    for expected in "${EXPECTED_ASSET_NAMES[@]}"; do
        [ "$candidate" != "$expected" ] || return 0
    done
    return 1
}

for asset in "${EXPECTED_ASSETS[@]}"; do
    asset_name="$(basename "$asset")"
    ! asset_name_is_expected "$asset_name" || {
        echo "Release assets must have unique file names: $asset_name"
        exit 1
    }
    EXPECTED_ASSET_NAMES+=("$asset_name")
done

RELEASE_ASSET_NAMES="$(gh release view "$TAG" --json assets --jq '.assets[].name')"
while IFS= read -r asset_name; do
    [ -z "$asset_name" ] || asset_name_is_expected "$asset_name" || {
        echo "Release $TAG contains unexpected asset: $asset_name"
        exit 1
    }
done <<< "$RELEASE_ASSET_NAMES"

for asset in "${EXPECTED_ASSETS[@]}"; do
    asset_name="$(basename "$asset")"
    if ! grep -F -x -q -- "$asset_name" <<< "$RELEASE_ASSET_NAMES"; then
        [ "$MODE" = "stage-draft" ] && [ "$RELEASE_IS_DRAFT" = "true" ] || {
            echo "Release $TAG is missing required asset: $asset_name"
            exit 1
        }
        gh release upload "$TAG" "$asset"
    fi
done

RELEASE_ASSETS="$(gh release view "$TAG" --json assets)"
REMOTE_ASSET_COUNT="$(printf '%s' "$RELEASE_ASSETS" | jq '.assets | length')"
[ "$REMOTE_ASSET_COUNT" -eq "${#EXPECTED_ASSET_NAMES[@]}" ] || {
    echo "Release $TAG contains $REMOTE_ASSET_COUNT assets, expected ${#EXPECTED_ASSET_NAMES[@]}."
    exit 1
}

for asset in "${EXPECTED_ASSETS[@]}"; do
    asset_name="$(basename "$asset")"
    MATCHING_ASSET_COUNT="$(
        printf '%s' "$RELEASE_ASSETS" |
            jq --arg name "$asset_name" '[.assets[] | select(.name == $name)] | length'
    )"
    [ "$MATCHING_ASSET_COUNT" -eq 1 ] || {
        echo "Release $TAG must contain exactly one asset named $asset_name."
        exit 1
    }

    REMOTE_ASSET_STATE="$(
        printf '%s' "$RELEASE_ASSETS" |
            jq -r --arg name "$asset_name" '.assets[] | select(.name == $name) | .state'
    )"
    [ "$REMOTE_ASSET_STATE" = "uploaded" ] || {
        echo "Release asset $asset_name is not fully uploaded."
        exit 1
    }

    LOCAL_ASSET_SIZE="$(wc -c < "$asset" | tr -d '[:space:]')"
    REMOTE_ASSET_SIZE="$(
        printf '%s' "$RELEASE_ASSETS" |
            jq -r --arg name "$asset_name" '.assets[] | select(.name == $name) | .size'
    )"
    [ "$REMOTE_ASSET_SIZE" = "$LOCAL_ASSET_SIZE" ] || {
        echo "Release asset $asset_name has size $REMOTE_ASSET_SIZE, expected $LOCAL_ASSET_SIZE."
        exit 1
    }

    LOCAL_ASSET_DIGEST="sha256:$(sha256sum "$asset" | cut -d ' ' -f 1)"
    REMOTE_ASSET_DIGEST="$(
        printf '%s' "$RELEASE_ASSETS" |
            jq -r --arg name "$asset_name" '.assets[] | select(.name == $name) | .digest'
    )"
    [ "$REMOTE_ASSET_DIGEST" = "$LOCAL_ASSET_DIGEST" ] || {
        echo "Release asset $asset_name has digest $REMOTE_ASSET_DIGEST, expected $LOCAL_ASSET_DIGEST."
        exit 1
    }
done

if [ "$MODE" = "stage-draft" ]; then
    if [ "$RELEASE_IS_DRAFT" = "true" ]; then
        RELEASE_STATE="$(gh release view "$TAG" --json isDraft,isPrerelease,body,name)"
        [ "$(printf '%s' "$RELEASE_STATE" | jq -r .isDraft)" = "true" ] || {
            echo "Release $TAG was published before draft verification completed."
            exit 1
        }
        [ "$(printf '%s' "$RELEASE_STATE" | jq -r .isPrerelease)" = "$EXPECTED_PRERELEASE" ] || {
            echo "Release $TAG changed prerelease classification during draft staging."
            exit 1
        }
        echo "Verified release draft $TAG is ready for release-note review and publication approval."
    else
        echo "Published release $TAG already has the complete verified asset set."
    fi
    exit 0
fi

release_notes_are_ready() {
    local state="$1"
    printf '%s' "$state" | jq -e --arg placeholder "$PLACEHOLDER_NOTES" '
        (.body // "") as $body |
        ($body | gsub("\\s"; "")) as $normalized_body |
        ($placeholder | gsub("\\s"; "")) as $normalized_placeholder |
        ($normalized_body != $normalized_placeholder) and ($normalized_body | length > 0)
    ' >/dev/null
}

release_notes_are_ready "$RELEASE_STATE" || {
    echo "Release $TAG still has empty or placeholder release notes."
    exit 1
}

if [ "$RELEASE_IS_DRAFT" = "false" ]; then
    echo "Published release $TAG already has the complete verified asset set and reviewed notes."
    exit 0
fi

RELEASE_STATE="$(gh release view "$TAG" --json isDraft,isPrerelease,body,name)"
[ "$(printf '%s' "$RELEASE_STATE" | jq -r .isDraft)" = "true" ] || {
    echo "Release $TAG was published before final verification completed."
    exit 1
}
[ "$(printf '%s' "$RELEASE_STATE" | jq -r .isPrerelease)" = "$EXPECTED_PRERELEASE" ] || {
    echo "Release $TAG changed prerelease classification before publication."
    exit 1
}
release_notes_are_ready "$RELEASE_STATE" || {
    echo "Release $TAG still has empty or placeholder release notes."
    exit 1
}

REVIEWED_BODY="$(printf '%s' "$RELEASE_STATE" | jq -c '.body')"
REVIEWED_NAME="$(printf '%s' "$RELEASE_STATE" | jq -c '.name')"

gh release edit "$TAG" --draft=false

PUBLISHED_STATE="$(gh release view "$TAG" --json isDraft,isPrerelease,body,name)"
[ "$(printf '%s' "$PUBLISHED_STATE" | jq -r .isDraft)" = "false" ] || {
    echo "Release $TAG remained a draft after publication."
    exit 1
}
[ "$(printf '%s' "$PUBLISHED_STATE" | jq -r .isPrerelease)" = "$EXPECTED_PRERELEASE" ] || {
    echo "Published release $TAG has the wrong prerelease classification."
    exit 1
}
[ "$(printf '%s' "$PUBLISHED_STATE" | jq -c '.body')" = "$REVIEWED_BODY" ] || {
    echo "Published release $TAG did not preserve the reviewed release notes."
    exit 1
}
[ "$(printf '%s' "$PUBLISHED_STATE" | jq -c '.name')" = "$REVIEWED_NAME" ] || {
    echo "Published release $TAG did not preserve the reviewed title."
    exit 1
}

echo "Published verified release $TAG with its reviewed title and notes unchanged."
