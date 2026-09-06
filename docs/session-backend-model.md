# LG Buddy Session Backend Model

This document defines the current desktop session backend model.

The goal is to unify providers semantically, not mechanically.

For the broader map of systemd, lifecycle, desktop, and command-entrypoint
events that consume these semantics, see
[runtime-event-handler-map.md](runtime-event-handler-map.md).

GNOME, native Wayland, `swayidle`, and future backends do not expose the same APIs or the same
event richness. LG Buddy should not force them to look identical at the
transport layer. Instead, the `session` module defines:

- the canonical event meanings LG Buddy cares about
- normalized observations with source identity and observation time

Source modules own their provider-specific connection or process, validation,
polling, and translation into that shared contract. The runner selects sources,
owns worker lifetime and inactivity timing, and dispatches policy.

## Design Rules

1. `session` owns semantics.
2. Source modules own provider-specific runtime mechanics and mapping.
3. Missing provider observations stay missing.
4. LG Buddy does not invent synthetic provider behavior just to fill gaps in the
   interface.
5. Auxiliary input sources belong to the session runtime, not desktop backend
   modules.

That means a source can omit `WakeRequested` without being treated as
incomplete. No source-facing capability object is needed when runtime behavior
does not consume it.

## Canonical Events

These are the semantic events the runtime should reason about.

| Event | Meaning |
| --- | --- |
| `Idle` | The backend reports the session/display has become idle. |
| `Active` | The backend reports the session/display is active again after an idle period. |
| `WakeRequested` | The backend explicitly requests the display be woken. |
| `UserActivity` | The backend can observe user activity before it emits a normal `Active` transition. |
| `BeforeSleep` | The backend reports that the system is about to suspend. |
| `AfterResume` | The backend reports that the system resumed from suspend. |
| `Lock` | The backend reports that the session should lock or has locked. |
| `Unlock` | The backend reports that the session should unlock or has unlocked. |

### Event Notes

- `Active` and `Unlock` are not the same thing.
  - Some backends can report an active display transition without a session
    unlock event.
- `UserActivity` is earlier and weaker than `Active`.
  - It exists for native desktop adapters that can expose fresh activity before
    the desktop emits its normal active/wake signal. GNOME + Mutter is the
    current production example.
  - It can also come from auxiliary activity sources owned by the session
    runtime, such as gamepad input that the desktop does not classify as
    activity.
- `WakeRequested` is optional.
  - Some providers expose an explicit wake request.
  - Others only expose idle/resume transitions.
- Lock state is an optional cross-cutting Linux source, not a prerequisite of a
  selected desktop backend. The shared session runtime observes the resolved
  graphical logind session's `LockedHint` when available. Initial or changed
  `true` maps to `Lock`; `false` maps to `Unlock` only after a prior locked
  state. Unlock is informational and never requests screen restore.

## Runtime Contract

Native sources publish `SessionObservation` values. Each observation carries a
canonical session event or inactivity fact, an `EventSource`, and the time it
was observed. Source modules do not decide whether to blank or restore the
screen.

GNOME and native Wayland feed activity facts to the shared runner, which owns
their configured inactivity deadline. `swayidle` owns its initial timeout but
publishes `Idle` and independent desktop-activity observations back to the same
runner. All three backends therefore share blank, restore, and post-blank
power-off policy.

## Provider Map

This is the current mapping for the known backends, with implementation status called out explicitly.

| Backend | Idle | Active | WakeRequested | UserActivity | Lock/Unlock | Timing and execution |
| --- | --- | --- | --- | --- | --- | --- |
| GNOME | Observed but not authoritative | Yes | Yes | Yes | Optional logind source | Shared runner owns the configured deadline over ScreenSaver and Mutter observations |
| Native Wayland | Observed but not authoritative | Resumed notification | No | Yes | Optional logind source | Shared runner owns the configured deadline using `ext_idle_notifier_v1` version 2 or newer |
| `swayidle` | Timeout becomes `Idle` | Resume becomes independent desktop activity | No | No direct equivalent | Optional logind source | Source process owns the configured initial timeout; shared runner owns policy and post-blank timing |

## Provider-Specific Mapping

### GNOME

Current mapping:

| Provider surface | Canonical meaning | Current Rust Status |
| --- | --- | --- |
| `org.gnome.ScreenSaver.ActiveChanged (true,)` | Idle observation that cannot bypass LG Buddy's timeout | Implemented |
| `org.gnome.ScreenSaver.ActiveChanged (false,)` | `Active` | Implemented |
| `org.gnome.ScreenSaver.WakeUpScreen` | `WakeRequested` | Implemented |
| Recent activity from `org.gnome.Mutter.IdleMonitor.GetIdletime` | `UserActivity` | Implemented |

Notes:

- GNOME requires GNOME Shell, `org.gnome.ScreenSaver`, and `org.gnome.Mutter.IdleMonitor`.
- LG Buddy owns the configured timeout value for this backend.
- LG Buddy owns one inactivity deadline. Desktop, auxiliary, active, and wake
  activity reports reset it; expiry after `screen_idle_timeout` triggers blanking.
- Mutter idletime is used only to detect recent desktop activity. Its absolute
  value does not trigger blanking.
- ScreenSaver idle cannot trigger blanking by itself. ScreenSaver active and
  wake signals reset the same LG Buddy deadline and remain restore observations
  evaluated by screen policy.

### Auxiliary Activity Sources

Linux gamepad input is a desktop-independent auxiliary activity source. The
shared session runtime owns its lifecycle and feeds
`UserActivityObserved` into the same inactivity engine as the selected desktop
provider. Resulting runtime events retain the `AuxiliaryInput` source.

