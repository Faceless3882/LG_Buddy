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

assert_lifecycle_topology_installed() {
    assert_file "$SYSTEM_SERVICE"
    assert_file "$LIFECYCLE_SERVICE"
    grep -q 'ExecStart=/usr/bin/lg-buddy lifecycle' "$LIFECYCLE_SERVICE"
    assert_file "$USER_SCREEN_SERVICE"
    assert_file "$USER_UPDATE_CHECK_SERVICE"
    assert_file "$USER_UPDATE_CHECK_TIMER"
    assert_file "$USER_UPDATE_CHECK_OVERRIDE"
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

HELP_OUTPUT="$("$BUNDLE_DIR/lg-buddy" 2>&1 || true)"
printf '%s\n' "$HELP_OUTPUT" | grep -q "lg-buddy"
printf '%s\n' "$HELP_OUTPUT" | grep -q "settings list"
printf '%s\n' "$HELP_OUTPUT" | grep -q "settings set <key> <value>"
printf '%s\n' "$HELP_OUTPUT" | grep -F -q "updates check [--channel stable|prerelease] [--notify]"
printf '%s\n' "$HELP_OUTPUT" | grep -F -q "updates background-check"

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
grep -q 'cooperating suspend sources' "$BUNDLE_DIR/README.md"
grep -q 'cooperative suspend rail' "$BUNDLE_DIR/docs/architecture-overview.md"
grep -q 'NetworkManager and logind cooperate through one suspend rail' "$BUNDLE_DIR/docs/runtime-event-handler-map.md"

if [ "$SKIP_PIP_INSTALL" -eq 0 ]; then
    assert_executable "$INSTALLED_BSCPYLGTV"
fi

INSTALLED_HELP_OUTPUT="$("$INSTALLED_BINARY" 2>&1 || true)"
printf '%s\n' "$INSTALLED_HELP_OUTPUT" | grep -q "lg-buddy"
printf '%s\n' "$INSTALLED_HELP_OUTPUT" | grep -q "settings list"
printf '%s\n' "$INSTALLED_HELP_OUTPUT" | grep -q "settings set <key> <value>"
printf '%s\n' "$INSTALLED_HELP_OUTPUT" | grep -F -q "updates check [--channel stable|prerelease] [--notify]"
printf '%s\n' "$INSTALLED_HELP_OUTPUT" | grep -F -q "updates background-check"

INSTALLED_VERSION_OUTPUT="$("$INSTALLED_BINARY" --version)"
printf '%s\n' "$INSTALLED_VERSION_OUTPUT" | grep -q "^lg-buddy "
printf '%s\n' "$INSTALLED_VERSION_OUTPUT" | grep -q "^version: "
printf '%s\n' "$INSTALLED_VERSION_OUTPUT" | grep -q "^channel: "
printf '%s\n' "$INSTALLED_VERSION_OUTPUT" | grep -q "^commit: "

# Existing profiles without the platform key remain on bscpylgtv. Materialize
# that choice through settings, then use a controlled raw-config fixture to
# prove lg_webos routes to native stored-credential authentication without
# requiring a real TV in the release smoke test.
sed -i '/^tvs_primary_platform=/d' "$CONFIG_FILE"
"$INSTALLED_BINARY" settings get tv.platform | grep -q '^bscpylgtv$'
"$INSTALLED_BINARY" settings set tv.platform bscpylgtv
grep -q '^tvs_primary_platform=bscpylgtv$' "$CONFIG_FILE"

sed -i 's/^tvs_primary_platform=bscpylgtv$/tvs_primary_platform=lg_webos/' "$CONFIG_FILE"
"$INSTALLED_BINARY" settings get tv.platform | grep -q '^lg_webos$'
if NATIVE_PLATFORM_OUTPUT="$("$INSTALLED_BINARY" brightness get 2>&1)"; then
    echo "Native platform smoke unexpectedly succeeded without a stored credential."
    exit 1
fi
printf '%s\n' "$NATIVE_PLATFORM_OUTPUT" | grep -F -q 'no stored platform access token is available'
printf '%s\n' "$NATIVE_PLATFORM_OUTPUT" | grep -F -q 'lg-buddy settings set tv.platform lg_webos'

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
grep -q '^tvs_primary_platform=bscpylgtv$' "$CONFIG_FILE"
grep -q '^screen_backend=gnome$' "$CONFIG_FILE"
grep -q '^screen_idle_blank=disabled$' "$CONFIG_FILE"
grep -q '^screen_idle_timeout=900$' "$CONFIG_FILE"
grep -q '^screen_restore_policy=aggressive$' "$CONFIG_FILE"
grep -q '^system_sleep_wake_policy=enabled$' "$CONFIG_FILE"
grep -q '^updates_auto_check=disabled$' "$CONFIG_FILE"
grep -q '^updates_channel=prerelease$' "$CONFIG_FILE"

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
