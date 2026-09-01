# LG Buddy Architecture Overview

This document describes the current LG Buddy architecture.

It is not a product roadmap. It is a map of what exists today and how the main pieces fit together.

For the top-level system, desktop, and service event paths that enter the
runtime, see [Runtime event handler map](runtime-event-handler-map.md).

## Repository Shape

The repository now has one runtime implementation and one setup surface:

- Rust runtime workspace
  - `Cargo.toml`
  - `crates/lg-buddy/`
- shell-based setup surface
  - `configure.sh`
  - `install.sh`
  - `uninstall.sh`
  - `bin/LG_Buddy_Common`
  - `systemd/`

The Rust runtime owns operational behavior. The remaining shell layer exists for configuration, installation, and removal.

## High-Level Runtime Shape

The Rust crate is organized as a small core with explicit boundaries:

```text
main.rs
  -> lib.rs
     -> parse CLI arguments
     -> dispatch command
        -> commands.rs
           -> load config/state/dependencies
           -> sources/
              -> linux/logind.rs
              -> linux/network_manager.rs
              -> desktop/gnome.rs
              -> desktop/swayidle.rs
           -> events.rs
           -> screen.rs
           -> lifecycle.rs
           -> policy.rs
           -> runtime_phase.rs
           -> tv.rs / wol.rs / state.rs
```

## Semantic Abstraction Ladder

LG Buddy is organized as a semantic abstraction ladder. Each rung translates
implementation-specific observations and outcomes into a smaller, stable
semantic contract for the rung above. For example:

- G923 HID reports become gamepad control observations, then `UserActivity`,
  then inactivity decisions and screen actions.
- webOS messages and power states become TV operation outcomes, which screen
  and lifecycle policy use without knowing the underlying protocol.

Each rung owns the interpretation, validation, postcondition verification, and
recovery that are fully scoped to its abstraction. It exports stable semantics,
not its internal representation. Decisions that require broader product context
remain with the higher-level policy layer.

This confines complexity rather than bubbling it upward. If every low-level
detail reaches the top, policy must understand every device, provider,
transport, and platform quirk, making the core progressively harder to reason
about and change. Allowing each layer to operate at its own altitude limits how
much of the system any one component must understand, localizes changes and
tests, and lets new implementations satisfy existing contracts without teaching
policy their mechanics.

This is the project-wide application of information hiding, Design by Contract,
and separation of policy from mechanism. The session rule to unify providers
semantically rather than mechanically is one instance of this principle.

## System Diagram

The current runtime can be visualized as several consumer paths into the Rust
runtime, and then one control path from policy code into the TV transport
boundary.

The main runtime consumers are:

- system lifecycle and service integrations, including systemd,
  NetworkManager, and logind
- desktop environment and session integrations, including GNOME, `swayidle`,
  and Linux input activity sources
- TTY users invoking the CLI directly
- frontend surfaces, currently the zenity brightness dialog, which delegate
  back through the CLI/API command surface

