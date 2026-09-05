#!/usr/bin/env python3

from __future__ import annotations

import argparse
import math
import sys
import time

import pyatspi


WINDOW_TITLE = "LG TV Brightness"
CONTROL_NAME = "OLED Pixel Brightness"
DEFAULT_TIMEOUT_SECONDS = 10
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


def find_ready_contract() -> tuple[list[object], object, object, object, object] | None:
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


def find_read_failure_contract() -> (
    tuple[list[object], object, object, object, object] | None
):
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
    alert = next(
        (
            item
            for item in accessibles
            if role(item) == pyatspi.ROLE_ALERT and name(item)
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
    retry = next(
        (
            item
            for item in accessibles
            if role(item) == pyatspi.ROLE_PUSH_BUTTON
            and normalized_name(item) == "Retry"
        ),
        None,
    )
    has_slider = any(
        role(item) == pyatspi.ROLE_SLIDER and name(item) == CONTROL_NAME
        for item in accessibles
    )
    if any(item is None for item in (heading, alert, cancel, retry)) or has_slider:
        return None
    return accessibles, heading, alert, cancel, retry


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description="Wait for and verify the installed GTK GUI's AT-SPI contract."
    )
    parser.add_argument(
        "--expected-state",
        choices=("ready", "read-failed"),
        default="ready",
        help="presentation state to observe (default: ready)",
    )
    parser.add_argument(
        "--expected-slider-value",
        type=float,
        help="wait until the ready slider exposes this value",
    )
    parser.add_argument(
        "--timeout",
        type=float,
        default=DEFAULT_TIMEOUT_SECONDS,
        help=f"maximum observation time in seconds (default: {DEFAULT_TIMEOUT_SECONDS})",
    )
    args = parser.parse_args()
    if args.timeout <= 0:
        parser.error("--timeout must be greater than zero")
    if args.expected_state != "ready" and args.expected_slider_value is not None:
        parser.error("--expected-slider-value requires --expected-state ready")
    return args


def observed_contract(
    expected_state: str,
    expected_slider_value: float | None,
) -> tuple[list[object], float | None] | None:
    if expected_state == "read-failed":
        failure_contract = find_read_failure_contract()
        if failure_contract is None:
            return None
        accessibles, _heading, _alert, cancel, retry = failure_contract
        try:
            if any(
                not action.getState().contains(pyatspi.STATE_FOCUSABLE)
                for action in (cancel, retry)
            ):
                return None
        except Exception:  # The UI may change between related remote queries.
            return None
        return accessibles, None

    contract = find_ready_contract()
    if contract is None:
        return None

    accessibles, _heading, slider, cancel, apply = contract
    try:
        if not slider.getState().contains(pyatspi.STATE_FOCUSABLE):
            return None
        if any(
            not action.getState().contains(pyatspi.STATE_FOCUSABLE)
            for action in (cancel, apply)
        ):
            return None
        slider_value = float(slider.queryValue().currentValue)
    except Exception:  # The UI may change between related remote queries.
        return None

    if not 0 <= slider_value <= 100:
        return None
    if expected_slider_value is not None and not math.isclose(
        slider_value, expected_slider_value, abs_tol=0.01
    ):
        return None
    return accessibles, slider_value


def main() -> int:
    args = parse_args()
    deadline = time.monotonic() + args.timeout
    contract = None
    while time.monotonic() < deadline:
        contract = observed_contract(args.expected_state, args.expected_slider_value)
        if contract is not None:
            break
        time.sleep(0.1)

    if contract is None:
        expected = f" {args.expected_state} state"
        if args.expected_slider_value is not None:
            expected += f" at slider value {args.expected_slider_value:g}"
        raise SystemExit(
            f"installed GUI did not expose its expected focusable{expected} over AT-SPI"
        )

    accessibles, slider_value = contract

    observed = sorted(
        {(role_name(item), name(item)) for item in accessibles if name(item)}
    )
    print("AT-SPI accessibility contract verified:")
    print(f"  presentation state: {args.expected_state}")
    if slider_value is not None:
        print(f"  slider value: {slider_value:g}")
    for observed_role, accessible_name in observed:
        print(f"  {observed_role}: {accessible_name}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
