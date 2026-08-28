# LG Buddy Session Backend Model

This document defines the current desktop session backend model.

The goal is to unify providers semantically, not mechanically.

For the broader map of systemd, lifecycle, desktop, and command-entrypoint
events that consume these semantics, see
[runtime-event-handler-map.md](runtime-event-handler-map.md).

GNOME, `swayidle`, and future backends do not expose the same APIs or the same
event richness. LG Buddy should not force them to look identical at the
transport layer. Instead, the `session` module should define:

- the canonical event meanings LG Buddy cares about
- the capability model for optional behavior
- the ownership model for idle timing

Backend-specific modules should only map their native surface into that shared
contract.

## Design Rules

1. `session` owns semantics.
2. Backend modules own provider-specific mapping.
3. Missing backend capabilities stay missing.
4. LG Buddy does not invent synthetic provider behavior just to fill gaps in the
   interface.
5. Auxiliary input sources belong to the session runtime, not desktop backend
   modules.

That means a backend can say "I do not emit `WakeRequested`" or "idle timeout is
desktop-managed" without being treated as incomplete.

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

## Capability Model

Backends should advertise what they can actually do.

The current Rust shape is:

```rust
enum IdleTimeoutSource {
    DesktopEnvironment,
    LgBuddyConfigured,
}

struct SessionBackendCapabilities {
    idle_timeout_source: IdleTimeoutSource,
    wake_requested: bool,
    before_sleep: bool,
    after_resume: bool,
    lock_unlock: bool,
    early_user_activity: bool,
}
```

### Capability Meanings

| Capability | Meaning |
| --- | --- |
| `idle_timeout_source` | Who owns the idle timeout policy for this backend. |
| `wake_requested` | Whether the backend can emit `WakeRequested`. |
| `before_sleep` | Whether the backend can emit `BeforeSleep`. |
| `after_resume` | Whether the backend can emit `AfterResume`. |
| `lock_unlock` | Whether the backend can emit `Lock` and `Unlock`. |
| `early_user_activity` | Whether the backend can emit `UserActivity` before `Active`. |

### Idle Timeout Ownership

This needs to be explicit because different providers work differently.

`DesktopEnvironment`
- The compositor or desktop already owns idle timing.
- LG Buddy reacts to the resulting events.
- No current production backend uses this mode.

`LgBuddyConfigured`
- LG Buddy must supply or manage the timeout value.
- The backend tool or adapter consumes that LG Buddy-controlled value.
- Examples: GNOME and `swayidle`.

This is separate from startup and wake retry delays.

Those delays are runtime policy, not session-backend idle policy.

## Provider Map

This is the current mapping for the known backends, with implementation status called out explicitly.

| Backend | Idle | Active | WakeRequested | UserActivity | BeforeSleep | AfterResume | Lock/Unlock | Idle Timeout Source | Current Rust Status |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| GNOME | Yes | Yes | Yes | Yes | No current surface in LG Buddy | No current surface in LG Buddy | No current surface in LG Buddy | `LgBuddyConfigured` | Implemented with LG Buddy-owned timeout policy over ScreenSaver and Mutter observations |
| `swayidle` | Yes | Yes | No | No direct equivalent | Yes | Yes | Yes, when built with systemd support | `LgBuddyConfigured` | Implemented for delegated `timeout -> Idle` and `resume -> Active`; `before-sleep`, `after-resume`, `lock`, and `unlock` are modeled but not executed |

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
shared native-session runtime owns its lifecycle and feeds
`UserActivityObserved` into the same inactivity engine as the selected desktop
provider. Resulting runtime events retain the `AuxiliaryInput` source.

The gamepad source owns its device set internally. It performs an initial scan,
refreshes on Linux input-device add, remove, and change events, and periodically
reconciles in case an event is missed. Standard controller input is read from
evdev. Logitech G923 wheel and pedal activity has a narrow raw HID fallback for
hosts where those reports do not appear on the evdev node.

GNOME is currently the only production desktop provider using the shared
native-session runtime. A future native Wayland provider should plug into that
runtime without acquiring any gamepad responsibility.

### `swayidle`

Current mapping:

| Provider surface | Canonical meaning | Current Rust Status |
| --- | --- | --- |
| `timeout <n> <cmd>` | `Idle` | Implemented |
| `resume <cmd>` | `Active` | Implemented |
| `before-sleep <cmd>` | `BeforeSleep` | Not implemented |
| `after-resume <cmd>` | `AfterResume` | Not implemented |
| `lock <cmd>` | `Lock` | Not implemented |
| `unlock <cmd>` | `Unlock` | Not implemented |

Notes:

- `swayidle` does not provide a clear equivalent of GNOME's `WakeRequested`.
- `swayidle` does not provide a Mutter-style early activity surface.
- LG Buddy owns the configured timeout value for this backend.

## Module Ownership

The code split is:

- `crates/lg-buddy/src/session.rs`
  - canonical events
  - capability model
  - backend-neutral traits and errors
- `crates/lg-buddy/src/session/runner.rs`
  - shared native-session orchestration and inactivity policy dispatch
- `crates/lg-buddy/src/session/gamepad/`
  - desktop-independent auxiliary input discovery and activity observations
- `crates/lg-buddy/src/sources/desktop/gnome.rs`
  - GNOME-specific probing and event mapping
- `crates/lg-buddy/src/sources/desktop/swayidle.rs`
  - `swayidle`-specific probing and event mapping

This keeps backend-specific details out of runtime policy and prevents each
backend from quietly defining its own semantics.