```mermaid
flowchart LR
    subgraph Desktop["Desktop Session / External Tools"]
        GNOME["GNOME session bus<br/>ScreenSaver / Mutter signals"]
        SWAY["swayidle<br/>idle hooks"]
        INPUT["Linux input devices<br/>gamepads / wheels / device events"]
        FDO_NOTIFY["desktop notification service<br/>org.freedesktop.Notifications"]
    end

    subgraph SystemLifecycle["System Lifecycle"]
        LOGIND["logind system bus<br/>PrepareForSleep"]
        NM["NetworkManager dispatcher<br/>pre-down"]
        UPDATE_TIMER["systemd user timer<br/>background update checks"]
    end

    subgraph TTY["TTY / CLI"]
        TERMINAL["terminal commands<br/>settings / brightness / volume / updates / manual actions"]
    end

    subgraph Frontend["Frontend"]
        ZENITY["zenity brightness dialog<br/>interactive prompt"]
    end

    subgraph Rust["Rust Runtime"]
        MAIN["main.rs / lib.rs<br/>CLI + command dispatch"]
        COMMANDS["commands.rs<br/>CLI/API dependency assembly"]
        EVENTS["events.rs<br/>canonical runtime events"]
        POLICY["policy.rs<br/>action / no-action / state trail"]
        NOTIFICATIONS["notifications.rs<br/>native desktop notifications"]
        SESSIONNOTIFY["session_notifications.rs<br/>session D-Bus surface / update notifications"]
        SCREEN["screen.rs<br/>session screen policy"]
        LIFECYCLE["lifecycle.rs<br/>machine lifecycle policy"]
        PHASE["runtime_phase.rs<br/>machine sleep phase provider"]
        CONFIG["config.rs<br/>config.env parsing"]
        STATE["state.rs<br/>runtime markers"]

        subgraph SessionSubsystem["Session Integration Subsystem"]
            BACKEND["backend.rs<br/>backend selection"]
            SESSIONMODEL["session.rs<br/>shared session model"]
            RUNNER["session::runner<br/>monitor + lifecycle commands"]
            GAMEPAD["session::gamepad<br/>gamepad activity source"]
            BUS["session_bus.rs<br/>generic D-Bus transport"]

            subgraph Sources["Source Adapters"]
                LOGINDADAPTER["sources/linux/logind.rs<br/>logind lifecycle mapping"]
                NMGATE["sources/linux/network_manager.rs<br/>pre-down event source"]
                GADAPTER["sources/desktop/gnome.rs<br/>GNOME probe + signal mapping"]
                WADAPTER["sources/desktop/wayland.rs<br/>Wayland registry + activity mapping"]
                SADAPTER["sources/desktop/swayidle.rs<br/>hook mapping + capability probe"]
            end
        end

        subgraph ExternalInterfaces["External Interfaces"]
            TV["tv.rs<br/>TvDevice / TvClient"]
            WOL["wol.rs<br/>Wake-on-LAN"]
        end
    end

    subgraph TVBoundary["TV Control Boundary"]
        BSCPY["bscpylgtvcommand"]
        WEBOS["native webOS session"]
        LGTV["LG TV"]
    end

    MAIN --> BACKEND
    MAIN --> RUNNER
    RUNNER --> BACKEND
    RUNNER --> BUS
    BACKEND --> GADAPTER
    BACKEND --> WADAPTER
    BACKEND --> SADAPTER

    GNOME --> BUS
    BUS --> GADAPTER
    GADAPTER -->|"SessionEvent"| SESSIONMODEL
    WADAPTER -->|"DesktopActivityObserved"| RUNNER
    LOGIND --> BUS
    BUS --> LOGINDADAPTER
    LOGINDADAPTER -->|"RuntimeEvent"| EVENTS
    NM --> MAIN
    TERMINAL --> MAIN
    ZENITY --> MAIN
    MAIN --> COMMANDS
    COMMANDS --> EVENTS
    COMMANDS --> NMGATE
    COMMANDS --> NOTIFICATIONS
    COMMANDS --> SESSIONNOTIFY
    COMMANDS --> SCREEN
    COMMANDS --> LIFECYCLE
    SCREEN --> POLICY
    LIFECYCLE --> POLICY
    SCREEN --> PHASE
    NMGATE --> LIFECYCLE

    SWAY -->|"delegated timeout / resume<br/>screen off / screen on CLI"| MAIN
    SADAPTER -.->|"modeled SessionEvent hooks"| SESSIONMODEL
    INPUT --> GAMEPAD
    GAMEPAD -->|"UserActivity"| RUNNER
    NOTIFICATIONS --> FDO_NOTIFY
    SESSIONNOTIFY --> FDO_NOTIFY
    SESSIONNOTIFY --> BUS
    RUNNER --> SESSIONNOTIFY
    SESSIONMODEL --> RUNNER

    RUNNER -->|"Idle / Active / WakeRequested /<br/>UserActivity"| SCREEN
    RUNNER -->|"AfterResume"| LIFECYCLE
    COMMANDS --> CONFIG
    COMMANDS --> STATE
    SCREEN --> STATE
    LIFECYCLE --> STATE
    SCREEN --> TV
    LIFECYCLE --> TV
    SCREEN --> WOL
    LIFECYCLE --> WOL

    TV -->|"tv.platform=bscpylgtv"| BSCPY --> LGTV
    TV -->|"tv.platform=lg_webos"| WEBOS --> LGTV
    WOL -->|"magic packet"| LGTV
```

The intended split is:

- `lib.rs`
  - public entry surface for the binary
  - command parsing
  - shared error types
- `commands.rs`
  - CLI/API command entrypoints
  - config, state, and dependency loading for command execution
  - command output handoff
- `events.rs`
  - canonical runtime event envelope and source classification
- `policy.rs`
  - explicit policy outcomes: selected actions, no-action decisions,
    diagnostics, and state-transition trail
