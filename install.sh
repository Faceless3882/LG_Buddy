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
RUNTIME_BINARY_OVERRIDDEN=0
UPGRADE_MODE=0
MUTATION_STARTED=0
UPGRADE_COMPLETED=0

usage() {
    cat <<EOF
Usage: $0 [--upgrade] [--runtime-binary /path/to/lg-buddy]

Install LG Buddy from an existing runtime binary.

Options:
  --upgrade         Upgrade an existing compatible release-bundle installation

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
            RUNTIME_BINARY_OVERRIDDEN=1
            shift 2
            ;;
        --upgrade)
            [ "$UPGRADE_MODE" -eq 0 ] || usage
            UPGRADE_MODE=1
            shift
            ;;
        -h|--help)
            usage
            ;;
        *)
            usage
            ;;
    esac
done

if [ "$UPGRADE_MODE" -eq 1 ] && [ "$RUNTIME_BINARY_OVERRIDDEN" -eq 1 ]; then
    echo "Error: --upgrade uses the verified lg-buddy binary from this release bundle."
    exit 1
fi

if [ -n "$INSTALL_ROOT" ]; then
    case "$INSTALL_ROOT" in
        /*) ;;
        *)
            echo "Error: LG_BUDDY_INSTALL_ROOT must be an absolute path."
            exit 1
            ;;
    esac
fi

if [ "$(id -u)" -eq 0 ]; then
    echo "Error: Do not run this script with sudo. It will prompt for sudo when needed."
    exit 1
fi

if [ "$UPGRADE_MODE" -eq 1 ]; then
    echo "Starting LG Buddy Upgrade"
else
    echo "Starting LG Buddy Installation"
fi
if [ -n "$INSTALL_ROOT" ]; then
    echo "Install root override: $INSTALL_ROOT"
fi

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

    if python3 -m venv "$tmp_venv_dir" >/dev/null 2>&1 &&
        "$tmp_venv_dir/bin/pip" --version >/dev/null 2>&1; then
        rm -rf "$tmp_venv_dir"
        return 0
    fi

    rm -rf "$tmp_venv_dir"
    return 1
}

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

install_missing_prerequisites() {
    if [ ${#MISSING_PKGS[@]} -eq 0 ]; then
        echo "All prerequisites satisfied."
        return
    fi

    echo ""
    echo "Missing: ${MISSING_PKGS[*]}"

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
}

check_fresh_install_prerequisites() {
    echo ""
    echo "Checking prerequisites..."
    MISSING_PKGS=()
    check_dep "python3-venv" "python3-venv" "check_python3_venv"
    check_dep "zenity" "zenity" "command -v zenity"
    install_missing_prerequisites
}

require_python_repair_prerequisites() {
    echo "Checking Python compatibility-platform repair prerequisites..."
    MISSING_PKGS=()
    check_dep "python3-venv" "python3-venv" "check_python3_venv"
    if [ ${#MISSING_PKGS[@]} -gt 0 ]; then
        echo "Upgrade requires Python environment repair, but these prerequisites are missing: ${MISSING_PKGS[*]}"
        echo "Install them manually and rerun the upgrade. No installation files were changed."
        exit 1
    fi
}

python_environment_healthy() {
    local python_version=""
    local site_packages=""

    python_version="$(python3 -c 'import sys; print(f"{sys.version_info.major}.{sys.version_info.minor}")')" || return 1
    site_packages="$VENV_DIR/lib/python$python_version/site-packages"

    [ -f "$VENV_DIR/pyvenv.cfg" ] &&
        [ -x "$VENV_DIR/bin/python" ] &&
        [ -x "$VENV_DIR/bin/pip" ] &&
        [ -x "$VENV_DIR/bin/bscpylgtvcommand" ] &&
        { [ -d "$site_packages/bscpylgtv" ] || [ -f "$site_packages/bscpylgtv.py" ]; }
}

load_upgrade_configuration() {
    CONFIG_FILE="$(sed -n '/[^[:space:]]/{p;q;}' "$CONFIG_POINTER_PATH")"
    [ -n "$CONFIG_FILE" ] || {
        echo "Installed config pointer is empty: $CONFIG_POINTER_PATH"
        exit 1
    }

    TV_PLATFORM="$(LG_BUDDY_CONFIG="$CONFIG_FILE" "$RUNTIME_BINARY" settings get tv.platform)"
    SCREEN_IDLE_BLANK="$(LG_BUDDY_CONFIG="$CONFIG_FILE" "$RUNTIME_BINARY" settings get screen.idle_blank)"
    SCREEN_MONITOR_CONFIGURED_BACKEND="$(LG_BUDDY_CONFIG="$CONFIG_FILE" "$RUNTIME_BINARY" settings get screen.backend)"
    SYSTEM_SLEEP_WAKE_POLICY="$(LG_BUDDY_CONFIG="$CONFIG_FILE" "$RUNTIME_BINARY" settings get system.sleep_wake_policy)"
    UPDATE_AUTO_CHECK="$(LG_BUDDY_CONFIG="$CONFIG_FILE" "$RUNTIME_BINARY" settings get updates.auto_check)"
    UPDATE_CHANNEL="$(LG_BUDDY_CONFIG="$CONFIG_FILE" "$RUNTIME_BINARY" settings get updates.channel)"
    CANDIDATE_VERSION_OUTPUT="$("$RUNTIME_BINARY" --version)"

    echo "Using existing configuration file at $CONFIG_FILE"
    echo "Preserving update channel: $UPDATE_CHANNEL"
}

prepare_installation_files() {
    if [ "$UPGRADE_MODE" -eq 0 ]; then
        CONFIG_POINTER_TMP="$(mktemp)"
        write_config_pointer "$CONFIG_POINTER_TMP" "$CONFIG_FILE"
    fi
    SYSTEM_CONFIG_OVERRIDE_TMP="$(mktemp)"
    write_config_override "$SYSTEM_CONFIG_OVERRIDE_TMP" "$CONFIG_FILE"
    NM_HOOK_TMP="$(mktemp)"
    write_nm_pre_down_hook "$NM_HOOK_TMP"
}

cleanup() {
    local status=$?

    if [ -n "$SYSTEM_CONFIG_OVERRIDE_TMP" ]; then
        rm -f "$SYSTEM_CONFIG_OVERRIDE_TMP"
    fi

    if [ -n "$CONFIG_POINTER_TMP" ]; then
        rm -f "$CONFIG_POINTER_TMP"
    fi

    if [ -n "$NM_HOOK_TMP" ]; then
        rm -f "$NM_HOOK_TMP"
    fi

    if [ "$status" -ne 0 ] && [ "$UPGRADE_MODE" -eq 1 ] && [ "$MUTATION_STARTED" -eq 1 ] && [ "$UPGRADE_COMPLETED" -eq 0 ]; then
        echo "LG Buddy upgrade did not complete after installation changes began." >&2
        echo "The installation may be partial; rerun this verified bundle with --upgrade after correcting the reported failure." >&2
    fi

    trap - EXIT
    exit "$status"
}

trap cleanup EXIT

resolve_runtime_binary
REPAIR_PYTHON_ENVIRONMENT=0

if [ "$UPGRADE_MODE" -eq 1 ]; then
    echo ""
    echo "Running candidate upgrade preflight..."
    "$RUNTIME_BINARY" upgrade-preflight "$SCRIPT_DIR"
    load_upgrade_configuration

    if [ "$TV_PLATFORM" = "lg_webos" ]; then
        echo "Native TV platform selected; preserving the existing Python environment unchanged."
    elif python_environment_healthy; then
        echo "Python compatibility environment is healthy; preserving it unchanged."
    else
        REPAIR_PYTHON_ENVIRONMENT=1
        "$RUNTIME_BINARY" upgrade-preflight "$SCRIPT_DIR" --repair-python
        require_python_repair_prerequisites
    fi
else
    check_fresh_install_prerequisites

# CONFIGURE FRESH INSTALLATION
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
fi

prepare_installation_files

# 4. CREATE VIRTUAL ENVIRONMENT
if [ "$UPGRADE_MODE" -eq 0 ] || [ "$REPAIR_PYTHON_ENVIRONMENT" -eq 1 ]; then
    MUTATION_STARTED=1
    echo "Creating Python virtual environment at $VENV_DIR..."
    # Recreate the helper venv so OS Python minor-version upgrades do not leave
    # bscpylgtv installed under an interpreter-specific site-packages directory
    # that the new `/usr/bin/python3` no longer reads.
    run_privileged python3 -m venv --clear "$VENV_DIR"
    echo "Done."

    if [ "$SKIP_PIP_INSTALL" = "1" ]; then
        echo "Skipping bscpylgtv installation because LG_BUDDY_SKIP_PIP_INSTALL=1."
    else
        echo "Installing bscpylgtv into the virtual environment..."
        run_privileged "$VENV_DIR/bin/pip" install bscpylgtv
        echo "Done."
    fi
fi

# 6. INSTALL RUST RUNTIME AND SUPPORT FILES
MUTATION_STARTED=1
echo "Installing Rust runtime and support files..."
run_privileged install -m 755 "$RUNTIME_BINARY" "$RUNTIME_INSTALL_PATH"
if [ "$UPGRADE_MODE" -eq 0 ]; then
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
fi
if [ "$UPGRADE_MODE" -eq 0 ]; then
    run_privileged install -d "$SYSTEM_LIB_DIR"
    run_privileged install -m 644 "$CONFIG_POINTER_TMP" "$CONFIG_POINTER_PATH"
fi
echo "Installing brightness control desktop entry..."
run_privileged install -d "$APPLICATIONS_DIR"
run_privileged install -m 644 "$SCRIPT_DIR/LG_Buddy_Brightness.desktop" "$DESKTOP_ENTRY_PATH"
if [ "$UPGRADE_MODE" -eq 0 ]; then
    cp "$SCRIPT_DIR/LG_Buddy_Brightness.desktop" ~/Desktop/ 2>/dev/null || true
elif [ -f "$HOME/Desktop/LG_Buddy_Brightness.desktop" ]; then
    cp "$SCRIPT_DIR/LG_Buddy_Brightness.desktop" "$HOME/Desktop/LG_Buddy_Brightness.desktop"
fi
echo "Done."

# 7. SETUP SYSTEMD SERVICES
echo "Copying and enabling systemd services..."
run_privileged install -d "$SYSTEMD_SYSTEM_DIR"
run_privileged install -d "$TMPFILES_CONF_DIR"
run_privileged install -m 644 "$SCRIPT_DIR/systemd/LG_Buddy.service" "$SYSTEMD_SERVICE_PATH"
run_privileged install -m 644 "$SCRIPT_DIR/systemd/lg_buddy.conf" "$TMPFILES_CONF_PATH"
run_privileged install -d "$SYSTEMD_SERVICE_OVERRIDE_DIR"
run_privileged install -m 644 "$SYSTEM_CONFIG_OVERRIDE_TMP" "${SYSTEMD_SERVICE_OVERRIDE_DIR}/config.conf"

if [ "$UPGRADE_MODE" -eq 0 ]; then
    cleanup_legacy_sleep_wake_handlers
fi

run_privileged install -m 644 "$SCRIPT_DIR/systemd/LG_Buddy_lifecycle.service" "$SYSTEMD_LIFECYCLE_SERVICE_PATH"
run_privileged install -d "$SYSTEMD_LIFECYCLE_OVERRIDE_DIR"
run_privileged install -m 644 "$SYSTEM_CONFIG_OVERRIDE_TMP" "${SYSTEMD_LIFECYCLE_OVERRIDE_DIR}/config.conf"
run_privileged install -d "$NM_PRE_DOWN_DIR"
run_privileged install -m 755 "$NM_HOOK_TMP" "$NM_LIFECYCLE_HOOK_PATH"

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
install -m 644 "$SCRIPT_DIR/systemd/LG_Buddy_update_check.service" "$USER_UPDATE_CHECK_SERVICE_PATH"
install -m 644 "$SCRIPT_DIR/systemd/LG_Buddy_update_check.timer" "$USER_UPDATE_CHECK_TIMER_PATH"
mkdir -p "$USER_UPDATE_CHECK_OVERRIDE_DIR"
install -m 644 "$SYSTEM_CONFIG_OVERRIDE_TMP" "${USER_UPDATE_CHECK_OVERRIDE_DIR}/config.conf"
echo "Done."

echo "Installing screen monitor user service..."
install -m 644 "$SCRIPT_DIR/systemd/LG_Buddy_screen.service" "$USER_SCREEN_SERVICE_PATH"
mkdir -p "$USER_SCREEN_OVERRIDE_DIR"
install -m 644 "$SYSTEM_CONFIG_OVERRIDE_TMP" "${USER_SCREEN_OVERRIDE_DIR}/config.conf"
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

if [ "$UPGRADE_MODE" -eq 1 ]; then
    INSTALLED_VERSION_OUTPUT="$("$RUNTIME_INSTALL_PATH" --version)"
    if ! cmp -s "$RUNTIME_BINARY" "$RUNTIME_INSTALL_PATH" || [ "$INSTALLED_VERSION_OUTPUT" != "$CANDIDATE_VERSION_OUTPUT" ]; then
        echo "Installed binary identity does not match the verified candidate." >&2
        echo "Rerun this verified bundle with --upgrade to repair the partial installation." >&2
        exit 1
    fi
    UPGRADE_COMPLETED=1
    echo "Upgrade complete!"
    echo "$INSTALLED_VERSION_OUTPUT"
else
    echo "Installation complete!"
    echo "The user-session service has been installed."
    echo "Please restart your computer for all changes to take full effect."
    echo "NOTE: On first use, you may need to accept a prompt on your TV to allow this application to connect."
fi