The gamepad source owns its device set internally. It performs an initial scan,
refreshes on Linux input-device add, remove, and change events, and periodically
reconciles in case an event is missed. Standard controller input is read from
evdev. Logitech G923 wheel and pedal activity has a narrow raw HID fallback for
hosts where those reports do not appear on the evdev node.

GNOME, native Wayland, and `swayidle` use the shared session runtime. The Wayland
provider owns only its connection, registry, seats, notifications, and activity
facts; it does not acquire gamepad responsibility.

### Session Lock State

Every enabled monitor backend also starts an optional system-bus observer for the
current graphical logind session. Session selection accepts only an active,
local `x11` or `wayland` session in a user class and owned by the current UID.
An explicit `XDG_SESSION_ID` is validated against those rules; without it, LG
Buddy requires exactly one matching session and refuses ambiguous candidates.

The observer resolves logind's current unique bus owner, subscribes to
`org.freedesktop.DBus.Properties.PropertiesChanged` from that owner for the
exact session, and reconciles the initial `LockedHint` before processing changes.
It watches ownership of `org.freedesktop.login1` and repeats session resolution,
subscription, and reconciliation when logind restarts.
A lock enters the existing blanked inactivity state and dispatches
`SessionLocked` from `LinuxLogind`, so configured-input, marker, sleep-phase,
and restore policy remain centralized in `screen.rs`. Unlock performs no screen
action. A lock-triggered blank starts a fixed one-second activity grace period.
Independent desktop or gamepad activity observed before the lock or inside that
period is ignored without canceling the pending timed power-off. At the grace
boundary, the first fresh independent activity can restore the picture while the
lock screen is still shown. The shared policy compares each sample's monotonic
observation time with the lock time, so delayed dispatch does not change the
decision. Provider wake/deactivation signals associated with unlocking do not
restore it; normal inactivity timing resumes from accepted fresh activity.

Known environment support follows whether the desktop or locker maintains
logind's `LockedHint` for the graphical session:

| Environment | Lock observation |
| --- | --- |
| GNOME Shell 40 or newer on Wayland or X11 | Supported |
| KDE Plasma 5.20 or newer on Wayland or X11 | Supported |
| niri built with D-Bus support and a valid `XDG_SESSION_ID` | Supported |
| stock sway with swaylock | Absent by default; `LockedHint` is not maintained |
| Hyprland with hyprlock | Absent by default; `LockedHint` is not maintained |

This behavior is opportunistic. If logind is unavailable or no eligible session
can be resolved, LG Buddy logs a diagnostic and continues ordinary idle/activity
monitoring. If the desktop or locker never updates `LockedHint`, no lock event is
observed and ordinary monitoring likewise continues unchanged. It does not use
desktop-name checks, logind lock-request signals, locker hooks, or a Wayland
session-lock protocol. The logind observer is not a backend eligibility or
selection requirement.

### Native Wayland

The native `wayland` backend requires `ext_idle_notifier_v1` version 2 or newer
and at least one advertised `wl_seat`. It monitors every seat, including
seats that currently advertise no input capabilities, using zero-timeout idle
notifications. `resumed` maps to desktop activity; `idled` remains
observational, so only LG Buddy's inactivity deadline can trigger blanking.

Seats are added and removed dynamically. Connection or dispatch loss, removal
of the bound notifier, or removal of the last seat is fatal to the provider and
causes the user service to retry. Explicit selection reports capability errors
without falling back. `auto` selects native Wayland after the complete GNOME
contract and before the deprecated `swayidle` compatibility backend.

### `swayidle`

Current mapping:

| Provider surface | Canonical meaning | Current Rust Status |
| --- | --- | --- |
| `timeout <n> <cmd>` | Publish `Idle` to the shared runner | Implemented |
| `resume <cmd>` | Publish independent desktop activity to the shared runner | Implemented |

Notes:

- `swayidle` is deprecated, remains accepted for existing explicit selections,
  and is planned for removal in 2.0.0 after the native provider remains
  field-validated across supported compositors and the 1.x migration window.
- `swayidle` does not provide a clear equivalent of GNOME's `WakeRequested`.
- `swayidle` does not provide a Mutter-style early activity surface.
- LG Buddy owns the configured timeout value for this backend.
- The shared runner owns lock observation, screen policy, and the post-blank
  power-off deadline. Gamepad activity can cancel that second deadline, but
  does not reset swayidle's source-owned initial timeout.

## Module Ownership

The code split is:

- `crates/lg-buddy/src/session.rs`
  - canonical events
  - normalized source observations
- `crates/lg-buddy/src/session/runner.rs`
  - source selection, worker lifetime, observation multiplexing, shared
    inactivity state, and policy dispatch
- `crates/lg-buddy/src/session/gamepad/`
  - desktop-independent auxiliary input discovery and activity observations
- `crates/lg-buddy/src/sources/desktop/gnome.rs`
  - GNOME session-bus connection, subscriptions, owner validation, Mutter
    polling, event loop, and observation mapping
- `crates/lg-buddy/src/sources/desktop/wayland.rs`
  - native Wayland registry, seat, idle-notification, and activity mapping
- `crates/lg-buddy/src/sources/linux/logind.rs`
  - system lifecycle mapping plus the optional current-session lock observer,
    including bus setup, session resolution, rebinding, and `LockedHint`
    translation
- `crates/lg-buddy/src/sources/desktop/swayidle.rs`
  - production `swayidle` process invocation and timeout/resume fact transport

This keeps backend-specific details out of runtime policy and prevents each
backend from quietly defining its own semantics.