- `notifications.rs`
  - native desktop notification dispatch through
    `org.freedesktop.Notifications`
  - passive notification delivery for brightness
- `session_notifications.rs`
  - LG Buddy-owned user-session D-Bus surface for update notification handoff
  - session-owned update notification dispatch through
    `org.freedesktop.Notifications`
  - update notification action handling for `View Release` and automatic
    update-check opt-out
  - hosted by the user-session `monitor` process
- `screen.rs`
  - pure session screen blank and restore policy decisions over already-read
    observations
  - edge glue that reads runtime phase and TV state, applies marker
    transitions, renders output, and dispatches TV/Wake-on-LAN effects
  - session marker ownership rules
  - screen restore policy and retry behavior for screen actions
- `lifecycle.rs`
  - pure startup, shutdown, system sleep pre-action, NetworkManager sleep-gate,
    and system resume decisions over already-read observations
  - edge glue that reads reboot state, TV state, and marker state, applies
    marker transitions, renders output, dispatches TV/Wake-on-LAN effects, and
    performs retry/backoff
  - locked, idempotent pre-sleep attempt handling
  - system marker ownership rules
- `runtime_phase.rs`
  - source-agnostic machine sleep phase read used by screen policy
  - Linux implementation reads logind `PreparingForSleep`
- `config.rs`
  - config path resolution
  - parsing of the existing `config.env` format
  - typed values for HDMI input, backend, MAC address, and idle timeout
- `state.rs`
  - runtime directory resolution
  - system/session state separation
  - ownership marker management
- `upgrade_preflight.rs`
  - observes whether the current release-bundle installation can be replaced
    safely
  - returns structured, actionable refusals without downloading or mutating
    anything
  - provides separate installed-runtime and verified-candidate entrypoints
- `tv.rs`
  - TV transport abstraction
  - profile-bound `bscpylgtvcommand` adapter
  - configured selection between the compatibility and native adapters
  - adapter-neutral errors and selected-client construction
  - typed facade for input, screen, power, brightness, and audio operations
- `web_os/adapter.rs`
  - profile-bound native webOS adapter
  - lazy authenticated session ownership, serialization, reuse, and invalidation
- `web_os/audio.rs`
  - typed native SSAP volume and mute requests and responses
- `wol.rs`
  - native Wake-on-LAN packet generation and UDP send
- `backend.rs`
  - backend selection and detection
  - `auto`, `gnome`, explicit `wayland`, and `swayidle` support
- `session.rs`
  - backend-neutral session event model
  - capability surface for desktop backends
  - top-level event consumption is mapped separately in
    [runtime-event-handler-map.md](runtime-event-handler-map.md)
- `session/inactivity.rs`
  - owns the configured inactivity deadline
  - resets the deadline from normalized activity observations and blanks when
    it expires
  - keeps blank and restore decisions edge-triggered instead of poll-triggered
- `session/gamepad/`
  - discovers readable Linux gamepad-like input devices
  - refreshes discovery from Linux input-device add, remove, and change events
  - periodically reconciles the watched device set in case an event is missed
  - maps raw controller events into activity observations
  - hosts device-specific adapters for supplemental activity surfaces
  - includes a Logitech G923 adapter for raw HID wheel and pedal reports that
    may not appear through evdev
  - detailed in [gamepad-subsystem.md](gamepad-subsystem.md)
- `session_bus.rs`
  - generic blocking D-Bus transport seam
  - session-bus use for the GNOME monitor runtime
  - system-bus use for the logind lifecycle runtime
- `session/runner.rs`
  - backend-neutral monitor and lifecycle runners
  - starts the user-session notification surface before screen backend work
  - keeps the user-session process alive when idle blanking is disabled or a
    screen backend is temporarily unavailable
  - combines backend observations with the inactivity engine
  - dispatches semantic session events into screen and lifecycle policy
  - runs delegated `swayidle` by invoking the current executable's
    `screen off` and `screen on` CLI commands
- `sources/linux/logind.rs`
  - Linux system lifecycle adapter
  - maps `org.freedesktop.login1` resume signals into canonical lifecycle
    events
  - reads the `PreparingForSleep` property used by the NetworkManager pre-down
    gate
- `sources/linux/network_manager.rs`
  - NetworkManager `pre-down` dispatcher source
  - emits `NetworkTeardownImminent` with the logind sleep-phase reading
- `sources/desktop/gnome.rs`
  - GNOME-specific capability probing plus ScreenSaver signal and IdleMonitor
    method mapping
