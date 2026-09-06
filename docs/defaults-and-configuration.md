# LG Buddy Defaults And Configuration

This document defines how LG Buddy should choose defaults and expose advanced
behavior.

The rule is: default installs should be useful without asking users to make
policy decisions during installation. Advanced behavior should be controlled by
documented values in the existing `config.env` file, but only when the
configuration point is worth its long-term cost.

## Product Stance

LG Buddy should prefer sensible defaults over installer prompts.

LG Buddy should also prefer fewer configuration options over broad
configurability. Every option becomes maintenance debt: it expands the behavior
matrix, increases regression risk, complicates documentation, and makes support
harder. A config key should exist only when the non-default behavior is a real
user need and the project is willing to preserve and test that behavior.

Installer prompts are appropriate for required facts that cannot be inferred,
such as the TV IP address, TV MAC address, configured HDMI input, or an explicit
path to a locally built runtime binary.

Installer prompts are not the right surface for policy choices such as:

- whether lifecycle automation should run
- whether restore behavior should be conservative or aggressive
- which future optional event sources should participate in monitor behavior

Those choices should have documented defaults and be tunable after installation.
They should become configurable only when the project intentionally accepts the
extra behavior surface.

## Configuration Surface

The primary persistent configuration surface is `config.env`.

The structured settings CLI is an access layer over that same file, not a second
storage location. A dotted setting key such as `screen.restore_policy` maps to
the existing `config.env` key `screen_restore_policy`. This keeps manual config
editing, `configure.sh`, installer preservation, and `lg-buddy settings` on one
durable source of truth.

The settings CLI should not hide malformed durable config. If a raw
`config.env` value fails validation, `settings list` and `settings describe`
should show it as invalid, and `settings get <key>` should return the validation
error. Mutation commands may still overwrite or unset the bad value so users can
repair the file through the structured interface.

The current public settings interface exposes one TV through `tv.ip`, `tv.mac`,
`tv.input`, and `tv.platform`. New writes store those values as
`tvs_primary_ip`, `tvs_primary_mac`, `tvs_primary_input`, and
`tvs_primary_platform`. The profile-shaped storage is only an extensibility
point; LG Buddy does not currently expose multiple TVs or TV profile selection.
Existing single-TV keys `tv_ip`, `tv_mac`, and `input` remain readable for
compatibility.

New behavior should use a config key when users may reasonably want to keep a
non-default choice across reinstalls or upgrades. Prefer enum-shaped values over
multiple booleans when the setting describes a mode.

Before adding a config key, answer:

- what real user need requires this to be configurable?
- can a better default remove the need for configuration?
- how many behavior combinations does this add?
- what tests will cover each supported mode?
- can this be documented clearly without exposing implementation details?

If those answers are weak, keep the behavior fixed and improve the default
instead.

Good shapes:

```ini
tvs_primary_ip=192.168.1.100
tvs_primary_mac=aa:bb:cc:dd:ee:ff
tvs_primary_input=HDMI_2
tvs_primary_platform=lg_webos
screen_restore_policy=conservative
screen_idle_blank=enabled
system_sleep_wake_policy=enabled
updates_auto_check=enabled
updates_channel=stable
```

Avoid adding installer-only state for product behavior. Environment variables
can still be useful for tests, release-bundle smoke checks, and non-interactive
packaging flows, but they should write or preserve `config.env` when they
represent durable user preference.

## Installer Behavior

The installer should:

- ask only for required setup facts
- write defaults for policy settings when a config file is created
- preserve existing valid config values on reinstall
- avoid asking policy questions that have sensible defaults
- document advanced changes instead of presenting them as install-time choices

When migrating old behavior to a new runtime path, the installer should clean up
legacy files and services that would conflict with the new default behavior. If
a user has opted out through config, the installer should honor that persisted
choice.

## Current Examples

`tvs_primary_platform` selects the control platform for the active TV profile:

- fresh profiles select `lg_webos` and pair before the configuration is saved
- existing profiles keep their explicit platform; a missing platform continues
  to resolve to `bscpylgtv` and is materialized as such when configuration is
  rewritten
- `bscpylgtv` remains an accepted explicit compatibility fallback
- existing users can opt into native control with
  `lg-buddy settings set tv.platform lg_webos`; ordinary foreground TV
  operations can also pair or repair credentials when needed
- shutdown, suspend, resume, startup, and network-teardown handling use stored
  credentials only and skip promptly when no credential is available
- this is the only platform selector; there is no separate backend, adapter,
  or migration override

`screen_restore_policy` follows this model:

- `conservative` is the default
- `aggressive` is available for users who want LG Buddy to reclaim the TV more
  assertively
- the choice lives in `config.env`
- it is writeable through `lg-buddy settings set screen.restore_policy <value>`
  because the command can apply screen-monitor changes

`screen_idle_blank` follows the same policy model:

- automatic session idle blank/restore defaults to enabled
- in supported environments, a reported session lock also requests the same
  immediate blank policy; unlock never requests restore
- a lock-triggered blank ignores desktop and gamepad activity for a fixed one
  second before accepting activity-driven restore; this is a safety constant,
  not a user setting
- users who want update notifications without session-driven TV control can set
  `screen_idle_blank=disabled`
- the installed user-session service still runs so notification handoff remains
  available
- the supported values are `enabled` and `disabled`
- there is no separate session-lock setting

`system_sleep_wake_policy` follows the same model:

- automatic system sleep/wake handling should default to enabled
- users who do not want it should opt out through `config.env` or
  `lg-buddy settings set system.sleep_wake_policy disabled`
- the installer should not ask every user whether sleep/wake automation should
  be enabled
- the supported values are `enabled` and `disabled`
- lifecycle service and NetworkManager hook installation are integration
  topology, while this setting controls runtime policy

`updates_auto_check` follows the same opt-out model:

- automatic update checks should default to enabled
- background checks should be low-frequency, currently weekly with randomized
  delay
- the installer should not ask every user whether update checks should run
- users who do not want background checks should opt out through `config.env` or
  `lg-buddy settings set updates.auto_check disabled`
- disabling automatic checks disables/stops the user timer instead of merely
  suppressing notifications after doing the work

`updates_channel` is the single release-channel preference for update
operations:

- `stable` is the default for manual and background checks
- `prerelease` explicitly opts all checks into considering both prerelease and
  stable releases
- the installed binary's own release channel does not override this preference
