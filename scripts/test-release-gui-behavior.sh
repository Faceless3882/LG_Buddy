#!/bin/bash

set -euo pipefail

usage() {
    echo "Usage: $0 <installed-lg-buddy> <config-file>"
    exit 1
}

RUNTIME_BINARY="${1:-}"
CONFIG_FILE="${2:-}"
SCRIPT_DIR="$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)"
REPOSITORY_ROOT="$(dirname "$SCRIPT_DIR")"
WORK_DIR="$(mktemp -d)"
STATE_FILE="$WORK_DIR/tv-state.json"
MOCK_COMMAND="$WORK_DIR/bscpylgtvcommand"
WINDOW_TITLE="LG TV Brightness"
GUI_PID=""
WINDOW_ID=""
ACCESSIBILITY_BUS_PID=""
ACCESSIBILITY_REGISTRY_PID=""

fail() {
    echo "$1" >&2
    exit 1
}

cleanup() {
    if [ -n "$GUI_PID" ] && kill -0 "$GUI_PID" 2>/dev/null; then
        kill "$GUI_PID"
        wait "$GUI_PID" 2>/dev/null || true
    fi
    if [ -n "$ACCESSIBILITY_REGISTRY_PID" ] && kill -0 "$ACCESSIBILITY_REGISTRY_PID" 2>/dev/null; then
        kill "$ACCESSIBILITY_REGISTRY_PID"
        wait "$ACCESSIBILITY_REGISTRY_PID" 2>/dev/null || true
    fi
    if [ -n "$ACCESSIBILITY_BUS_PID" ] && kill -0 "$ACCESSIBILITY_BUS_PID" 2>/dev/null; then
        kill "$ACCESSIBILITY_BUS_PID"
        wait "$ACCESSIBILITY_BUS_PID" 2>/dev/null || true
    fi
    rm -rf "$WORK_DIR"
}
trap cleanup EXIT

[ -x "$RUNTIME_BINARY" ] || usage
[ -r "$CONFIG_FILE" ] || usage
[ -n "${DISPLAY:-}" ] || fail "DISPLAY is required for GUI behavior smoke."
[ -n "${DBUS_SESSION_BUS_ADDRESS:-}" ] || fail "A D-Bus session is required for GUI behavior smoke."
command -v xdotool >/dev/null || fail "xdotool is required for GUI behavior smoke."

cp "$CONFIG_FILE" "$WORK_DIR/config.env"
CONFIG_FILE="$WORK_DIR/config.env"
if grep -q '^tvs_primary_platform=' "$CONFIG_FILE"; then
    sed -i 's/^tvs_primary_platform=.*/tvs_primary_platform=bscpylgtv/' "$CONFIG_FILE"
else
    printf '%s\n' 'tvs_primary_platform=bscpylgtv' >>"$CONFIG_FILE"
fi

cat >"$MOCK_COMMAND" <<EOF
#!/bin/sh
exec python3 "$REPOSITORY_ROOT/tools/mock_bscpylgtvcommand.py" --state "$STATE_FILE" "\$@"
EOF
chmod 755 "$MOCK_COMMAND"
export LG_BUDDY_CONFIG="$CONFIG_FILE"
export LG_BUDDY_BSCPYLGTV_COMMAND="$MOCK_COMMAND"

start_gui() {
    local accessibility="${1:-disabled}"
    local color_scheme="${2:-}"
    local scale="${3:-}"
    local -a gui_environment=(
        ADW_DISABLE_PORTAL=1
        GDK_BACKEND=x11
        GDK_DEBUG=no-portals
    )
    [ -z "$color_scheme" ] || gui_environment+=("ADW_DEBUG_COLOR_SCHEME=$color_scheme")
    [ -z "$scale" ] || gui_environment+=("GDK_SCALE=$scale")
    WINDOW_ID=""
    if [ "$accessibility" = "enabled" ]; then
        env -u NO_AT_BRIDGE "${gui_environment[@]}" \
            "$RUNTIME_BINARY" brightness >"$WORK_DIR/gui.output" 2>&1 &
    else
        env "${gui_environment[@]}" NO_AT_BRIDGE=1 \
            "$RUNTIME_BINARY" brightness >"$WORK_DIR/gui.output" 2>&1 &
    fi
    GUI_PID=$!
    for ((attempt = 0; attempt < 300; attempt++)); do
        WINDOW_ID="$(xdotool search --onlyvisible --name "^${WINDOW_TITLE}$" 2>/dev/null | head -n1 || true)"
        [ -z "$WINDOW_ID" ] || return 0
        kill -0 "$GUI_PID" 2>/dev/null || fail "GUI exited before presenting its window."
        sleep 0.1
    done
    fail "GUI did not present its window."
}

