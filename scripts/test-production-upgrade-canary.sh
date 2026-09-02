#!/bin/bash

set -euo pipefail
umask 0022

usage() {
    cat <<EOF
Usage: $0 \
  --previous-archive <path> --previous-sha256 <digest> \
  --previous-tag <tag> --previous-version <version> \
  --previous-channel <channel> --previous-target <target> \
  --previous-commit <sha> \
  --expected-tag <tag> --expected-version <version> \
  --expected-channel <channel> --expected-target <target> \
  --expected-commit <sha> [--work-dir <dir>]
EOF
    exit 1
}

fail() {
    echo "$1" >&2
    exit 1
}

validate_archive_paths() {
    local archive="$1"
    local entry=""

    while IFS= read -r entry; do
        case "$entry" in
            /*) fail "Archive contains an absolute path: $entry" ;;
        esac
        if printf '%s\n' "$entry" | grep -Eq '(^|/)\.\.(/|$)'; then
            fail "Archive contains a parent-directory traversal path: $entry"
        fi
    done < <(tar -tzf "$archive")
}

SCRIPT_DIR="$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)"
PREVIOUS_ARCHIVE=""
PREVIOUS_SHA256=""
PREVIOUS_TAG=""
PREVIOUS_VERSION=""
PREVIOUS_CHANNEL=""
PREVIOUS_TARGET=""
PREVIOUS_COMMIT=""
EXPECTED_TAG=""
EXPECTED_VERSION=""
EXPECTED_CHANNEL=""
EXPECTED_TARGET=""
EXPECTED_COMMIT=""
WORK_DIR=""

while [ "$#" -gt 0 ]; do
    case "$1" in
        --previous-archive) PREVIOUS_ARCHIVE="${2:-}"; shift 2 ;;
        --previous-sha256) PREVIOUS_SHA256="${2:-}"; shift 2 ;;
        --previous-tag) PREVIOUS_TAG="${2:-}"; shift 2 ;;
        --previous-version) PREVIOUS_VERSION="${2:-}"; shift 2 ;;
        --previous-channel) PREVIOUS_CHANNEL="${2:-}"; shift 2 ;;
        --previous-target) PREVIOUS_TARGET="${2:-}"; shift 2 ;;
        --previous-commit) PREVIOUS_COMMIT="${2:-}"; shift 2 ;;
        --expected-tag) EXPECTED_TAG="${2:-}"; shift 2 ;;
        --expected-version) EXPECTED_VERSION="${2:-}"; shift 2 ;;
        --expected-channel) EXPECTED_CHANNEL="${2:-}"; shift 2 ;;
        --expected-target) EXPECTED_TARGET="${2:-}"; shift 2 ;;
        --expected-commit) EXPECTED_COMMIT="${2:-}"; shift 2 ;;
        --work-dir) WORK_DIR="${2:-}"; shift 2 ;;
        *) usage ;;
    esac
done

for required in \
    PREVIOUS_ARCHIVE PREVIOUS_SHA256 PREVIOUS_TAG PREVIOUS_VERSION \
    PREVIOUS_CHANNEL PREVIOUS_TARGET PREVIOUS_COMMIT EXPECTED_TAG \
    EXPECTED_VERSION EXPECTED_CHANNEL EXPECTED_TARGET EXPECTED_COMMIT
do
    [ -n "${!required}" ] || usage
done

[ -f "$PREVIOUS_ARCHIVE" ] || fail "Previous archive not found: $PREVIOUS_ARCHIVE"
[ "$EXPECTED_TARGET" = "$PREVIOUS_TARGET" ] || fail "Production canary target $EXPECTED_TARGET does not match baseline target $PREVIOUS_TARGET."
printf '%s\n' "$PREVIOUS_SHA256" | grep -Eq '^[0-9a-f]{64}$' || fail "Previous archive SHA-256 must be 64 lowercase hexadecimal characters."
command -v script >/dev/null || fail "The util-linux script command is required for the confirmation PTY."

ACTUAL_PREVIOUS_SHA256="$(sha256sum "$PREVIOUS_ARCHIVE" | awk '{print $1}')"
[ "$ACTUAL_PREVIOUS_SHA256" = "$PREVIOUS_SHA256" ] || fail "Previous archive digest is $ACTUAL_PREVIOUS_SHA256, expected $PREVIOUS_SHA256."
python3 "$SCRIPT_DIR/release_bundle_manifest.py" validate \
    --archive "$PREVIOUS_ARCHIVE" \
    --expected-release-tag "$PREVIOUS_TAG" \
    --expected-version "$PREVIOUS_VERSION" \
    --expected-channel "$PREVIOUS_CHANNEL" \
    --expected-target "$PREVIOUS_TARGET" \
    --expected-commit "$PREVIOUS_COMMIT"
PYTHONPATH="$SCRIPT_DIR" python3 - "$PREVIOUS_VERSION" "$EXPECTED_VERSION" <<'PY'
import sys
from release_promotion import SemVer

if SemVer.parse(sys.argv[2]) <= SemVer.parse(sys.argv[1]):
    raise SystemExit(
        f"expected candidate {sys.argv[2]} must advance baseline {sys.argv[1]}"
    )
PY
validate_archive_paths "$PREVIOUS_ARCHIVE"

CLEANUP_WORK_DIR=0
if [ -z "$WORK_DIR" ]; then
    WORK_DIR="$(mktemp -d)"
    CLEANUP_WORK_DIR=1
else
    mkdir -p "$WORK_DIR"
fi

cleanup() {
    if [ "$CLEANUP_WORK_DIR" -eq 1 ]; then
        rm -rf "$WORK_DIR"
    fi
}
trap cleanup EXIT

EXTRACT_DIR="$WORK_DIR/previous"
mkdir -p "$EXTRACT_DIR"
tar --no-same-owner -C "$EXTRACT_DIR" -xzf "$PREVIOUS_ARCHIVE"
mapfile -t PREVIOUS_ROOTS < <(find "$EXTRACT_DIR" -mindepth 1 -maxdepth 1 -type d -print)
[ "${#PREVIOUS_ROOTS[@]}" -eq 1 ] || fail "Previous archive must contain exactly one top-level directory."
PREVIOUS_BUNDLE="${PREVIOUS_ROOTS[0]}"
[ -x "$PREVIOUS_BUNDLE/install.sh" ] || fail "Previous installer is not executable."
[ -x "$PREVIOUS_BUNDLE/lg-buddy" ] || fail "Previous binary is not executable."
python3 "$SCRIPT_DIR/release_bundle_manifest.py" validate \
    --manifest "$PREVIOUS_BUNDLE/release-manifest.json" \
    --binary "$PREVIOUS_BUNDLE/lg-buddy" \
    --expected-release-tag "$PREVIOUS_TAG" \
    --expected-version "$PREVIOUS_VERSION" \
    --expected-channel "$PREVIOUS_CHANNEL" \
    --expected-target "$PREVIOUS_TARGET" \
    --expected-commit "$PREVIOUS_COMMIT"

INSTALL_ROOT="$WORK_DIR/root"
HOME_DIR="$WORK_DIR/home"
XDG_CONFIG_HOME="$HOME_DIR/.config"
XDG_CACHE_HOME="$HOME_DIR/.cache"
mkdir -p "$INSTALL_ROOT" "$HOME_DIR/Desktop" "$XDG_CACHE_HOME"

export HOME="$HOME_DIR"
export XDG_CONFIG_HOME
export XDG_CACHE_HOME
export LG_BUDDY_INSTALL_ROOT="$INSTALL_ROOT"
export LG_BUDDY_SUDO_CMD="none"
export LG_BUDDY_NONINTERACTIVE="1"
export LG_BUDDY_SKIP_SYSTEMD_ACTIONS="1"
export LG_BUDDY_SKIP_PIP_INSTALL="1"
export LG_BUDDY_TV_IP="192.168.60.20"
export LG_BUDDY_TV_MAC="02:00:00:00:00:60"
export LG_BUDDY_INPUT="HDMI_3"
export LG_BUDDY_SCREEN_BACKEND="auto"
export LG_BUDDY_SYSTEM_SLEEP_WAKE_POLICY="enabled"
export PIP_DISABLE_PIP_VERSION_CHECK="1"
export PIP_NO_PYTHON_VERSION_WARNING="1"

(
    cd "$PREVIOUS_BUNDLE"
    ./install.sh
)

CONFIG_FILE="$XDG_CONFIG_HOME/lg-buddy/config.env"
INSTALLED_BINARY="$INSTALL_ROOT/usr/bin/lg-buddy"
INSTALLED_POINTER="$INSTALL_ROOT/usr/lib/lg-buddy/config-path"
NATIVE_TOKEN_FILE="$XDG_CONFIG_HOME/lg-buddy/tvs/primary/access-token.json"
VENV_MARKER="$INSTALL_ROOT/usr/bin/LG_Buddy_PIP/production-canary-native-marker"
[ -x "$INSTALLED_BINARY" ] || fail "Baseline binary was not installed."
[ -f "$CONFIG_FILE" ] || fail "Baseline config was not installed."
[ -f "$INSTALLED_POINTER" ] || fail "Baseline config pointer was not installed."

export LG_BUDDY_CONFIG="$CONFIG_FILE"
"$INSTALLED_BINARY" settings set screen.backend gnome
"$INSTALLED_BINARY" settings set screen.idle_blank disabled
"$INSTALLED_BINARY" settings set updates.auto_check disabled
"$INSTALLED_BINARY" settings set updates.channel prerelease
sed -i 's/^tvs_primary_platform=bscpylgtv$/tvs_primary_platform=lg_webos/' "$CONFIG_FILE"
grep -q '^tvs_primary_platform=lg_webos$' "$CONFIG_FILE"
mkdir -p "$(dirname "$NATIVE_TOKEN_FILE")"
printf '%s\n' '{"access_token":"production-canary-native-token"}' >"$NATIVE_TOKEN_FILE"
chmod 600 "$NATIVE_TOKEN_FILE"
touch "$VENV_MARKER"

CONFIG_SNAPSHOT="$WORK_DIR/config.snapshot"
POINTER_SNAPSHOT="$WORK_DIR/config-pointer.snapshot"
TOKEN_SNAPSHOT="$WORK_DIR/access-token.snapshot"
cp "$CONFIG_FILE" "$CONFIG_SNAPSHOT"
cp "$INSTALLED_POINTER" "$POINTER_SNAPSHOT"
cp "$NATIVE_TOKEN_FILE" "$TOKEN_SNAPSHOT"

CANARY_OUTPUT="$WORK_DIR/production-upgrade-canary.output"
CANARY_COMMAND="$(printf '%q' "$INSTALLED_BINARY") updates install"
printf 'yes\n' | script -qefc "$CANARY_COMMAND" /dev/null >"$CANARY_OUTPUT"

grep -F -q "Current: $PREVIOUS_VERSION ($PREVIOUS_CHANNEL, commit $PREVIOUS_COMMIT)" "$CANARY_OUTPUT"
grep -F -q "Target: $EXPECTED_VERSION ($EXPECTED_CHANNEL, commit $EXPECTED_COMMIT)" "$CANARY_OUTPUT"
grep -F -q "Release: https://github.com/Staphylococcus/LG_Buddy/releases/tag/$EXPECTED_TAG" "$CANARY_OUTPUT"
grep -F -q "Installed: $EXPECTED_VERSION ($EXPECTED_CHANNEL, commit $EXPECTED_COMMIT)" "$CANARY_OUTPUT"

EXPECTED_VERSION_OUTPUT="$(printf 'lg-buddy %s\nversion: %s\nchannel: %s\ncommit: %s' \
    "$EXPECTED_VERSION" "$EXPECTED_VERSION" "$EXPECTED_CHANNEL" "$EXPECTED_COMMIT")"
ACTUAL_VERSION_OUTPUT="$("$INSTALLED_BINARY" --version)"
[ "$ACTUAL_VERSION_OUTPUT" = "$EXPECTED_VERSION_OUTPUT" ] || fail "Installed identity does not match the published canary target."
cmp -s "$CONFIG_SNAPSHOT" "$CONFIG_FILE" || fail "Production upgrade changed the user configuration."
cmp -s "$POINTER_SNAPSHOT" "$INSTALLED_POINTER" || fail "Production upgrade changed the config pointer."
cmp -s "$TOKEN_SNAPSHOT" "$NATIVE_TOKEN_FILE" || fail "Production upgrade changed the native credential."
[ -e "$VENV_MARKER" ] || fail "Production native upgrade recreated the Python environment."
"$INSTALLED_BINARY" settings get updates.channel | grep -q '^prerelease$'

CANDIDATE_CHECK_OUTPUT="$WORK_DIR/candidate-update-check.output"
UPDATE_CACHE_FILE="$XDG_CACHE_HOME/lg-buddy/update-check.json"
rm -f "$UPDATE_CACHE_FILE"
"$INSTALLED_BINARY" updates check >"$CANDIDATE_CHECK_OUTPUT"
grep -F -q "status: up to date" "$CANDIDATE_CHECK_OUTPUT"
grep -F -q "current: $EXPECTED_VERSION ($EXPECTED_CHANNEL)" "$CANDIDATE_CHECK_OUTPUT"
grep -F -q "latest: $EXPECTED_VERSION ($EXPECTED_CHANNEL)" "$CANDIDATE_CHECK_OUTPUT"
grep -F -q "url: https://github.com/Staphylococcus/LG_Buddy/releases/tag/$EXPECTED_TAG" "$CANDIDATE_CHECK_OUTPUT"
{
    echo
    echo "Published candidate update check:"
    cat "$CANDIDATE_CHECK_OUTPUT"
} >>"$CANARY_OUTPUT"

echo "Production GitHub upgrade canary passed: $PREVIOUS_VERSION -> $EXPECTED_VERSION"
