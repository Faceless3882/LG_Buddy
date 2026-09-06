#!/usr/bin/env python3

"""Stateful mock for LG Buddy's delegated swayidle timeout/resume process."""

from __future__ import annotations

import argparse
import json
import subprocess
import sys
import time
from pathlib import Path


DEFAULT_STATE = {
    "emissions": [],
    "linger_seconds": 0.0,
}


def parse_global_args(argv: list[str]) -> tuple[Path, list[str]]:
    parser = argparse.ArgumentParser(add_help=False)
    parser.add_argument("--state", required=True)
    parsed, remaining = parser.parse_known_args(argv)
    return Path(parsed.state), remaining


def load_state(path: Path) -> dict[str, object]:
    if not path.exists():
        return DEFAULT_STATE.copy()

    with path.open("r", encoding="utf-8") as handle:
        data = json.load(handle)

    state = DEFAULT_STATE.copy()
    state.update(data)
    state.setdefault("emissions", [])
    return state


def save_state(path: Path, state: dict[str, object]) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    with path.open("w", encoding="utf-8") as handle:
        json.dump(state, handle, sort_keys=True)


def parse_invocation(argv: list[str]) -> tuple[str, str]:
    if argv[:1] == ["-w"]:
        argv = argv[1:]

    if len(argv) != 5 or argv[0] != "timeout" or argv[3] != "resume":
        raise ValueError("expected: [-w] timeout <seconds> <command> resume <command>")

    int(argv[1])
    return argv[2], argv[4]


def emit_command(command: str) -> None:
    subprocess.run(["/bin/sh", "-c", command], check=False)


def emit_planned_events(
    state: dict[str, object], timeout_command: str, resume_command: str
) -> None:
    emissions = state.get("emissions", [])
    if not isinstance(emissions, list):
        raise TypeError("state emissions must be a list")

    for emission in emissions:
        if emission == "timeout":
            emit_command(timeout_command)
        elif emission == "resume":
            emit_command(resume_command)

    state["emissions"] = []


def main(argv: list[str]) -> int:
    state_path, args = parse_global_args(argv)
    state = load_state(state_path)

    try:
        timeout_command, resume_command = parse_invocation(args)
    except (IndexError, ValueError) as err:
        print(str(err), file=sys.stderr)
        save_state(state_path, state)
        return 2

    emit_planned_events(state, timeout_command, resume_command)
    time.sleep(float(state.get("linger_seconds", 0.0)))
    save_state(state_path, state)
    return 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