- `sources/desktop/wayland.rs`
  - native Wayland capability probing and dynamic registry/seat ownership
  - maps zero-timeout resumed notifications into desktop activity facts
- `sources/desktop/swayidle.rs`
  - `swayidle`-specific capability probing and hook-to-event mapping
  - models the `swayidle` hook surface; production timeout/resume handling
    currently delegates through the CLI/API command path

The session-facing pieces should be read as one subsystem:

- `backend.rs`
  - selects the active session backend
- `session.rs`
  - defines the homogenized session contract
- `session/inactivity.rs`
  - owns session-phase synthesis and the configured inactivity deadline
- `session/gamepad/`
  - supplies auxiliary user-activity observations for controller input
  - owns gamepad device discovery, event-triggered refresh, and reconciliation
  - see [gamepad-subsystem.md](gamepad-subsystem.md) for adapter and lifecycle details
- `session/runner.rs`
  - owns the shared native-session runtime, including the gamepad source
    lifecycle independently of the selected desktop provider
  - converts provider and auxiliary input into activity observations, resets
    the inactivity deadline, and dispatches source-classified runtime policy
  - treats `screen_idle_blank=disabled` as a passive user-session mode that
    preserves update notification handoff without TV idle blank/restore actions
  - treats delegated `swayidle` as a CLI/API client for timeout/resume actions
  - owns the `lifecycle` event loop for system sleep/wake handling
- `sources/linux/logind.rs`
  - adapts Linux system lifecycle signals into canonical lifecycle events
- `sources/desktop/gnome.rs`, `sources/desktop/wayland.rs`, and
  `sources/desktop/swayidle.rs`
  - adapt or model backend-specific surfaces against that shared session
    contract; the production `swayidle` timeout/resume path enters through
    CLI/API commands

## Command Model

The intended public user-action surface is:

- `power on`
- `power off`
- `brightness`
- `brightness get`
- `brightness set <0-100>`
- `volume`
- `volume <0-100>`
- `volume up`
- `volume down`
- `volume mute [on|off]`
- `screen off`
- `screen on`
- `settings list`
- `settings describe [KEY]`
- `settings get <KEY>`
- `settings set <KEY> <VALUE>`
- `settings unset <KEY>`
- `updates check [--notify]`
- `updates install`

The binary also retains package-owned and compatibility entrypoints during the
public-surface migration:

- `startup [auto|boot|wake]`
- `shutdown`
- `sleep-pre`
- `sleep`
- `nm-pre-down`
- `screen-off`
- `screen-on`
- `monitor`
- `lifecycle`
- `detect-backend`
- `updates background-check`

`lib.rs` parses the command line into a typed command enum and dispatches into
the runtime command handlers in `commands.rs` and `session/runner.rs`.
`commands.rs` then delegates screen and lifecycle decisions to their domain
modules and delegates platform ingestion to `sources/`. The on-demand
`updates check` command reads the saved `updates.channel` policy and consumes
the GitHub Releases API without entering the screen, lifecycle, or scheduling
paths. `updates install` adds the user-confirmed upgrade orchestration: initial
host preflight, fresh settings-driven discovery, target identity resolution,
explicit terminal confirmation, verified bundle acquisition, candidate
preflight, direct `install.sh --upgrade` execution, and installed identity
verification. The verified bundle and acquisition lock remain owned until the
installer and final verification finish. `updates background-check` is the
timer-owned wrapper: it exits before GitHub/cache work when
`updates.auto_check` is disabled and otherwise delegates to the same
settings-driven check path with notification intent enabled. When notification
is requested and an update is available, the one-shot CLI process hands the
resolved update facts to the LG Buddy-owned user-session D-Bus surface. The
running session process then owns desktop notification dispatch, notification
ids, the `View Release` action, and the notification opt-out action. The
opt-out action persists `updates.auto_check=disabled` through the settings API,
which also disables/stops the installed update-check timer. The update command
owns an operational cache under the user cache directory for GitHub ETag,
latest release metadata, and last-notified release state used by the observable
update notification policy; that cache is not user configuration and is not
part of the settings API.
The `brightness get` and `brightness set` commands use the TV picture
abstraction in `tv.rs` for typed OLED brightness validation and live TV
read/write operations. The interactive brightness dialog delegates its TV
operations back through those CLI commands.
The `volume` family uses the TV audio abstraction for typed volume and mute
operations. Setting or stepping volume explicitly unmutes after the volume
operation; mute toggle reads the current state before writing its inverse.

