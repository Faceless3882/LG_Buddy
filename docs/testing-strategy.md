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
- native Wayland registry, seat, and resumed-notification mapping
- logind current-session selection and `LockedHint` mapping
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
- `crates/lg-buddy/src/sources/desktop/wayland.rs`
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
- native Wayland provider capability and registry-churn behavior
- logind lifecycle, current-session lock state, and NetworkManager gate behavior
  against a private system-bus harness
- desktop and auxiliary gamepad activity resetting one LG Buddy-owned deadline
- update-install orchestration ordering against an injected runtime, including
  refusal, decline, concurrent acquisition, candidate preflight, installer,
  identity mismatch, cleanup, and success paths

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
- native Wayland provider tests should model registry discovery, protocol-version
  rejection, every advertised seat, resumed-only activity, and fatal provider
  loss without requiring a compositor
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
- when the graphical session locks, LG Buddy blanks the TV without waiting for
  the inactivity timeout
- when aggressive restore policy is enabled, wake/activity can restore even without a marker
- when GNOME is available, backend detection resolves to `gnome`
- when fresh configuration accepts the default `lg_webos` platform, pairing
  stores the credential before setup completes
- when an existing profile has no platform value, configuration preserves and
  materializes the `bscpylgtv` compatibility fallback
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

The GTK frontend applies these same layers without moving application
behavior into GUI tests. Its presentation contract, renderer boundary, and
test split are defined in
[GUI target architecture](gui-target-architecture.md).

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
The scenarios exercise the production unsigned registration manifest and
alert-backed Luna brightness payload while the server enforces exact
firmware-profile behavior and device state transitions. The server also retains
the legacy direct SSAP brightness path as an observed webOS24 behavior; its
webOS26 profile rejects the blacklisted legacy certificate and direct SSAP
write. These are two service-invocation paths over one websocket transport, not
two production routes. Authentication history and pairing prompts are recorded
for assertions. The scenarios cover opt-in and credential outcomes plus
representative brightness, screen, input, and power operations; detailed
transport faults remain in the native client tests. Volume and mute scenarios
exercise the same native audio endpoints characterized against local hardware.
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
- native Wayland protocol-version and seat discovery
- native Wayland resumed-notification and registry-removal mapping
- gamepad activity integration with the LG Buddy inactivity deadline
- screen runtime-phase eligibility over the private logind system-bus seam
- logind lock state entering the shared blanked state without making unlock a
  restore trigger, while fresh independent activity still restores
- logind lock monitoring rebinding and reconciling after logind changes its
  unique D-Bus owner

Native Wayland changes also require manual checks on Plasma/KWin and at least
one other target compositor. Verify that explicit and automatic `wayland`
detection and monitor startup succeed, unsupported capability or connection
cases report a precise fallback reason, and `auto` retains the
GNOME-then-native-Wayland-then-`swayidle` order. Release-facing changes must
keep the static x86_64 musl build and release-bundle smoke test green, including
preservation and deprecation reporting for an existing `swayidle` config.

### Gamepad activity

Subsystem design and adapter guidance live in
[gamepad-subsystem.md](gamepad-subsystem.md).

Primary concern:

- module behavior for device discovery, device-event filtering, evdev event
  mapping, device adapter support detection, per-device state, and activity
  policy

Secondary concern:

- module interoperability in the shared native-session runner path
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
remains installed, and legacy systemd sleep hooks are absent. Its upgrade phase
proves refusal before sudo, skips configuration, preserves config and native
credentials byte-for-byte, conditionally preserves or repairs the Python
environment, replaces the owned bundle assets, checks service action order, and
verifies the installed runtime against the candidate bytes and identity.

The focused release-manifest suite covers deterministic serialization, schema
and critical-field handling, duplicate and missing fields, canonical identity
formats, archive layout, and runtime/GUI target and identity mismatches. The
bundle smoke test exercises the same validator against both generated and
installed executables, verifies their static/dynamic linkage split, and drives
the installed GTK window through mocked read, apply, failure/retry, and cancel
paths under Xvfb. Fedora and Arch lanes repeat installed launch checks with
keyboard-only behavior, external AT-SPI role/name/value checks, visibly distinct
light/dark rendering, and 1x/2x window-geometry coverage. The display-backed
renderer suite separately asserts the same GTK semantics directly at the widget
boundary.

The Rust release-bundle acquisition suite covers exact asset selection, fresh
release metadata, bounded responses and downloads, GitHub and published digest
agreement, lightweight and annotated tags, restrictive staging and locking,
hostile archive types and paths, manifest identity, non-executing embedded
binary identity, and cleanup on success or failure. Run it with:

```bash
cargo test -p lg-buddy release_bundle::tests --lib
```

The normal suite replays GitHub release-response shapes through both a valid
current-contract bundle and the observed historical `v1.4.0-beta.1` metadata.
The historical payload is reduced to a deterministic pre-manifest archive and
must still be rejected at the manifest boundary without contacting GitHub.

The upgrade-preflight module uses injected process, service-manager,
filesystem, and ownership facts around a real temporary-root installation
fixture. Its focused suite covers a passing mutable FHS layout plus symlinked,
mounted, incompletely or wrongly owned, untrusted-writable, read-only,
hard-linked, legacy, conflicting-drop-in, malformed-candidate, and
unavailable-service-manager refusals. Table-driven cases exercise every path
policy's permission contract and every declared candidate input. Candidate
containment cases reject untrusted and non-sticky shared-writable ancestors
while preserving root-owned sticky temporary directories. Virtualenv mutation
checks are conditional on an actual compatibility-environment repair and refuse
unsafe roots or nested mount points before clearing. Run it with:

```bash
cargo test -p lg-buddy upgrade_preflight::tests --lib
```

The initial and candidate checks are deliberately non-mutating. Orchestration
tests for their consumers must separately prove that a refusal prevents release
client, confirmation, sudo, and installer effects.

The cross-version bundle smoke test adds the real release boundary that a
same-bundle reinstall cannot cover. It verifies the pinned public
`v1.4.0-beta.2` digest and identity before extraction, installs it into an
isolated root and home, populates non-default settings and native credentials,
and upgrades to an explicit candidate archive. It checks initial and candidate
refusals before network, sudo, or mutation, then verifies preserved user state,
candidate-owned file replacement, service action order, and final identity.

After a prerelease is public, `production-prerelease-canary` installs the same
baseline and drives its real `updates install` command through a PTY against
GitHub. It then clears the update cache and proves that the newly installed
candidate sees itself as GitHub's newest published release. The canary records
that sanitized newest-release response, the release-by-tag response, tag ref,
and asset redirects as a workflow artifact. Signed redirect queries and URL
userinfo are never retained. The observed beta.2 newest-release fields also
live in `crates/lg-buddy/testdata/github/` and are replayed by the normal offline
Rust suite. A successful canary on the exact prerelease commit is a
stable-promotion prerequisite.

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
