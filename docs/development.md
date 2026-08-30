# Development

This document covers building, local installation, validation, release tooling, and contributor-facing repository details.

## Build Prerequisites

Compiling the Rust runtime requires:

- a Rust toolchain with `cargo`
- a working C toolchain

Running the interactive installer, exercising the legacy TV fallback, and
testing release bundles also requires:

- `python3-venv`
- `python3-pip`
- `zenity`

Backend-specific tools used in development and local testing:

- `swayidle` for the `swayidle` monitor backend
- readable `/dev/input/event*` devices for local gamepad activity testing
- readable `/dev/hidraw*` devices when testing the Logitech G923 raw HID fallback

For GNOME end-to-end work, the running session also needs the full GNOME contract:

- GNOME Shell
- `org.gnome.ScreenSaver`
- `org.gnome.Mutter.IdleMonitor`

The C toolchain is required because `cargo build` now compiles vendored
`libdbus` as part of the dependency graph. On common Linux distributions that
usually means:

- Debian/Ubuntu/Pop!_OS: `build-essential`
- Fedora: `gcc`
- Arch: `base-devel`

## Build

Build the runtime from source with:

```bash
cargo build --release -p lg-buddy
```

Official release builds inject version identity into the binary:

```bash
LG_BUDDY_RELEASE_VERSION=X.Y.Z LG_BUDDY_BUILD_COMMIT="$(git rev-parse HEAD)" cargo build --release -p lg-buddy
```

Without those environment variables, `lg-buddy --version` reports the Cargo
package version with `channel: dev` and `commit: unknown`.
Supported channel values are `dev`, `prerelease`, and `stable`.

The resulting binary will be at:

```text
./target/release/lg-buddy
```

## Install a Locally Built Binary

`install.sh` is installer-only. It does not build the runtime.

To install a binary you built yourself:

```bash
./install.sh --runtime-binary ./target/release/lg-buddy
```

To install from a release bundle instead, extract the archive and run:

```bash
./install.sh
```

## Validation

Useful checks during development:

```bash
cargo test -p lg-buddy --lib
cargo test -p lg-buddy --test cucumber
cargo clippy -p lg-buddy --all-targets --all-features -- -D warnings
bash -n install.sh uninstall.sh configure.sh bin/LG_Buddy_Common scripts/build-release-bundle.sh scripts/test-release-bundle.sh scripts/publish-release-assets.sh
python3 scripts/test_release_promotion.py
```

Optional hardware smoke for gamepad activity:

```bash
LG_BUDDY_GAMEPAD_SMOKE_SECS=20 cargo test -p lg-buddy --lib \
  session::gamepad::tests::hardware_smoke_reports_real_gamepad_activity \
  -- --ignored --nocapture
```

Run that from a desktop session that has read access to the connected
controllers. The test uses the production gamepad activity source and requires
manual input during the capture window. To smoke-test hotplug behavior, start
the monitor and connect or disconnect a controller; the gamepad source should
refresh without restarting the service. The production monitor also performs a
periodic reconciliation scan for missed device events.

For gamepad subsystem internals and adapter contribution guidance, see
[gamepad-subsystem.md](gamepad-subsystem.md).

## Release Tooling

Build a release bundle locally with:

```bash
LG_BUDDY_RELEASE_VERSION=0.0.0-dev \
LG_BUDDY_BUILD_COMMIT="$(git rev-parse HEAD)" \
cargo build --release -p lg-buddy --target x86_64-unknown-linux-gnu
./scripts/build-release-bundle.sh --target x86_64-unknown-linux-gnu --version 0.0.0-dev
```

The builder requires a full release commit and expects the matching release
binary to exist under:

```text
./target/<target>/release/lg-buddy
```

Smoke test a generated release bundle with:

```bash
./scripts/test-release-bundle.sh --archive ./dist/lg-buddy-0.0.0-dev-x86_64-unknown-linux-gnu.tar.gz
```

The smoke test validates `release-manifest.json` against the archive name and
bundled binary before running installer code. It then installs into a temporary
root and checks both TV platforms, native credential preservation across
upgrades, lifecycle and NetworkManager hook topology, and uninstall cleanup
without mutating the host installation.