This keeps CLI parsing separate from operational behavior.

## Release Bundle Acquisition Boundary

`release_bundle.rs` turns a selected GitHub release into an owned, verified
candidate without invoking its installer. Acquisition refreshes the selected
tag directly from the fixed LG Buddy repository instead of trusting cached
asset metadata, resolves the tag to a bounded immutable commit, and requires
exactly one Linux-musl archive and one checksum asset.

Asset downloads use fixed GitHub API URLs, bounded bodies and deadlines, and an
explicit one-hop HTTPS release-asset redirect policy. Both assets must match
GitHub's SHA-256 digest and declared size; the archive digest must also match
the single corresponding entry in `sha256sums.txt`.

The process holds a nonblocking filesystem lock while staging under a private
user-cache directory. It scans the complete archive before extraction,
rejecting path aliases, traversal, links, special files, duplicate entries,
unsafe modes, excessive sizes, and unexpected layout. The manifest must agree
with the release, target, and resolved commit. The extracted ELF is never run:
its build-generated, linker-retained identity record is parsed as data and must
independently agree on version, channel, target, tag, and commit. The returned
guard owns the verified candidate and removes its staging tree when dropped;
no executable, installer, sudo, or configuration action runs in this boundary.

## Host Upgrade Safety Boundary

`upgrade_preflight.rs` checks observable host and installation state. It does
not infer upgrade support from the distribution name, a build flag, an install
receipt, or where the binary originally came from.

The initial preflight expects the running binary to be the mutable
`/usr/bin/lg-buddy` installation. It checks the conventional release-bundle
filesystem topology, ordinary file and directory types, ownership, writable
mounts, config-pointer discovery, readable configuration state, user and system
integrations, systemd manager availability, and the absence of legacy layouts
that would require migration. Each path is tied to the upgrade
operation that consumes it: file replacement, executable replacement,
directory mutation, read-only input, or exact drop-in replacement. Those
policies carry their ownership, permission, link, mount, and containment
invariants. Symlinks, mounted or multiply linked replacement targets,
untrusted writable system paths, unexpected systemd drop-ins, read-only
mutation targets, and special files in owned config state are refused.

After a bundle has been verified, its candidate binary can run the second
preflight. That pass rechecks the installed state, proves it is executing the
candidate from the supplied bundle root, and checks the candidate manifest,
installer, runtime, desktop entry, and systemd assets before any privileged
mutation. Candidate inputs must be owner-usable and not writable by another
user. The external ancestor chain must remain root- or user-owned and cannot be
shared-writable unless sticky-directory semantics protect its trusted child.
Configuration and pairing scripts are deliberately excluded because the
non-interactive upgrade mode preserves existing configuration and credentials
without invoking them.

The installer then reads the existing platform choice and checks the legacy
Python environment without mutating either. Native installations and healthy
compatibility environments preserve that directory unchanged. Only an
unhealthy compatibility environment triggers a second candidate preflight for
recursive repair; that conditional pass also refuses unsafe virtualenv roots
and nested mounts before the directory is cleared.

These checks are a conservative, evolving safety boundary, not an exhaustive
host-support declaration or a promise that no later privileged operation can
fail. New observable checks can be added as real installations expose unsafe
conditions; callers only consume the structured compatibility result.

## Core Control Flows

### `screen off`

`screen off` is an idle policy action.

Flow:

1. Load config.
2. Resolve the session state marker path.
3. For session-originated events, read the runtime sleep phase through
   `runtime_phase.rs`.
4. If machine sleep is pending and lifecycle automation is enabled, record a
   no-action decision and do not touch the TV.
5. Query the TV's current input.
6. If the configured HDMI input is active:
   - try to blank the screen
   - if blanking fails, fall back to `power_off`
   - create the ownership marker on success
7. If another input is active:
   - clear the marker
   - do nothing to the TV

### `screen on`

`screen on` is a resume policy action.

Flow:

1. Load config.
2. Resolve the session marker.
3. For session-originated events, read the runtime sleep phase through
   `runtime_phase.rs`.
4. If machine sleep is pending and lifecycle automation is enabled, record a
   no-action decision and do not touch the TV.
5. Apply `screen_restore_policy`:
   - `conservative`: skip if the marker is missing
   - `aggressive`: continue even without the marker
