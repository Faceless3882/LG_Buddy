#!/bin/bash

set -euo pipefail
umask 0022

usage() {
    echo "Usage: $0 --archive <path-to-release.tar.gz> [--work-dir <dir>] [--skip-pip-install] [--expected-tag <tag> --expected-version <version> --expected-channel <channel> --expected-target <target> --expected-commit <sha>]"
    exit 1
}

assert_file() {
    local path="$1"

    if [ ! -f "$path" ]; then
        echo "Expected file not found: $path"
        exit 1
    fi
}

assert_mode() {
    local path="$1"
    local expected="$2"
    local actual=""

    actual="$(stat -c '%a' "$path")"
    if [ "$actual" != "$expected" ]; then
        echo "Expected mode $expected for $path, got $actual"
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
    local install_argument_output=""
    local install_argument_status=0

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
    printf '%s\n' "$help_output" | grep -F -q "updates install"
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

    "$binary" updates install --help | grep -F -q "updates install"
    if install_argument_output="$("$binary" updates install 1.5.0 2>&1)"; then
        echo "Updates install unexpectedly accepted a version argument: $binary"
        exit 1
    else
        install_argument_status=$?
    fi
    if [ "$install_argument_status" -ne 2 ]; then
        echo "Updates install argument rejection returned status $install_argument_status instead of 2: $binary"
        exit 1
    fi
    printf '%s\n' "$install_argument_output" | grep -F -q 'unexpected arguments for `updates install`: 1.5.0'

    for hidden in startup shutdown screen-off screen-on "updates background-check" upgrade-preflight; do
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
    assert_hidden_compatibility_alias "$binary" upgrade-preflight upgrade-preflight "$BUNDLE_DIR"
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

SCRIPT_DIR="$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)"
ARCHIVE=""
WORK_DIR=""
SKIP_PIP_INSTALL=0
EXPECTED_TAG=""
EXPECTED_VERSION=""
EXPECTED_CHANNEL=""
EXPECTED_TARGET=""
EXPECTED_COMMIT=""

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
        --expected-tag)
            EXPECTED_TAG="${2:-}"
            shift 2
            ;;
        --expected-version)
            EXPECTED_VERSION="${2:-}"
            shift 2
            ;;
        --expected-channel)
            EXPECTED_CHANNEL="${2:-}"
            shift 2
            ;;
        --expected-target)
            EXPECTED_TARGET="${2:-}"
            shift 2
            ;;
        --expected-commit)
            EXPECTED_COMMIT="${2:-}"
            shift 2
            ;;
        *)
            usage
            ;;
    esac
done

[ -n "$ARCHIVE" ] || usage
[ -z "$EXPECTED_VERSION$EXPECTED_CHANNEL$EXPECTED_COMMIT" ] || {
    [ -n "$EXPECTED_VERSION" ] && \
        [ -n "$EXPECTED_CHANNEL" ] && \
        [ -n "$EXPECTED_COMMIT" ] || usage
}
[ -z "$EXPECTED_TAG$EXPECTED_TARGET" ] || {
    [ -n "$EXPECTED_TAG" ] && \
        [ -n "$EXPECTED_TARGET" ] && \
        [ -n "$EXPECTED_VERSION" ] || usage
}
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

MANIFEST_EXPECTATIONS=()
if [ -n "$EXPECTED_VERSION" ]; then
    MANIFEST_EXPECTATIONS=(
        --expected-version "$EXPECTED_VERSION"
        --expected-channel "$EXPECTED_CHANNEL"
        --expected-commit "$EXPECTED_COMMIT"
    )
fi
if [ -n "$EXPECTED_TAG" ]; then
    MANIFEST_EXPECTATIONS+=(
        --expected-release-tag "$EXPECTED_TAG"
        --expected-target "$EXPECTED_TARGET"
    )
fi

# Validate archive identity without extracting or executing archive content.
python3 "$SCRIPT_DIR/release_bundle_manifest.py" validate \
    --archive "$ARCHIVE" \
    "${MANIFEST_EXPECTATIONS[@]}"
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
assert_file "$BUNDLE_DIR/release-manifest.json"
assert_mode "$BUNDLE_DIR/release-manifest.json" 644
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