Run the focused manifest contract tests with:

```bash
python3 scripts/test_release_bundle_manifest.py
```

Dry-run the GitHub release publish step with:

```bash
GH_RELEASE_DRY_RUN=1 ./scripts/publish-release-assets.sh --dist-dir ./dist --tag v0.0.0-dev
```

Official releases are created only through a reviewed `dev` promotion PR. For
the branch contract and recovery process, see
[release-process.md](release-process.md).

## Repository Layout

| Path | Purpose |
| --- | --- |
| `crates/lg-buddy/src/lib.rs` | CLI parsing and command dispatch |
| `crates/lg-buddy/src/commands.rs` | Runtime command entrypoints and dependency assembly |
| `crates/lg-buddy/src/events.rs` | Canonical runtime event vocabulary |
| `crates/lg-buddy/src/policy.rs` | Policy outcome, action, no-action, diagnostic, and state-transition types |
| `crates/lg-buddy/src/screen.rs` | Session screen blank/restore policy |
| `crates/lg-buddy/src/lifecycle.rs` | Startup, shutdown, system sleep, and system resume policy |
| `crates/lg-buddy/src/runtime_phase.rs` | Runtime sleep-phase provider abstraction |
| `crates/lg-buddy/src/session/runner.rs` | Session monitor loop |
| `crates/lg-buddy/src/session/inactivity.rs` | Session inactivity deadline and phase synthesis |
| `crates/lg-buddy/src/session/gamepad/` | Gamepad activity discovery, device-event refresh, adapters, capture, registry, and policy |
| `crates/lg-buddy/src/session_bus.rs` | Generic D-Bus transport used by session and system event sources |
| `crates/lg-buddy/src/sources/linux/logind.rs` | Linux logind lifecycle signal and property adapter |
| `crates/lg-buddy/src/sources/linux/network_manager.rs` | NetworkManager pre-down lifecycle source adapter |
| `crates/lg-buddy/src/sources/desktop/gnome.rs` | GNOME backend integration |
| `crates/lg-buddy/src/sources/desktop/wayland.rs` | Native Wayland idle/activity provider |
| `crates/lg-buddy/src/sources/desktop/swayidle.rs` | `swayidle` backend integration |
| `crates/lg-buddy/src/tv.rs` | TV transport boundary and facade |
| `crates/lg-buddy/src/web_os/` | Native webOS client, profile-bound TV adapter, domain operations, and test support |
| `crates/lg-buddy/src/wol.rs` | Native Wake-on-LAN support |
| `configure.sh` | Interactive configuration tool |
| `install.sh` | Installer for an existing binary |
| `uninstall.sh` | Uninstaller |
| `scripts/release_bundle_manifest.py` | Release-bundle identity manifest creator and validator |
| `scripts/build-release-bundle.sh` | Release bundle builder |
| `scripts/test-release-bundle.sh` | Release bundle smoke test |
| `scripts/publish-release-assets.sh` | GitHub release publish helper |
| `scripts/release_promotion.py` | Promotion version, ancestry, and tag validator |
| `.github/workflows/ci.yml` | CI validation workflow |
| `.github/workflows/promotion-check.yml` | Trusted promotion PR contract check |
| `.github/workflows/release.yml` | Approved promotion build, publication, and ref finalizer |
| `bin/LG_Buddy_Common` | Shared shell config helper used by setup scripts |
| `systemd/` | Installed unit files and tmpfiles config, including the logind lifecycle service |
| `docs/architecture-overview.md` | Runtime architecture |
| `docs/defaults-and-configuration.md` | Product defaults and persistent configuration guidance |
| `docs/gamepad-subsystem.md` | Gamepad activity architecture and adapter guidance |
| `docs/runtime-event-handler-map.md` | Top-level system, desktop, and runtime event handler map |
| `docs/session-backend-model.md` | Session backend semantics and capability model |
| `docs/testing-strategy.md` | Test strategy and scope |
| `docs/webos-testing.md` | Native webOS evidence and mock testing strategy |
