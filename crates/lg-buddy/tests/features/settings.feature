Feature: Settings CLI
  LG Buddy should expose structured settings over the existing config.env file.

  Scenario: settings list shows values and supported operations
    Given a temporary LG Buddy config using input HDMI_2
    When I run the command "settings list"
    Then the command succeeds
    And stdout contains "tv.ip=192.0.2.42 (config.env, read-write, ops: get,describe,set)"
    And stdout contains "tv.mac=aa:bb:cc:dd:ee:ff (config.env, read-write, ops: get,describe,set)"
    And stdout contains "tv.input=HDMI_2 (config.env, read-write, ops: get,describe,set)"
    And stdout contains "tv.platform=bscpylgtv (default, read-write, ops: get,describe,set,unset)"
    And stdout contains "screen.backend=auto (config.env, read-write, ops: get,describe,set,unset)"
    And stdout contains "screen.idle_blank=enabled (default, read-write, ops: get,describe,set,unset)"
    And stdout contains "screen.restore_policy=conservative (default, read-write, ops: get,describe,set,unset)"
    And stdout contains "system.sleep_wake_policy=enabled (default, read-write, ops: get,describe,set,unset)"
    And stdout contains "updates.auto_check=enabled (default, read-write, ops: get,describe,set,unset)"
    And stdout contains "updates.channel=stable (default, read-write, ops: get,describe,set,unset)"

  Scenario: settings help describes the public commands
    When I run the command "settings --help"
    Then the command succeeds
    And stdout contains "settings list"
    And stdout contains "settings describe [KEY]"
    And stdout contains "settings get <KEY>"
    And stdout contains "settings set <KEY> <VALUE>"
    And stdout contains "settings unset <KEY>"

  Scenario: settings set help is scoped to the subcommand
    When I run the command "settings set --help"
    Then the command succeeds
    And stdout contains "settings set <KEY> <VALUE>"
    And stdout does not contain "settings list"

  Scenario: Invalid settings syntax shows scoped usage
    When I run the command "settings set screen.backend"
    Then the command fails
    And the command exits with status 2
    And stderr contains "missing setting value for `settings set`"
    And stderr contains "settings set <KEY> <VALUE>"
    And stderr does not contain "settings list"

  Scenario: Global help exposes settings without the backend diagnostic alias
    When I run the command "--help"
    Then the command succeeds
    And stdout contains "settings"
    And stdout does not contain "detect-backend"

  Scenario: settings describe annotates auto with the resolved GNOME backend
    Given a temporary LG Buddy config using input HDMI_2
    And GNOME Shell is available
    And the executable PATH is isolated
    When I run the command "settings describe screen.backend"
    Then the command succeeds
    And stdout contains "current: auto (gnome)"
    And stdout contains "allowed values: auto (gnome), gnome, swayidle"
    When I run the command "settings get screen.backend"
    Then the command succeeds
    And stdout is "auto"

  Scenario: settings describe annotates auto with the swayidle fallback
    Given a temporary LG Buddy config using input HDMI_2
    And the executable PATH is isolated
    And swayidle is installed
    When I run the command "settings describe screen.backend"
    Then the command succeeds
    And stdout contains "current: auto (swayidle)"
    And stdout contains "allowed values: auto (swayidle), gnome, swayidle"

  Scenario: settings describe remains available without a detected backend
    Given a temporary LG Buddy config using input HDMI_2
    And the executable PATH is isolated
    When I run the command "settings describe screen.backend"
    Then the command succeeds
    And stdout contains "current: auto (no backend currently available)"
    And stdout contains "allowed values: auto (no backend currently available), gnome, swayidle"

  Scenario: settings describe shows required TV operations
    Given a temporary LG Buddy config using input HDMI_2
    When I run the command "settings describe tv.input"
    Then the command succeeds
    And stdout contains "tv.input"
    And stdout contains "storage key: tvs_primary_input"
    And stdout contains "default: required"
    And stdout contains "supported operations: get, describe, set"

  Scenario: settings describe exposes the sole TV platform selector
    Given a temporary LG Buddy config using input HDMI_2
    When I run the command "settings describe tv.platform"
    Then the command succeeds
    And stdout contains "storage key: tvs_primary_platform"
    And stdout contains "default: bscpylgtv"
    And stdout contains "supported operations: get, describe, set, unset"
    And stdout contains "allowed values: bscpylgtv, lg_webos"

  Scenario: settings rejects an invalid TV platform without altering config
    Given a temporary LG Buddy config using input HDMI_2
    And the current config is remembered
    When I run the command "settings set tv.platform native"
    Then the command fails
    And stderr contains "invalid value for setting `tv.platform`"
    And config.env is unchanged

  Scenario: settings describe shows lifecycle policy operations
    Given a temporary LG Buddy config using input HDMI_2
    When I run the command "settings describe system.sleep_wake_policy"
    Then the command succeeds
    And stdout contains "system.sleep_wake_policy"
    And stdout contains "mutability: read-write"
    And stdout contains "supported operations: get, describe, set, unset"

  Scenario: settings describe shows idle blank policy operations
    Given a temporary LG Buddy config using input HDMI_2
    When I run the command "settings describe screen.idle_blank"
    Then the command succeeds
    And stdout contains "screen.idle_blank"
    And stdout contains "storage key: screen_idle_blank"
    And stdout contains "allowed values: enabled, disabled"
    And stdout contains "apply: restart-user-screen-service"

  Scenario: settings describe shows update check operations
    Given a temporary LG Buddy config using input HDMI_2
    When I run the command "settings describe updates.auto_check"
    Then the command succeeds
    And stdout contains "updates.auto_check"
    And stdout contains "storage key: updates_auto_check"
    And stdout contains "allowed values: enabled, disabled"
    And stdout contains "apply: manage-update-check-timer"

  Scenario: settings set writes config.env and reports skipped apply
    Given a temporary LG Buddy config using input HDMI_2
    And systemd apply actions are skipped
    When I run the command "settings set screen.idle_timeout 600"
    Then the command succeeds
    And stdout contains "screen.idle_timeout=600"
    And stdout contains "apply: skipped systemd apply"
    And config.env contains "screen_idle_timeout=600"

  Scenario: settings set writes TV settings to profile-shaped storage
    Given a temporary LG Buddy config using input HDMI_2
    And LG Buddy session runtime is isolated
    And a mock TV client
    And the TV is on input HDMI_3
    When I run the command "settings set tv.input HDMI_3"
    Then the command succeeds
    And stdout contains "tv.input=HDMI_3"
    And stdout contains "apply: no runtime apply action required"
    And config.env contains "tvs_primary_input=HDMI_3"
    And config.env does not contain "input=HDMI_2"
    When I run the command "screen off"
    Then the command succeeds
    And the TV client received "turn_screen_off"

  Scenario: settings set writes a restore policy consumed by screen runtime
    Given a temporary LG Buddy config using input HDMI_3
    And systemd apply actions are skipped
    And LG Buddy session runtime is isolated
    And a mock TV client
    And the TV is on input HDMI_3
    And the TV screen is blanked
    When I run the command "settings set screen.restore_policy aggressive"
    Then the command succeeds
    And config.env contains "screen_restore_policy=aggressive"
    When I run the command "screen on"
    Then the command succeeds
    And stdout contains "Aggressive restore policy is enabled"
    And the TV client received "turn_screen_on"
    And the session marker is absent

  Scenario: settings unset removes an override and restores the default
    Given a temporary LG Buddy config using input HDMI_2
    And the screen restore policy is "aggressive"
    And systemd apply actions are skipped
    When I run the command "settings unset screen.restore_policy"
    Then the command succeeds
    And stdout contains "screen.restore_policy unset"
    And config.env does not contain "screen_restore_policy="
    When I run the command "settings get screen.restore_policy"
    Then the command succeeds
    And stdout is "conservative"

  Scenario: settings set restarts an active user screen service
    Given a temporary LG Buddy config using input HDMI_2
    And the user screen service is active
    When I run the command "settings set screen.backend gnome"
    Then the command succeeds
    And stdout contains "apply: restarted LG_Buddy_screen.service"
    And stdout does not contain "Description=LG Buddy Screen Monitor Service"
    And config.env contains "screen_backend=gnome"
    And systemctl was invoked with "--user restart LG_Buddy_screen.service"

  Scenario: settings set writes lifecycle policy without restarting services
    Given a temporary LG Buddy config using input HDMI_2
    When I run the command "settings set system.sleep_wake_policy disabled"
    Then the command succeeds
    And stdout contains "system.sleep_wake_policy=disabled"
    And stdout contains "apply: no runtime apply action required"
    And config.env contains "system_sleep_wake_policy=disabled"

  Scenario: settings set writes update check opt out
    Given a temporary LG Buddy config using input HDMI_2
    And systemd apply actions are skipped
    When I run the command "settings set updates.auto_check disabled"
    Then the command succeeds
    And stdout contains "updates.auto_check=disabled"
    And stdout contains "apply: skipped systemd apply"
    And config.env contains "updates_auto_check=disabled"