# The identity query is the first command executed from the extracted bundle.
python3 "$SCRIPT_DIR/release_bundle_manifest.py" validate \
    --manifest "$BUNDLE_DIR/release-manifest.json" \
    --binary "$BUNDLE_DIR/lg-buddy" \
    "${MANIFEST_EXPECTATIONS[@]}"
assert_cli_surface "$BUNDLE_DIR/lg-buddy"

VERSION_OUTPUT="$("$BUNDLE_DIR/lg-buddy" --version)"
printf '%s\n' "$VERSION_OUTPUT" | grep -q "^lg-buddy "
printf '%s\n' "$VERSION_OUTPUT" | grep -q "^version: "
printf '%s\n' "$VERSION_OUTPUT" | grep -q "^channel: "
printf '%s\n' "$VERSION_OUTPUT" | grep -q "^commit: "

FRESH_CONFIG_HOME="$WORK_DIR/fresh-config-home"
FRESH_CONFIG_OUTPUT="$WORK_DIR/fresh-config.output"
mkdir -p "$FRESH_CONFIG_HOME"
(
    unset LG_BUDDY_NONINTERACTIVE LG_BUDDY_SCREEN_BACKEND LG_BUDDY_CONFIG
    export HOME="$FRESH_CONFIG_HOME"
    export XDG_CONFIG_HOME="$FRESH_CONFIG_HOME/.config"
    export LG_BUDDY_RUNTIME_BINARY="$BUNDLE_DIR/lg-buddy"
    export LG_BUDDY_SKIP_SYSTEMD_ACTIONS="1"
    printf '%s\n' \
        '192.0.2.10' 'aa:bb:cc:dd:ee:ff' '2' '1' 'Y' '1' '300' '1' 'Y' \
        | "$BUNDLE_DIR/configure.sh" >"$FRESH_CONFIG_OUTPUT" 2>&1
)
grep -F -q '  3) wayland' "$FRESH_CONFIG_OUTPUT"
if grep -F -q 'swayidle' "$FRESH_CONFIG_OUTPUT"; then
    echo "Fresh interactive configuration presented swayidle."
    exit 1
fi
grep -q '^screen_backend=auto$' "$FRESH_CONFIG_HOME/.config/lg-buddy/config.env"

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
TMPFILES_CONFIG="$INSTALL_ROOT/etc/tmpfiles.d/lg_buddy.conf"
LEGACY_SLEEP_SERVICE="$INSTALL_ROOT/etc/systemd/system/LG_Buddy_sleep.service"
LEGACY_WAKE_SERVICE="$INSTALL_ROOT/etc/systemd/system/LG_Buddy_wake.service"
SYSTEM_SLEEP_HOOK="$INSTALL_ROOT/usr/lib/systemd/system-sleep/LG_Buddy_sleep_hook"
USER_SCREEN_SERVICE="$HOME/.config/systemd/user/LG_Buddy_screen.service"
USER_UPDATE_CHECK_SERVICE="$HOME/.config/systemd/user/LG_Buddy_update_check.service"
USER_UPDATE_CHECK_TIMER="$HOME/.config/systemd/user/LG_Buddy_update_check.timer"
USER_UPDATE_CHECK_OVERRIDE="$HOME/.config/systemd/user/LG_Buddy_update_check.service.d/config.conf"
DESKTOP_ENTRY="$INSTALL_ROOT/usr/share/applications/LG_Buddy_Brightness.desktop"
USER_DESKTOP_ENTRY="$HOME/Desktop/LG_Buddy_Brightness.desktop"
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
python3 "$SCRIPT_DIR/release_bundle_manifest.py" validate \
    --manifest "$BUNDLE_DIR/release-manifest.json" \
    --binary "$INSTALLED_BINARY" \
    "${MANIFEST_EXPECTATIONS[@]}"

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

