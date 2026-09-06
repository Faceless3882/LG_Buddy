# User Guide

This guide covers day-to-day LG Buddy use after installation. For implementation
details and package integration, see [Technical references](#technical-references).

## Common Commands

The installed command is:

```bash
lg-buddy <command>
```

Common commands include:

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
lg-buddy --version
```

Use `lg-buddy <command> --help` or `lg-buddy help <command>` for command-specific
syntax.

- `power on` wakes the TV and restores the configured input.
- `power off` powers off the TV when it is on the configured input and no reboot
  is pending.
- `screen off` blanks the TV output while remembering that LG Buddy blanked it.
- `screen on` restores the output according to the configured restore policy.
- `brightness` opens the GTK brightness window. If the GUI executable is absent
  from a transitional installation, LG Buddy uses the retained Zenity dialog.
  `brightness get` and `brightness set <0-100>` remain headless and read or
  change OLED brightness directly.
- `volume` prints the current level, `mute` when muted, or `unknown` when the TV
  does not expose a numeric level. `volume <0-100>`, `volume up`, and `volume
  down` change the volume and unmute the TV. `volume mute [on|off]` toggles or
  explicitly sets mute.
- `--version` reports the installed version and, for official builds, release
  metadata.

LG Buddy's services normally run automatically, so most users only need these
commands and the settings described below.

## Configuration

Run the configurator to change TV identity, control platform, idle behavior, or
installed service wiring:

```bash
./configure.sh
```

For individual changes, use the settings commands:

```bash
lg-buddy settings list
lg-buddy settings describe screen.restore_policy
lg-buddy settings get screen.idle_timeout
lg-buddy settings set tv.input HDMI_2
lg-buddy settings set screen.idle_timeout 600
lg-buddy settings unset screen.restore_policy
```

`settings describe` shows a setting's current value, default, accepted values,
and where the current value came from. `settings unset` restores the default
when the setting supports it.

Current settings are:

| Setting | Purpose |
| --- | --- |
| `tv.ip` | TV network address. |
| `tv.mac` | TV MAC address used for Wake-on-LAN. |
| `tv.input` | Input LG Buddy manages, such as `HDMI_2`. |
| `tv.platform` | TV control implementation: native `lg_webos` or the `bscpylgtv` compatibility fallback. |
| `screen.backend` | Desktop idle backend: `auto`, `gnome`, `wayland`, or deprecated compatibility value `swayidle`. |
| `screen.idle_blank` | Enable or disable automatic idle blanking. |
| `screen.idle_timeout` | Seconds of inactivity before blanking; defaults to 300. |
| `screen.restore_policy` | `conservative` or `aggressive` restore behavior. |
| `system.sleep_wake_policy` | Enable or disable TV handling around system sleep. |
| `updates.auto_check` | Enable or disable automatic update checks. |
| `updates.channel` | Check the `stable` or `prerelease` release channel. |

Changes that affect desktop monitoring automatically restart the user service
when it is installed and active or enabled. Update settings also apply to the
installed update timer.

LG Buddy stores these settings in `config.env`, normally at
`$XDG_CONFIG_HOME/lg-buddy/config.env`, or `~/.config/lg-buddy/config.env` when
`XDG_CONFIG_HOME` is unset. The settings CLI, configurator, installer, and
manual edits all use this file. Prefer the settings CLI for ordinary changes
because it validates values and applies any required service changes.

If a manual edit leaves an invalid value, `settings list` and
`settings describe` identify it. Repair it with `settings set`, `settings
unset`, or another manual edit.

## Automatic Screen Blanking

Automatic blanking is enabled by default. Change its main settings with:

```bash
lg-buddy settings set screen.idle_blank enabled
lg-buddy settings set screen.idle_timeout 600
lg-buddy settings set screen.restore_policy conservative
```

Set `screen.idle_blank` to `disabled` to stop idle-driven TV blanking and
restoring. The user service remains available for update notifications.

When an automatic idle or session-lock action successfully blanks the screen,
LG Buddy powers the TV off after five more minutes without activity. This grace
period is a fixed safety default rather than a setting. Any desktop or gamepad
activity cancels the pending power-off and restores the screen. Before powering
off, LG Buddy rechecks its ownership marker, the configured input, and machine
lifecycle state; it skips safely if those checks no longer permit the action.

The restore policies are:

- `conservative`: restore only when LG Buddy previously blanked or powered off
  the TV. This is the default.
- `aggressive`: also attempt to restore the TV on activity or system wake when
  no LG Buddy marker exists.

### Choosing a Desktop Backend

| Backend | When to use it |
| --- | --- |
| `auto` | Default. Prefers compatible GNOME, then compatible native Wayland, then the deprecated `swayidle` fallback when installed. |
| `gnome` | A GNOME Shell session with the required GNOME idle services. |
| `wayland` | Force native monitoring on a compositor that advertises `ext_idle_notifier_v1` version 2 or newer and at least one `wl_seat`. |
| `swayidle` | Deprecated compatibility backend for existing installations and older compositors. Fresh interactive configuration does not offer it. |

Select a backend persistently:

```bash
lg-buddy settings set screen.backend wayland
```

Return to automatic selection:

```bash
lg-buddy settings unset screen.backend
```

An explicitly selected backend reports a compatibility error rather than
silently switching to another backend. `auto` reports why it moved past GNOME
or native Wayland. Existing explicit `swayidle` selections remain valid and are
never silently rewritten, but emit a deprecation notice.

Check the selected backend and user service:

```bash
lg-buddy settings describe screen.backend
systemctl --user status LG_Buddy_screen.service
journalctl --user -u LG_Buddy_screen.service --since today
```

For `auto`, `settings describe` prints the configured selection, resolved
backend, and fallback reason separately. Unsupported native sessions report
the compositor connection or protocol limitation before using `swayidle` or
reporting that no backend is available.

The `swayidle` compatibility window lasts through the 1.x release line, with
removal planned for 2.0.0. Removal requires native Wayland monitoring to remain
field-validated on supported non-GNOME compositors, precise unsupported-session
diagnostics, and a released migration window in which existing configurations
continue to run without being rewritten.

### Gamepad Activity

With the `gnome` and `wayland` backends, supported controller activity resets
the same idle timer as desktop activity. On the deprecated `swayidle` backend,
controller activity can restore an already blanked screen and cancel its
pending timed power-off, but it does not reset swayidle's initial timeout. No
additional setting is required.

If controller activity is ignored, verify that the user running
`LG_Buddy_screen.service` can read the controller's Linux input devices. Normal
desktop sessions usually receive this access automatically through seat device
permissions. See [gamepad-subsystem.md](gamepad-subsystem.md) for supported
input paths and hardware troubleshooting.

## TV Control Platform

`tv.platform` selects the TV control implementation:

- `lg_webos`: the native Rust implementation and fresh-profile default.
- `bscpylgtv`: the explicit Python compatibility fallback.

Fresh configuration verifies native pairing before it saves the profile.
Existing profiles retain their selected platform. If an older profile has no
platform key, it continues to resolve to `bscpylgtv`; rewriting that profile
through the configurator materializes the compatibility choice instead of
silently moving it to native control.

Move an existing profile to the native implementation with:

```bash
lg-buddy settings set tv.platform lg_webos
```

LG Buddy connects to the TV and verifies the credential before saving this
choice. Accept the pairing prompt on the TV if one appears. Foreground TV
commands can pair or repair credentials when necessary; unattended startup,
shutdown, suspend, and resume handling use an existing credential and do not
open a pairing prompt.

Select and persist the compatibility fallback with:

```bash
lg-buddy settings set tv.platform bscpylgtv
```

`settings unset tv.platform` removes the explicit choice and therefore resolves
to `bscpylgtv` for legacy compatibility; it does not apply the fresh-profile
default.

For support and troubleshooting, inspect the effective platform, its source,
and accepted values with:

```bash
lg-buddy settings describe tv.platform
```

If native pairing or its power-state verification fails, setup leaves the
profile unsaved. Confirm that the TV is reachable and accept its pairing prompt,
then rerun configuration or `settings set tv.platform lg_webos`. Select
`bscpylgtv` explicitly if native control is not usable on that TV.

## System Sleep And Wake

Default installs power off the TV before system sleep and restore it after
wake. Disable this behavior without removing the integration:

```bash
lg-buddy settings set system.sleep_wake_policy disabled
```

Re-enable it with:

```bash
lg-buddy settings set system.sleep_wake_policy enabled
```

While system sleep is pending, desktop idle events do not issue competing TV
commands.

Check the lifecycle service with:

```bash
systemctl status LG_Buddy_lifecycle.service
journalctl -u LG_Buddy_lifecycle.service --since today
```

## Updates

Check for updates manually:

```bash
lg-buddy updates check
lg-buddy updates check --notify
lg-buddy settings set updates.channel prerelease
lg-buddy updates check
lg-buddy updates install
```

The saved `updates.channel` setting controls every check, regardless of the
installed binary's own release channel. `stable` checks stable releases only;
`prerelease` accepts GitHub's newest published stable or prerelease. Release
promotion requires every version to advance both release-channel heads, so the
newest published release is also the highest semantic version.

`updates install` is an assisted, foreground upgrade. It checks whether the
current host and installation are safely upgradeable before discovery, shows
the current and target version/channel/commit, and requires you to type `yes`
in a terminal before downloading the release bundle. It then verifies the
bundle, reruns preflight from the candidate, invokes `install.sh --upgrade`,
and verifies the installed release identity. It does not accept channel or
version arguments, downgrade, migrate legacy installations, or run unattended.

`v1.4.0-beta.2` is the first release that contains `updates install`. Older
installations require one normal manual installation of an updater-capable
release before this assisted path is available.

`--notify` sends a desktop notification through the running user service. When
supported by the desktop, the notification includes actions to open the release
or disable future automatic notifications. LG Buddy does not repeatedly notify
for the same release.

Control scheduled checks with:

```bash
lg-buddy settings set updates.auto_check disabled
lg-buddy settings set updates.auto_check enabled
lg-buddy settings set updates.channel prerelease
```

Disabling automatic checks does not disable manual `updates check` or
`updates install` commands. Both use the saved `updates.channel` setting.

## Technical References

These documents cover details intentionally omitted from this user guide:

- [Session backend model](session-backend-model.md): source observations,
  event semantics, protocol requirements, and idle-timeout ownership.
- [Gamepad activity subsystem](gamepad-subsystem.md): device discovery,
  permissions, adapters, and hardware testing.
- [Defaults and configuration](defaults-and-configuration.md): configuration
  storage, defaults, compatibility, and installer policy.
- [Runtime event handler map](runtime-event-handler-map.md): service
  entrypoints and lifecycle event routing.
- [Architecture overview](architecture-overview.md): runtime boundaries and
  TV integration architecture.
- [Development guide](development.md): building, installing, and validating a
  local binary.

## Uninstall

To remove LG Buddy:

```bash
chmod +x ./uninstall.sh
./uninstall.sh
```

This removes the installed services, desktop entry, Rust runtime binary, and
Python TV-control environment. If you choose to remove user configuration, it
also removes the config file and profile-scoped native TV credentials.
