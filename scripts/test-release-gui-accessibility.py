#!/usr/bin/env python3

from __future__ import annotations

import sys
import time

import pyatspi


WINDOW_TITLE = "LG TV Brightness"
CONTROL_NAME = "OLED Pixel Brightness"
TIMEOUT_SECONDS = 10
MAX_ACCESSIBLES = 256


def accessible_tree() -> list[object]:
    desktop = pyatspi.Registry.getDesktop(0)
    pending = [desktop]
    observed: list[object] = []
    while pending and len(observed) < MAX_ACCESSIBLES:
        accessible = pending.pop()
        observed.append(accessible)
        try:
            pending.extend(reversed(list(accessible)))
        except Exception:  # Defunct remote accessibles are expected during traversal.
            continue
    return observed


def name(accessible: object) -> str:
    try:
        return str(accessible.name or "")
    except Exception:  # The remote accessible may disappear between queries.
        return ""


def role(accessible: object) -> object | None:
    try:
        return accessible.getRole()
    except Exception:  # The remote accessible may disappear between queries.
        return None


def role_name(accessible: object) -> str:
    try:
        return str(accessible.getRoleName())
    except Exception:  # The remote accessible may disappear between queries.
        return "unknown"


def normalized_name(accessible: object) -> str:
    return name(accessible).replace("_", "")


def find_contract() -> tuple[list[object], object, object, object, object] | None:
    accessibles = accessible_tree()
    if not any(name(item) == WINDOW_TITLE for item in accessibles):
        return None

    heading = next(
        (
            item
            for item in accessibles
            if role(item) == pyatspi.ROLE_HEADING and name(item) == CONTROL_NAME
        ),
        None,
    )
    slider = next(
        (
            item
            for item in accessibles
            if role(item) == pyatspi.ROLE_SLIDER and name(item) == CONTROL_NAME
        ),
        None,
    )
    cancel = next(
        (
            item
            for item in accessibles
            if role(item) == pyatspi.ROLE_PUSH_BUTTON
            and normalized_name(item) == "Cancel"
        ),
        None,
    )
    apply = next(
        (
            item
            for item in accessibles
            if role(item) == pyatspi.ROLE_PUSH_BUTTON
            and normalized_name(item) == "Apply"
        ),
        None,
    )
    if any(item is None for item in (heading, slider, cancel, apply)):
        return None
    return accessibles, heading, slider, cancel, apply


def main() -> int:
    deadline = time.monotonic() + TIMEOUT_SECONDS
    contract = None
    while time.monotonic() < deadline:
        contract = find_contract()
        if contract is not None:
            break
        time.sleep(0.1)

    if contract is None:
        raise SystemExit(
            "installed GUI did not expose its window, heading, slider, and actions over AT-SPI"
        )

    accessibles, _heading, slider, cancel, apply = contract
    if not slider.getState().contains(pyatspi.STATE_FOCUSABLE):
        raise SystemExit("installed GUI slider is not exposed as focusable over AT-SPI")
    for action in (cancel, apply):
        if not action.getState().contains(pyatspi.STATE_FOCUSABLE):
            raise SystemExit(
                f"installed GUI action {normalized_name(action)!r} is not exposed as focusable over AT-SPI"
            )

    slider_value = slider.queryValue().currentValue
    if not 0 <= slider_value <= 100:
        raise SystemExit(f"installed GUI exposed invalid slider value {slider_value}")

    observed = sorted(
        {
            (role_name(item), name(item))
            for item in accessibles
            if name(item)
        }
    )
    print("AT-SPI accessibility contract verified:")
    for observed_role, accessible_name in observed:
        print(f"  {observed_role}: {accessible_name}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