6. Try the adapter-neutral screen-unblank operation.
7. On failure, fall back to Wake-on-LAN plus repeated input-restore attempts.
8. If input restore reports that the screen is not visible, unblank it and retry
   the input so the complete restore is verified.
9. Clear the marker on success.
10. Leave the marker in place if wake recovery fails.

### `startup`

`startup` handles both cold-boot and wake restoration behavior.

Flow:

1. Load config.
2. Resolve the system-scope marker.
3. Decide behavior from `StartupMode` and `screen_restore_policy`:
   - `boot`: always restore
   - `wake`: restore only when policy allows it
   - `auto`: treat marker presence as wake, otherwise boot
4. Clear the marker before attempting restore.
5. Send Wake-on-LAN.
6. Retry `set_input` until the TV is reachable on the configured HDMI input or attempts are exhausted.

### `shutdown`

`shutdown` is a guard-rail policy action.

Flow:

1. Load config.
2. Ask `systemctl list-jobs` whether a reboot is pending.
3. If reboot is pending, skip TV power-off.
4. Otherwise query current input.
5. If the configured HDMI input is active, issue `power_off`.
6. If input query fails, still attempt `power_off`.
7. Power-off failures are logged but do not abort shutdown handling.

### `lifecycle`

`lifecycle` is the system sleep/wake event loop. Linux pre-sleep TV power-off is
owned by one cooperative suspend rail that accepts both logind
`PrepareForSleep(true)` and NetworkManager `pre-down` opportunities.

Flow:

1. Load config and suppress lifecycle TV actions while
   `system_sleep_wake_policy=disabled`.
2. Open the system bus.
3. Subscribe to logind `PrepareForSleep` signals.
4. On `PrepareForSleep(true)`:
   - enter the central suspend rail under the logind delay inhibitor
   - run one bounded pre-sleep TV decision unless another source already owns
     or completed the cycle
5. On `PrepareForSleep(false)`:
   - run wake restore policy from the canonical logind resume event
   - clear sleep-cycle coordination state
6. If config is changed to disable lifecycle handling while the service is
   running, stop the lifecycle monitor cleanly.

The NetworkManager pre-down gate runs `lg-buddy nm-pre-down`. That command reads
logind `PreparingForSleep`; false or read failure returns quickly, true runs an
idempotent pre-sleep rail before NetworkManager tears down the interface. If
logind already owns the cycle, NetworkManager waits for a terminal rail outcome
or bounded timeout before releasing teardown.

### Hidden `detect-backend` compatibility entrypoint

`detect-backend` resolves the desktop backend to use for existing package
callers. It is hidden from public help while those callers migrate to the
shared settings/backend presentation.

Selection order:

1. `LG_BUDDY_SCREEN_BACKEND` override if present
2. `screen_backend` from config
3. default to `auto`

Detection behavior:

- `auto` prefers GNOME when the current session satisfies the full GNOME contract and the session bus is reachable
- otherwise falls back to `swayidle` if installed
- explicit `wayland` validates `ext_idle_notifier_v1` version 2 or newer plus
  at least one advertised seat and does not fall back
- `auto` does not select native Wayland yet
- other forced backends validate their required services or commands

## TV Integration Boundary

The TV layer is intentionally split into two levels:

- low-level transport trait: `TvClient`
- higher-level domain facade: `TvDevice`

`TvClient` models adapter-neutral operations for one configured TV profile. The
target address and implementation-specific credential context are bound when a
client is constructed; policy cannot redirect a client by passing an address to
an operation.

The current contract covers:

- `current_input`
- `set_input`
- `oled_brightness`
- `set_oled_brightness`
- `audio_status`
- `set_volume`
- `volume_up`
- `volume_down`
- `set_muted`
- `power_off`
- `blank_screen`
- `unblank_screen`

`TvDevice` provides a more readable surface to policy code:

- `tv.input().current()`
- `tv.input().set(...)`
- `tv.screen().blank()`
- `tv.screen().unblank()`
- `tv.power().off()`
- `tv.power().wake(...)`
- `tv.picture().oled_brightness()`
- `tv.picture().set_oled_brightness(...)`
- `tv.audio().status()`
- `tv.audio().set_volume(...)`
- `tv.audio().volume_up()` / `tv.audio().volume_down()`
- `tv.audio().set_muted(...)`

Successful effectful operations return no transport-specific output. Failures
are normalized into typed TV errors. Policy may react to adapter-neutral
outcomes such as the screen not being visible, but transport and platform state
remain inside the adapter. Wake-on-LAN keeps the configured network identity at
`TvDevice`; adapter operations do not accept targeting data.

