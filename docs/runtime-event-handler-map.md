# LG Buddy Runtime Event Handler Map

This document maps the top-level events LG Buddy consumes and the handler paths
that act on them.

It complements [session-backend-model.md](session-backend-model.md). The session
backend model defines canonical session semantics. This document describes how
real system, desktop, and user-service entrypoints reach runtime policy.
Product-wide defaults and advanced configuration rules are documented in
[defaults-and-configuration.md](defaults-and-configuration.md).

## Event Vocabulary

LG Buddy has four related but distinct event/result shapes.

| Shape | Owner | Purpose |
| --- | --- | --- |
| Command entrypoint | `lib.rs` / `commands.rs` / `session::runner` | External service, hook, or user command invokes one runtime command. |
| Runtime event | `events.rs` | Source-classified fact or intent, such as CLI/API, Linux logind, Linux NetworkManager, desktop session, or auxiliary input. |
| Session event | `session.rs` | Backend-neutral event such as `Idle`, `Active`, `WakeRequested`, `BeforeSleep`, or `AfterResume`. |
| Inactivity observation | `session/inactivity.rs` | Lower-level activity fact such as desktop input, wake request, or auxiliary input. |
| Policy outcome | `policy.rs` | Explicit selected actions, no-action decisions, diagnostics, and state transitions. |

The command entrypoint layer remains the external integration surface. The
session event layer is active for native monitor behavior and delegated backend
modeling. System lifecycle handling is normalized through `RuntimeEvent` and
the lifecycle policy domain. The inactivity observation layer owns one
deadline. Every activity observation resets it; expiry produces an
edge-triggered blank decision.

GNOME and native Wayland feed the same normalized inactivity observations into
the shared native-session path.

## Current Top-Level Handlers

| External event source | Runtime entrypoint | Primary handler | Current action |
| --- | --- | --- | --- |
| system boot / service start | `lg-buddy startup boot` | `commands` -> `lifecycle` | Send Wake-on-LAN and restore the configured input. |
| system shutdown / service stop | `lg-buddy shutdown` | `commands` -> `lifecycle` | Power off the TV when the configured input is active, unless a reboot is pending. |
| NetworkManager `pre-down` while logind `PreparingForSleep=true` | `lg-buddy nm-pre-down` | `sources::linux::network_manager` -> `lifecycle` | Join the central suspend rail before network teardown; wait for an in-progress logind rail or run the pre-sleep TV decision. |
| logind `PrepareForSleep(true)` | `lg-buddy lifecycle` | `sources::linux::logind` -> `session::runner` -> `lifecycle` | Enter the central suspend rail under the logind delay inhibitor so systems without a NetworkManager `pre-down` hook still get bounded pre-sleep TV handling. |
| logind `PrepareForSleep(false)` | `lg-buddy lifecycle` | `sources::linux::logind` -> `session::runner` -> `lifecycle` | Run wake restore policy and clear sleep-cycle coordination state. |
| graphical logind session `LockedHint=true` | `lg-buddy monitor` | `sources::linux::logind` -> `session::runner` -> `screen` | Enter the normal blanked inactivity state and apply session screen-off policy. |
| user graphical session start | `lg-buddy monitor` | `session::runner::run_monitor` | Detect the session backend and run the selected monitor path. |
| manual screen blank | `lg-buddy screen off` | `commands` -> `screen` | Blank or power off the TV if LG Buddy owns the configured input. |
| manual screen restore | `lg-buddy screen on` | `commands` -> `screen` | Restore the screen when marker and restore-policy rules allow it. |
| manual update check | `lg-buddy updates check` | `updates` -> saved channel policy -> GitHub releases API | Check for an available release independently of the automatic update-check setting. |
| user-confirmed update install | `lg-buddy updates install` | `update_install` -> host preflight -> release verification -> candidate preflight -> `install.sh --upgrade` -> installed identity verification | Install only a newer release from the saved channel after explicit terminal confirmation. |
| user update-check timer | `lg-buddy updates background-check` | `updates` -> saved channel policy -> GitHub releases API -> session notification handoff | Check for updates when automatic checks are enabled and notify once per release. |

