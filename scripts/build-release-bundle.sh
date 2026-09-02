#!/bin/bash

set -euo pipefail

usage() {
    echo "Usage: $0 --target <rust-target> --version <version> [--output-dir <dir>]"
    exit 1
}

SCRIPT_DIR="$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)"
REPO_ROOT="$(CDPATH= cd -- "$SCRIPT_DIR/.." && pwd)"
OUTPUT_DIR="$REPO_ROOT/dist"
TARGET=""
VERSION=""

while [ "$#" -gt 0 ]; do
    case "$1" in
        --target)
            TARGET="${2:-}"
            shift 2
            ;;
        --version)
            VERSION="${2:-}"
            shift 2
            ;;
        --output-dir)
            OUTPUT_DIR="${2:-}"
            shift 2
            ;;
        *)
            usage
            ;;
    esac
done

[ -n "$TARGET" ] || usage
[ -n "$VERSION" ] || usage

BINARY_PATH="$REPO_ROOT/target/$TARGET/release/lg-buddy"
BUNDLE_NAME="lg-buddy-$VERSION-$TARGET"
BUNDLE_DIR="$OUTPUT_DIR/$BUNDLE_NAME"
ARCHIVE_PATH="$OUTPUT_DIR/$BUNDLE_NAME.tar.gz"

if [ ! -f "$BINARY_PATH" ]; then
    echo "Expected release binary at $BINARY_PATH"
    exit 1
fi

rm -rf "$BUNDLE_DIR" "$ARCHIVE_PATH"

install -d "$BUNDLE_DIR"
install -d "$BUNDLE_DIR/bin"
install -d "$BUNDLE_DIR/docs"
install -d "$BUNDLE_DIR/systemd"

install -m 755 "$BINARY_PATH" "$BUNDLE_DIR/lg-buddy"
install -m 755 "$REPO_ROOT/install.sh" "$BUNDLE_DIR/install.sh"
install -m 755 "$REPO_ROOT/configure.sh" "$BUNDLE_DIR/configure.sh"
install -m 755 "$REPO_ROOT/uninstall.sh" "$BUNDLE_DIR/uninstall.sh"
install -m 755 "$REPO_ROOT/bin/LG_Buddy_Common" "$BUNDLE_DIR/bin/LG_Buddy_Common"
install -m 644 "$REPO_ROOT/LG_Buddy_Brightness.desktop" "$BUNDLE_DIR/LG_Buddy_Brightness.desktop"
install -m 644 "$REPO_ROOT/README.md" "$BUNDLE_DIR/README.md"
install -m 644 "$REPO_ROOT/LICENSE" "$BUNDLE_DIR/LICENSE"
python3 "$SCRIPT_DIR/release_bundle_manifest.py" create \
    --output "$BUNDLE_DIR/release-manifest.json" \
    --release-tag "v$VERSION" \
    --target "$TARGET" \
    --binary "$BUNDLE_DIR/lg-buddy"
chmod 644 "$BUNDLE_DIR/release-manifest.json"
install -m 644 "$REPO_ROOT/systemd/LG_Buddy.service" "$BUNDLE_DIR/systemd/LG_Buddy.service"
install -m 644 "$REPO_ROOT/systemd/LG_Buddy_lifecycle.service" "$BUNDLE_DIR/systemd/LG_Buddy_lifecycle.service"
install -m 644 "$REPO_ROOT/systemd/LG_Buddy_screen.service" "$BUNDLE_DIR/systemd/LG_Buddy_screen.service"
install -m 644 "$REPO_ROOT/systemd/LG_Buddy_update_check.service" "$BUNDLE_DIR/systemd/LG_Buddy_update_check.service"
install -m 644 "$REPO_ROOT/systemd/LG_Buddy_update_check.timer" "$BUNDLE_DIR/systemd/LG_Buddy_update_check.timer"
install -m 644 "$REPO_ROOT/systemd/lg_buddy.conf" "$BUNDLE_DIR/systemd/lg_buddy.conf"
cp -R "$REPO_ROOT/docs/." "$BUNDLE_DIR/docs/"

tar -C "$OUTPUT_DIR" -czf "$ARCHIVE_PATH" "$BUNDLE_NAME"
echo "Created $ARCHIVE_PATH"
