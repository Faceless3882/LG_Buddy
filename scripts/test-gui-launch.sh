#!/bin/bash

set -euo pipefail
umask 0022

GUI_BINARY="${1:-./target/debug/lg-buddy-gui}"
APPLICATION_ID="io.github.staphylococcus.LGBuddy"
WINDOW_TITLE="LG TV Brightness"
GUI_PID=""
WINDOW_IDS=()

fail() {
    echo "$1" >&2
    exit 1
}

cleanup() {
    if [ -n "$GUI_PID" ] && kill -0 "$GUI_PID" 2>/dev/null; then
        kill "$GUI_PID"
        wait "$GUI_PID" 2>/dev/null || true
    fi
}
trap cleanup EXIT

[ -x "$GUI_BINARY" ] || fail "GUI binary is not executable: $GUI_BINARY"
[ -n "${DISPLAY:-}" ] || fail "DISPLAY is required for the GUI launch smoke test."
[ -n "${DBUS_SESSION_BUS_ADDRESS:-}" ] || fail "A D-Bus session is required for the GUI launch smoke test."
command -v gapplication >/dev/null || fail "gapplication is required for the GUI launch smoke test."
command -v xdotool >/dev/null || fail "xdotool is required for the GUI launch smoke test."

ADW_DISABLE_PORTAL=1 GDK_BACKEND=x11 GDK_DEBUG=no-portals NO_AT_BRIDGE=1 \
    "$GUI_BINARY" brightness &
GUI_PID=$!

for ((attempt = 0; attempt < 300; attempt++)); do
    if ! kill -0 "$GUI_PID" 2>/dev/null; then
        status=0
        wait "$GUI_PID" || status=$?
        fail "GUI process exited before presenting a window with status $status."
    fi

    mapfile -t WINDOW_IDS < <(
        xdotool search --onlyvisible --name "^${WINDOW_TITLE}$" 2>/dev/null || true
    )
    [ "${#WINDOW_IDS[@]}" -le 1 ] || fail "GUI presented duplicate brightness windows."
    [ "${#WINDOW_IDS[@]}" -eq 0 ] || break
    sleep 0.1
done

[ "${#WINDOW_IDS[@]}" -eq 1 ] || fail "GUI did not present the brightness window."

ADW_DISABLE_PORTAL=1 GDK_BACKEND=x11 GDK_DEBUG=no-portals NO_AT_BRIDGE=1 \
    "$GUI_BINARY" brightness
mapfile -t WINDOW_IDS < <(
    xdotool search --onlyvisible --name "^${WINDOW_TITLE}$" 2>/dev/null || true
)
[ "${#WINDOW_IDS[@]}" -eq 1 ] || fail "Reactivation did not preserve one brightness window."

gapplication action "$APPLICATION_ID" quit
wait "$GUI_PID"
GUI_PID=""
