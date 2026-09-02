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
  --candidate-archive <path> --candidate-tag <tag> \
  --candidate-version <version> --candidate-channel <channel> \
  --candidate-target <target> --candidate-commit <sha> \
  [--work-dir <dir>]
EOF
    exit 1
}

fail() {
    echo "$1" >&2
    exit 1
}

assert_file() {
    [ -f "$1" ] || fail "Expected file not found: $1"
}

assert_executable() {
    [ -x "$1" ] || fail "Expected executable not found: $1"
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

extract_bundle() {
    local archive="$1"
    local destination="$2"
    local roots=()

    mkdir -p "$destination"
    tar --no-same-owner -C "$destination" -xzf "$archive"
    mapfile -t roots < <(find "$destination" -mindepth 1 -maxdepth 1 -type d -print)
    [ "${#roots[@]}" -eq 1 ] || fail "Release archive must contain exactly one top-level directory: $archive"
    printf '%s\n' "${roots[0]}"
}

tree_digest() {
    local install_root="$1"
    local user_home="$2"

    tar \
        --sort=name \
        --mtime='@0' \
        --owner=0 \
        --group=0 \
        --numeric-owner \
        -C "$(dirname "$install_root")" \
        -cf - "$(basename "$install_root")" \
        -C "$(dirname "$user_home")" \
        "$(basename "$user_home")" |
        sha256sum | awk '{print $1}'
}

SCRIPT_DIR="$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)"
PREVIOUS_ARCHIVE=""
PREVIOUS_SHA256=""
PREVIOUS_TAG=""
PREVIOUS_VERSION=""
PREVIOUS_CHANNEL=""
PREVIOUS_TARGET=""
PREVIOUS_COMMIT=""
CANDIDATE_ARCHIVE=""
CANDIDATE_TAG=""
CANDIDATE_VERSION=""
CANDIDATE_CHANNEL=""
CANDIDATE_TARGET=""
CANDIDATE_COMMIT=""
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
        --candidate-archive) CANDIDATE_ARCHIVE="${2:-}"; shift 2 ;;
        --candidate-tag) CANDIDATE_TAG="${2:-}"; shift 2 ;;
        --candidate-version) CANDIDATE_VERSION="${2:-}"; shift 2 ;;
        --candidate-channel) CANDIDATE_CHANNEL="${2:-}"; shift 2 ;;
        --candidate-target) CANDIDATE_TARGET="${2:-}"; shift 2 ;;
        --candidate-commit) CANDIDATE_COMMIT="${2:-}"; shift 2 ;;
        --work-dir) WORK_DIR="${2:-}"; shift 2 ;;
        *) usage ;;
    esac
done

for required in \
    PREVIOUS_ARCHIVE PREVIOUS_SHA256 PREVIOUS_TAG PREVIOUS_VERSION \
    PREVIOUS_CHANNEL PREVIOUS_TARGET PREVIOUS_COMMIT CANDIDATE_ARCHIVE \
    CANDIDATE_TAG CANDIDATE_VERSION CANDIDATE_CHANNEL CANDIDATE_TARGET \
    CANDIDATE_COMMIT
do
    [ -n "${!required}" ] || usage
done

assert_file "$PREVIOUS_ARCHIVE"
assert_file "$CANDIDATE_ARCHIVE"
printf '%s\n' "$PREVIOUS_SHA256" | grep -Eq '^[0-9a-f]{64}$' || fail "Previous archive SHA-256 must be 64 lowercase hexadecimal characters."
command -v strace >/dev/null || fail "strace is required for the no-network refusal control."

ACTUAL_PREVIOUS_SHA256="$(sha256sum "$PREVIOUS_ARCHIVE" | awk '{print $1}')"
[ "$ACTUAL_PREVIOUS_SHA256" = "$PREVIOUS_SHA256" ] || fail "Previous archive digest is $ACTUAL_PREVIOUS_SHA256, expected $PREVIOUS_SHA256."

python3 "$SCRIPT_DIR/release_bundle_manifest.py" validate \
    --archive "$PREVIOUS_ARCHIVE" \
    --expected-release-tag "$PREVIOUS_TAG" \
    --expected-version "$PREVIOUS_VERSION" \
    --expected-channel "$PREVIOUS_CHANNEL" \
    --expected-target "$PREVIOUS_TARGET" \
    --expected-commit "$PREVIOUS_COMMIT"