Compatibility command surfaces still exist for direct/manual invocation:

| Command | Current role |
| --- | --- |
| `lg-buddy screen-off` | Hidden compatibility alias for `lg-buddy screen off`. |
| `lg-buddy screen-on` | Hidden compatibility alias for `lg-buddy screen on`. |
| `lg-buddy sleep-pre` | Direct pre-sleep policy command retained for manual/debug invocation. |
| `lg-buddy startup wake` | Direct wake restore policy command retained for manual/debug invocation. |
| `lg-buddy sleep` | Legacy NetworkManager pre-down behavior. It is not installed as a default event handler. |

These handlers are intentionally conservative around ownership:

- session-scope screen blanking uses the session marker
- system sleep uses the system marker
- restore behavior is gated by `screen_restore_policy`
- shutdown does not write ownership markers

## Runtime Event Pipeline

LG Buddy now uses a source-agnostic event and policy boundary for the screen and
lifecycle paths:

```text
system lifecycle sources
session lock state
desktop idle/activity sources
auxiliary activity sources
  -> narrow source adapters
  -> RuntimeEvent / normalized session events / inactivity observations
  -> InactivityEngine
  -> screen and lifecycle policy
  -> PolicyOutcome
  -> TV / Wake-on-LAN / state effects
```

Source adapters report facts. They do not own marker semantics, restore policy,
retries, Wake-on-LAN, or TV transport behavior.

Examples:

| Source category | Example source | Runtime representation |
| --- | --- | --- |
| system lifecycle | `org.freedesktop.login1`, platform-native lifecycle APIs | `MachinePreparingForSleep`, `MachineResumed`, `NetworkTeardownImminent` |
| session lock state | current graphical logind session `LockedHint` | `SessionLocked`, `SessionUnlocked` from `LinuxLogind` |
| desktop activity | Mutter, native Wayland idle protocols | activity observations |
| desktop wake request | GNOME ScreenSaver wake signal, future equivalents | `WakeRequested` |
| auxiliary activity | Linux gamepad input | `UserActivityObserved` |

## Monitor Event Paths

### Native Inactivity Path

The native inactivity path is used by GNOME and native Wayland, including
Wayland selected by `auto`. Both feed activity facts into the same inactivity model instead of
delegating blank/restore commands to an external tool.

```text
native desktop activity facts
auxiliary activity facts
  -> inactivity observations
  -> reset the InactivityEngine deadline
  -> configured timeout expires
  -> Idle / Active / WakeRequested / UserActivity ─┐
                                                    ├-> screen policy
current-session logind LockedHint=true              │
  -> immediate Lock / BlankNow ─────────────────────┘
```

Current native-runtime inputs:

| Provider surface | Runtime representation | Consumed by |
| --- | --- | --- |
| `org.gnome.ScreenSaver.ActiveChanged(true)` | Non-authoritative idle observation | GNOME source -> shared runner; does not change the LG Buddy deadline |
| `org.gnome.ScreenSaver.ActiveChanged(false)` | `ProviderActive` | `InactivityEngine` |
| `org.gnome.ScreenSaver.WakeUpScreen` | `WakeRequested` | `InactivityEngine` |
| Recent activity reported by `org.gnome.Mutter.IdleMonitor.GetIdletime` | `DesktopActivityObserved` | `InactivityEngine` |
| Linux gamepad activity | `UserActivityObserved` from `AuxiliaryInput` | Shared native-session runtime -> `InactivityEngine` |
| Initial or changed logind `LockedHint=true` | `SessionEvent::Lock` from `LinuxLogind` | Shared native-session runtime -> `InactivityEngine` |
| Changed logind `LockedHint=false` after lock | `SessionEvent::Unlock` from `LinuxLogind` | Clear the observed lock state without requesting screen restore |

The desktop rows are GNOME-specific source surfaces. Gamepad activity is an
independent auxiliary source owned by the shared native-session runtime. The
key architectural point is that blank/restore decisions are made after
normalization, not inside a desktop adapter.