### TV Implementations

`tv.platform` selects the production TV implementation. `bscpylgtvcommand`
remains the compatibility default, including when an existing profile has no
platform value. `lg_webos` explicitly selects the native Rust implementation.

The Rust runtime talks to it through `BscpylgtvCommandClient`, which:

- belongs to one configured TV address
- shells out to the configured command path
- keeps subprocess output and exit status inside the legacy adapter
- maps reads and failures into the shared domain contract
- privately verifies screen visibility after input restore and screen unblank

`SelectedTvClient` is the internal delegation point for the configured legacy
or native implementation. `WebOsTvClient` owns one lazily authenticated
websocket session behind a mutex, reuses it while healthy, and discards it after
transport or framing failure. Native effectful operations verify their own
postconditions before reporting success. After an ambiguous failure the adapter
may reconnect for safe read-only verification, but it never replays the
effectful operation. The legacy adapter performs its equivalent power-state
readback through `bscpylgtvcommand`; neither implementation exposes webOS power
states to policy code.

Native picture settings have two known service-invocation paths. A direct SSAP
write sends `ssap://settings/setSystemSettings` on the websocket. The Luna path
uses that same websocket to create and close a temporary notification alert;
the alert callback invokes
`luna://com.webos.settingsservice/setSystemSettings` inside the TV. Luna is
therefore not a second network transport.

LG Buddy uses only the alert-backed Luna path for brightness writes. It does not
try direct SSAP first, select a path from detected firmware, or fall back between
the two. Direct SSAP is rejected on affected firmware, while the Luna path is
the one supported by both evidence-backed firmware profiles. The direct path
remains represented in tests only so the mock can preserve the observed
firmware difference.

