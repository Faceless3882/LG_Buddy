#!/bin/bash

set -euo pipefail
umask 0022

SCRIPT_DIR="$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)"
REPOSITORY_ROOT="$(dirname "$SCRIPT_DIR")"
RUNTIME_BINARY="${1:-$REPOSITORY_ROOT/target/debug/lg-buddy}"
GUI_BINARY="${2:-$REPOSITORY_ROOT/target/debug/lg-buddy-gui}"
WORK_DIR="$(mktemp -d)"
INSTALL_ROOT="$WORK_DIR/root"
HOME_DIR="$WORK_DIR/home"

fail() {
    echo "$1" >&2
    exit 1
}

cleanup() {
    rm -rf "$WORK_DIR"
}
trap cleanup EXIT

[ "$(id -u)" -ne 0 ] || fail "Installed GUI smoke must run as a regular user."
[ -x "$RUNTIME_BINARY" ] || fail "Runtime binary is not executable: $RUNTIME_BINARY"
[ -x "$GUI_BINARY" ] || fail "GUI binary is not executable: $GUI_BINARY"
[ -n "${DISPLAY:-}" ] || fail "DISPLAY is required for the installed GUI smoke test."
[ -n "${DBUS_SESSION_BUS_ADDRESS:-}" ] || fail "A D-Bus session is required for the installed GUI smoke test."

RUNTIME_BINARY="$(realpath "$RUNTIME_BINARY")"
GUI_BINARY="$(realpath "$GUI_BINARY")"
mkdir -p "$INSTALL_ROOT" "$HOME_DIR/Desktop"

export HOME="$HOME_DIR"
export XDG_CONFIG_HOME="$HOME_DIR/.config"
export LG_BUDDY_INSTALL_ROOT="$INSTALL_ROOT"
export LG_BUDDY_SUDO_CMD="none"
export LG_BUDDY_NONINTERACTIVE="1"
export LG_BUDDY_SKIP_SYSTEMD_ACTIONS="1"
export LG_BUDDY_SKIP_PIP_INSTALL="1"
export LG_BUDDY_TV_IP="192.0.2.10"
export LG_BUDDY_TV_MAC="02:00:00:00:00:10"
export LG_BUDDY_INPUT="HDMI_1"
export LG_BUDDY_TV_PLATFORM="bscpylgtv"
export LG_BUDDY_SCREEN_BACKEND="auto"
export LG_BUDDY_SYSTEM_SLEEP_WAKE_POLICY="enabled"

bash "$REPOSITORY_ROOT/install.sh" \
    --runtime-binary "$RUNTIME_BINARY" \
    --gui-binary "$GUI_BINARY"

INSTALLED_RUNTIME="$INSTALL_ROOT/usr/bin/lg-buddy"
INSTALLED_GUI="$INSTALL_ROOT/usr/bin/lg-buddy-gui"
DESKTOP_ENTRY="$INSTALL_ROOT/usr/share/applications/LG_Buddy_Brightness.desktop"
CONFIG_FILE="$XDG_CONFIG_HOME/lg-buddy/config.env"
NATIVE_TOKEN="$XDG_CONFIG_HOME/lg-buddy/tvs/primary/access-token.json"

[ -x "$INSTALLED_RUNTIME" ] || fail "Installed runtime is missing."
[ -x "$INSTALLED_GUI" ] || fail "Installed GUI is missing."
[ "$(stat -c '%a' "$INSTALLED_GUI")" = "755" ] || fail "Installed GUI mode is not 755."
[ "$(stat -c '%u' "$INSTALLED_GUI")" = "$(id -u)" ] || fail "Installed GUI has the wrong owner."
cmp -s "$RUNTIME_BINARY" "$INSTALLED_RUNTIME" || fail "Installed runtime bytes differ from the candidate."
cmp -s "$GUI_BINARY" "$INSTALLED_GUI" || fail "Installed GUI bytes differ from the candidate."
grep -F -x -q 'Exec=/usr/bin/lg-buddy brightness' "$DESKTOP_ENTRY" || fail "Desktop entry does not use the stable brightness launcher."
grep -F -x -q 'Terminal=false' "$DESKTOP_ENTRY" || fail "Desktop entry would open a terminal."

export LG_BUDDY_CONFIG="$CONFIG_FILE"
bash "$SCRIPT_DIR/test-gui-launch.sh" "$INSTALLED_RUNTIME"

mkdir -p "$(dirname "$NATIVE_TOKEN")"
printf '%s\n' '{"access_token":"installed-gui-smoke-token"}' >"$NATIVE_TOKEN"
chmod 600 "$NATIVE_TOKEN"
CONFIG_SNAPSHOT="$WORK_DIR/config.snapshot"
TOKEN_SNAPSHOT="$WORK_DIR/token.snapshot"
cp "$CONFIG_FILE" "$CONFIG_SNAPSHOT"
cp "$NATIVE_TOKEN" "$TOKEN_SNAPSHOT"

unset LG_BUDDY_REMOVE_CONFIG
bash "$REPOSITORY_ROOT/uninstall.sh"

[ ! -e "$INSTALLED_RUNTIME" ] || fail "Uninstall left the runtime installed."
[ ! -e "$INSTALLED_GUI" ] || fail "Uninstall left the GUI installed."
[ ! -e "$DESKTOP_ENTRY" ] || fail "Uninstall left the desktop entry installed."
cmp -s "$CONFIG_SNAPSHOT" "$CONFIG_FILE" || fail "Uninstall changed the user configuration."
cmp -s "$TOKEN_SNAPSHOT" "$NATIVE_TOKEN" || fail "Uninstall changed the native credential."