python3 "$SCRIPT_DIR/release_bundle_manifest.py" validate \
    --archive "$CANDIDATE_ARCHIVE" \
    --expected-release-tag "$CANDIDATE_TAG" \
    --expected-version "$CANDIDATE_VERSION" \
    --expected-channel "$CANDIDATE_CHANNEL" \
    --expected-target "$CANDIDATE_TARGET" \
    --expected-commit "$CANDIDATE_COMMIT"

PYTHONPATH="$SCRIPT_DIR" python3 - "$PREVIOUS_VERSION" "$CANDIDATE_VERSION" <<'PY'
import sys
from release_promotion import SemVer

previous = SemVer.parse(sys.argv[1])
candidate = SemVer.parse(sys.argv[2])
if candidate <= previous:
    raise SystemExit(
        f"candidate version {sys.argv[2]} must advance previous version {sys.argv[1]}"
    )
PY

validate_archive_paths "$PREVIOUS_ARCHIVE"
validate_archive_paths "$CANDIDATE_ARCHIVE"

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

PREVIOUS_BUNDLE="$(extract_bundle "$PREVIOUS_ARCHIVE" "$WORK_DIR/previous")"
CANDIDATE_BUNDLE="$(extract_bundle "$CANDIDATE_ARCHIVE" "$WORK_DIR/candidate")"
assert_executable "$PREVIOUS_BUNDLE/install.sh"
assert_executable "$PREVIOUS_BUNDLE/lg-buddy"
assert_executable "$CANDIDATE_BUNDLE/install.sh"
assert_executable "$CANDIDATE_BUNDLE/lg-buddy"
python3 "$SCRIPT_DIR/release_bundle_manifest.py" validate \
    --manifest "$PREVIOUS_BUNDLE/release-manifest.json" \
    --binary "$PREVIOUS_BUNDLE/lg-buddy" \
    --expected-release-tag "$PREVIOUS_TAG" \
    --expected-version "$PREVIOUS_VERSION" \
    --expected-channel "$PREVIOUS_CHANNEL" \
    --expected-target "$PREVIOUS_TARGET" \
    --expected-commit "$PREVIOUS_COMMIT"
python3 "$SCRIPT_DIR/release_bundle_manifest.py" validate \
    --manifest "$CANDIDATE_BUNDLE/release-manifest.json" \
    --binary "$CANDIDATE_BUNDLE/lg-buddy" \
    --expected-release-tag "$CANDIDATE_TAG" \
    --expected-version "$CANDIDATE_VERSION" \
    --expected-channel "$CANDIDATE_CHANNEL" \
    --expected-target "$CANDIDATE_TARGET" \
    --expected-commit "$CANDIDATE_COMMIT"

INSTALL_ROOT="$WORK_DIR/root"
HOME_DIR="$WORK_DIR/home"
XDG_CONFIG_HOME="$HOME_DIR/.config"
mkdir -p "$INSTALL_ROOT" "$HOME_DIR/Desktop"