"$INSTALLED_BINARY" settings set screen.backend swayidle
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
grep -q '^screen_backend=swayidle$' "$CONFIG_FILE"
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
LEGACY_CONFIGURE_OUTPUT="$WORK_DIR/legacy-configure.output"

(
    unset LG_BUDDY_SCREEN_BACKEND
    unset LG_BUDDY_SCREEN_IDLE_TIMEOUT
    unset LG_BUDDY_SCREEN_RESTORE_POLICY
    unset LG_BUDDY_SYSTEM_SLEEP_WAKE_POLICY
    export LG_BUDDY_TV_IP="192.168.1.11"
    export LG_BUDDY_TV_MAC="11:22:33:44:55:66"
    export LG_BUDDY_INPUT="HDMI_3"
    cd "$BUNDLE_DIR"
    ./configure.sh >"$LEGACY_CONFIGURE_OUTPUT" 2>&1
)

grep -F -q 'Warning: swayidle is a deprecated compatibility backend planned for removal in LG Buddy 2.0.0' "$LEGACY_CONFIGURE_OUTPUT"

grep -q '^tvs_primary_ip=192.168.1.11$' "$CONFIG_FILE"
grep -q '^tvs_primary_mac=11:22:33:44:55:66$' "$CONFIG_FILE"
grep -q '^tvs_primary_input=HDMI_3$' "$CONFIG_FILE"
grep -q '^tvs_primary_platform=lg_webos$' "$CONFIG_FILE"
grep -q '^screen_backend=swayidle$' "$CONFIG_FILE"
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

# A real upgrade must refuse before sudo when preflight fails, then preserve an
# opted-in native installation while replacing every owned candidate asset.
NATIVE_ACCESS_TOKEN_FILE="$(dirname "$CONFIG_FILE")/tvs/primary/access-token.json"
NATIVE_PROFILE_DIR="$(dirname "$NATIVE_ACCESS_TOKEN_FILE")"
NATIVE_PROFILES_DIR="$(dirname "$NATIVE_PROFILE_DIR")"
NATIVE_ACCESS_TOKEN_SNAPSHOT="$WORK_DIR/native-access-token.snapshot"
CONFIG_SNAPSHOT="$WORK_DIR/config.snapshot"
CONFIG_POINTER_SNAPSHOT="$WORK_DIR/config-pointer.snapshot"
NATIVE_VENV_MARKER="$INSTALL_ROOT/usr/bin/LG_Buddy_PIP/native-upgrade-marker"
NATIVE_ACCESS_TOKEN_CONTENT='{"access_token":"release-smoke-native-token"}'
mkdir -p "$NATIVE_PROFILE_DIR"
printf '%s\n' "$NATIVE_ACCESS_TOKEN_CONTENT" >"$NATIVE_ACCESS_TOKEN_FILE"
chmod 600 "$NATIVE_ACCESS_TOKEN_FILE"
cp "$NATIVE_ACCESS_TOKEN_FILE" "$NATIVE_ACCESS_TOKEN_SNAPSHOT"
cp "$CONFIG_FILE" "$CONFIG_SNAPSHOT"
cp "$INSTALLED_POINTER" "$CONFIG_POINTER_SNAPSHOT"
touch "$NATIVE_VENV_MARKER"
"$INSTALLED_BINARY" settings get tv.platform | grep -q '^lg_webos$'

printf '#!/bin/sh\nprintf "stale installed runtime\\n"\n' >"$INSTALLED_BINARY"
chmod 755 "$INSTALLED_BINARY"
for stale_target in \
    "$SYSTEM_SERVICE" \
    "$LIFECYCLE_SERVICE" \
    "$TMPFILES_CONFIG" \
    "$DESKTOP_ENTRY" \
    "$USER_SCREEN_SERVICE" \
    "$USER_UPDATE_CHECK_SERVICE" \
    "$USER_UPDATE_CHECK_TIMER" \
    "$NM_LIFECYCLE_HOOK"
do
    printf 'stale installed integration\n' >"$stale_target"
done