After a lock blank, only independent desktop or gamepad activity newer than the
lock can produce `RestoreNow`. Stale observations and GNOME provider
active/wake signals do not make unlock itself a restore trigger.

The resulting decisions are dispatched as:

| Inactivity decision | Dispatched event | Policy target |
| --- | --- | --- |
| `BlankNow` | `SessionEvent::Idle` -> `RuntimeEvent` from `DesktopSession` | `screen::run_screen_off_from_env_for_event` |
| `BlankNow` from session lock | `SessionEvent::Lock` -> `RuntimeEvent::SessionLocked` from `LinuxLogind` | `screen::run_screen_off_from_env_for_event` |
| `RestoreNow` from provider active | `SessionEvent::Active` -> `RuntimeEvent` from `DesktopSession` | `screen::run_screen_on_from_env_for_event` |
| `RestoreNow` from wake request | `SessionEvent::WakeRequested` -> `RuntimeEvent` from `DesktopSession` | `screen::run_screen_on_from_env_for_event` |
| `RestoreNow` from desktop activity | `SessionEvent::UserActivity` -> `RuntimeEvent` from `DesktopSession` | `screen::run_screen_on_from_env_for_event` |
| `RestoreNow` from auxiliary activity | `SessionEvent::UserActivity` -> `RuntimeEvent` from `AuxiliaryInput` | `screen::run_screen_on_from_env_for_event` |

### Delegated `swayidle` CLI/API Path

The `swayidle` monitor is a delegated CLI/API client path.

```text
swayidle timeout/resume
  -> external command string
  -> lg-buddy screen off / lg-buddy screen on
  -> canonical CLI/API RuntimeEvent
  -> screen policy
```

`sources/desktop/swayidle.rs` owns the production process invocation and creates
only the delegated timeout/resume command shape shown above. It does not expose
an unused hook-to-event capability model.

This deprecated path remains for existing explicit selections and as an
automatic compatibility fallback on unsupported native sessions. It is
delegated, but it is not a separate screen-policy quirks mode: `swayidle` re-enters LG Buddy
through the same CLI/API command surface as manual `screen off` and `screen on`.
Retiring it means replacing delegated timeout/resume execution with native
idle/activity facts that feed the same inactivity engine used by the current
native path.

## System Lifecycle Event Handling

LG Buddy handles system lifecycle through one Linux lifecycle subsystem with two
cooperating Linux event sources:

```text
NetworkManager pre-down
  -> lg-buddy nm-pre-down
  -> logind PreparingForSleep property read
  -> cooperative suspend rail
  -> TV action executor

org.freedesktop.login1 PrepareForSleep(true)
  -> lg-buddy lifecycle
  -> cooperative suspend rail
  -> TV action executor

org.freedesktop.login1 PrepareForSleep(false)
  -> lg-buddy lifecycle
  -> MachineResumed runtime event
  -> lifecycle restore policy
  -> TV action executor
```

NetworkManager and logind cooperate through one suspend rail. NetworkManager
`pre-down` remains the strongest network-up opportunity: it reads logind
`PreparingForSleep` synchronously; false or read failure returns quickly, true
enters the rail while NetworkManager is still holding interface teardown. If
logind already owns the rail, NetworkManager waits for a terminal outcome or a
bounded timeout before releasing teardown.

The lifecycle service subscribes to logind manager signals on the system bus and
holds a sleep delay inhibitor while idle. `PrepareForSleep(true)` enters the
same suspend rail so systems without a NetworkManager `pre-down` hook still get
a bounded pre-sleep TV decision. `PrepareForSleep(false)` runs wake restore
policy and clears per-cycle coordination state.

The installer must not leave old lifecycle owners active. It removes or disables
these legacy artifacts during install and uninstall:

- `LG_Buddy_sleep.service`
- `LG_Buddy_wake.service`
- old unit override directories for those services
- `/etc/NetworkManager/dispatcher.d/pre-down.d/LG_Buddy_sleep`
- `/usr/lib/systemd/system-sleep/LG_Buddy_sleep_hook`

Current lifecycle signal mapping:

