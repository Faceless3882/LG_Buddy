#!/bin/bash

# Exit on any error
set -e

SCRIPT_DIR="$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)"
INSTALL_ROOT="${LG_BUDDY_INSTALL_ROOT:-}"
INSTALL_ROOT="${INSTALL_ROOT%/}"
SUDO_CMD="${LG_BUDDY_SUDO_CMD:-sudo}"
NONINTERACTIVE="${LG_BUDDY_NONINTERACTIVE:-0}"
SKIP_SYSTEMD_ACTIONS="${LG_BUDDY_SKIP_SYSTEMD_ACTIONS:-0}"
SKIP_PIP_INSTALL="${LG_BUDDY_SKIP_PIP_INSTALL:-0}"
DEFAULT_RUNTIME_BINARY="$SCRIPT_DIR/lg-buddy"
RUNTIME_BINARY="$DEFAULT_RUNTIME_BINARY"

usage() {
    cat <<EOF
Usage: $0 [--runtime-binary /path/to/lg-buddy]

Install LG Buddy from an existing runtime binary.

Defaults:
  --runtime-binary defaults to ./lg-buddy next to install.sh
EOF
    exit 1
}

while [ "$#" -gt 0 ]; do
    case "$1" in
        --runtime-binary)
            RUNTIME_BINARY="${2:-}"
            [ -n "$RUNTIME_BINARY" ] || usage
            shift 2
            ;;
        -h|--help)
            usage
            ;;
        *)
            usage
            ;;
    esac
done

if [ "$(id -u)" -eq 0 ]; then
    echo "Error: Do not run this script with sudo. It will prompt for sudo when needed."
    exit 1
fi

echo "Starting LG Buddy Installation"
if [ -n "$INSTALL_ROOT" ]; then
    echo "Install root override: $INSTALL_ROOT"
fi

# 1. CHECK PREREQUISITES
echo ""
echo "Checking prerequisites..."

MISSING_PKGS=()
SCREEN_MONITOR_AVAILABLE=0
SCREEN_MONITOR_CONFIGURED_BACKEND="auto"
SCREEN_MONITOR_RUNTIME_BACKEND=""
SCREEN_IDLE_BLANK="enabled"
SYSTEM_CONFIG_OVERRIDE_TMP=""
CONFIG_POINTER_TMP=""
NM_HOOK_TMP=""
INSTALL_CMD=()

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
VENV_DIR="${SYSTEM_BIN_DIR}/LG_Buddy_PIP"
SYSTEM_LIB_DIR="$(prefix_path "/usr/lib/lg-buddy")"
CONFIG_POINTER_PATH="${SYSTEM_LIB_DIR}/config-path"
COMMON_HELPER_PATH="${SYSTEM_LIB_DIR}/common.sh"
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
TMPFILES_CONF_DIR="$(prefix_path "/etc/tmpfiles.d")"
TMPFILES_CONF_PATH="${TMPFILES_CONF_DIR}/lg_buddy.conf"
NM_PRE_DOWN_DIR="$(prefix_path "/etc/NetworkManager/dispatcher.d/pre-down.d")"
NM_SLEEP_HOOK_PATH="${NM_PRE_DOWN_DIR}/LG_Buddy_sleep"
NM_LIFECYCLE_HOOK_PATH="${NM_PRE_DOWN_DIR}/LG_Buddy_lifecycle"
APPLICATIONS_DIR="$(prefix_path "/usr/share/applications")"
DESKTOP_ENTRY_PATH="${APPLICATIONS_DIR}/LG_Buddy_Brightness.desktop"
USER_SYSTEMD_DIR="${HOME}/.config/systemd/user"
USER_SCREEN_SERVICE_PATH="${USER_SYSTEMD_DIR}/LG_Buddy_screen.service"
USER_SCREEN_OVERRIDE_DIR="${USER_SYSTEMD_DIR}/LG_Buddy_screen.service.d"
USER_UPDATE_CHECK_SERVICE_PATH="${USER_SYSTEMD_DIR}/LG_Buddy_update_check.service"
USER_UPDATE_CHECK_TIMER_PATH="${USER_SYSTEMD_DIR}/LG_Buddy_update_check.timer"
USER_UPDATE_CHECK_OVERRIDE_DIR="${USER_SYSTEMD_DIR}/LG_Buddy_update_check.service.d"