INSTALLER_STUB_DIR="$WORK_DIR/installer-stubs"
SUDO_MARKER="$WORK_DIR/sudo-invoked"
SUDO_SPY="$INSTALLER_STUB_DIR/sudo-spy"
REFUSAL_OUTPUT="$WORK_DIR/upgrade-refusal.output"
mkdir -p "$INSTALLER_STUB_DIR"
cat >"$SUDO_SPY" <<'EOF'
#!/bin/sh
: >"${LG_BUDDY_SUDO_MARKER:?}"
exit 97
EOF
chmod 755 "$SUDO_SPY"
mv "$SYSTEM_SERVICE" "$SYSTEM_SERVICE.preflight-refusal"
if (
    export LG_BUDDY_SUDO_CMD="$SUDO_SPY"
    export LG_BUDDY_SUDO_MARKER="$SUDO_MARKER"
    cd "$BUNDLE_DIR"
    ./install.sh --upgrade >"$REFUSAL_OUTPUT" 2>&1
); then
    echo "Upgrade unexpectedly passed with a missing installed service."
    exit 1
fi
mv "$SYSTEM_SERVICE.preflight-refusal" "$SYSTEM_SERVICE"
grep -F -q 'upgrade preflight: refused' "$REFUSAL_OUTPUT"
[ ! -e "$SUDO_MARKER" ] || {
    echo "Upgrade requested sudo after a candidate preflight refusal."
    exit 1
}
grep -F -q 'stale installed integration' "$DESKTOP_ENTRY"
grep -F -q 'stale installed runtime' "$INSTALLED_BINARY"
cmp -s "$CONFIG_SNAPSHOT" "$CONFIG_FILE" || {
    echo "Refused upgrade changed the user configuration."
    exit 1
}

CONFIGURE_SCRIPT_SNAPSHOT="$WORK_DIR/configure.sh.snapshot"
CONFIGURE_MARKER="$WORK_DIR/configure-invoked"
SERVICE_ACTION_LOG="$WORK_DIR/upgrade-service-actions.log"
PARTIAL_SERVICE_ACTION_LOG="$WORK_DIR/partial-upgrade-service-actions.log"
EXPECTED_SERVICE_ACTION_LOG="$WORK_DIR/expected-upgrade-service-actions.log"
UPGRADE_OUTPUT="$WORK_DIR/upgrade.output"
PARTIAL_UPGRADE_OUTPUT="$WORK_DIR/partial-upgrade.output"
cp -p "$BUNDLE_DIR/configure.sh" "$CONFIGURE_SCRIPT_SNAPSHOT"
cat >"$BUNDLE_DIR/configure.sh" <<'EOF'
#!/bin/sh
: >"${LG_BUDDY_CONFIGURE_MARKER:?}"
exit 91
EOF
chmod 755 "$BUNDLE_DIR/configure.sh"
cat >"$INSTALLER_STUB_DIR/systemctl" <<'EOF'
#!/bin/sh
set -eu
printf 'systemctl %s\n' "$*" >>"${LG_BUDDY_SERVICE_ACTION_LOG:?}"
case "$*" in
    is-system-running|"--user is-system-running")
        printf 'running\n'
        ;;
    daemon-reload)
        if [ "${LG_BUDDY_FAIL_SYSTEM_RELOAD:-0}" = "1" ]; then
            exit 73
        fi
        ;;
esac
EOF
cat >"$INSTALLER_STUB_DIR/systemd-tmpfiles" <<'EOF'
#!/bin/sh
set -eu
printf 'tmpfiles %s\n' "$*" >>"${LG_BUDDY_SERVICE_ACTION_LOG:?}"
EOF
chmod 755 "$INSTALLER_STUB_DIR/systemctl" "$INSTALLER_STUB_DIR/systemd-tmpfiles"

PARTIAL_UPGRADE_STATUS=0
if (
    export PATH="$INSTALLER_STUB_DIR:$PATH"
    export LG_BUDDY_CONFIGURE_MARKER="$CONFIGURE_MARKER"
    export LG_BUDDY_SERVICE_ACTION_LOG="$PARTIAL_SERVICE_ACTION_LOG"
    export LG_BUDDY_FAIL_SYSTEM_RELOAD="1"
    export LG_BUDDY_SKIP_SYSTEMD_ACTIONS="0"
    cd "$BUNDLE_DIR"
    ./install.sh --upgrade >"$PARTIAL_UPGRADE_OUTPUT" 2>&1
); then
    echo "Upgrade unexpectedly succeeded after a simulated post-mutation failure."
    exit 1
