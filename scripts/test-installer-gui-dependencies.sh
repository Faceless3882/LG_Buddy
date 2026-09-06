#!/bin/bash

set -euo pipefail
umask 0022

usage() {
    echo "Usage: $0 <install.sh> <runtime-binary> <gui-binary> <apt|dnf|pacman>"
    exit 1
}

[ "$#" -eq 4 ] || usage
[ "$(id -u)" -ne 0 ] || {
    echo "Installer dependency smoke must run as a regular user."
    exit 1
}

INSTALL_SCRIPT="$(realpath "$1")"
RUNTIME_BINARY="$(realpath "$2")"
GUI_BINARY="$(realpath "$3")"
PACKAGE_MANAGER="$4"

case "$PACKAGE_MANAGER" in
    apt)
        EXPECTED_ARGS="install -y libgtk-4-1 libadwaita-1-0"
        EXPECTED_MANUAL="sudo apt install libgtk-4-1 libadwaita-1-0"
        ;;
    dnf)
        EXPECTED_ARGS="install -y gtk4 libadwaita"
        EXPECTED_MANUAL="sudo dnf install gtk4 libadwaita"
        ;;
    pacman)
        EXPECTED_ARGS="-S --noconfirm gtk4 libadwaita"
        EXPECTED_MANUAL="sudo pacman -S gtk4 libadwaita"
        ;;
    *) usage ;;
esac

WORK_DIR="$(mktemp -d)"
STUB_DIR="$WORK_DIR/stubs"
DEPENDENCIES_INSTALLED="$WORK_DIR/dependencies-installed"
PACKAGE_LOG="$WORK_DIR/package-manager.log"
SEQUENCE_LOG="$WORK_DIR/sequence.log"
GUI_WRAPPER="$WORK_DIR/lg-buddy-gui"
PROBE="$WORK_DIR/gui-runtime-probe"

cleanup() {
    rm -rf "$WORK_DIR"
}
trap cleanup EXIT

mkdir -p "$STUB_DIR"

cat >"$PROBE" <<'EOF'
#!/bin/sh
[ -e "${LG_BUDDY_DEPENDENCIES_INSTALLED:?}" ]
EOF

cat >"$STUB_DIR/$PACKAGE_MANAGER" <<'EOF'
#!/bin/sh
set -eu
printf '%s\n' "$*" >"${LG_BUDDY_PACKAGE_LOG:?}"
printf '%s\n' package-manager >>"${LG_BUDDY_SEQUENCE_LOG:?}"
if [ "${LG_BUDDY_FAKE_PACKAGE_MANAGER_FAIL:-0}" = "1" ]; then
    exit 65
fi
if [ "${LG_BUDDY_FAKE_INSTALL_SATISFIES:-1}" = "1" ]; then
    : >"${LG_BUDDY_DEPENDENCIES_INSTALLED:?}"
fi
EOF

cat >"$STUB_DIR/systemctl" <<'EOF'
#!/bin/sh
case "$*" in
    is-system-running|"--user is-system-running") printf '%s\n' running ;;
esac
exit 0
EOF

cat >"$STUB_DIR/systemd-tmpfiles" <<'EOF'
#!/bin/sh
exit 0
EOF

cat >"$GUI_WRAPPER" <<'EOF'
#!/bin/sh
set -eu
printf '%s\n' gui-identity >>"${LG_BUDDY_SEQUENCE_LOG:?}"
exec "${LG_BUDDY_REAL_GUI:?}" "$@"
EOF

chmod 755 "$PROBE" "$GUI_WRAPPER" "$STUB_DIR/$PACKAGE_MANAGER" \
    "$STUB_DIR/systemctl" "$STUB_DIR/systemd-tmpfiles"

run_fresh_install() {
    local scenario="$1"
    local output="$2"
    local auto_install="$3"
    local install_satisfies="$4"
    local package_manager_fails="${5:-0}"
    local root="$WORK_DIR/$scenario/root"
    local home="$WORK_DIR/$scenario/home"

    mkdir -p "$root" "$home/Desktop"
    (
        export PATH="$STUB_DIR:$PATH"
        export HOME="$home"
        export XDG_CONFIG_HOME="$home/.config"
        export LG_BUDDY_INSTALL_ROOT="$root"
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
        export LG_BUDDY_GUI_RUNTIME_PROBE="$PROBE"
        export LG_BUDDY_DEPENDENCIES_INSTALLED="$DEPENDENCIES_INSTALLED"
        export LG_BUDDY_PACKAGE_LOG="$PACKAGE_LOG"
        export LG_BUDDY_SEQUENCE_LOG="$SEQUENCE_LOG"
        export LG_BUDDY_REAL_GUI="$GUI_BINARY"
        export LG_BUDDY_FAKE_INSTALL_SATISFIES="$install_satisfies"
        export LG_BUDDY_FAKE_PACKAGE_MANAGER_FAIL="$package_manager_fails"
        if [ "$auto_install" = "1" ]; then
            export LG_BUDDY_AUTO_INSTALL_DEPS="yes"
        else
            unset LG_BUDDY_AUTO_INSTALL_DEPS
        fi
        cd "$(dirname "$INSTALL_SCRIPT")"
        bash "$INSTALL_SCRIPT" \
            --runtime-binary "$RUNTIME_BINARY" \
            --gui-binary "$GUI_WRAPPER" >"$output" 2>&1
    )
}