wait_for_calls() {
    local command="$1"
    local count="$2"
    for ((attempt = 0; attempt < 300; attempt++)); do
        if python3 - "$STATE_FILE" "$command" "$count" <<'PY'
import json
import sys
from pathlib import Path

path = Path(sys.argv[1])
if not path.exists():
    raise SystemExit(1)
state = json.loads(path.read_text(encoding="utf-8"))
observed = sum(call.get("command") == sys.argv[2] for call in state.get("calls", []))
raise SystemExit(0 if observed >= int(sys.argv[3]) else 1)
PY
        then
            return 0
        fi
        sleep 0.1
    done
    fail "Timed out waiting for $count $command mock calls."
}

finish_gui() {
    wait "$GUI_PID"
    GUI_PID=""
}

start_accessibility_bus() {
    local launcher=""
    local registry=""
    local candidate=""

    for candidate in \
        "$(command -v at-spi-bus-launcher 2>/dev/null || true)" \
        /usr/libexec/at-spi-bus-launcher \
        /usr/lib/at-spi-bus-launcher \
        /usr/lib/at-spi2-core/at-spi-bus-launcher; do
        if [ -n "$candidate" ] && [ -x "$candidate" ]; then
            launcher="$candidate"
            break
        fi
    done
    [ -n "$launcher" ] || fail "at-spi-bus-launcher is required for accessibility verification."

    for candidate in \
        "$(command -v at-spi2-registryd 2>/dev/null || true)" \
        /usr/libexec/at-spi2-registryd \
        /usr/lib/at-spi2-registryd \
        /usr/lib/at-spi2-core/at-spi2-registryd; do
        if [ -n "$candidate" ] && [ -x "$candidate" ]; then
            registry="$candidate"
            break
        fi
    done
    [ -n "$registry" ] || fail "at-spi2-registryd is required for accessibility verification."

    "$launcher" --launch-immediately >"$WORK_DIR/at-spi-bus.output" 2>&1 &
    ACCESSIBILITY_BUS_PID=$!
    sleep 0.2
    "$registry" --use-gnome-session >"$WORK_DIR/at-spi-registry.output" 2>&1 &
    ACCESSIBILITY_REGISTRY_PID=$!
    sleep 0.2
    kill -0 "$ACCESSIBILITY_REGISTRY_PID" 2>/dev/null || \
        fail "AT-SPI accessibility registry exited before the GUI test."
}

# Read current state, reach the slider through the focus chain, edit it through
# the keyboard, and apply it through its mnemonic.
printf '%s\n' '{"backlight":50,"calls":[],"plan":{}}' >"$STATE_FILE"
start_gui
wait_for_calls get_picture_settings 1
sleep 0.2
xdotool windowfocus --sync "$WINDOW_ID"
for _ in 1 2 3; do
    xdotool key --window "$WINDOW_ID" Tab Right
done
xdotool key --window "$WINDOW_ID" alt+a
wait_for_calls set_settings 1
finish_gui
python3 - "$STATE_FILE" <<'PY'
import json
import sys

state = json.load(open(sys.argv[1], encoding="utf-8"))
assert state["backlight"] != 50, state
assert any(call.get("command") == "set_settings" for call in state["calls"]), state
PY

