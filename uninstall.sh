#!/bin/bash

SCRIPT_DIR="$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)"
INSTALL_ROOT="${LG_BUDDY_INSTALL_ROOT:-}"
INSTALL_ROOT="${INSTALL_ROOT%/}"
SUDO_CMD="${LG_BUDDY_SUDO_CMD:-sudo}"
NONINTERACTIVE="${LG_BUDDY_NONINTERACTIVE:-0}"
SKIP_SYSTEMD_ACTIONS="${LG_BUDDY_SKIP_SYSTEMD_ACTIONS:-0}"
REMOVE_CONFIG_RESPONSE="${LG_BUDDY_REMOVE_CONFIG:-}"

prefix_path() {
    local path="$1"

    if [ -n "$INSTALL_ROOT" ]; then
        printf '%s%s\n' "$INSTALL_ROOT" "$path"
    else
        printf '%s\n' "$path"
    fi
}

run_privileged() {
    if [ "$SUDO_CMD" = "none" ]; then
        "$@"
    else
        "$SUDO_CMD" "$@"
    fi
}

SYSTEM_BIN_DIR="$(prefix_path "/usr/bin")"
RUNTIME_INSTALL_PATH="${SYSTEM_BIN_DIR}/lg-buddy"
GUI_INSTALL_PATH="${SYSTEM_BIN_DIR}/lg-buddy-gui"
VENV_DIR="${SYSTEM_BIN_DIR}/LG_Buddy_PIP"
SYSTEM_LIB_DIR="$(prefix_path "/usr/lib/lg-buddy")"
COMMON_HELPER_PATH="${SYSTEM_LIB_DIR}/common.sh"
CONFIG_POINTER_PATH="${SYSTEM_LIB_DIR}/config-path"
SYSTEM_SLEEP_HOOK_PATH="$(prefix_path "/usr/lib/systemd/system-sleep/LG_Buddy_sleep_hook")"
SYSTEMD_SYSTEM_DIR="$(prefix_path "/etc/systemd/system")"
SYSTEMD_SERVICE_PATH="${SYSTEMD_SYSTEM_DIR}/LG_Buddy.service"
SYSTEMD_LIFECYCLE_SERVICE_PATH="${SYSTEMD_SYSTEM_DIR}/LG_Buddy_lifecycle.service"
SYSTEMD_WAKE_SERVICE_PATH="${SYSTEMD_SYSTEM_DIR}/LG_Buddy_wake.service"
SYSTEMD_SLEEP_SERVICE_PATH="${SYSTEMD_SYSTEM_DIR}/LG_Buddy_sleep.service"
SYSTEMD_SERVICE_OVERRIDE_DIR="${SYSTEMD_SYSTEM_DIR}/LG_Buddy.service.d"
SYSTEMD_LIFECYCLE_OVERRIDE_DIR="${SYSTEMD_SYSTEM_DIR}/LG_Buddy_lifecycle.service.d"
SYSTEMD_WAKE_OVERRIDE_DIR="${SYSTEMD_SYSTEM_DIR}/LG_Buddy_wake.service.d"
SYSTEMD_SLEEP_OVERRIDE_DIR="${SYSTEMD_SYSTEM_DIR}/LG_Buddy_sleep.service.d"
TMPFILES_CONF_PATH="$(prefix_path "/etc/tmpfiles.d/lg_buddy.conf")"
NM_SLEEP_HOOK_PATH="$(prefix_path "/etc/NetworkManager/dispatcher.d/pre-down.d/LG_Buddy_sleep")"
NM_LIFECYCLE_HOOK_PATH="$(prefix_path "/etc/NetworkManager/dispatcher.d/pre-down.d/LG_Buddy_lifecycle")"
APPLICATIONS_DIR="$(prefix_path "/usr/share/applications")"
DESKTOP_ENTRY_PATH="${APPLICATIONS_DIR}/io.github.staphylococcus.LGBuddy.desktop"
LEGACY_DESKTOP_ENTRY_PATH="${APPLICATIONS_DIR}/LG_Buddy_Brightness.desktop"
APP_ICON_PATH="$(prefix_path "/usr/share/icons/hicolor/scalable/apps/io.github.staphylococcus.LGBuddy.svg")"
RUN_STATE_DIR="$(prefix_path "/run/lg_buddy")"
USER_SYSTEMD_DIR="${HOME}/.config/systemd/user"
USER_SCREEN_SERVICE_PATH="${USER_SYSTEMD_DIR}/LG_Buddy_screen.service"
USER_SCREEN_OVERRIDE_DIR="${USER_SYSTEMD_DIR}/LG_Buddy_screen.service.d"
USER_UPDATE_CHECK_SERVICE_PATH="${USER_SYSTEMD_DIR}/LG_Buddy_update_check.service"
USER_UPDATE_CHECK_TIMER_PATH="${USER_SYSTEMD_DIR}/LG_Buddy_update_check.timer"
USER_UPDATE_CHECK_OVERRIDE_DIR="${USER_SYSTEMD_DIR}/LG_Buddy_update_check.service.d"

