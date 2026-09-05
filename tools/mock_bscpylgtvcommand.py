#!/usr/bin/env python3

"""Stateful mock for bscpylgtvcommand.

This mock is intentionally shaped like the installed CLI:
- `get_input` prints a WebOS app id on stdout
- successful mutating commands print a Python-dict-style success payload
- command failures print a PyLGTVCmdError traceback to stderr and exit non-zero

The mock keeps TV state in a JSON file so tests can drive real subprocess flows.
"""

from __future__ import annotations

import argparse
import json
import os
import sys
import time
from pathlib import Path


DEFAULT_STATE = {
    "power_on": True,
    "screen_on": True,
    "input": "HDMI_3",
    "backlight": 50,
    "volume": 20,
    "muted": False,
    "plan": {},
    "calls": [],
}


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description="Mock bscpylgtvcommand")
    parser.add_argument(
        "--state",
        required=True,
        help="Path to the JSON file that stores mock TV state",
    )
    parser.add_argument(
        "-p",
        dest="key_file_path",
        help="Path to the sqlite key file",
    )
    parser.add_argument("tv_ip")
    parser.add_argument("command")
    parser.add_argument("command_args", nargs="*")
    return parser.parse_args()


def load_state(path: Path) -> dict[str, object]:
    if not path.exists():
        return DEFAULT_STATE.copy()

    with path.open("r", encoding="utf-8") as handle:
        data = json.load(handle)

    state = DEFAULT_STATE.copy()
    state.update(data)
    state.setdefault("plan", {})
    state.setdefault("calls", [])
    return state


def save_state(path: Path, state: dict[str, object]) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    with path.open("w", encoding="utf-8") as handle:
        json.dump(state, handle, sort_keys=True)


def record_call(
    state: dict[str, object],
    tv_ip: str,
    command: str,
    command_args: list[str],
    key_file_path: str | None,
) -> None:
    calls = state.setdefault("calls", [])
    if not isinstance(calls, list):
        raise TypeError("state calls must be a list")

    calls.append(
        {
            "tv_ip": tv_ip,
            "command": command,
            "args": command_args,
            "key_file_path": key_file_path,
            "user": os.environ.get("USER"),
        }
    )


def next_planned_step(state: dict[str, object], command: str) -> dict[str, object] | None:
    plan = state.setdefault("plan", {})
    if not isinstance(plan, dict):
        raise TypeError("state plan must be a dict")

    steps = plan.get(command)
    if not isinstance(steps, list) or not steps:
        return None

    step = steps.pop(0)
    if not isinstance(step, dict):
        raise TypeError("plan step must be a dict")
    return step


def input_to_app_id(input_name: str) -> str:
    return f"com.webos.app.{input_name.lower().replace('_', '')}"


def success() -> int:
    print("{'returnValue': True}")
    return 0


def success_with_stdout(stdout: str) -> int:
    if stdout:
        sys.stdout.write(stdout)
    return 0


def command_error(payload: dict[str, object]) -> int:
    traceback = (
        "Traceback (most recent call last):\n"
        '  File "/usr/bin/LG_Buddy_PIP/bin/bscpylgtvcommand", line 8, in <module>\n'
        "    sys.exit(bscpylgtvcommand())\n"
        "             ~~~~~~~~~~~~~~~~^^\n"
        '  File "/usr/bin/LG_Buddy_PIP/lib/python3.13/site-packages/bscpylgtv/utils.py", line 165, in bscpylgtvcommand\n'
        "    asyncio.run(runloop(args))\n"
        "    ~~~~~~~~~~~^^^^^^^^^^^^^^^\n"
        '  File "/usr/lib/python3.13/asyncio/runners.py", line 195, in run\n'
        "    return runner.run(main)\n"
        "           ~~~~~~~~~~^^^^^^\n"
        '  File "/usr/lib/python3.13/asyncio/runners.py", line 118, in run\n'
        "    return self._loop.run_until_complete(task)\n"
        "           ~~~~~~~~~~~~~~~~~~~~~~~~~~~~~^^^^^^\n"
        '  File "/usr/lib/python3.13/asyncio/base_events.py", line 725, in run_until_complete\n'
        "    return future.result()\n"
        "           ~~~~~~~~~~~~~^^\n"
        '  File "/usr/bin/LG_Buddy_PIP/lib/python3.13/site-packages/bscpylgtv/utils.py", line 47, in runloop\n'
        "    print(await getattr(client, cmd_name)(*cmd_args))\n"
        "          ^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^\n"
        '  File "/usr/bin/LG_Buddy_PIP/lib/python3.13/site-packages/bscpylgtv/webos_client.py", line 934, in mock\n'
        "    return await self.request(...)\n"
        "           ^^^^^^^^^^^^^^^^^^^^^^^\n"
        f"bscpylgtv.exceptions.PyLGTVCmdError: {payload}\n"
    )
    sys.stderr.write(traceback)
    return 1