# A failed read stays visible and Retry performs a fresh read.
printf '%s\n' '{"backlight":64,"calls":[],"plan":{"get_picture_settings":[{"result":"error","status":1,"stderr":"planned read failure"},{"result":"success","stdout":"{\u0027backlight\u0027: 64}"}]}}' >"$STATE_FILE"
start_gui
wait_for_calls get_picture_settings 1
sleep 0.2
xdotool windowfocus --sync "$WINDOW_ID"
xdotool key --window "$WINDOW_ID" alt+r
wait_for_calls get_picture_settings 2
xdotool windowfocus --sync "$WINDOW_ID"
xdotool key --window "$WINDOW_ID" alt+c
finish_gui

# Cancelling the loading window never writes a value.
printf '%s\n' '{"backlight":37,"calls":[],"plan":{"get_picture_settings":[{"result":"success","stdout":"{\u0027backlight\u0027: 37}","delay_seconds":2}]}}' >"$STATE_FILE"
start_gui
xdotool windowfocus --sync "$WINDOW_ID"
xdotool key --window "$WINDOW_ID" alt+c
finish_gui
python3 - "$STATE_FILE" <<'PY'
import json
import sys
from pathlib import Path

path = Path(sys.argv[1])
if path.exists():
    state = json.loads(path.read_text(encoding="utf-8"))
    assert not any(call.get("command") == "set_settings" for call in state.get("calls", [])), state
PY

if [ "${LG_BUDDY_TEST_PLATFORM_CONTRACT:-0}" = "1" ]; then
    command -v xwd >/dev/null || fail "xwd is required for theme verification."
    start_accessibility_bus

    capture_platform_state() {
        local label="$1"
        local color_scheme="$2"
        local scale="$3"
        local accessibility="$4"
        local geometry=""
        local screenshot="$WORK_DIR/$label.xwd"

        printf '%s\n' '{"backlight":50,"calls":[],"plan":{}}' >"$STATE_FILE"
        start_gui "$accessibility" "$color_scheme" "$scale"
        wait_for_calls get_picture_settings 1
        sleep 0.3
        geometry="$(xdotool getwindowgeometry --shell "$WINDOW_ID")"
        PLATFORM_WIDTH="$(printf '%s\n' "$geometry" | sed -n 's/^WIDTH=//p')"
        PLATFORM_HEIGHT="$(printf '%s\n' "$geometry" | sed -n 's/^HEIGHT=//p')"
        xwd -silent -id "$WINDOW_ID" -out "$screenshot"
        PLATFORM_MEAN="$(python3 "$SCRIPT_DIR/xwd_mean.py" "$screenshot")"

        if [ "$accessibility" = "enabled" ]; then
            python3 "$SCRIPT_DIR/test-release-gui-accessibility.py"
        fi

        xdotool windowfocus --sync "$WINDOW_ID"
        xdotool key --window "$WINDOW_ID" alt+c
        finish_gui
    }

    capture_platform_state light prefer-light 1 enabled
    LIGHT_WIDTH="$PLATFORM_WIDTH"
    LIGHT_HEIGHT="$PLATFORM_HEIGHT"
    LIGHT_MEAN="$PLATFORM_MEAN"
    capture_platform_state dark prefer-dark 2 disabled
    DARK_WIDTH="$PLATFORM_WIDTH"
    DARK_HEIGHT="$PLATFORM_HEIGHT"
    DARK_MEAN="$PLATFORM_MEAN"

    python3 - "$LIGHT_WIDTH" "$LIGHT_HEIGHT" "$LIGHT_MEAN" "$DARK_WIDTH" "$DARK_HEIGHT" "$DARK_MEAN" <<'PY'
import sys

light_width, light_height = map(int, sys.argv[1:3])
light_mean = float(sys.argv[3])
dark_width, dark_height = map(int, sys.argv[4:6])
dark_mean = float(sys.argv[6])

if dark_width < light_width * 1.5 or dark_height < light_height * 1.5:
    raise SystemExit(
        f"2x scale did not materially enlarge the window: "
        f"{light_width}x{light_height} -> {dark_width}x{dark_height}"
    )
if light_mean < dark_mean + 0.15:
    raise SystemExit(
        f"light and dark themes were not visibly distinct: {light_mean:.3f} vs {dark_mean:.3f}"
    )
PY
fi

echo "Release GUI behavior smoke passed."