export HOME="$HOME_DIR"
export XDG_CONFIG_HOME
export LG_BUDDY_INSTALL_ROOT="$INSTALL_ROOT"
export LG_BUDDY_SUDO_CMD="none"
export LG_BUDDY_NONINTERACTIVE="1"
export LG_BUDDY_SKIP_SYSTEMD_ACTIONS="1"
export LG_BUDDY_SKIP_PIP_INSTALL="1"
export LG_BUDDY_TV_IP="192.168.50.20"
export LG_BUDDY_TV_MAC="02:00:00:00:00:20"
export LG_BUDDY_INPUT="HDMI_3"
export LG_BUDDY_TV_PLATFORM="bscpylgtv"
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
SYSTEM_SERVICE="$INSTALL_ROOT/etc/systemd/system/LG_Buddy.service"
LIFECYCLE_SERVICE="$INSTALL_ROOT/etc/systemd/system/LG_Buddy_lifecycle.service"
TMPFILES_CONFIG="$INSTALL_ROOT/etc/tmpfiles.d/lg_buddy.conf"
SYSTEM_SERVICE_OVERRIDE="$INSTALL_ROOT/etc/systemd/system/LG_Buddy.service.d/config.conf"
LIFECYCLE_SERVICE_OVERRIDE="$INSTALL_ROOT/etc/systemd/system/LG_Buddy_lifecycle.service.d/config.conf"
NM_LIFECYCLE_HOOK="$INSTALL_ROOT/etc/NetworkManager/dispatcher.d/pre-down.d/LG_Buddy_lifecycle"
SYSTEM_DESKTOP_ENTRY="$INSTALL_ROOT/usr/share/applications/LG_Buddy_Brightness.desktop"
USER_DESKTOP_ENTRY="$HOME_DIR/Desktop/LG_Buddy_Brightness.desktop"
USER_SCREEN_SERVICE="$HOME_DIR/.config/systemd/user/LG_Buddy_screen.service"
USER_SCREEN_OVERRIDE="$HOME_DIR/.config/systemd/user/LG_Buddy_screen.service.d/config.conf"
USER_UPDATE_SERVICE="$HOME_DIR/.config/systemd/user/LG_Buddy_update_check.service"
USER_UPDATE_TIMER="$HOME_DIR/.config/systemd/user/LG_Buddy_update_check.timer"
USER_UPDATE_OVERRIDE="$HOME_DIR/.config/systemd/user/LG_Buddy_update_check.service.d/config.conf"
NATIVE_TOKEN_FILE="$XDG_CONFIG_HOME/lg-buddy/tvs/primary/access-token.json"
VENV_MARKER="$INSTALL_ROOT/usr/bin/LG_Buddy_PIP/cross-version-native-marker"

for installed_path in \
    "$CONFIG_FILE" "$INSTALLED_BINARY" "$INSTALLED_POINTER" "$SYSTEM_SERVICE" \
    "$LIFECYCLE_SERVICE" "$TMPFILES_CONFIG" "$NM_LIFECYCLE_HOOK" \
    "$SYSTEM_DESKTOP_ENTRY" "$USER_DESKTOP_ENTRY" "$USER_SCREEN_SERVICE" \
    "$USER_UPDATE_SERVICE" "$USER_UPDATE_TIMER"
do
    assert_file "$installed_path"
done

export LG_BUDDY_CONFIG="$CONFIG_FILE"
"$INSTALLED_BINARY" settings set screen.backend swayidle
"$INSTALLED_BINARY" settings set screen.idle_timeout 900
"$INSTALLED_BINARY" settings set screen.restore_policy aggressive
"$INSTALLED_BINARY" settings set screen.idle_blank disabled
"$INSTALLED_BINARY" settings set system.sleep_wake_policy disabled
"$INSTALLED_BINARY" settings set tv.ip 192.168.50.21
"$INSTALLED_BINARY" settings set tv.mac 02:00:00:00:00:21
"$INSTALLED_BINARY" settings set tv.input HDMI_4
"$INSTALLED_BINARY" settings set updates.auto_check disabled
"$INSTALLED_BINARY" settings set updates.channel prerelease
sed -i 's/^tvs_primary_platform=bscpylgtv$/tvs_primary_platform=lg_webos/' "$CONFIG_FILE"
grep -q '^tvs_primary_platform=lg_webos$' "$CONFIG_FILE"

mkdir -p "$(dirname "$NATIVE_TOKEN_FILE")"
printf '%s\n' '{"access_token":"cross-version-native-token"}' >"$NATIVE_TOKEN_FILE"
chmod 600 "$NATIVE_TOKEN_FILE"
touch "$VENV_MARKER"

CONFIG_SNAPSHOT="$WORK_DIR/config.snapshot"
POINTER_SNAPSHOT="$WORK_DIR/config-pointer.snapshot"
TOKEN_SNAPSHOT="$WORK_DIR/access-token.snapshot"
cp "$CONFIG_FILE" "$CONFIG_SNAPSHOT"
cp "$INSTALLED_POINTER" "$POINTER_SNAPSHOT"
cp "$NATIVE_TOKEN_FILE" "$TOKEN_SNAPSHOT"