else
    PARTIAL_UPGRADE_STATUS=$?
fi
[ "$PARTIAL_UPGRADE_STATUS" -eq 73 ] || {
    cat "$PARTIAL_UPGRADE_OUTPUT"
    echo "Partial upgrade returned status $PARTIAL_UPGRADE_STATUS instead of 73."
    exit 1
}
grep -F -q 'upgrade did not complete after installation changes began' "$PARTIAL_UPGRADE_OUTPUT"
grep -F -q 'installation may be partial' "$PARTIAL_UPGRADE_OUTPUT"
grep -F -q 'rerun this verified bundle with --upgrade' "$PARTIAL_UPGRADE_OUTPUT"
[ ! -e "$CONFIGURE_MARKER" ] || {
    echo "Partial upgrade invoked configure.sh."
    exit 1
}
[ -e "$NATIVE_VENV_MARKER" ] || {
    echo "Partial native upgrade recreated the Python virtual environment."
    exit 1
}
cmp -s "$CONFIG_SNAPSHOT" "$CONFIG_FILE"
cmp -s "$CONFIG_POINTER_SNAPSHOT" "$INSTALLED_POINTER"
cmp -s "$NATIVE_ACCESS_TOKEN_SNAPSHOT" "$NATIVE_ACCESS_TOKEN_FILE"

mkdir -p "$(dirname "$USER_DESKTOP_ENTRY")"
printf 'stale user desktop launcher\n' >"$USER_DESKTOP_ENTRY"

UPGRADE_STATUS=0
if (
    export PATH="$INSTALLER_STUB_DIR:$PATH"
    export LG_BUDDY_CONFIGURE_MARKER="$CONFIGURE_MARKER"
    export LG_BUDDY_SERVICE_ACTION_LOG="$SERVICE_ACTION_LOG"
    export LG_BUDDY_SKIP_SYSTEMD_ACTIONS="0"
    cd "$BUNDLE_DIR"
    ./install.sh --upgrade >"$UPGRADE_OUTPUT" 2>&1
); then
    :
else
    UPGRADE_STATUS=$?
fi
cp -p "$CONFIGURE_SCRIPT_SNAPSHOT" "$BUNDLE_DIR/configure.sh"
if [ "$UPGRADE_STATUS" -ne 0 ]; then
    cat "$UPGRADE_OUTPUT"
    echo "Native release-bundle upgrade failed with status $UPGRADE_STATUS."
    exit 1
fi

[ ! -e "$CONFIGURE_MARKER" ] || {
    echo "Upgrade invoked configure.sh."
    exit 1
}
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
    echo "Upgrade service actions did not match the defined order."
    diff -u "$EXPECTED_SERVICE_ACTION_LOG" "$SERVICE_ACTION_LOG" || true
    exit 1
}

cmp -s "$CONFIG_SNAPSHOT" "$CONFIG_FILE" || {
    echo "Upgrade changed the user configuration."
    exit 1
}
"$INSTALLED_BINARY" settings describe screen.backend \
    | grep -F -q 'deprecation: swayidle is a deprecated compatibility backend planned for removal in LG Buddy 2.0.0'