def raw_error(status: int, stdout: str, stderr: str) -> int:
    if stdout:
        sys.stdout.write(stdout)
    if stderr:
        sys.stderr.write(stderr)
    return status


def powered_off_error() -> int:
    return command_error(
        {
            "type": "error",
            "id": 0,
            "error": "500 Application error",
            "payload": {
                "returnValue": False,
                "state": "Off",
                "errorCode": "-1",
                "errorText": "TV is off",
            },
        }
    )


def active_screen_error() -> int:
    return command_error(
        {
            "type": "error",
            "id": 0,
            "error": "500 Application error",
            "payload": {
                "returnValue": False,
                "state": "Active",
                "errorCode": "-102",
                "errorText": "The current sub state must be 'screen off'",
            },
        }
    )


def apply_state_update(state: dict[str, object], update: dict[str, object] | None) -> None:
    if update:
        state.update(update)


def apply_success_defaults(state: dict[str, object], command: str, command_args: list[str]) -> None:
    if command == "set_input" and len(command_args) == 1:
        state["input"] = command_args[0]
        state["screen_on"] = True
    elif command == "set_volume" and len(command_args) == 1:
        try:
            state["volume"] = int(command_args[0])
        except ValueError:
            pass
    elif command == "volume_up":
        state["volume"] = min(100, int(state["volume"]) + 1)
    elif command == "volume_down":
        state["volume"] = max(0, int(state["volume"]) - 1)
    elif command == "set_mute" and len(command_args) == 1:
        if command_args[0].lower() in {"true", "false"}:
            state["muted"] = command_args[0].lower() == "true"


def execute_planned_step(
    state: dict[str, object],
    command: str,
    command_args: list[str],
    step: dict[str, object],
) -> int:
    delay_seconds = step.get("delay_seconds", 0)
    if not isinstance(delay_seconds, (int, float)) or delay_seconds < 0:
        raise TypeError("plan delay_seconds must be a non-negative number")
    time.sleep(delay_seconds)
    result = step.get("result")
    state_update = step.get("state_update")
    if state_update is not None and not isinstance(state_update, dict):
        raise TypeError("plan state_update must be a dict")

    if result == "success":
        apply_success_defaults(state, command, command_args)
        apply_state_update(state, state_update)
        return success_with_stdout(str(step.get("stdout", "")))
    apply_state_update(state, state_update)
    if result == "error":
        return raw_error(
            int(step.get("status", 1)),
            str(step.get("stdout", "")),
            str(step.get("stderr", "")),
        )
    if result == "active_screen_error":
        return active_screen_error()
    if result == "powered_off_error":
        return powered_off_error()

    raise ValueError(f"unsupported planned result: {result!r}")


