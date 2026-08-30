#!/bin/bash

set -euo pipefail

usage() {
    echo "Usage: $0 --archive <path-to-release-tar.gz> [--work-dir <dir>] [--skip-pip-install]"
    exit 1
}

assert_file() {
    local path="$1"

    if [ ! -f "$path" ]; then
        echo "Expected file not found: $path"
        exit 1
    fi
}

assert_executable() {
    local path="$1"

    if [ ! -x "$path" ]; then
        echo "Expected executable not found: $path"
        exit 1
    fi
}

assert_hidden_compatibility_alias() {
    local binary="$1"
    local command_name="$2"
    local output=""
    local status=0
    shift 2

    if output="$("$binary" "$@" extra 2>&1)"; then
        echo "Compatibility alias unexpectedly accepted an extra argument: $command_name"
        exit 1
    else
        status=$?
    fi

    if [ "$status" -ne 2 ]; then
        echo "Compatibility alias returned status $status instead of 2: $command_name"
        exit 1
    fi

    printf '%s\n' "$output" | grep -F -q "unexpected arguments for \`$command_name\`: extra"
}

assert_cli_surface() {
    local binary="$1"
    local help_output=""
    local no_args_output=""
    local removed_channel_output=""
    local removed_channel_status=0

    help_output="$("$binary" --help)"
    no_args_output="$("$binary")"
    if [ "$no_args_output" != "$help_output" ]; then
        echo "No-argument output did not match global help: $binary"
        exit 1
    fi

    printf '%s\n' "$help_output" | grep -q "lg-buddy"
    printf '%s\n' "$help_output" | grep -F -q "volume <0-100>"
    printf '%s\n' "$help_output" | grep -F -q "volume up"
    printf '%s\n' "$help_output" | grep -F -q "volume down"
    printf '%s\n' "$help_output" | grep -F -q "volume mute"
    printf '%s\n' "$help_output" | grep -F -q "volume mute on"
    printf '%s\n' "$help_output" | grep -F -q "volume mute off"
    printf '%s\n' "$help_output" | grep -F -q "power on"
    printf '%s\n' "$help_output" | grep -F -q "power off"
    printf '%s\n' "$help_output" | grep -F -q "screen off"
    printf '%s\n' "$help_output" | grep -F -q "screen on"
    printf '%s\n' "$help_output" | grep -q "settings list"
    printf '%s\n' "$help_output" | grep -q "settings set <KEY> <VALUE>"
    printf '%s\n' "$help_output" | grep -F -q "updates check [--notify]"
    if printf '%s\n' "$help_output" | grep -F -q -- "--channel"; then
        echo "Removed updates --channel option appeared in public help: $binary"
        exit 1
    fi

    if removed_channel_output="$("$binary" updates check --channel stable 2>&1)"; then
        echo "Removed updates --channel option was unexpectedly accepted: $binary"
        exit 1
    else
        removed_channel_status=$?
    fi
    if [ "$removed_channel_status" -ne 2 ]; then
        echo "Removed updates --channel option returned status $removed_channel_status instead of 2: $binary"
        exit 1
    fi
    printf '%s\n' "$removed_channel_output" | grep -F -q 'unexpected arguments for `updates check`: --channel stable'

    for hidden in startup shutdown screen-off screen-on "updates background-check"; do
        if printf '%s\n' "$help_output" | grep -F -q "$hidden"; then
            echo "Hidden entrypoint appeared in public help: $hidden"
            exit 1
        fi
    done

    "$binary" power on --help | grep -F -q "power on"
    "$binary" power off --help | grep -F -q "power off"
    "$binary" volume --help | grep -F -q "volume <0-100>"
    "$binary" volume up --help | grep -F -q "volume up"
    "$binary" volume mute on --help | grep -F -q "volume mute on"
    "$binary" screen off --help | grep -F -q "screen off"
    "$binary" screen on --help | grep -F -q "screen on"

    assert_hidden_compatibility_alias "$binary" startup startup boot
    assert_hidden_compatibility_alias "$binary" shutdown shutdown
    assert_hidden_compatibility_alias "$binary" screen-off screen-off
    assert_hidden_compatibility_alias "$binary" screen-on screen-on
}