check_dep() {
    local label="$1"
    local pkg="$2"
    local check_cmd="$3"
    if eval "$check_cmd" &>/dev/null; then
        echo "  [OK]      $label"
    else
        echo "  [MISSING] $label"
        MISSING_PKGS+=("$pkg")
    fi
}

check_python3_venv() {
    local tmp_venv_dir=""
    tmp_venv_dir="$(mktemp -d)" || return 1

    if python3 -m venv "$tmp_venv_dir" >/dev/null 2>&1; then
        rm -rf "$tmp_venv_dir"
        return 0
    fi

    rm -rf "$tmp_venv_dir"
    return 1
}

check_dep "python3-venv" "python3-venv" "check_python3_venv"
check_dep "python3-pip" "python3-pip" "/usr/bin/python3 -m pip --version"
check_dep "zenity" "zenity" "command -v zenity"

write_config_override() {
    local override_file="$1"
    local config_path="$2"
    local escaped_config_path=""

    escaped_config_path="${config_path//\\/\\\\}"
    escaped_config_path="${escaped_config_path//\"/\\\"}"

    cat >"$override_file" <<EOF
[Service]
Environment="LG_BUDDY_CONFIG=$escaped_config_path"
EOF
}

write_config_pointer() {
    local pointer_file="$1"
    local config_path="$2"

    printf '%s\n' "$config_path" >"$pointer_file"
}

write_nm_pre_down_hook() {
    local hook_file="$1"

    cat >"$hook_file" <<EOF
#!/bin/sh
set -eu

if [ "\${2:-}" != "pre-down" ]; then
    exit 0
fi

exec /usr/bin/lg-buddy nm-pre-down
EOF
}

cleanup_legacy_sleep_wake_handlers() {
    if [ "$SKIP_SYSTEMD_ACTIONS" = "1" ]; then
        echo "Skipping legacy sleep/wake systemctl cleanup because LG_BUDDY_SKIP_SYSTEMD_ACTIONS=1."
    else
        run_privileged systemctl disable LG_Buddy_wake.service 2>/dev/null || true
        run_privileged systemctl disable LG_Buddy_sleep.service 2>/dev/null || true
        run_privileged systemctl stop LG_Buddy_wake.service 2>/dev/null || true
        run_privileged systemctl stop LG_Buddy_sleep.service 2>/dev/null || true
    fi

    run_privileged rm -f "$SYSTEMD_WAKE_SERVICE_PATH"
    run_privileged rm -f "$SYSTEMD_SLEEP_SERVICE_PATH"
    run_privileged rm -f "${SYSTEMD_WAKE_OVERRIDE_DIR}/config.conf"
    run_privileged rm -f "${SYSTEMD_SLEEP_OVERRIDE_DIR}/config.conf"
    run_privileged rmdir "$SYSTEMD_WAKE_OVERRIDE_DIR" 2>/dev/null || true
    run_privileged rmdir "$SYSTEMD_SLEEP_OVERRIDE_DIR" 2>/dev/null || true
    run_privileged rm -f "$NM_SLEEP_HOOK_PATH"
    run_privileged rm -f "$SYSTEM_SLEEP_HOOK_PATH"
}

resolve_runtime_binary() {
    if [ ! -f "$RUNTIME_BINARY" ]; then
        echo "LG Buddy runtime binary not found at: $RUNTIME_BINARY"
        echo "Build lg-buddy separately first, or use an official release bundle."
        exit 1
    fi

    if [ ! -x "$RUNTIME_BINARY" ]; then
        echo "LG Buddy runtime binary is not executable: $RUNTIME_BINARY"
        echo "Run chmod +x on the binary or provide a valid executable path."
        exit 1
    fi

    echo "Using lg-buddy runtime binary: $RUNTIME_BINARY"
}

