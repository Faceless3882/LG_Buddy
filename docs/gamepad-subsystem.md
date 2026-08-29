# Gamepad Activity Subsystem

This document describes the Linux gamepad activity subsystem used by the
runtime monitor.

It is implementation guidance, not a roadmap. The goal is to make controller,
wheel, and similar HID input count as user activity without adding user-facing
modes or device-specific configuration.

## Purpose

The subsystem watches readable Linux input devices and reports controller input
as auxiliary user activity to the session runner. The shared native-session
runtime starts it independently of the selected desktop provider. GNOME and
native Wayland both use that runtime, but neither the gamepad source nor its
lifecycle belongs to a desktop environment.

The subsystem does not decide whether to blank or restore the TV. It only
reports activity. The session runner and screen policy remain responsible for
screen behavior.

## Runtime Flow

1. The shared native-session runtime in `session/runner.rs` starts a background
   gamepad activity thread alongside the selected desktop provider.
2. `session/gamepad/devices.rs` scans `/dev/input/event*`, opens each event
   node, checks whether its capabilities look gamepad-like, records
   vendor/product IDs, and maps related `/dev/hidraw*` paths through sysfs.
3. `session/gamepad/reader.rs` opens generic evdev readers for devices that
   passed discovery. It emits raw button and axis observations.
4. `session/gamepad/adapters/` asks registered device adapters whether they
   support each discovered device. Matching adapters may add supplemental
   readers for device surfaces that are not adequately represented through
   evdev.
5. `session/gamepad/registry.rs` stores per-device activity state.
6. `session/gamepad/activity.rs` evaluates raw observations. Button activity is
   immediate. Axis activity is compared against a moving baseline so arbitrary
   resting positions, such as a wheel left turned, do not continuously count as
   input.
7. The runner receives throttled `AuxiliaryInput` activity observations,
   preserves that source on the resulting runtime event, and resets the same
   inactivity deadline used by desktop activity.

## Module Map

| Path | Responsibility |
| --- | --- |
| `session/gamepad/mod.rs` | Public subsystem entrypoints, source setup, refresh, polling, diagnostics |
| `session/gamepad/devices.rs` | `/dev/input/event*` discovery, capability classification, metadata, hidraw lookup |
| `session/gamepad/device_events.rs` | Linux uevent listener for input and hidraw add/remove/change notifications |
| `session/gamepad/reader.rs` | Generic evdev event reader and raw event mapping |
| `session/gamepad/activity.rs` | Pure activity policy for buttons and axes |
| `session/gamepad/registry.rs` | Per-device state registry that applies activity policy |
| `session/gamepad/adapters/` | Device-specific supplemental activity adapters |
| `session/gamepad/hidraw.rs` | Reusable nonblocking raw HID report reader for adapters |

## Refresh And Lifecycle

The monitor performs an initial discovery scan when the gamepad thread starts.
After that it refreshes the watched device set from three triggers:

- relevant Linux uevents for input and hidraw device add, remove, or change
- short retry refreshes after transient discovery or open failures
- periodic reconciliation scans in case a uevent was missed

Refreshes are intentionally full rescans. Device events only say that something
changed; discovery owns the authoritative view of currently useful devices and
adapter readers.

State cleanup follows these rules:

- devices confidently absent from a successful discovery scan are removed
- adapter readers not returned by a successful discovery scan are removed
- devices that fail inspection are retained, because the old open file
  descriptor may still be valid while permissions or udev state settle
- a failed input-directory scan does not prune existing readers
- polling read errors remove the failing reader immediately

This keeps hotplug responsive while avoiding needless loss of already-open
readers during transient permission or metadata failures.

## Adapter API

Adapters live under `session/gamepad/adapters/` and implement
`GamepadActivityAdapter`.

An adapter decides whether it supports a discovered `GamepadDevice` and returns
zero or more `ActivityReaderSpec`s. A reader spec has a stable
`ActivityReaderKey` and opens an `ActivityReader`. Adapter readers map device
data into normal button or axis observations. Those
observations pass through the shared movement and noise policy before they can
count as activity. Receiving an opaque report is not itself proof of user
activity because devices may emit unsolicited status reports while untouched.

Adapters should stay narrow. They should not own refresh scheduling, retries,
registry cleanup, idle policy, screen behavior, or user configuration.

The Logitech G923 adapter is the current example. It matches the wheel by
vendor/product ID and attaches raw HID readers for wheel and pedal reports that
may not appear through the generic evdev path.

## Adding Device Support

Use this shape for a new device adapter:

1. Add a file under `crates/lg-buddy/src/session/gamepad/adapters/`.
2. Match devices narrowly, preferably by vendor/product ID. If a whole device
   family is supported, document the reason in the adapter tests or comments.
3. Reuse `RawHidReportReader` to read reports from a hidraw surface.
4. Add a custom reader that extracts meaningful controls as `RawGamepadEvent` values;
   ignore sequence, timestamp, status, and other non-input fields.
5. Register the adapter in `adapters/mod.rs`.
6. Add tests for support matching and reader specs. If the adapter parses
   reports, add parser tests with captured representative payloads.

Pull requests that add device support should include the tested device name,
vendor/product IDs, which input surfaces were checked, what was missing from the
generic evdev path, and how the behavior was validated.

## Testing

Default tests should cover the behavior without requiring real hardware:

- capability classification and event-node filtering
- sysfs hidraw mapping and device metadata propagation
- uevent filtering and refresh scheduling
- adapter support matching and reader spec keys
- registry cleanup and retention rules
- button, axis, baseline, and cooldown policy
- backend-neutral native-session integration of gamepad activity into
  inactivity observations

Use the hardware smoke test when changing behavior that depends on real input
devices:

```bash
LG_BUDDY_GAMEPAD_SMOKE_SECS=20 cargo test -p lg-buddy --lib \
  session::gamepad::tests::hardware_smoke_reports_real_gamepad_activity \
  -- --ignored --nocapture
```

Run the smoke test from a desktop session with read access to the connected
controllers. Move buttons, sticks, wheels, and pedals during the capture window
and confirm activity is reported for each surface the change is meant to cover.