assert_lifecycle_topology_installed() {
    assert_file "$SYSTEM_SERVICE"
    assert_file "$LIFECYCLE_SERVICE"
    grep -q 'ExecStart=/usr/bin/lg-buddy lifecycle' "$LIFECYCLE_SERVICE"
    assert_file "$USER_SCREEN_SERVICE"
    assert_file "$USER_UPDATE_CHECK_SERVICE"
    assert_file "$USER_UPDATE_CHECK_TIMER"
    assert_file "$USER_UPDATE_CHECK_OVERRIDE"
    grep -q '^ExecStart=/usr/bin/lg-buddy updates background-check$' "$USER_UPDATE_CHECK_SERVICE"
    grep -q '^OnCalendar=weekly$' "$USER_UPDATE_CHECK_TIMER"
    grep -q '^WantedBy=graphical-session.target$' "$USER_UPDATE_CHECK_TIMER"
    [ ! -e "$LEGACY_SLEEP_SERVICE" ] || {
        echo "Legacy sleep service installed unexpectedly: $LEGACY_SLEEP_SERVICE"
        exit 1
    }
    [ ! -e "$LEGACY_WAKE_SERVICE" ] || {
        echo "Legacy wake service installed unexpectedly: $LEGACY_WAKE_SERVICE"
        exit 1
    }
    [ ! -e "$NM_SLEEP_HOOK" ] || {
        echo "NetworkManager sleep hook installed unexpectedly: $NM_SLEEP_HOOK"
        exit 1
    }
    [ ! -e "$SYSTEM_SLEEP_HOOK" ] || {
        echo "Legacy systemd sleep hook installed unexpectedly: $SYSTEM_SLEEP_HOOK"
        exit 1
    }
    assert_executable "$NM_LIFECYCLE_HOOK"
    grep -q 'lg-buddy nm-pre-down' "$NM_LIFECYCLE_HOOK"
}