cleanup() {
    if [ -n "$SYSTEM_CONFIG_OVERRIDE_TMP" ]; then
        rm -f "$SYSTEM_CONFIG_OVERRIDE_TMP"
    fi

    if [ -n "$CONFIG_POINTER_TMP" ]; then
        rm -f "$CONFIG_POINTER_TMP"
    fi

    if [ -n "$NM_HOOK_TMP" ]; then
        rm -f "$NM_HOOK_TMP"
    fi

}

trap cleanup EXIT

if [ ${#MISSING_PKGS[@]} -gt 0 ]; then
    echo ""
    echo "Missing: ${MISSING_PKGS[*]}"

    # Detect package manager
    if command -v apt &>/dev/null; then
        PM="apt"
        INSTALL_CMD=(apt install -y)
    elif command -v dnf &>/dev/null; then
        PM="dnf"
        INSTALL_CMD=(dnf install -y)
    elif command -v pacman &>/dev/null; then
        PM="pacman"
        INSTALL_CMD=(pacman -S --noconfirm)
    else
        PM=""
    fi

    if [ -n "$PM" ]; then
        AUTO_INSTALL="${LG_BUDDY_AUTO_INSTALL_DEPS:-}"
        if [ -z "$AUTO_INSTALL" ] && [ "$NONINTERACTIVE" != "1" ]; then
            read -p "Install missing packages with $PM now? (y/N) " AUTO_INSTALL
        fi
        case "$AUTO_INSTALL" in
            [Yy]*)
                run_privileged "${INSTALL_CMD[@]}" "${MISSING_PKGS[@]}"
                ;;
            *)
                echo "Please install the missing packages manually and re-run install.sh."
                exit 1
                ;;
        esac
    else
        echo "Could not detect a supported package manager (apt/dnf/pacman)."
        echo "Please install the missing packages manually and re-run install.sh."
        exit 1
    fi
else
    echo "All prerequisites satisfied."
fi

# 2. RESOLVE RUST RUNTIME
resolve_runtime_binary

# 3. CONFIGURE SCRIPTS
echo ""
echo "Running configuration script..."
# Make sure configure.sh is executable
if [ ! -x "$SCRIPT_DIR/configure.sh" ]; then
    chmod +x "$SCRIPT_DIR/configure.sh"
fi
LG_BUDDY_RUNTIME_BINARY="$RUNTIME_BINARY" "$SCRIPT_DIR/configure.sh"
CONFIG_FILE="$(bash "$SCRIPT_DIR/bin/LG_Buddy_Common" --user-config-path)"
SCREEN_IDLE_BLANK="$(sed -n 's/^screen_idle_blank=//p' "$CONFIG_FILE" | tail -n1)"
case "$SCREEN_IDLE_BLANK" in
    enabled|disabled) ;;
    *) SCREEN_IDLE_BLANK="enabled" ;;
esac
SCREEN_MONITOR_CONFIGURED_BACKEND="$(sed -n 's/^screen_backend=//p' "$CONFIG_FILE" | tail -n1)"
SCREEN_MONITOR_CONFIGURED_BACKEND="${SCREEN_MONITOR_CONFIGURED_BACKEND:-auto}"
SYSTEM_SLEEP_WAKE_POLICY="$(sed -n 's/^system_sleep_wake_policy=//p' "$CONFIG_FILE" | tail -n1)"
case "$SYSTEM_SLEEP_WAKE_POLICY" in
    enabled|disabled) ;;
    *) SYSTEM_SLEEP_WAKE_POLICY="enabled" ;;
esac
UPDATE_AUTO_CHECK="$(sed -n 's/^updates_auto_check=//p' "$CONFIG_FILE" | tail -n1)"
case "$UPDATE_AUTO_CHECK" in
    enabled|disabled) ;;
    *) UPDATE_AUTO_CHECK="enabled" ;;
esac
echo "Using configuration file at $CONFIG_FILE"
echo "Configuration complete."

echo ""
if [ "$SCREEN_IDLE_BLANK" = "disabled" ]; then
    echo "Screen idle blanking is disabled by config; user-session service will still run for notifications."