def main() -> int:
    args = parse_args()
    state_path = Path(args.state)
    state = load_state(state_path)
    command = args.command
    record_call(state, args.tv_ip, command, list(args.command_args), args.key_file_path)

    planned_step = next_planned_step(state, command)
    if planned_step is not None:
        exit_code = execute_planned_step(state, command, list(args.command_args), planned_step)
        save_state(state_path, state)
        return exit_code

    if command == "get_input":
        if not state["power_on"]:
            save_state(state_path, state)
            return powered_off_error()
        print(input_to_app_id(str(state["input"])))
        save_state(state_path, state)
        return 0

    if command == "get_power_state":
        if not state["power_on"]:
            save_state(state_path, state)
            return powered_off_error()
        power_state = "Active" if state["screen_on"] else "Screen Off"
        print({"returnValue": True, "state": power_state})
        save_state(state_path, state)
        return 0

    if command == "get_picture_settings":
        if not state["power_on"]:
            save_state(state_path, state)
            return powered_off_error()
        print(
            {
                "contrast": 85,
                "backlight": int(state["backlight"]),
                "brightness": 50,
                "color": 55,
            }
        )
        save_state(state_path, state)
        return 0

    if command == "get_audio_status":
        if not state["power_on"]:
            save_state(state_path, state)
            return powered_off_error()
        print(
            {
                "returnValue": True,
                "volume": int(state["volume"]),
                "mute": bool(state["muted"]),
            }
        )
        save_state(state_path, state)
        return 0

    if command == "set_input":
        if not state["power_on"]:
            save_state(state_path, state)
            return powered_off_error()
        if len(args.command_args) != 1:
            print("set_input requires one input argument", file=sys.stderr)
            save_state(state_path, state)
            return 2
        state["input"] = args.command_args[0]
        state["screen_on"] = True
        save_state(state_path, state)
        return success()

    if command == "set_settings":
        if not state["power_on"]:
            save_state(state_path, state)
            return powered_off_error()
        if len(args.command_args) != 2:
            print("set_settings requires category and JSON payload", file=sys.stderr)
            save_state(state_path, state)
            return 2
        if args.command_args[0] != "picture":
            print("set_settings mock only supports picture category", file=sys.stderr)
            save_state(state_path, state)
            return 2

        try:
            payload = json.loads(args.command_args[1])
        except json.JSONDecodeError as exc:
            print(f"invalid JSON payload: {exc}", file=sys.stderr)
            save_state(state_path, state)
            return 2

        backlight = payload.get("backlight")
        if not isinstance(backlight, int):
            print("set_settings mock requires integer backlight", file=sys.stderr)
            save_state(state_path, state)
            return 2

        state["backlight"] = backlight
        save_state(state_path, state)
        return success()

    if command == "set_volume":
        if not state["power_on"]:
            save_state(state_path, state)
            return powered_off_error()
        if len(args.command_args) != 1:
            print("set_volume requires one volume argument", file=sys.stderr)
            save_state(state_path, state)
            return 2
        try:
            volume = int(args.command_args[0])
        except ValueError:
            print("set_volume requires an integer volume", file=sys.stderr)
            save_state(state_path, state)
            return 2
        if not 0 <= volume <= 100:
            print("set_volume requires a volume from 0 to 100", file=sys.stderr)
            save_state(state_path, state)
            return 2
        state["volume"] = volume
        save_state(state_path, state)
        return success()

    if command in {"volume_up", "volume_down"}:
        if not state["power_on"]:
            save_state(state_path, state)
            return powered_off_error()
        if command == "volume_up":
            state["volume"] = min(100, int(state["volume"]) + 1)
        else:
            state["volume"] = max(0, int(state["volume"]) - 1)
        save_state(state_path, state)
        return success()

    if command == "set_mute":
        if not state["power_on"]:
            save_state(state_path, state)
            return powered_off_error()
        if len(args.command_args) != 1 or args.command_args[0].lower() not in {
            "true",
            "false",
        }:
            print("set_mute requires true or false", file=sys.stderr)
            save_state(state_path, state)
            return 2
        state["muted"] = args.command_args[0].lower() == "true"
        save_state(state_path, state)
        return success()

    if command == "turn_screen_off":
        if not state["power_on"]:
            save_state(state_path, state)
            return powered_off_error()
        state["screen_on"] = False
        save_state(state_path, state)
        return success()

    if command == "turn_screen_on":
        if not state["power_on"]:
            save_state(state_path, state)
            return powered_off_error()
        if state["screen_on"]:
            save_state(state_path, state)
            return active_screen_error()
        state["screen_on"] = True
        save_state(state_path, state)
        return success()

    if command == "power_off":
        state["power_on"] = False
        state["screen_on"] = False
        save_state(state_path, state)
        return success()

    if command == "power_on":
        state["power_on"] = True
        state["screen_on"] = True
        save_state(state_path, state)
        return success()

    print(f"unsupported mock command: {command}", file=sys.stderr)
    save_state(state_path, state)
    return 2


if __name__ == "__main__":
    raise SystemExit(main())
