# LG Buddy Testing Strategy

This document keeps the testing strategy practical.

The repository does not need a large test taxonomy. It needs confidence in three things:

1. modules behave as expected within their own scope
2. modules interoperate correctly
3. user needs are actually met

Everything in the strategy should serve one of those three questions.

## 1. Module Behavior

This layer asks:

- does each module do its own job correctly?
- does it fail clearly when inputs are invalid or dependencies misbehave?
- can we trust the module in isolation before wiring it into a larger flow?

This is where most tests should live.

### What belongs here

- config parsing and validation
- path resolution
- state marker behavior
- Wake-on-LAN packet construction
- backend selection rules
- GNOME signal-to-event mapping
- gamepad device discovery, device-event filtering, raw event mapping, registry
  behavior, and activity policy
- TV command output parsing
- screen and lifecycle policy branching, retry logic, and state-transition
  outcomes

### How to test it

- pure unit tests where possible
- small trait-based fakes for internal collaborators
- subprocess mocks only when the module’s own responsibility includes an external process boundary

### Current examples

- `crates/lg-buddy/src/config.rs`
- `crates/lg-buddy/src/state.rs`
- `crates/lg-buddy/src/backend.rs`
- `crates/lg-buddy/src/sources/desktop/gnome.rs`
- `crates/lg-buddy/src/wol.rs`
- `crates/lg-buddy/src/tv.rs`
- `crates/lg-buddy/src/commands.rs`
- `crates/lg-buddy/src/screen.rs`
- `crates/lg-buddy/src/lifecycle.rs`
- `crates/lg-buddy/src/runtime_phase.rs`
- `crates/lg-buddy/src/sources/linux/network_manager.rs`

### Design rule

If a bug can be explained entirely within one module, the first test that catches it should usually live at this layer.

## 2. Module Interoperability

This layer asks:

- do the modules work together through their real boundaries?
- do config, env overrides, state directories, subprocesses, and command orchestration behave correctly together?
- do our mocks match the external contracts we actually depend on?

This is the place for integration tests and contract tests.

### What belongs here

- runtime entrypoints loading a real temporary `config.env`
- settings CLI writes feeding normal runtime config loading and apply behavior
- command flows using real env overrides
- runtime state directories and marker files
- subprocess contracts to external tools
- backend detection against mocked command/process boundaries
- GNOME runner behavior against a private session-bus harness
- logind lifecycle and NetworkManager gate behavior against a private system-bus
  harness
- GNOME and auxiliary gamepad activity resetting one LG Buddy-owned deadline

### How to test it

- use the shared Rust harness in `crates/lg-buddy/tests/support/mod.rs`
- use contract mocks for external dependencies
- keep the tests black-box enough to validate boundaries, but still fast enough for normal development

### Current examples

- `crates/lg-buddy/tests/mock_bscpylgtvcommand.rs`
- `crates/lg-buddy/tests/runtime_entrypoints.rs`
- `tools/mock_bscpylgtvcommand.py`

### Contract-mock rule

Mock the API surface we consume, not the whole system behind it.

Examples:

- the TV mock reproduces `bscpylgtvcommand` command line, exit status, stdout, and stderr behavior that LG Buddy cares about
- GNOME monitor/runtime tests should use the private session-bus harness for ScreenSaver signals and Mutter idletime
- logind lifecycle/runtime tests should use the private system-bus harness for
  `PreparingForSleep` and `PrepareForSleep` behavior

If a contract shape is unclear, probe the real dependency and update the mock.

## 3. User Needs

This layer asks:

- does LG Buddy do what the user expects?
- does the visible behavior match the product promise?
- do key user scenarios still work end to end?

This is the thinnest layer, but it is the one that keeps the other two honest.

### What belongs here

- readable acceptance scenarios for the main flows
- hardware smoke checks for visible TV behavior
- host-level checks for install/service wiring when those are part of the user experience

### How to test it

- use a small number of acceptance scenarios
- keep them focused on important user outcomes
- stay mock-backed by default
- use real hardware only when the actual visible behavior matters

### Cucumber fits here

Cucumber should be treated as a user-needs tool, not as a separate testing philosophy.

It is useful when we want to express scenarios like:

- when the configured HDMI input is active and the user goes idle, LG Buddy blanks the TV and records ownership
- when the user returns after LG Buddy blanked the TV, LG Buddy restores the screen
- when aggressive restore policy is enabled, wake/activity can restore even without a marker
- when GNOME is available, backend detection resolves to `gnome`
- when the user chooses `lg_webos` during initial configuration, pairing stores
  the credential before setup completes
- when native credentials are missing or stale, ordinary TV commands pair or
  repair them as part of the operation
- when native credentials are missing, shutdown and suspend-related commands
  skip immediately without connecting or opening a pairing prompt