else
    echo "Checking screen idle/resume backend for configured mode ($SCREEN_MONITOR_CONFIGURED_BACKEND)..."
    case "$SCREEN_MONITOR_CONFIGURED_BACKEND" in
        gnome)
            SCREEN_MONITOR_AVAILABLE=1
            SCREEN_MONITOR_RUNTIME_BACKEND="$(LG_BUDDY_SCREEN_BACKEND=gnome "$RUNTIME_BINARY" detect-backend 2>/dev/null || true)"
            if [ "$SCREEN_MONITOR_RUNTIME_BACKEND" = "gnome" ]; then
                echo "  [OK]      current session satisfies the GNOME backend contract"
            else
                SCREEN_MONITOR_RUNTIME_BACKEND=""
                echo "  [INFO]    current session did not verify the full GNOME backend contract"
                echo "            GNOME requires GNOME Shell, org.gnome.ScreenSaver, and org.gnome.Mutter.IdleMonitor."
                echo "            The user-session service will retry until a compatible session is available."
            fi
            ;;
        wayland)
            SCREEN_MONITOR_AVAILABLE=1
            SCREEN_MONITOR_RUNTIME_BACKEND="$(LG_BUDDY_SCREEN_BACKEND=wayland "$RUNTIME_BINARY" detect-backend 2>/dev/null || true)"
            if [ "$SCREEN_MONITOR_RUNTIME_BACKEND" = "wayland" ]; then
                echo "  [OK]      current session satisfies the native Wayland backend contract"
            else
                SCREEN_MONITOR_RUNTIME_BACKEND=""
                echo "  [INFO]    current session did not verify the native Wayland backend contract"
                echo "            Wayland requires ext_idle_notifier_v1 version 2 or newer and at least one advertised seat."
                echo "            The user-session service will retry until a compatible session is available."
            fi
            ;;
        swayidle)
            if command -v swayidle &>/dev/null; then
                echo "  [OK]      swayidle (configured backend)"
                SCREEN_MONITOR_AVAILABLE=1
                SCREEN_MONITOR_RUNTIME_BACKEND="swayidle"
            else
                echo "  [MISSING] swayidle (required for the configured backend)"
                echo "            The user-session service will retry until swayidle is available."
            fi
            ;;
        *)
            if command -v swayidle &>/dev/null; then
                echo "  [OK]      swayidle (wlroots/COSMIC backend)"
                SCREEN_MONITOR_AVAILABLE=1
            else
                echo "  [OPTIONAL] swayidle (required for wlroots/COSMIC backend)"
            fi

            SCREEN_MONITOR_RUNTIME_BACKEND="$("$RUNTIME_BINARY" detect-backend 2>/dev/null || true)"
            if [ -n "$SCREEN_MONITOR_RUNTIME_BACKEND" ]; then
                SCREEN_MONITOR_AVAILABLE=1
                echo "  [OK]      current session backend: $SCREEN_MONITOR_RUNTIME_BACKEND"
            else
                echo "  [INFO]    no supported backend detected in the current session"
                echo "            The user-session service will retry until a supported backend is available."
            fi
            ;;
    esac
fi

# 4. CREATE VIRTUAL ENVIRONMENT
echo "Creating Python virtual environment at $VENV_DIR..."
# Recreate the helper venv so OS Python minor-version upgrades do not leave
# bscpylgtv installed under an interpreter-specific site-packages directory
# that the new `/usr/bin/python3` no longer reads.
run_privileged python3 -m venv --clear "$VENV_DIR"
echo "Done."

# 5. INSTALL BSCPYLGTV
if [ "$SKIP_PIP_INSTALL" = "1" ]; then
    echo "Skipping bscpylgtv installation because LG_BUDDY_SKIP_PIP_INSTALL=1."
else
    echo "Installing bscpylgtv into the virtual environment..."
    run_privileged "$VENV_DIR/bin/pip" install bscpylgtv
    echo "Done."
fi