if [ -r "$SCRIPT_DIR/bin/LG_Buddy_Common" ]; then
    . "$SCRIPT_DIR/bin/LG_Buddy_Common"
elif [ -r "$COMMON_HELPER_PATH" ]; then
    . "$COMMON_HELPER_PATH"
fi

if declare -F lg_buddy_effective_config_path >/dev/null 2>&1; then
    CONFIG_FILE="$(lg_buddy_effective_config_path 2>/dev/null || true)"
fi

CONFIG_FILE="${CONFIG_FILE:-${LG_BUDDY_CONFIG:-${XDG_CONFIG_HOME:-$HOME/.config}/lg-buddy/config.env}}"
CONFIG_DIR="$(dirname "$CONFIG_FILE")"
TV_PROFILES_DIR="${CONFIG_DIR}/tvs"

echo "Disabling & removing services..."
echo "(This might turn off your TV)"
if [ "$NONINTERACTIVE" != "1" ]; then
    sleep 3
fi
if [ "$SKIP_SYSTEMD_ACTIONS" = "1" ]; then
    echo "Skipping systemd disable/stop actions because LG_BUDDY_SKIP_SYSTEMD_ACTIONS=1."
else
    run_privileged systemctl disable LG_Buddy.service 2>/dev/null || true
    run_privileged systemctl disable LG_Buddy_lifecycle.service 2>/dev/null || true
    run_privileged systemctl disable LG_Buddy_wake.service 2>/dev/null || true
    run_privileged systemctl disable LG_Buddy_sleep.service 2>/dev/null || true
    systemctl --user disable LG_Buddy_update_check.timer 2>/dev/null || true
    systemctl --user disable LG_Buddy_screen.service 2>/dev/null || true
    run_privileged systemctl stop LG_Buddy.service 2>/dev/null || true
    run_privileged systemctl stop LG_Buddy_lifecycle.service 2>/dev/null || true
    run_privileged systemctl stop LG_Buddy_wake.service 2>/dev/null || true
    run_privileged systemctl stop LG_Buddy_sleep.service 2>/dev/null || true
    systemctl --user stop LG_Buddy_update_check.timer 2>/dev/null || true
    systemctl --user stop LG_Buddy_screen.service 2>/dev/null || true