BASELINE_PREFLIGHT_OUTPUT="$WORK_DIR/compatible-baseline-preflight.output"
"$PREVIOUS_BUNDLE/lg-buddy" upgrade-preflight "$PREVIOUS_BUNDLE" >"$BASELINE_PREFLIGHT_OUTPUT"
grep -F -x -q 'upgrade preflight: compatible' "$BASELINE_PREFLIGHT_OUTPUT"

CANDIDATE_PREFLIGHT_OUTPUT="$WORK_DIR/compatible-candidate-preflight.output"
"$CANDIDATE_BUNDLE/lg-buddy" upgrade-preflight "$CANDIDATE_BUNDLE" >"$CANDIDATE_PREFLIGHT_OUTPUT"
grep -F -x -q 'upgrade preflight: compatible' "$CANDIDATE_PREFLIGHT_OUTPUT"

INSTALLER_STUB_DIR="$WORK_DIR/installer-stubs"
SUDO_MARKER="$WORK_DIR/sudo-invoked"
SUDO_SPY="$INSTALLER_STUB_DIR/sudo-spy"
REFUSAL_OUTPUT="$WORK_DIR/initial-preflight-refusal.output"
NETWORK_TRACE="$WORK_DIR/initial-preflight-network.trace"
mkdir -p "$INSTALLER_STUB_DIR"
cat >"$SUDO_SPY" <<'EOF'
#!/bin/sh
: >"${LG_BUDDY_SUDO_MARKER:?}"
exit 97
EOF
chmod 755 "$SUDO_SPY"

mv "$SYSTEM_SERVICE" "$SYSTEM_SERVICE.incompatible"
REFUSAL_TREE_BEFORE="$(tree_digest "$INSTALL_ROOT" "$HOME_DIR")"
REFUSAL_STATUS=0
if (
    LG_BUDDY_SUDO_CMD="$SUDO_SPY" \
    LG_BUDDY_SUDO_MARKER="$SUDO_MARKER" \
    strace -f -qq -e trace=network -o "$NETWORK_TRACE" \
        "$INSTALLED_BINARY" updates install >"$REFUSAL_OUTPUT" 2>&1
); then
    fail "Incompatible previous installation unexpectedly passed the initial preflight."
else
    REFUSAL_STATUS=$?
fi
[ "$REFUSAL_STATUS" -eq 1 ] || fail "Initial preflight refusal returned status $REFUSAL_STATUS instead of 1."
grep -F -q 'upgrade preflight: refused' "$REFUSAL_OUTPUT"
grep -F -q "$SYSTEM_SERVICE" "$REFUSAL_OUTPUT"
[ ! -e "$SUDO_MARKER" ] || fail "Initial preflight refusal invoked sudo."
if grep -E -q 'AF_INET|AF_INET6' "$NETWORK_TRACE"; then
    fail "Initial preflight refusal attempted network access."
fi
REFUSAL_TREE_AFTER="$(tree_digest "$INSTALL_ROOT" "$HOME_DIR")"
[ "$REFUSAL_TREE_BEFORE" = "$REFUSAL_TREE_AFTER" ] || fail "Initial preflight refusal mutated the installation or user state."
mv "$SYSTEM_SERVICE.incompatible" "$SYSTEM_SERVICE"

CANDIDATE_REFUSAL_OUTPUT="$WORK_DIR/candidate-preflight-refusal.output"
CANDIDATE_NETWORK_TRACE="$WORK_DIR/candidate-preflight-network.trace"
CANDIDATE_SERVICE="$CANDIDATE_BUNDLE/systemd/LG_Buddy.service"
mv "$CANDIDATE_SERVICE" "$CANDIDATE_SERVICE.incompatible"
CANDIDATE_REFUSAL_TREE_BEFORE="$(tree_digest "$INSTALL_ROOT" "$HOME_DIR")"
if (
    cd "$CANDIDATE_BUNDLE"
    LG_BUDDY_SUDO_CMD="$SUDO_SPY" \
    LG_BUDDY_SUDO_MARKER="$SUDO_MARKER" \
    strace -f -qq -e trace=network -o "$CANDIDATE_NETWORK_TRACE" \
        ./install.sh --upgrade >"$CANDIDATE_REFUSAL_OUTPUT" 2>&1
); then
    fail "Malformed candidate unexpectedly passed its preflight."