# 6. INSTALL RUST RUNTIME AND SUPPORT FILES
echo "Installing Rust runtime and support files..."
run_privileged install -m 755 "$RUNTIME_BINARY" "$RUNTIME_INSTALL_PATH"
run_privileged rm -f "${SYSTEM_BIN_DIR}/LG_Buddy_Startup"
run_privileged rm -f "${SYSTEM_BIN_DIR}/LG_Buddy_Shutdown"
run_privileged rm -f "${SYSTEM_BIN_DIR}/LG_Buddy_Screen_On"
run_privileged rm -f "${SYSTEM_BIN_DIR}/LG_Buddy_Screen_Off"
run_privileged rm -f "${SYSTEM_BIN_DIR}/LG_Buddy_Screen_Monitor"
run_privileged rm -f "${SYSTEM_BIN_DIR}/LG_Buddy_sleep_pre"
run_privileged rm -f "${SYSTEM_BIN_DIR}/LG_Buddy_Brightness"
run_privileged rm -f "$COMMON_HELPER_PATH"
run_privileged rm -f "$CONFIG_POINTER_PATH"
run_privileged rmdir "$SYSTEM_LIB_DIR" 2>/dev/null || true
run_privileged install -d "$SYSTEM_LIB_DIR"
CONFIG_POINTER_TMP="$(mktemp)"
write_config_pointer "$CONFIG_POINTER_TMP" "$CONFIG_FILE"
run_privileged install -m 644 "$CONFIG_POINTER_TMP" "$CONFIG_POINTER_PATH"
rm -f "$CONFIG_POINTER_TMP"
CONFIG_POINTER_TMP=""
echo "Installing brightness control desktop entry..."
run_privileged mkdir -p "$APPLICATIONS_DIR"
run_privileged cp "$SCRIPT_DIR/LG_Buddy_Brightness.desktop" "$DESKTOP_ENTRY_PATH"
cp "$SCRIPT_DIR/LG_Buddy_Brightness.desktop" ~/Desktop/ 2>/dev/null || true
echo "Done."

# 7. SETUP SYSTEMD SERVICES
echo "Copying and enabling systemd services..."
run_privileged install -d "$SYSTEMD_SYSTEM_DIR"
run_privileged install -d "$TMPFILES_CONF_DIR"
run_privileged cp "$SCRIPT_DIR/systemd/LG_Buddy.service" "$SYSTEMD_SERVICE_PATH"
run_privileged cp "$SCRIPT_DIR/systemd/lg_buddy.conf" "$TMPFILES_CONF_PATH"
run_privileged install -d "$SYSTEMD_SERVICE_OVERRIDE_DIR"
SYSTEM_CONFIG_OVERRIDE_TMP="$(mktemp)"
write_config_override "$SYSTEM_CONFIG_OVERRIDE_TMP" "$CONFIG_FILE"
run_privileged install -m 644 "$SYSTEM_CONFIG_OVERRIDE_TMP" "${SYSTEMD_SERVICE_OVERRIDE_DIR}/config.conf"
rm -f "$SYSTEM_CONFIG_OVERRIDE_TMP"
SYSTEM_CONFIG_OVERRIDE_TMP=""

cleanup_legacy_sleep_wake_handlers

run_privileged cp "$SCRIPT_DIR/systemd/LG_Buddy_lifecycle.service" "$SYSTEMD_LIFECYCLE_SERVICE_PATH"
run_privileged install -d "$SYSTEMD_LIFECYCLE_OVERRIDE_DIR"
SYSTEM_CONFIG_OVERRIDE_TMP="$(mktemp)"
write_config_override "$SYSTEM_CONFIG_OVERRIDE_TMP" "$CONFIG_FILE"
run_privileged install -m 644 "$SYSTEM_CONFIG_OVERRIDE_TMP" "${SYSTEMD_LIFECYCLE_OVERRIDE_DIR}/config.conf"
rm -f "$SYSTEM_CONFIG_OVERRIDE_TMP"
SYSTEM_CONFIG_OVERRIDE_TMP=""
run_privileged install -d "$NM_PRE_DOWN_DIR"
NM_HOOK_TMP="$(mktemp)"
write_nm_pre_down_hook "$NM_HOOK_TMP"
run_privileged install -m 755 "$NM_HOOK_TMP" "$NM_LIFECYCLE_HOOK_PATH"
rm -f "$NM_HOOK_TMP"
NM_HOOK_TMP=""

