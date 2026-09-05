#!/bin/bash

set -euo pipefail

usage() {
    echo "Usage: $0 <lg-buddy> <lg-buddy-gui> [maximum-glibc-version]"
    exit 1
}

RUNTIME_BINARY="${1:-}"
GUI_BINARY="${2:-}"
MAXIMUM_GLIBC="${3:-2.39}"

[ -x "$RUNTIME_BINARY" ] || usage
[ -x "$GUI_BINARY" ] || usage

command -v readelf >/dev/null || {
    echo "readelf is required for release linkage verification." >&2
    exit 1
}
command -v ldd >/dev/null || {
    echo "ldd is required for release linkage verification." >&2
    exit 1
}

if readelf -l "$RUNTIME_BINARY" | grep -q 'Requesting program interpreter'; then
    echo "Headless runtime is dynamically linked." >&2
    exit 1
fi
if readelf -d "$RUNTIME_BINARY" 2>/dev/null | grep -q '(NEEDED)'; then
    echo "Headless runtime declares dynamic library dependencies." >&2
    exit 1
fi

if ldd "$GUI_BINARY" 2>&1 | grep -q 'not found'; then
    echo "GUI has unresolved dynamic library dependencies:" >&2
    ldd "$GUI_BINARY" >&2 || true
    exit 1
fi

HIGHEST_GLIBC="$(
    readelf --version-info "$GUI_BINARY" |
        sed -n 's/.*Name: GLIBC_\([0-9][0-9.]*\).*/\1/p' |
        sort -Vu |
        tail -n1
)"
[ -n "$HIGHEST_GLIBC" ] || {
    echo "GUI does not declare a GLIBC symbol baseline." >&2
    exit 1
}
if [ "$(printf '%s\n%s\n' "$HIGHEST_GLIBC" "$MAXIMUM_GLIBC" | sort -V | tail -n1)" != "$MAXIMUM_GLIBC" ]; then
    echo "GUI requires GLIBC_$HIGHEST_GLIBC, above supported GLIBC_$MAXIMUM_GLIBC." >&2
    exit 1
fi

echo "Release linkage verified: static runtime; GUI GLIBC <= $MAXIMUM_GLIBC."