fi
grep -F -q 'upgrade preflight: refused' "$CANDIDATE_REFUSAL_OUTPUT"
grep -F -q "$CANDIDATE_SERVICE" "$CANDIDATE_REFUSAL_OUTPUT"
[ ! -e "$SUDO_MARKER" ] || fail "Candidate preflight refusal invoked sudo."
if grep -E -q 'AF_INET|AF_INET6' "$CANDIDATE_NETWORK_TRACE"; then
    fail "Candidate preflight refusal attempted network access."
fi
CANDIDATE_REFUSAL_TREE_AFTER="$(tree_digest "$INSTALL_ROOT" "$HOME_DIR")"
[ "$CANDIDATE_REFUSAL_TREE_BEFORE" = "$CANDIDATE_REFUSAL_TREE_AFTER" ] || fail "Candidate preflight refusal mutated the installation or user state."
mv "$CANDIDATE_SERVICE.incompatible" "$CANDIDATE_SERVICE"

for stale_target in \
    "$INSTALLED_BINARY" "$SYSTEM_SERVICE" "$LIFECYCLE_SERVICE" \
    "$TMPFILES_CONFIG" "$NM_LIFECYCLE_HOOK" "$SYSTEM_DESKTOP_ENTRY" \
    "$USER_DESKTOP_ENTRY" "$USER_SCREEN_SERVICE" "$USER_UPDATE_SERVICE" \
    "$USER_UPDATE_TIMER"
do
    printf 'stale previous-version asset\n' >"$stale_target"
done
chmod 755 "$INSTALLED_BINARY" "$NM_LIFECYCLE_HOOK"

SERVICE_ACTION_LOG="$WORK_DIR/service-actions.log"
EXPECTED_SERVICE_ACTION_LOG="$WORK_DIR/expected-service-actions.log"
UPGRADE_OUTPUT="$WORK_DIR/cross-version-upgrade.output"
cat >"$INSTALLER_STUB_DIR/systemctl" <<'EOF'
#!/bin/sh
set -eu
printf 'systemctl %s\n' "$*" >>"${LG_BUDDY_SERVICE_ACTION_LOG:?}"
case "$*" in
    is-system-running|"--user is-system-running") printf 'running\n' ;;
esac
EOF
cat >"$INSTALLER_STUB_DIR/systemd-tmpfiles" <<'EOF'
#!/bin/sh
set -eu
printf 'tmpfiles %s\n' "$*" >>"${LG_BUDDY_SERVICE_ACTION_LOG:?}"
EOF
chmod 755 "$INSTALLER_STUB_DIR/systemctl" "$INSTALLER_STUB_DIR/systemd-tmpfiles"

(
    export PATH="$INSTALLER_STUB_DIR:$PATH"
    export LG_BUDDY_SERVICE_ACTION_LOG="$SERVICE_ACTION_LOG"
    export LG_BUDDY_SKIP_SYSTEMD_ACTIONS="0"
    cd "$CANDIDATE_BUNDLE"
    ./install.sh --upgrade >"$UPGRADE_OUTPUT" 2>&1
)

grep -F -q 'Upgrade complete!' "$UPGRADE_OUTPUT"
cat >"$EXPECTED_SERVICE_ACTION_LOG" <<EOF
systemctl is-system-running
systemctl --user is-system-running
tmpfiles --create $TMPFILES_CONFIG
systemctl daemon-reload
systemctl enable LG_Buddy.service
systemctl enable LG_Buddy_lifecycle.service
systemctl restart LG_Buddy_lifecycle.service
systemctl --user daemon-reload
systemctl --user enable LG_Buddy_screen.service
systemctl --user restart LG_Buddy_screen.service
systemctl --user disable --now LG_Buddy_update_check.timer
EOF
cmp -s "$EXPECTED_SERVICE_ACTION_LOG" "$SERVICE_ACTION_LOG" || {
    diff -u "$EXPECTED_SERVICE_ACTION_LOG" "$SERVICE_ACTION_LOG" || true
    fail "Cross-version upgrade service actions did not match the defined order."
}