if [ "$SKIP_SYSTEMD_ACTIONS" = "1" ]; then
    echo "Skipping systemd tmpfiles and enable actions because LG_BUDDY_SKIP_SYSTEMD_ACTIONS=1."
else
    run_privileged systemd-tmpfiles --create "$TMPFILES_CONF_PATH"
    run_privileged systemctl daemon-reload
    run_privileged systemctl enable LG_Buddy.service
    run_privileged systemctl enable LG_Buddy_lifecycle.service
    run_privileged systemctl restart LG_Buddy_lifecycle.service
fi
echo "Done."

# 8. INSTALL USER SERVICES
echo "Installing background update check user timer..."
mkdir -p "$USER_SYSTEMD_DIR"
cp "$SCRIPT_DIR/systemd/LG_Buddy_update_check.service" "$USER_UPDATE_CHECK_SERVICE_PATH"
cp "$SCRIPT_DIR/systemd/LG_Buddy_update_check.timer" "$USER_UPDATE_CHECK_TIMER_PATH"
mkdir -p "$USER_UPDATE_CHECK_OVERRIDE_DIR"
write_config_override "${USER_UPDATE_CHECK_OVERRIDE_DIR}/config.conf" "$CONFIG_FILE"
echo "Done."

echo "Installing screen monitor user service..."
cp "$SCRIPT_DIR/systemd/LG_Buddy_screen.service" "$USER_SCREEN_SERVICE_PATH"
mkdir -p "$USER_SCREEN_OVERRIDE_DIR"
write_config_override "${USER_SCREEN_OVERRIDE_DIR}/config.conf" "$CONFIG_FILE"
if [ "$SKIP_SYSTEMD_ACTIONS" != "1" ]; then
    systemctl --user daemon-reload
fi

if [ "$SKIP_SYSTEMD_ACTIONS" = "1" ]; then
    echo "Skipping user service enable/start because LG_BUDDY_SKIP_SYSTEMD_ACTIONS=1."
else
    systemctl --user enable LG_Buddy_screen.service
    systemctl --user restart LG_Buddy_screen.service
    if [ "$SCREEN_IDLE_BLANK" = "disabled" ]; then
        echo "LG_Buddy_screen.service enabled and started for session notifications; idle blanking is disabled by config."
    elif [ -n "$SCREEN_MONITOR_RUNTIME_BACKEND" ]; then
        echo "LG_Buddy_screen.service enabled and started using the $SCREEN_MONITOR_RUNTIME_BACKEND backend."
    elif [ "$SCREEN_MONITOR_AVAILABLE" -eq 1 ]; then
        echo "LG_Buddy_screen.service enabled and started; it will retry until the configured screen backend is available."
    else
        echo "LG_Buddy_screen.service enabled and started for session notifications."
        echo "It will retry idle blanking until a compatible screen backend is available."
    fi

    if [ "$UPDATE_AUTO_CHECK" = "enabled" ]; then
        systemctl --user enable LG_Buddy_update_check.timer
        if systemctl --user is-active --quiet graphical-session.target; then
            systemctl --user start LG_Buddy_update_check.timer
            echo "LG_Buddy_update_check.timer enabled and started."
        else
            echo "LG_Buddy_update_check.timer enabled; it will start with the graphical session."
        fi
    else
        systemctl --user disable --now LG_Buddy_update_check.timer 2>/dev/null || true
        echo "LG_Buddy_update_check.timer installed but disabled by config."
    fi
fi

if [ "$SYSTEM_SLEEP_WAKE_POLICY" = "enabled" ]; then
    echo "System sleep/wake TV control enabled via LG_Buddy_lifecycle.service and NetworkManager pre-down gate."
else
    echo "System sleep/wake TV control disabled by config. Lifecycle integration is installed and will no-op until re-enabled."
fi

echo "Installation complete!"
echo "The user-session service has been installed."
echo "Please restart your computer for all changes to take full effect."
echo "NOTE: On first use, you may need to accept a prompt on your TV to allow this application to connect."
