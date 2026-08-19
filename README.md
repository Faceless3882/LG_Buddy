# LG Buddy

Inspired by [LGTV Companion for Windows](https://github.com/JPersson77/LGTVCompanion), LG Buddy makes an LG WebOS TV behave more like a monitor for a Linux PC.

It can:

- turn the TV on at boot and wake
- turn the TV off at shutdown and before system sleep
- blank and restore the panel on supported desktop idle backends
- keep the panel awake from gamepad activity on GNOME
- adjust OLED pixel brightness with a small desktop dialog or CLI command

GNOME is not required. Official release bundles include a prebuilt `lg-buddy`
binary, so normal installation does not require a Rust toolchain.

If you build `lg-buddy` from source instead of using a release bundle, `cargo`
now also needs a working C toolchain because the vendored `libdbus` runtime is
compiled as part of the build.

## Desktop Compatibility

| Functionality | GNOME | Non-GNOME Wayland with `swayidle` | Other Linux sessions | Notes |
| --- | --- | --- | --- | --- |
| Turn TV on at boot and wake | ✅ | ✅ | ✅ | Desktop-independent Linux lifecycle integration |
| Turn TV off at shutdown and before system sleep | ✅ | ✅ | ✅ | Desktop-independent Linux lifecycle integration |
| Blank and restore on desktop idle/activity | ✅ | ✅ | ❌ | `screen_backend=auto` prefers GNOME, then falls back to `swayidle` when installed |
| Keep the panel awake from gamepad activity | ✅ | ❌ | ❌ | Implemented in the GNOME monitor runtime |
| OLED brightness CLI | ✅ | ✅ | ✅ | Desktop-independent runtime command |
| OLED brightness desktop dialog | ✅ | ✅ | ✅ | Requires `zenity` |
| Settings CLI | ✅ | ✅ | ✅ | Desktop-independent runtime command |
| Manual and background update checks | ✅ | ✅ | ✅ | Desktop notifications depend on the session notification service and desktop notification support |

## Before You Install

Install prerequisites:

- `python3-venv`
- `python3-pip`
- `zenity`

Backend-specific:

- GNOME backend: compatible GNOME Shell session
- Non-GNOME Wayland backend: `swayidle`

The GNOME backend requires a compatible GNOME session with:

- GNOME Shell
- `org.gnome.ScreenSaver`
- `org.gnome.Mutter.IdleMonitor`

At runtime, GNOME support now uses a persistent in-process session-bus client
for shell detection, ScreenSaver signals, and Mutter idletime polling.
The GNOME monitor also observes readable Linux gamepad input devices so
controller activity can keep the TV output awake even when GNOME does not count
that input as desktop activity. It refreshes the watched device set when Linux
reports input-device add, remove, or change events, with a periodic
reconciliation scan as a fallback.

Typical package installs:

**Debian/Ubuntu/Pop!_OS**
```bash
sudo apt install python3-venv python3-pip zenity
# Optional for non-GNOME Wayland sessions:
sudo apt install swayidle
```

**Fedora**
```bash
sudo dnf install python3 python3-pip python3-virtualenv zenity
# Optional for non-GNOME Wayland sessions:
sudo dnf install swayidle
```

**Arch**
```bash
sudo pacman -S python python-pip python-virtualenv zenity
# Optional for non-GNOME Wayland sessions:
sudo pacman -S swayidle
```

For source builds, also install a C toolchain:

- Debian/Ubuntu/Pop!_OS: `build-essential`
- Fedora: `gcc`
- Arch: `base-devel`

## Install

1. Download the release archive for your platform.
2. Extract it.
3. Run:

```bash
chmod +x ./install.sh
./install.sh
```

The installer will prompt for your TV IP, MAC address, HDMI input, TV control platform, and session idle blanking details, then install the required services. If you choose the native `lg_webos` platform, accept the pairing prompt on the TV during setup. System sleep/wake handling uses the lifecycle service plus NetworkManager pre-down gate as cooperating suspend sources unless you opt out in `config.env`.

With the default `bscpylgtv` platform, you may instead see the pairing prompt on first use:

<https://github.com/chros73/bscpylgtv/blob/master/docs/guides/first_use.md>

## Day to Day

LG Buddy is mostly automatic after installation.

- To inspect settings, run `lg-buddy settings list`
- To change supported settings, use `lg-buddy settings set <key> <value>`
- To inspect the active TV platform, run `lg-buddy settings get tv.platform`
- To see the active desktop idle backend, run `lg-buddy detect-backend`
- To inspect TV brightness, run `lg-buddy brightness get`
- To set TV brightness directly, run `lg-buddy brightness set <0-100>`
- To inspect the installed runtime version, run `lg-buddy --version`
- To check GitHub releases on demand, run `lg-buddy updates check`; add
  `--notify` to send a desktop notification when an update is available
- Weekly background update checks are installed by default; opt out with
  `lg-buddy settings set updates.auto_check disabled`
- To rerun full setup for TV IP, MAC address, or HDMI input, run `./configure.sh`
- To check the user-session service, run `systemctl --user status LG_Buddy_screen.service`
- To remove LG Buddy, run `./uninstall.sh`

The settings CLI is a structured layer over `config.env`. These examples write
the same file that manual editing and `configure.sh` use:

```bash
lg-buddy settings describe tv.input
lg-buddy settings set tv.input HDMI_2
lg-buddy settings get tv.platform
lg-buddy settings set screen.idle_blank disabled
lg-buddy settings describe screen.restore_policy
lg-buddy settings set screen.idle_timeout 600
lg-buddy settings set screen.restore_policy aggressive
lg-buddy settings set system.sleep_wake_policy disabled
lg-buddy settings set updates.auto_check disabled
lg-buddy settings set updates.channel prerelease
lg-buddy settings unset screen.restore_policy
```

Settings can also be edited directly in `config.env`:

```ini
tvs_primary_ip=192.168.1.100
tvs_primary_mac=aa:bb:cc:dd:ee:ff
tvs_primary_input=HDMI_2
tvs_primary_platform=bscpylgtv
screen_idle_blank=enabled
screen_backend=auto
screen_idle_timeout=300
screen_restore_policy=conservative
system_sleep_wake_policy=enabled
updates_auto_check=enabled
updates_channel=stable
```

`tv_ip`, `tv_mac`, and `input` are still accepted as legacy single-TV keys, but
new writes use the `tvs_primary_*` shape so the storage can grow later without
changing the current single-TV settings interface.

The TV platform defaults to `bscpylgtv`, including when
`tvs_primary_platform` is absent from an existing profile. The experimental
native Rust webOS platform is an explicit opt-in:

```bash
lg-buddy settings set tv.platform lg_webos
```

Before saving that selection, LG Buddy connects in the foreground, reuses or
acquires the profile's native credential, and verifies it with a safe TV state
read. Accept any pairing prompt on the TV. If pairing or verification fails,
the previous platform remains selected. Background services never initiate
pairing. Switch back with `lg-buddy settings set tv.platform bscpylgtv`.

Use the settings command for native opt-in rather than editing
`tvs_primary_platform` directly, because a direct edit bypasses credential
preflight.

If a direct `config.env` edit leaves a value malformed, `lg-buddy settings list`
and `describe` show it as invalid instead of silently treating it as default or
missing. `lg-buddy settings get <key>` fails with the validation error so the
bad entry can be fixed with `settings set`, `settings unset` when supported, or
by editing `config.env`.

`screen_restore_policy=conservative` is the default. LG Buddy only restores when a matching LG Buddy marker says it previously blanked or powered off the TV.

Set `screen_restore_policy=aggressive` to let session wake/activity and system wake restore the TV even when no LG Buddy marker exists. This is intentionally more aggressive and can turn the TV on in cases where another device or a manual action powered it off.

`marker_only` is still accepted as a legacy alias for `conservative`.

`screen_idle_blank=enabled` is the default. Set
`screen_idle_blank=disabled` if you want the user-session service to stay
available for update notifications without running idle-driven TV blank/restore
behavior.

`system_sleep_wake_policy=enabled` is the default. Set
`system_sleep_wake_policy=disabled` if you do not want LG Buddy to control the
TV around system sleep and wake. The lifecycle service and NetworkManager
pre-down hook stay installed and no-op while the policy is disabled.

`updates_auto_check=enabled` is the default. Set
`updates_auto_check=disabled` if you do not want the installed user timer to
check for updates and notify you when a release is available. Manual
`lg-buddy updates check` commands still work when automatic checks are disabled.
Update notifications also include a `Never Notify Again` button when the
desktop notification service supports actions; it disables automatic update
checks through the same setting.
`updates_channel=stable` is the default for scheduled checks. Set it to
`prerelease` to opt in to prerelease update notifications.

## More Help

- [User guide](docs/user-guide.md)
- [Development](docs/development.md)
- [Defaults and configuration](docs/defaults-and-configuration.md)
- [Runtime event handler map](docs/runtime-event-handler-map.md)
- [Gamepad subsystem](docs/gamepad-subsystem.md)
- [Contributing](CONTRIBUTING.md)
- [Release process](docs/release-process.md)

## Credits

- <https://github.com/chros73> for `bscpylgtv`
- <https://github.com/JPersson77> for the original inspiration
- <https://github.com/Faceless3882> for the original shell script implementation
