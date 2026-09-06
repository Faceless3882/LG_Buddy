# LG Buddy

LG Buddy makes an LG webOS TV behave more like a monitor for a Linux PC.
It is inspired by
[LGTV Companion for Windows](https://github.com/JPersson77/LGTVCompanion).

LG Buddy can:

- turn the TV on at boot and wake
- turn the TV off at shutdown and before system sleep
- blank and restore the panel on supported desktop idle backends, then power it
  off after five additional minutes without activity
- keep the panel awake when supported gamepads are active
- adjust OLED pixel brightness from a desktop dialog or the command line
- control TV volume and mute from the command line
- manage settings and check for updates from the command line

GNOME is not required. Official release bundles include a prebuilt `lg-buddy`
binary, so normal installation does not require a Rust toolchain.

## Desktop Compatibility

| Functionality | GNOME | Compatible native Wayland | Wayland with `swayidle` | Other Linux sessions |
| --- | --- | --- | --- | --- |
| TV control at boot, shutdown, sleep, and wake | ✅ | ✅ | ✅ | ✅ |
| Idle blank and activity restore | ✅ | ✅ | ✅ | ❌ |
| Gamepad activity keeps the panel awake | ✅ | ✅ | ❌ | ❌ |
| Brightness, volume, settings, and update commands | ✅ | ✅ | ✅ | ✅ |
| Brightness desktop dialog | ✅ | ✅ | ✅ | ✅ |

The default `auto` backend prefers a complete GNOME session, then native
Wayland when the compositor provides `ext_idle_notifier_v1` version 2 or newer
and at least one seat. It uses `swayidle` only as a deprecated compatibility
fallback when native monitoring is unavailable. Inspect the decision with:

```bash
lg-buddy settings describe screen.backend
```

See the [user guide](docs/user-guide.md#automatic-screen-blanking) for backend
selection and troubleshooting. Protocol and event details are documented in the
[session backend model](docs/session-backend-model.md).

## Before You Install

Fresh installation selects the native `lg_webos` control path, which does not
require Python. Native-only packages can omit the Python client, `venv`, and
`pip`. The release-bundle installer still provisions `bscpylgtv` as an explicit
compatibility fallback, so it checks for Python 3 with a `venv` that provisions
`pip`, plus `zenity`. Official release bundles target the Ubuntu 24.04 runtime
baseline: GTK 4.14, libadwaita 1.5, and glibc 2.39 or newer. The installer
verifies that the GUI executable can load and has the same release identity as
the runtime before changing the installation.
Zenity remains the compatibility fallback when the GUI executable is absent.
`swayidle` is needed only by an existing explicit selection or as the deprecated
compatibility fallback.

### Debian, Ubuntu, and Pop!_OS

```bash
sudo apt install python3-venv python3-pip zenity libgtk-4-1 libadwaita-1-0
# Deprecated compatibility fallback only:
sudo apt install swayidle
```

### Fedora

```bash
sudo dnf install python3 python3-pip python3-virtualenv zenity gtk4 libadwaita
# Deprecated compatibility fallback only:
sudo dnf install swayidle
```

### Arch Linux

```bash
sudo pacman -S python python-pip python-virtualenv zenity gtk4 libadwaita
# Deprecated compatibility fallback only:
sudo pacman -S swayidle
```

Source builds also require a Rust toolchain and a working C toolchain because
the vendored D-Bus library is compiled during the build. See the
[development guide](docs/development.md) for build instructions.

## Install

1. Download and extract the release archive for your platform.
2. Run the installer as your regular user:

```bash
chmod +x ./install.sh
./install.sh
```

Do not run the installer with `sudo`; it requests elevated access when needed.

The installer asks for the TV's IP address, MAC address, HDMI input, control
platform, and desktop idle preferences, then installs the required services.

Fresh setup defaults to the native `lg_webos` platform and verifies pairing
before saving the configuration, so accept the prompt on the TV. You can
instead select the explicit `bscpylgtv` compatibility fallback; its prompt may
appear on first use. See the
[bscpylgtv first-use guide](https://github.com/chros73/bscpylgtv/blob/master/docs/guides/first_use.md).

To check, verify, and install the next release from your saved update channel,
run `lg-buddy updates install` as your regular user. It checks host
compatibility, shows the exact target identity, asks for explicit confirmation,
and then runs the verified bundle's upgrade installer. Upgrade mode preserves
configuration and credentials and does not repeat setup or pairing;
incompatible and legacy layouts are refused rather than migrated.

`v1.4.0-beta.2` is the first release with `updates install`; older versions
need one normal manual release-bundle installation before assisted upgrades are
available.

The shell installer targets conventional Linux installations with mutable
system locations. First-class NixOS packaging is tracked in
[issue #24](https://github.com/Staphylococcus/LG_Buddy/issues/24).

## Quick Start

LG Buddy's services run automatically after installation. Common commands are:

```bash
lg-buddy power on
lg-buddy power off
lg-buddy screen off
lg-buddy screen on
lg-buddy brightness
lg-buddy brightness get
lg-buddy brightness set 65
lg-buddy volume
lg-buddy volume 20
lg-buddy volume up
lg-buddy volume mute
lg-buddy settings list
lg-buddy settings describe screen.backend
lg-buddy updates check
lg-buddy updates install
lg-buddy --version
```

Change an individual setting with:

```bash
lg-buddy settings set <key> <value>
```

For example:

```bash
lg-buddy settings set screen.idle_timeout 600
lg-buddy settings set screen.restore_policy aggressive
lg-buddy settings set updates.auto_check disabled
```

Run `lg-buddy <command> --help` for scoped syntax. To revisit the complete
interactive setup, run `./configure.sh`.

The [user guide](docs/user-guide.md) covers all settings, desktop backends, TV
platform selection, updates, service checks, and uninstalling.

## Documentation

- [User guide](docs/user-guide.md)
- [Development guide](docs/development.md)
- [Architecture overview](docs/architecture-overview.md)
- [Session backend model](docs/session-backend-model.md)
- [Gamepad activity subsystem](docs/gamepad-subsystem.md)
- [Defaults and configuration](docs/defaults-and-configuration.md)
- [Runtime event handler map](docs/runtime-event-handler-map.md)
- [Contributing](CONTRIBUTING.md)
- [Release process](docs/release-process.md)

## Credits

- [chros73](https://github.com/chros73) for `bscpylgtv`
- [JPersson77](https://github.com/JPersson77) for the original inspiration
- [Faceless3882](https://github.com/Faceless3882) for the original shell script
  implementation