fi
run_privileged rm -f "$SYSTEMD_SERVICE_PATH"
run_privileged rm -f "$SYSTEMD_LIFECYCLE_SERVICE_PATH"
run_privileged rm -f "$SYSTEMD_WAKE_SERVICE_PATH"
run_privileged rm -f "$SYSTEMD_SLEEP_SERVICE_PATH"
run_privileged rm -f "${SYSTEMD_SERVICE_OVERRIDE_DIR}/config.conf"
run_privileged rm -f "${SYSTEMD_LIFECYCLE_OVERRIDE_DIR}/config.conf"
run_privileged rm -f "${SYSTEMD_WAKE_OVERRIDE_DIR}/config.conf"
run_privileged rm -f "${SYSTEMD_SLEEP_OVERRIDE_DIR}/config.conf"
run_privileged rmdir "$SYSTEMD_SERVICE_OVERRIDE_DIR" 2>/dev/null || true
run_privileged rmdir "$SYSTEMD_LIFECYCLE_OVERRIDE_DIR" 2>/dev/null || true
run_privileged rmdir "$SYSTEMD_WAKE_OVERRIDE_DIR" 2>/dev/null || true
run_privileged rmdir "$SYSTEMD_SLEEP_OVERRIDE_DIR" 2>/dev/null || true
rm -f "$USER_SCREEN_SERVICE_PATH"
rm -rf "$USER_SCREEN_OVERRIDE_DIR"
rm -f "$USER_UPDATE_CHECK_SERVICE_PATH"
rm -f "$USER_UPDATE_CHECK_TIMER_PATH"
rm -rf "$USER_UPDATE_CHECK_OVERRIDE_DIR"
if [ "$SKIP_SYSTEMD_ACTIONS" != "1" ]; then
    run_privileged systemctl daemon-reload
    systemctl --user daemon-reload
fi
echo "Done."

echo "Removing scripts"
run_privileged rm -f "$RUNTIME_INSTALL_PATH"
run_privileged rm -f "$GUI_INSTALL_PATH"
run_privileged rm -f "${SYSTEM_BIN_DIR}/LG_Buddy_Startup"
run_privileged rm -f "${SYSTEM_BIN_DIR}/LG_Buddy_Shutdown"
run_privileged rm -f "${SYSTEM_BIN_DIR}/LG_Buddy_Screen_On"
run_privileged rm -f "${SYSTEM_BIN_DIR}/LG_Buddy_Screen_Off"
run_privileged rm -f "${SYSTEM_BIN_DIR}/LG_Buddy_Screen_Monitor"
run_privileged rm -f "${SYSTEM_BIN_DIR}/LG_Buddy_sleep_pre"
run_privileged rm -f "${SYSTEM_BIN_DIR}/LG_Buddy_Brightness"
run_privileged rm -f "$NM_SLEEP_HOOK_PATH"
run_privileged rm -f "$NM_LIFECYCLE_HOOK_PATH"
run_privileged rm -f "$SYSTEM_SLEEP_HOOK_PATH"
run_privileged rm -f "$TMPFILES_CONF_PATH"
run_privileged rm -f "$COMMON_HELPER_PATH"
run_privileged rm -f "$CONFIG_POINTER_PATH"
run_privileged rmdir "$SYSTEM_LIB_DIR" 2>/dev/null || true
run_privileged rm -rf "$RUN_STATE_DIR"

echo "Removing desktop entries"
run_privileged rm -f "$DESKTOP_ENTRY_PATH"
run_privileged rm -f "$LEGACY_DESKTOP_ENTRY_PATH"
run_privileged rm -f "$APP_ICON_PATH"
rm -f "$HOME/Desktop/io.github.staphylococcus.LGBuddy.desktop"
rm -f "$HOME/Desktop/LG_Buddy_Brightness.desktop"

echo "Removing python virtual environment"
run_privileged rm -rf "$VENV_DIR"

if [ -f "$CONFIG_FILE" ]; then
    REMOVE_CONFIG="$REMOVE_CONFIG_RESPONSE"
    if [ -z "$REMOVE_CONFIG" ] && [ "$NONINTERACTIVE" != "1" ]; then
        read -p "Remove user configuration at $CONFIG_FILE? [y/N] " REMOVE_CONFIG
    fi
    case "$REMOVE_CONFIG" in
        [Yy]*|1|true|TRUE|True|yes|YES|Yes)
            rm -f "$CONFIG_FILE"
            rm -rf "$TV_PROFILES_DIR"
            rmdir "$CONFIG_DIR" 2>/dev/null || true
            echo "Removed user configuration."
            ;;
        *)
            echo "Keeping user configuration at $CONFIG_FILE"
            ;;
    esac
fi

echo "Done."