cmp -s "$CONFIG_SNAPSHOT" "$CONFIG_FILE" || fail "Cross-version upgrade changed the user configuration."
cmp -s "$POINTER_SNAPSHOT" "$INSTALLED_POINTER" || fail "Cross-version upgrade changed the installed config pointer."
cmp -s "$TOKEN_SNAPSHOT" "$NATIVE_TOKEN_FILE" || fail "Cross-version upgrade changed the native credential."
[ -e "$VENV_MARKER" ] || fail "Cross-version native upgrade recreated the Python environment."

cmp -s "$CANDIDATE_BUNDLE/lg-buddy" "$INSTALLED_BINARY"
cmp -s "$CANDIDATE_BUNDLE/systemd/LG_Buddy.service" "$SYSTEM_SERVICE"
cmp -s "$CANDIDATE_BUNDLE/systemd/LG_Buddy_lifecycle.service" "$LIFECYCLE_SERVICE"
cmp -s "$CANDIDATE_BUNDLE/systemd/lg_buddy.conf" "$TMPFILES_CONFIG"
cmp -s "$CANDIDATE_BUNDLE/LG_Buddy_Brightness.desktop" "$SYSTEM_DESKTOP_ENTRY"
cmp -s "$CANDIDATE_BUNDLE/LG_Buddy_Brightness.desktop" "$USER_DESKTOP_ENTRY"
cmp -s "$CANDIDATE_BUNDLE/systemd/LG_Buddy_screen.service" "$USER_SCREEN_SERVICE"
cmp -s "$CANDIDATE_BUNDLE/systemd/LG_Buddy_update_check.service" "$USER_UPDATE_SERVICE"
cmp -s "$CANDIDATE_BUNDLE/systemd/LG_Buddy_update_check.timer" "$USER_UPDATE_TIMER"
grep -F -q 'exec /usr/bin/lg-buddy nm-pre-down' "$NM_LIFECYCLE_HOOK"
for override in \
    "$SYSTEM_SERVICE_OVERRIDE" "$LIFECYCLE_SERVICE_OVERRIDE" \
    "$USER_SCREEN_OVERRIDE" "$USER_UPDATE_OVERRIDE"
do
    assert_file "$override"
    grep -F -q "LG_BUDDY_CONFIG=$CONFIG_FILE" "$override"
done

grep -q '^screen_backend=swayidle$' "$CONFIG_FILE"
grep -q '^screen_idle_timeout=900$' "$CONFIG_FILE"
grep -q '^screen_restore_policy=aggressive$' "$CONFIG_FILE"
grep -q '^screen_idle_blank=disabled$' "$CONFIG_FILE"
grep -q '^system_sleep_wake_policy=disabled$' "$CONFIG_FILE"
grep -q '^tvs_primary_ip=192.168.50.21$' "$CONFIG_FILE"
grep -q '^tvs_primary_mac=02:00:00:00:00:21$' "$CONFIG_FILE"
grep -q '^tvs_primary_input=HDMI_4$' "$CONFIG_FILE"
grep -q '^tvs_primary_platform=lg_webos$' "$CONFIG_FILE"
grep -q '^updates_auto_check=disabled$' "$CONFIG_FILE"
grep -q '^updates_channel=prerelease$' "$CONFIG_FILE"
"$INSTALLED_BINARY" settings describe screen.backend \
    | grep -F -q 'deprecation: swayidle is a deprecated compatibility backend planned for removal in LG Buddy 2.0.0'

python3 "$SCRIPT_DIR/release_bundle_manifest.py" validate \
    --manifest "$CANDIDATE_BUNDLE/release-manifest.json" \
    --binary "$INSTALLED_BINARY" \
    --expected-release-tag "$CANDIDATE_TAG" \
    --expected-version "$CANDIDATE_VERSION" \
    --expected-channel "$CANDIDATE_CHANNEL" \
    --expected-target "$CANDIDATE_TARGET" \
    --expected-commit "$CANDIDATE_COMMIT"

echo "Cross-version upgrade smoke test passed: $PREVIOUS_VERSION -> $CANDIDATE_VERSION"