validate_archive_paths() {
    local archive="$1"
    local entry=""

    while IFS= read -r entry; do
        case "$entry" in
            /*)
                echo "Archive contains an absolute path: $entry"
                exit 1
                ;;
        esac

        if printf '%s\n' "$entry" | grep -Eq '(^|/)\.\.(/|$)'; then
            echo "Archive contains a parent-directory traversal path: $entry"
            exit 1
        fi
    done < <(tar -tzf "$archive")
}

ARCHIVE=""
WORK_DIR=""
SKIP_PIP_INSTALL=0

while [ "$#" -gt 0 ]; do
    case "$1" in
        --archive)
            ARCHIVE="${2:-}"
            shift 2
            ;;
        --work-dir)
            WORK_DIR="${2:-}"
            shift 2
            ;;
        --skip-pip-install)
            SKIP_PIP_INSTALL=1
            shift
            ;;
        *)
            usage
            ;;
    esac
done

[ -n "$ARCHIVE" ] || usage
[ -f "$ARCHIVE" ] || {
    echo "Archive not found: $ARCHIVE"
    exit 1
}

CLEANUP_WORK_DIR=0
if [ -z "$WORK_DIR" ]; then
    WORK_DIR="$(mktemp -d)"
    CLEANUP_WORK_DIR=1
fi

cleanup() {
    if [ "$CLEANUP_WORK_DIR" -eq 1 ]; then
        rm -rf "$WORK_DIR"
    fi
}

trap cleanup EXIT

EXTRACT_DIR="$WORK_DIR/extracted"
INSTALL_ROOT="$WORK_DIR/root"
HOME_DIR="$WORK_DIR/home"
XDG_CONFIG_HOME="$HOME_DIR/.config"

mkdir -p "$EXTRACT_DIR" "$INSTALL_ROOT" "$HOME_DIR"

validate_archive_paths "$ARCHIVE"
tar -C "$EXTRACT_DIR" -xzf "$ARCHIVE"
BUNDLE_DIR="$(find "$EXTRACT_DIR" -mindepth 1 -maxdepth 1 -type d | head -n1)"

[ -n "$BUNDLE_DIR" ] || {
    echo "Release archive did not contain a top-level bundle directory."
    exit 1
}

assert_executable "$BUNDLE_DIR/install.sh"
assert_executable "$BUNDLE_DIR/configure.sh"
assert_executable "$BUNDLE_DIR/uninstall.sh"
assert_executable "$BUNDLE_DIR/lg-buddy"
assert_executable "$BUNDLE_DIR/bin/LG_Buddy_Common"
assert_file "$BUNDLE_DIR/LG_Buddy_Brightness.desktop"
assert_file "$BUNDLE_DIR/README.md"
assert_file "$BUNDLE_DIR/LICENSE"
assert_file "$BUNDLE_DIR/docs/architecture-overview.md"
assert_file "$BUNDLE_DIR/docs/runtime-event-handler-map.md"
assert_file "$BUNDLE_DIR/docs/user-guide.md"
assert_file "$BUNDLE_DIR/docs/development.md"
assert_file "$BUNDLE_DIR/docs/release-process.md"
assert_file "$BUNDLE_DIR/systemd/LG_Buddy.service"
assert_file "$BUNDLE_DIR/systemd/LG_Buddy_lifecycle.service"
assert_file "$BUNDLE_DIR/systemd/LG_Buddy_screen.service"
assert_file "$BUNDLE_DIR/systemd/LG_Buddy_update_check.service"
assert_file "$BUNDLE_DIR/systemd/LG_Buddy_update_check.timer"

assert_cli_surface "$BUNDLE_DIR/lg-buddy"

VERSION_OUTPUT="$("$BUNDLE_DIR/lg-buddy" --version)"
printf '%s\n' "$VERSION_OUTPUT" | grep -q "^lg-buddy "
printf '%s\n' "$VERSION_OUTPUT" | grep -q "^version: "
printf '%s\n' "$VERSION_OUTPUT" | grep -q "^channel: "
printf '%s\n' "$VERSION_OUTPUT" | grep -q "^commit: "

export HOME="$HOME_DIR"
export XDG_CONFIG_HOME="$XDG_CONFIG_HOME"
export LG_BUDDY_INSTALL_ROOT="$INSTALL_ROOT"
export LG_BUDDY_SUDO_CMD="none"
export LG_BUDDY_NONINTERACTIVE="1"
export LG_BUDDY_SKIP_SYSTEMD_ACTIONS="1"
export LG_BUDDY_TV_IP="192.168.1.10"
export LG_BUDDY_TV_MAC="aa:bb:cc:dd:ee:ff"
export LG_BUDDY_INPUT="HDMI_2"
export LG_BUDDY_SCREEN_BACKEND="auto"
export LG_BUDDY_SYSTEM_SLEEP_WAKE_POLICY="enabled"
export PIP_DISABLE_PIP_VERSION_CHECK="1"
export PIP_NO_PYTHON_VERSION_WARNING="1"

if [ "$SKIP_PIP_INSTALL" -eq 1 ]; then
    export LG_BUDDY_SKIP_PIP_INSTALL="1"
fi

(
    cd "$BUNDLE_DIR"
    ./install.sh
)

CONFIG_FILE="$XDG_CONFIG_HOME/lg-buddy/config.env"
INSTALLED_BINARY="$INSTALL_ROOT/usr/bin/lg-buddy"
INSTALLED_VENV_PIP="$INSTALL_ROOT/usr/bin/LG_Buddy_PIP/bin/pip"
INSTALLED_BSCPYLGTV="$INSTALL_ROOT/usr/bin/LG_Buddy_PIP/bin/bscpylgtvcommand"
STALE_VENV_MARKER="$INSTALL_ROOT/usr/bin/LG_Buddy_PIP/lib/python-old/site-packages/stale-marker"
INSTALLED_POINTER="$INSTALL_ROOT/usr/lib/lg-buddy/config-path"
SYSTEM_SERVICE="$INSTALL_ROOT/etc/systemd/system/LG_Buddy.service"
LIFECYCLE_SERVICE="$INSTALL_ROOT/etc/systemd/system/LG_Buddy_lifecycle.service"
LEGACY_SLEEP_SERVICE="$INSTALL_ROOT/etc/systemd/system/LG_Buddy_sleep.service"
LEGACY_WAKE_SERVICE="$INSTALL_ROOT/etc/systemd/system/LG_Buddy_wake.service"
SYSTEM_SLEEP_HOOK="$INSTALL_ROOT/usr/lib/systemd/system-sleep/LG_Buddy_sleep_hook"
USER_SCREEN_SERVICE="$HOME/.config/systemd/user/LG_Buddy_screen.service"
USER_UPDATE_CHECK_SERVICE="$HOME/.config/systemd/user/LG_Buddy_update_check.service"
USER_UPDATE_CHECK_TIMER="$HOME/.config/systemd/user/LG_Buddy_update_check.timer"
USER_UPDATE_CHECK_OVERRIDE="$HOME/.config/systemd/user/LG_Buddy_update_check.service.d/config.conf"
DESKTOP_ENTRY="$INSTALL_ROOT/usr/share/applications/LG_Buddy_Brightness.desktop"
NM_SLEEP_HOOK="$INSTALL_ROOT/etc/NetworkManager/dispatcher.d/pre-down.d/LG_Buddy_sleep"
NM_LIFECYCLE_HOOK="$INSTALL_ROOT/etc/NetworkManager/dispatcher.d/pre-down.d/LG_Buddy_lifecycle"

# The installed Rust binary does not know about LG_BUDDY_INSTALL_ROOT, so pin
# CLI config operations to the smoke-test sandbox instead of any host install.
export LG_BUDDY_CONFIG="$CONFIG_FILE"

assert_file "$CONFIG_FILE"
assert_executable "$INSTALLED_BINARY"
assert_executable "$INSTALLED_VENV_PIP"
assert_file "$INSTALLED_POINTER"
assert_lifecycle_topology_installed
assert_file "$DESKTOP_ENTRY"
if grep -q 'LG_BUDDY_CONFIG' "$NM_LIFECYCLE_HOOK"; then
    echo "NetworkManager lifecycle hook should rely on installed config pointer, not embed LG_BUDDY_CONFIG."
    exit 1
fi

grep -q '^tvs_primary_ip=192.168.1.10$' "$CONFIG_FILE"
grep -q '^tvs_primary_mac=aa:bb:cc:dd:ee:ff$' "$CONFIG_FILE"
grep -q '^tvs_primary_input=HDMI_2$' "$CONFIG_FILE"
grep -q '^tvs_primary_platform=bscpylgtv$' "$CONFIG_FILE"
grep -q '^screen_idle_blank=enabled$' "$CONFIG_FILE"
grep -q '^screen_backend=auto$' "$CONFIG_FILE"
grep -q '^system_sleep_wake_policy=enabled$' "$CONFIG_FILE"
grep -q "$CONFIG_FILE" "$INSTALLED_POINTER"
grep -F -q 'TV control at boot, shutdown, sleep, and wake' "$BUNDLE_DIR/README.md"
grep -q 'cooperative suspend rail' "$BUNDLE_DIR/docs/architecture-overview.md"
grep -q 'NetworkManager and logind cooperate through one suspend rail' "$BUNDLE_DIR/docs/runtime-event-handler-map.md"

if [ "$SKIP_PIP_INSTALL" -eq 0 ]; then
    assert_executable "$INSTALLED_BSCPYLGTV"
fi

assert_cli_surface "$INSTALLED_BINARY"

INSTALLED_VERSION_OUTPUT="$("$INSTALLED_BINARY" --version)"
printf '%s\n' "$INSTALLED_VERSION_OUTPUT" | grep -q "^lg-buddy "
printf '%s\n' "$INSTALLED_VERSION_OUTPUT" | grep -q "^version: "
printf '%s\n' "$INSTALLED_VERSION_OUTPUT" | grep -q "^channel: "
printf '%s\n' "$INSTALLED_VERSION_OUTPUT" | grep -q "^commit: "

# Existing profiles without the platform key remain on bscpylgtv. Materialize
# that choice through settings, then use a controlled raw-config fixture to
# prove an unpaired native shutdown skips immediately without contacting a TV.
sed -i '/^tvs_primary_platform=/d' "$CONFIG_FILE"
"$INSTALLED_BINARY" settings get tv.platform | grep -q '^bscpylgtv$'
"$INSTALLED_BINARY" settings set tv.platform bscpylgtv
grep -q '^tvs_primary_platform=bscpylgtv$' "$CONFIG_FILE"

sed -i 's/^tvs_primary_platform=bscpylgtv$/tvs_primary_platform=lg_webos/' "$CONFIG_FILE"
"$INSTALLED_BINARY" settings get tv.platform | grep -q '^lg_webos$'
NATIVE_PLATFORM_OUTPUT="$("$INSTALLED_BINARY" shutdown 2>&1)"
printf '%s\n' "$NATIVE_PLATFORM_OUTPUT" | grep -F -q 'No stored native TV credential; skipping unattended TV control.'

"$INSTALLED_BINARY" settings set tv.platform bscpylgtv
grep -q '^tvs_primary_platform=bscpylgtv$' "$CONFIG_FILE"

"$INSTALLED_BINARY" settings set screen.backend gnome
"$INSTALLED_BINARY" settings set screen.idle_timeout 900
"$INSTALLED_BINARY" settings set screen.idle_timeout 90000
grep -q '^screen_idle_timeout=86400$' "$CONFIG_FILE"
"$INSTALLED_BINARY" settings set screen.idle_timeout 900
"$INSTALLED_BINARY" settings set screen.restore_policy aggressive
"$INSTALLED_BINARY" settings set screen.idle_blank disabled
"$INSTALLED_BINARY" settings set tv.ip 192.168.1.12
"$INSTALLED_BINARY" settings set tv.mac 22:33:44:55:66:77
"$INSTALLED_BINARY" settings set tv.input HDMI_4
"$INSTALLED_BINARY" settings get updates.auto_check | grep -q '^enabled$'
"$INSTALLED_BINARY" settings set updates.auto_check disabled
"$INSTALLED_BINARY" settings set updates.channel prerelease
BACKGROUND_UPDATE_OUTPUT="$("$INSTALLED_BINARY" updates background-check)"
printf '%s\n' "$BACKGROUND_UPDATE_OUTPUT" | grep -F -q 'background: skipped (automatic update checks disabled)'
grep -q '^screen_backend=gnome$' "$CONFIG_FILE"
grep -q '^screen_idle_blank=disabled$' "$CONFIG_FILE"
grep -q '^screen_idle_timeout=900$' "$CONFIG_FILE"
grep -q '^screen_restore_policy=aggressive$' "$CONFIG_FILE"
grep -q '^tvs_primary_ip=192.168.1.12$' "$CONFIG_FILE"
grep -q '^tvs_primary_mac=22:33:44:55:66:77$' "$CONFIG_FILE"
grep -q '^tvs_primary_input=HDMI_4$' "$CONFIG_FILE"
grep -q '^tvs_primary_platform=bscpylgtv$' "$CONFIG_FILE"
grep -q '^updates_auto_check=disabled$' "$CONFIG_FILE"
grep -q '^updates_channel=prerelease$' "$CONFIG_FILE"

# Configure should read inline-commented platform values with the same value
# semantics as the Rust config parser, then persist the sanitized choice.
sed -i 's/^tvs_primary_platform=bscpylgtv$/  tvs_primary_platform = bscpylgtv # legacy/' "$CONFIG_FILE"
printf '%s\n' 'tvs_primary_platform =  lg_webos # experimental' >>"$CONFIG_FILE"

(
    unset LG_BUDDY_SCREEN_BACKEND
    unset LG_BUDDY_SCREEN_IDLE_TIMEOUT
    unset LG_BUDDY_SCREEN_RESTORE_POLICY
    unset LG_BUDDY_SYSTEM_SLEEP_WAKE_POLICY
    export LG_BUDDY_TV_IP="192.168.1.11"
    export LG_BUDDY_TV_MAC="11:22:33:44:55:66"
    export LG_BUDDY_INPUT="HDMI_3"
    cd "$BUNDLE_DIR"
    ./configure.sh
)

grep -q '^tvs_primary_ip=192.168.1.11$' "$CONFIG_FILE"
grep -q '^tvs_primary_mac=11:22:33:44:55:66$' "$CONFIG_FILE"
grep -q '^tvs_primary_input=HDMI_3$' "$CONFIG_FILE"
grep -q '^tvs_primary_platform=lg_webos$' "$CONFIG_FILE"
grep -q '^screen_backend=gnome$' "$CONFIG_FILE"
grep -q '^screen_idle_blank=disabled$' "$CONFIG_FILE"
grep -q '^screen_idle_timeout=900$' "$CONFIG_FILE"
grep -q '^screen_restore_policy=aggressive$' "$CONFIG_FILE"
grep -q '^system_sleep_wake_policy=enabled$' "$CONFIG_FILE"
grep -q '^updates_auto_check=disabled$' "$CONFIG_FILE"
grep -q '^updates_channel=prerelease$' "$CONFIG_FILE"

VALID_PLATFORM_CONFIG="$WORK_DIR/config-valid-platform.snapshot"
cp "$CONFIG_FILE" "$VALID_PLATFORM_CONFIG"
for invalid_platform_line in \
    '  tvs_primary_platform = # explicit empty' \
    'tvs_primary_platform=not-a-platform'
do
    cp "$VALID_PLATFORM_CONFIG" "$CONFIG_FILE"
    sed -i "s/^tvs_primary_platform=lg_webos$/${invalid_platform_line}/" "$CONFIG_FILE"
    INVALID_PLATFORM_CONFIG="$WORK_DIR/config-invalid-platform.snapshot"
    INVALID_PLATFORM_OUTPUT="$WORK_DIR/config-invalid-platform.output"
    cp "$CONFIG_FILE" "$INVALID_PLATFORM_CONFIG"
    if (
        cd "$BUNDLE_DIR"
        ./configure.sh >"$INVALID_PLATFORM_OUTPUT" 2>&1
    ); then
        echo "configure.sh unexpectedly accepted invalid TV platform: $invalid_platform_line"
        exit 1
    fi
    cmp -s "$INVALID_PLATFORM_CONFIG" "$CONFIG_FILE" || {
        echo "configure.sh rewrote invalid TV platform config: $invalid_platform_line"
        exit 1
    }
    grep -F -q 'invalid TV platform' "$INVALID_PLATFORM_OUTPUT"
done
cp "$VALID_PLATFORM_CONFIG" "$CONFIG_FILE"
rm -f "$VALID_PLATFORM_CONFIG" "$INVALID_PLATFORM_CONFIG" "$INVALID_PLATFORM_OUTPUT"

# A real in-place install must preserve an opted-in platform and its
# profile-scoped native credential. Do this before the existing uninstall and
# fresh-install coverage below, without removing the user configuration.
NATIVE_ACCESS_TOKEN_FILE="$(dirname "$CONFIG_FILE")/tvs/primary/access-token.json"
NATIVE_PROFILE_DIR="$(dirname "$NATIVE_ACCESS_TOKEN_FILE")"
NATIVE_PROFILES_DIR="$(dirname "$NATIVE_PROFILE_DIR")"
NATIVE_ACCESS_TOKEN_SNAPSHOT="$WORK_DIR/native-access-token.snapshot"
NATIVE_ACCESS_TOKEN_CONTENT='{"access_token":"release-smoke-native-token"}'
mkdir -p "$NATIVE_PROFILE_DIR"
printf '%s\n' "$NATIVE_ACCESS_TOKEN_CONTENT" >"$NATIVE_ACCESS_TOKEN_FILE"
chmod 600 "$NATIVE_ACCESS_TOKEN_FILE"
cp "$NATIVE_ACCESS_TOKEN_FILE" "$NATIVE_ACCESS_TOKEN_SNAPSHOT"
"$INSTALLED_BINARY" settings get tv.platform | grep -q '^lg_webos$'

(
    unset LG_BUDDY_TV_IP
    unset LG_BUDDY_TV_MAC
    unset LG_BUDDY_INPUT
    unset LG_BUDDY_SCREEN_BACKEND
    unset LG_BUDDY_SCREEN_IDLE_TIMEOUT
    unset LG_BUDDY_SCREEN_RESTORE_POLICY
    unset LG_BUDDY_SYSTEM_SLEEP_WAKE_POLICY
    unset LG_BUDDY_DISABLE_SLEEP_WAKE
    cd "$BUNDLE_DIR"
    ./install.sh
)

assert_file "$CONFIG_FILE"
grep -q '^tvs_primary_platform=lg_webos$' "$CONFIG_FILE"
"$INSTALLED_BINARY" settings get tv.platform | grep -q '^lg_webos$'
assert_file "$NATIVE_ACCESS_TOKEN_FILE"
cmp -s "$NATIVE_ACCESS_TOKEN_SNAPSHOT" "$NATIVE_ACCESS_TOKEN_FILE" || {
    echo "In-place install changed the stored native access token."
    exit 1
}
rm -f "$NATIVE_ACCESS_TOKEN_SNAPSHOT"

export LG_BUDDY_REMOVE_CONFIG="1"
(
    cd "$BUNDLE_DIR"
    ./uninstall.sh
)

[ ! -e "$INSTALLED_BINARY" ] || {
    echo "Installed binary still present after uninstall: $INSTALLED_BINARY"
    exit 1
}
[ ! -e "$INSTALLED_VENV_PIP" ] || {
    echo "Installed Python virtual environment still present after uninstall: $INSTALLED_VENV_PIP"
    exit 1
}
[ ! -e "$INSTALLED_POINTER" ] || {
    echo "Config pointer still present after uninstall: $INSTALLED_POINTER"
    exit 1
}
[ ! -e "$SYSTEM_SERVICE" ] || {
    echo "System service still present after uninstall: $SYSTEM_SERVICE"
    exit 1
}
[ ! -e "$LIFECYCLE_SERVICE" ] || {
    echo "Lifecycle service still present after uninstall: $LIFECYCLE_SERVICE"
    exit 1
}
[ ! -e "$USER_SCREEN_SERVICE" ] || {
    echo "User screen service still present after uninstall: $USER_SCREEN_SERVICE"
    exit 1
}
[ ! -e "$USER_UPDATE_CHECK_SERVICE" ] || {
    echo "User update check service still present after uninstall: $USER_UPDATE_CHECK_SERVICE"
    exit 1
}
[ ! -e "$USER_UPDATE_CHECK_TIMER" ] || {
    echo "User update check timer still present after uninstall: $USER_UPDATE_CHECK_TIMER"
    exit 1
}
[ ! -e "$USER_UPDATE_CHECK_OVERRIDE" ] || {
    echo "User update check override still present after uninstall: $USER_UPDATE_CHECK_OVERRIDE"
    exit 1
}
[ ! -e "$DESKTOP_ENTRY" ] || {
    echo "Desktop entry still present after uninstall: $DESKTOP_ENTRY"
    exit 1
}
[ ! -e "$NM_SLEEP_HOOK" ] || {
    echo "NetworkManager sleep hook still present after uninstall: $NM_SLEEP_HOOK"
    exit 1
}
[ ! -e "$SYSTEM_SLEEP_HOOK" ] || {
    echo "Legacy systemd sleep hook still present after uninstall: $SYSTEM_SLEEP_HOOK"
    exit 1
}
[ ! -e "$NM_LIFECYCLE_HOOK" ] || {
    echo "NetworkManager lifecycle hook still present after uninstall: $NM_LIFECYCLE_HOOK"
    exit 1
}
[ ! -e "$CONFIG_FILE" ] || {
    echo "User config still present after uninstall: $CONFIG_FILE"
    exit 1
}
[ ! -e "$NATIVE_ACCESS_TOKEN_FILE" ] || {
    echo "Native access token still present after uninstall: $NATIVE_ACCESS_TOKEN_FILE"
    exit 1
}
[ ! -e "$NATIVE_PROFILE_DIR" ] || {
    echo "Native TV profile still present after uninstall: $NATIVE_PROFILE_DIR"
    exit 1
}
[ ! -e "$NATIVE_PROFILES_DIR" ] || {
    echo "Native TV profiles directory still present after uninstall: $NATIVE_PROFILES_DIR"
    exit 1
}

export LG_BUDDY_SYSTEM_SLEEP_WAKE_POLICY="disabled"
export LG_BUDDY_SKIP_PIP_INSTALL="1"
mkdir -p "$(dirname "$STALE_VENV_MARKER")"
touch "$STALE_VENV_MARKER"
(
    cd "$BUNDLE_DIR"
    ./install.sh
)

assert_file "$CONFIG_FILE"
assert_executable "$INSTALLED_BINARY"
[ ! -e "$STALE_VENV_MARKER" ] || {
    echo "Installer left stale virtualenv contents in place: $STALE_VENV_MARKER"
    exit 1
}
assert_lifecycle_topology_installed
grep -q '^screen_idle_blank=enabled$' "$CONFIG_FILE"
grep -q '^system_sleep_wake_policy=disabled$' "$CONFIG_FILE"

(
    cd "$BUNDLE_DIR"
    ./uninstall.sh
)

[ ! -e "$INSTALLED_BINARY" ] || {
    echo "Installed binary still present after disabled-policy uninstall: $INSTALLED_BINARY"
    exit 1
}
[ ! -e "$LIFECYCLE_SERVICE" ] || {
    echo "Lifecycle service still present after disabled-policy uninstall: $LIFECYCLE_SERVICE"
    exit 1
}
[ ! -e "$USER_UPDATE_CHECK_SERVICE" ] || {
    echo "User update check service still present after disabled-policy uninstall: $USER_UPDATE_CHECK_SERVICE"
    exit 1
}
[ ! -e "$USER_UPDATE_CHECK_TIMER" ] || {
    echo "User update check timer still present after disabled-policy uninstall: $USER_UPDATE_CHECK_TIMER"
    exit 1
}
[ ! -e "$CONFIG_FILE" ] || {
    echo "User config still present after disabled-policy uninstall: $CONFIG_FILE"
    exit 1
}
[ ! -e "$NM_LIFECYCLE_HOOK" ] || {
    echo "NetworkManager lifecycle hook still present after disabled-policy uninstall: $NM_LIFECYCLE_HOOK"
    exit 1
}

echo "Release bundle smoke test passed for $ARCHIVE"