It is not the right place for:

- detailed retry/backoff cases
- low-level parsing
- most contract-shape validation
- installer internals

So cucumber sits on top of the first two layers:

- it reuses module-behavior confidence
- it reuses interoperability harnesses and mocks
- it expresses user-visible outcomes in readable form

## Applying The Strategy To This Repo

### Rust runtime core

Primary concern:

- module behavior

Secondary concern:

- module interoperability

Examples:

- `config.rs`, `state.rs`, `tv.rs`, `backend.rs`, `screen.rs`,
  `lifecycle.rs`, `runtime_phase.rs`, `sources/linux/network_manager.rs`

### External tool boundaries

Primary concern:

- module interoperability

Examples:

- `bscpylgtvcommand`
- later, possibly `systemctl` and `swayidle`

### Native webOS boundary

The native webOS client tests use one stateful server for complete webOS frames,
device state, and protocol-fault scenarios. Characterization tests keep its TV
behavior aligned with observed hardware evidence.

Cucumber adds the process-level product boundary. It runs the real `lg-buddy`
binary against the same stateful server over TLS on the standard webOS port.
The server enforces the observed registration permissions, signed brightness
write envelope, request payloads, and device state transitions while recording
authentication history and pairing prompts. The scenarios cover opt-in and
credential outcomes plus representative brightness, screen, input, and power
operations; detailed transport faults remain in the native client tests.
The process-level fixture binds `127.0.0.1:3001`, matching the production TV
endpoint, so that port must be free while the serial Cucumber suite runs.

The evidence workflow, response ownership rules, and semantic scenario model
are documented in
[webos-testing.md](webos-testing.md).

### Desktop backend work

Primary concern:

- module behavior for parsing and capability logic

Secondary concern:

- module interoperability in the runner path

Examples:

- GNOME capability probing
- GNOME signal mapping
- GNOME monitor and idletime integration over the session-bus seam
- gamepad activity integration with the LG Buddy inactivity deadline
- screen runtime-phase eligibility over the private logind system-bus seam

### Gamepad activity

Subsystem design and adapter guidance live in
[gamepad-subsystem.md](gamepad-subsystem.md).

Primary concern:

- module behavior for device discovery, device-event filtering, evdev event
  mapping, device adapter support detection, per-device state, and activity
  policy

Secondary concern:

- module interoperability in the GNOME runner path
- runner refresh scheduling when device events arrive or reconciliation is due

Discovery coverage should include event-node filtering, readable-device
failures, sysfs hidraw mapping, device metadata propagation, device-event
parsing, adapter reader specs, and refresh debounce/reconciliation behavior.
Real hotplug is useful for manual validation but should not be required by the
default suite.

Hardware validation:

- use the ignored smoke test when changing real input-device behavior:

```bash
LG_BUDDY_GAMEPAD_SMOKE_SECS=20 cargo test -p lg-buddy --lib \
  session::gamepad::tests::hardware_smoke_reports_real_gamepad_activity \
  -- --ignored --nocapture
```

That test intentionally requires local readable input devices and manual
controller movement. It is not part of the default suite.

### Shell, systemd, and install flow

Primary concern:

- user needs

Secondary concern:

- module interoperability

These should not dominate the Rust test suite, but they still matter because installation and service wiring remain part of the real user path.

The release-bundle smoke test covers the current installed lifecycle topology:
the logind lifecycle service remains installed, the NetworkManager pre-down hook
remains installed, and legacy systemd sleep hooks are absent. It also verifies
that a missing TV platform remains `bscpylgtv`, explicit platform values survive
reconfiguration, and `lg_webos` routes to the stored-credential-only native path
and reports a missing credential without initiating background pairing.

## Current Practical Gaps

The most important remaining gaps are:

- real-host validation for installer and service wiring beyond the release-bundle
  temporary-root smoke test
- broader validation of the remaining shell setup surface
- any future coverage needed for richer `swayidle` hooks beyond `timeout` and `resume`

## Near-Term Priorities

The next testing work should be:

1. keep strengthening module-behavior tests where runtime logic is still moving
2. keep hardware smoke checks targeted and documented near the code path they validate
3. decide how much of the installer and service wiring deserves automated host validation
4. add targeted coverage only if new backend or setup behavior is introduced

## Default Developer Loop

The day-to-day loop should stay simple:

1. `cargo fmt --all`
2. `cargo clippy --all-targets --all-features -- -D warnings`
3. `cargo test -p lg-buddy`

That loop covers most of the first two questions:

- do modules behave correctly?
- do the important runtime boundaries interoperate correctly?

The third question, user needs, should be covered by a small acceptance layer and selected smoke checks, not by trying to force every test into daily local runs.