| logind surface | Canonical event | Runtime action |
| --- | --- | --- |
| `PreparingForSleep` property | `NetworkTeardownImminent { machine_sleep_pending }` in the NetworkManager source path; `RuntimePhaseRead` in screen eligibility | Gate pre-sleep policy and block session screen TV I/O during pending machine sleep. |
| `PrepareForSleep(true)` | `MachinePreparingForSleep` | `run_sleep_pre_for_event` through the central suspend rail. |
| `PrepareForSleep(false)` | `MachineResumed` | `run_system_resume` |

The current `SessionEventDispatcher` handles these session events when a
backend path dispatches them. The production `swayidle` path delegates timeout
and resume to direct `screen off` / `screen on` CLI/API commands.

| Session event | Current action |
| --- | --- |
| `Idle` | Run `screen off`. |
| `Active` | Run `screen on`. |
| `WakeRequested` | Run `screen on`. |
| `UserActivity` | Run `screen on`. |
| `BeforeSleep` | Run pre-sleep TV power-off policy. |
| `AfterResume` | Run wake restore policy. |
| `Lock` | Run `screen off` through normal session policy. |
| `Unlock` | Log the transition without a screen action. |

For session-originated `Idle`, `Active`, `WakeRequested`, `UserActivity`, and
`Lock`, screen policy checks `runtime_phase.rs` before doing TV I/O. If logind reports
that machine sleep is pending and lifecycle automation is enabled, screen policy
records a runtime-phase no-action decision and leaves TV/state untouched. If the
phase read fails, screen policy fails open and proceeds with the ordinary
screen action.

The source-owned logind lock observer resolves only the current UID's active,
local graphical session. `XDG_SESSION_ID`, when present, is validated; otherwise
exactly one eligible session must exist. The source owns the system-bus
connection, subscriptions, owner rebinding, reconciliation, and translation to
normalized lock observations. Missing, ambiguous, or unavailable session state
produces a diagnostic without changing inactivity behavior or native backend
eligibility. A desktop or locker that never updates `LockedHint` simply produces
no lock events. Unlock never sends a wake, restore, or activity event. Fresh
independent activity observed while locked remains able to restore the screen
through the existing marker policy.

## Lifecycle Default And Migration Stance

The general default/configuration stance is defined in
[defaults-and-configuration.md](defaults-and-configuration.md). Applied to the
lifecycle path:

- automatic system sleep/wake TV control defaults to enabled
- users who do not want automatic sleep/wake TV control opt out through
  `system_sleep_wake_policy=disabled`
- default installs do not ask whether lifecycle automation should run
- NetworkManager `pre-down` and logind `PrepareForSleep(true)` cooperate through
  one central suspend rail
- logind `PrepareForSleep(false)` owns resume restore and per-cycle cleanup
- legacy systemd and old NetworkManager sleep/wake handlers are cleanup targets,
  not parallel runtime handlers
- legacy cleanup honors a persisted opt-out config value

## Native Non-GNOME Wayland Shape

Native non-GNOME Wayland idle monitoring is separate from logind lifecycle work
and follows the same native inactivity path.

Target event path:

```text
Wayland idle/activity facts
  -> native Wayland adapter
  -> inactivity observations
  -> InactivityEngine
  -> screen policy
```

That keeps the responsibilities separate:

- logind reports machine lifecycle
- desktop adapters report activity facts
- native Wayland reports Wayland idle/activity facts
- gamepad input reports auxiliary user activity
- LG Buddy policy decides when those facts should blank or restore the TV

## Remaining Migration Notes

The current architecture has the Linux lifecycle sources, screen policy,
lifecycle policy, runtime phase guard, and source adapter namespace in place.
Remaining work should stay scoped:

1. Keep native Wayland monitoring separate from the logind lifecycle path.
2. Keep `swayidle` working without rewriting existing configuration throughout
   the documented 1.x compatibility window.
3. Preserve the one-lifecycle-owner invariant in installer, release-bundle, and
   uninstall tests.
4. Treat future platform lifecycle providers, such as a possible macOS provider,
   as source adapters that emit the same canonical lifecycle events.