cmp -s "$CONFIG_POINTER_SNAPSHOT" "$INSTALLED_POINTER" || {
    echo "Upgrade changed the installed config pointer."
    exit 1
}
cmp -s "$NATIVE_ACCESS_TOKEN_SNAPSHOT" "$NATIVE_ACCESS_TOKEN_FILE" || {
    echo "Upgrade changed the stored native access token."
    exit 1
}
[ -e "$NATIVE_VENV_MARKER" ] || {
    echo "Native upgrade recreated the Python virtual environment."
    exit 1
}
cmp -s "$BUNDLE_DIR/lg-buddy" "$INSTALLED_BINARY"
cmp -s "$BUNDLE_DIR/systemd/LG_Buddy.service" "$SYSTEM_SERVICE"
cmp -s "$BUNDLE_DIR/systemd/LG_Buddy_lifecycle.service" "$LIFECYCLE_SERVICE"
cmp -s "$BUNDLE_DIR/systemd/lg_buddy.conf" "$TMPFILES_CONFIG"
cmp -s "$BUNDLE_DIR/LG_Buddy_Brightness.desktop" "$DESKTOP_ENTRY"
cmp -s "$BUNDLE_DIR/LG_Buddy_Brightness.desktop" "$USER_DESKTOP_ENTRY"
cmp -s "$BUNDLE_DIR/systemd/LG_Buddy_screen.service" "$USER_SCREEN_SERVICE"
cmp -s "$BUNDLE_DIR/systemd/LG_Buddy_update_check.service" "$USER_UPDATE_CHECK_SERVICE"
cmp -s "$BUNDLE_DIR/systemd/LG_Buddy_update_check.timer" "$USER_UPDATE_CHECK_TIMER"
assert_lifecycle_topology_installed
"$INSTALLED_BINARY" settings get updates.channel | grep -q '^prerelease$'

# Healthy compatibility-platform environments are preserved; an unhealthy one
# takes the separately preflighted repair path.
"$INSTALLED_BINARY" settings set tv.platform bscpylgtv
VENV_PYTHON="$INSTALL_ROOT/usr/bin/LG_Buddy_PIP/bin/python"
VENV_SITE_PACKAGES="$("$VENV_PYTHON" -c 'import site; print(site.getsitepackages()[0])')"
if ! "$VENV_PYTHON" -c 'import bscpylgtv' >/dev/null 2>&1; then
    mkdir -p "$VENV_SITE_PACKAGES/bscpylgtv"
    printf '__version__ = "smoke"\n' >"$VENV_SITE_PACKAGES/bscpylgtv/__init__.py"
fi
if [ ! -x "$INSTALLED_BSCPYLGTV" ]; then
    printf '#!/bin/sh\nexit 0\n' >"$INSTALLED_BSCPYLGTV"
    chmod 755 "$INSTALLED_BSCPYLGTV"
fi
rm -f "$USER_DESKTOP_ENTRY"
HEALTHY_VENV_MARKER="$INSTALL_ROOT/usr/bin/LG_Buddy_PIP/healthy-upgrade-marker"
touch "$HEALTHY_VENV_MARKER"
(
    export LG_BUDDY_SKIP_PIP_INSTALL="1"
    cd "$BUNDLE_DIR"
    ./install.sh --upgrade
)
[ -e "$HEALTHY_VENV_MARKER" ] || {
    echo "Healthy compatibility environment was recreated during upgrade."
    exit 1
}
[ ! -e "$USER_DESKTOP_ENTRY" ] || {
    echo "Upgrade recreated a user-removed Desktop launcher."
    exit 1
}

rm -f "$INSTALLED_BSCPYLGTV"
REPAIR_VENV_MARKER="$INSTALL_ROOT/usr/bin/LG_Buddy_PIP/repair-upgrade-marker"
touch "$REPAIR_VENV_MARKER"
(
    export LG_BUDDY_SKIP_PIP_INSTALL="1"
    cd "$BUNDLE_DIR"
    ./install.sh --upgrade
)
[ ! -e "$REPAIR_VENV_MARKER" ] || {
    echo "Unhealthy compatibility environment was not repaired."
    exit 1
}
assert_executable "$INSTALLED_VENV_PIP"

rm -rf "$INSTALL_ROOT/usr/bin/LG_Buddy_PIP"
(
    export LG_BUDDY_SKIP_PIP_INSTALL="1"
    cd "$BUNDLE_DIR"
    ./install.sh --upgrade
)
assert_executable "$INSTALLED_VENV_PIP"

assert_file "$CONFIG_FILE"
assert_file "$NATIVE_ACCESS_TOKEN_FILE"
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