Keeping native TV control within the Rust runtime removes the Python client
from that selected operation path. This is useful groundwork for declarative or
immutable distributions such as NixOS, but it is not yet first-class NixOS
installation support. The shell installer still provisions the compatibility
fallback and writes conventional mutable system locations; alternative install
layouts are tracked in
[issue #24](https://github.com/Staphylococcus/LG_Buddy/issues/24).

## State Model

State is intentionally small.

The runtime currently uses two ownership markers:

- `screen_off_by_us` in session scope
- `screen_off_by_us` in system scope

The ownership markers answer one question:

- did LG Buddy blank or power off the TV as part of its own policy?

It does not answer whether restore should always be blocked.
In `aggressive` mode, restore may proceed even when the marker is absent.
When the system-scope marker exists after a sleep pre-action, session screen
actions defer to the lifecycle resume path while `system_sleep_wake_policy` is
enabled. The lifecycle path keeps that marker present while it waits for network
readiness and attempts input restore, then clears it after success or exhausted
restore attempts.

There are two scopes:

- `System`
  - default path under `/run/lg_buddy`
- `Session`
  - default path under `$XDG_RUNTIME_DIR/lg_buddy`
  - fallback under `/run/user/<uid>/lg_buddy`

This is a direct replacement for the earlier ad hoc script coordination pattern.

The cooperative suspend rail uses system-scope lock and cycle state files to
prevent concurrent pre-sleep handlers from racing each other. Repeated hooks are
expected to be safe through idempotent TV policy and persisted terminal cycle
outcomes.

## Desktop Backend Strategy

Desktop backends are treated as adapters, not owners of policy.

The runtime core owns:

- config
- state
- TV control
- Wake-on-LAN
- retries and recovery behavior
- lifecycle decisions

Desktop backends should only answer questions like:

- which backend is active?
- which session signals are available?
- how should backend-specific signals map into runtime events?

`session.rs` defines the backend-neutral semantic contract:

- canonical session events
  - `Idle`
  - `Active`
  - `WakeRequested`
  - `UserActivity`
  - `BeforeSleep`
  - `AfterResume`
  - `Lock`
  - `Unlock`
- backend capability flags
- idle-timeout ownership semantics

The detailed session model is documented in `docs/session-backend-model.md`.

`sources/desktop/gnome.rs` is the native GNOME adapter. It currently provides:

- capability probing
- mapping from GNOME D-Bus monitor lines into `SessionEvent`
- the GNOME event and idletime sources used by `lg-buddy monitor`

`sources/desktop/wayland.rs` is the native non-GNOME adapter. It owns the
Wayland connection, registry, every advertised seat, and zero-timeout idle
notifications. Resumed notifications become desktop activity observations in
the shared inactivity runtime; compositor idle does not directly blank the TV.

`sources/desktop/swayidle.rs` is the delegated-tool adapter. It currently provides:

- capability probing
- mapping from `swayidle` hooks into `SessionEvent`

Production `swayidle` monitor execution does not dispatch those modeled events
directly for timeout/resume. It starts `swayidle` with command strings pointing
back to the current LG Buddy executable:

- `screen off` for timeout
- `screen on` for resume

That means `swayidle` acts as a CLI/API client of LG Buddy. It is delegated, but
not a separate quirks path for screen policy: the invoked commands load config
and state normally, construct canonical CLI/API runtime events, and enter
`screen.rs` through the same command surface as manual invocations.

The session subsystem is intentionally asymmetric where the providers are
asymmetric:

- the current GNOME provider treats ScreenSaver active/wake and recent Mutter
  input as activity that resets LG Buddy's inactivity deadline; ScreenSaver
  idle is not a blanking authority
- the shared native-session runtime consumes gamepad activity directly from
  Linux input devices as `AuxiliaryInput`, independently of desktop providers;
  GNOME and native Wayland use this runtime
- the gamepad source refreshes its device set from Linux device add, remove, and
  change events, with periodic reconciliation for missed events
- delegated `swayidle` monitor execution is implemented as CLI/API delegation
  for `timeout` and `resume` parity with the shell monitor
- `swayidle` systemd-style hooks such as `before-sleep`, `after-resume`,
  `lock`, and `unlock` are not wired into monitor behavior; system lifecycle is
  handled by the NetworkManager pre-down gate plus logind lifecycle service
  instead

`swayidle` remains the external-tool compatibility backend while native
Wayland is explicit opt-in. Automatic native selection and later deprecation of
the delegated path are separate work.

## Configuration and Override Surface

The runtime is designed to be testable and relocatable.

Important environment overrides:

- `LG_BUDDY_CONFIG`
  - explicit config file path
- `LG_BUDDY_SCREEN_BACKEND`
  - force backend selection
- `LG_BUDDY_BSCPYLGTV_COMMAND`
  - override TV command path
- `LG_BUDDY_SYSTEM_RUNTIME_DIR`
  - override system state directory
- `LG_BUDDY_SESSION_RUNTIME_DIR`
  - override session state directory
- `LG_BUDDY_SYSTEMCTL`
  - override the `systemctl` command path used by shutdown logic

These exist mainly so the runtime can be tested without mutating real system paths or depending on globally installed commands.

## Testing Shape

The test strategy has three layers:

- unit tests for parsing, state, backend selection, and policy
- subprocess-backed integration tests for TV behavior
- manual hardware probes when exact external behavior is unclear

TV-facing tests exercise the production protocol boundaries instead of relying
only on in-memory fakes. The compatibility adapter uses a stateful subprocess
mock, while the native adapter uses a centralized stateful webOS test server.

Relevant test assets:

- `tools/mock_bscpylgtvcommand.py`
- `crates/lg-buddy/tests/support/mod.rs`
- `crates/lg-buddy/tests/mock_bscpylgtvcommand.rs`
- `crates/lg-buddy/src/web_os/test_support/test_server.rs`
- `crates/lg-buddy/src/web_os/observed_behavior.rs`
- `crates/lg-buddy/tests/features/webos.feature`

The legacy mock preserves the command and response shapes observed from the
installed client. Native behavior claimed as real is linked to hardware
evidence and modeled by the centralized server; defensive protocol faults are
identified separately. See [Native webOS testing](webos-testing.md).

## Current Boundary

The Rust runtime currently owns:

- config loading
- state handling
- TV abstraction
- Wake-on-LAN
- backend detection
- startup
- shutdown
- system lifecycle handling through the cooperative logind/NetworkManager
  suspend rail plus logind resume monitor
- screen off
- screen on
- brightness control
- volume and mute control
- `monitor` command with GNOME, native Wayland, and `swayidle` paths

The shell layer still owns:

- interactive configuration
- installation
- uninstallation

What is still not implemented:

- `swayidle` `before-sleep`, `after-resume`, `lock`, and `unlock` handling
- additional desktop backends
- an immutable-distribution install layout that avoids conventional `/usr`
  writes

So the current architecture should be read as a Rust-owned runtime with a thin shell setup surface.