REFUSAL_OUTPUT="$WORK_DIR/refusal.output"
if run_fresh_install refusal "$REFUSAL_OUTPUT" 0 1; then
    echo "Noninteractive install unexpectedly installed missing GUI dependencies."
    exit 1
fi
grep -F -q '  [MISSING] GTK 4.14 or newer' "$REFUSAL_OUTPUT"
grep -F -q '  [MISSING] libadwaita 1.5 or newer' "$REFUSAL_OUTPUT"
grep -F -q "$EXPECTED_MANUAL" "$REFUSAL_OUTPUT"
[ ! -e "$PACKAGE_LOG" ] || {
    echo "Refused dependency installation invoked $PACKAGE_MANAGER."
    exit 1
}
[ ! -e "$SEQUENCE_LOG" ] || {
    echo "Refused dependency installation executed the GUI candidate."
    exit 1
}
[ -z "$(find "$WORK_DIR/refusal/root" -mindepth 1 -print -quit)" ] || {
    echo "Refused dependency installation mutated the installation root."
    exit 1
}

UNAVAILABLE_OUTPUT="$WORK_DIR/unavailable.output"
rm -f "$PACKAGE_LOG" "$SEQUENCE_LOG" "$DEPENDENCIES_INSTALLED"
if run_fresh_install unavailable "$UNAVAILABLE_OUTPUT" 1 0 1; then
    echo "Install unexpectedly accepted an unavailable GUI runtime package."
    exit 1
fi
[ "$(cat "$PACKAGE_LOG")" = "$EXPECTED_ARGS" ]
grep -F -q 'Failed to install the missing packages.' "$UNAVAILABLE_OUTPUT"
grep -F -q "$EXPECTED_MANUAL" "$UNAVAILABLE_OUTPUT"
[ "$(cat "$SEQUENCE_LOG")" = "package-manager" ]
[ -z "$(find "$WORK_DIR/unavailable/root" -mindepth 1 -print -quit)" ] || {
    echo "Unavailable GUI runtime packages mutated the installation root."
    exit 1
}

UNSATISFIED_OUTPUT="$WORK_DIR/unsatisfied.output"
rm -f "$PACKAGE_LOG" "$SEQUENCE_LOG" "$DEPENDENCIES_INSTALLED"
if run_fresh_install unsatisfied "$UNSATISFIED_OUTPUT" 1 0; then
    echo "Install unexpectedly accepted insufficient GUI runtime versions."
    exit 1
fi
[ "$(cat "$PACKAGE_LOG")" = "$EXPECTED_ARGS" ]
grep -F -q 'The installed packages do not satisfy the GUI runtime requirements.' "$UNSATISFIED_OUTPUT"
grep -F -q "$EXPECTED_MANUAL" "$UNSATISFIED_OUTPUT"
[ "$(cat "$SEQUENCE_LOG")" = "package-manager" ]
[ -z "$(find "$WORK_DIR/unsatisfied/root" -mindepth 1 -print -quit)" ] || {
    echo "Insufficient GUI runtime versions mutated the installation root."
    exit 1
}

SUCCESS_OUTPUT="$WORK_DIR/success.output"
rm -f "$PACKAGE_LOG" "$SEQUENCE_LOG" "$DEPENDENCIES_INSTALLED"
if ! run_fresh_install success "$SUCCESS_OUTPUT" 1 1; then
    cat "$SUCCESS_OUTPUT"
    echo "Fresh install failed after satisfying GUI runtime dependencies."
    exit 1
fi
[ "$(cat "$PACKAGE_LOG")" = "$EXPECTED_ARGS" ]
[ "$(sed -n '1p' "$SEQUENCE_LOG")" = "package-manager" ]
[ "$(sed -n '2p' "$SEQUENCE_LOG")" = "gui-identity" ]
[ "$(grep -F -c package-manager "$SEQUENCE_LOG")" -eq 1 ]
[ -x "$WORK_DIR/success/root/usr/bin/lg-buddy" ]
[ -x "$WORK_DIR/success/root/usr/bin/lg-buddy-gui" ]

echo "$PACKAGE_MANAGER installer dependency smoke passed."
