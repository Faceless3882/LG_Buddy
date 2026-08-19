Feature: Native webOS TV platform
  LG Buddy should expose the same product behavior through its native webOS platform,
  while pairing only from an explicit foreground settings command.

  Scenario: Opting in pairs the TV and the stored token authenticates later commands
    Given a temporary LG Buddy config using input HDMI_2
    And a native webOS TV on input HDMI_3 with brightness 100
    When I run the command "settings set tv.platform lg_webos"
    Then the command succeeds
    And stdout contains "pairing required; accept the prompt on the TV"
    And stdout contains "stored access token"
    And stdout contains "native webOS preflight succeeded: power_state=Active"
    And config.env contains "tvs_primary_platform=lg_webos"
    And a valid native TV access token is stored
    And the native TV registration tokens are "none"
    When I run the command "brightness get"
    Then the command succeeds
    And stdout is "100"
    And the native TV registration tokens are "none,webos-test-access-token"
    And the native TV connection count is 2
    And the native TV pairing prompt count is 1

  Scenario: A missing token fails before a background command connects to the TV
    Given a temporary LG Buddy config using input HDMI_2
    And the existing config selects TV platform "lg_webos"
    And a native webOS TV on input HDMI_2 with brightness 90
    When I run the command "brightness get"
    Then the command fails
    And stderr contains "no stored platform access token is available"
    And stderr contains "settings set tv.platform lg_webos"
    And no native TV access token is stored
    And the native TV connection count is 0
    And the native TV registration tokens are ""

  Scenario: A stale token fails without background re-pairing or credential replacement
    Given a temporary LG Buddy config using input HDMI_2
    And the existing config selects TV platform "lg_webos"
    And a native webOS TV on input HDMI_2 with brightness 90
    And a stale native TV access token is stored
    When I run the command "brightness get"
    Then the command fails
    And stderr contains "requires foreground pairing"
    And stderr contains "settings set tv.platform lg_webos"
    And the native TV access token is "stale-cucumber-access-token"
    And the native TV connection count is 1
    And the native TV registration tokens are "stale-cucumber-access-token"
    And the native TV pairing prompt count is 1

  Scenario: Foreground opt-in repairs a stale token before persisting the platform
    Given a temporary LG Buddy config using input HDMI_2
    And the existing config selects TV platform "bscpylgtv"
    And a native webOS TV on input HDMI_2 with brightness 90
    And a stale native TV access token is stored
    When I run the command "settings set tv.platform lg_webos"
    Then the command succeeds
    And stdout contains "using stored access token"
    And stdout contains "pairing required; accept the prompt on the TV"
    And stdout contains "native webOS preflight succeeded: power_state=Active"
    And config.env contains "tvs_primary_platform=lg_webos"
    And a valid native TV access token is stored
    And the native TV connection count is 2
    And the native TV registration tokens are "stale-cucumber-access-token,none"
    And the native TV pairing prompt count is 2

  Scenario: Rejected foreground pairing leaves the platform and credentials unchanged
    Given a temporary LG Buddy config using input HDMI_2
    And a native webOS TV on input HDMI_2 with brightness 90
    And the native webOS TV rejects pairing
    And the current config is remembered
    When I run the command "settings set tv.platform lg_webos"
    Then the command fails
    And stderr contains "webOS pairing was rejected: pairing denied"
    And config.env is unchanged
    And no native TV access token is stored
    And the native TV connection count is 1
    And the native TV registration tokens are "none"

  Scenario: Native brightness writes use the observed signed protocol and update the TV
    Given a temporary LG Buddy config using input HDMI_2
    And the existing config selects TV platform "lg_webos"
    And a native webOS TV on input HDMI_2 with brightness 90
    And a valid native TV access token is stored
    When I run the command "brightness set 66"
    Then the command succeeds
    And stdout contains "Set OLED pixel brightness to 66%."
    And the TV brightness is 66
    And the native TV registration tokens are "webos-test-access-token"
    And the native TV pairing prompt count is 0

  Scenario: Native screen blanking and restoration preserve screen ownership
    Given a temporary LG Buddy config using input HDMI_2
    And the existing config selects TV platform "lg_webos"
    And LG Buddy session runtime is isolated
    And a native webOS TV on input HDMI_2 with brightness 90
    And a valid native TV access token is stored
    When I run the command "screen-off"
    Then the command succeeds
    And the session marker exists
    And the TV screen is blanked
    And the native TV registration tokens are "webos-test-access-token"
    And the native TV pairing prompt count is 0
    When I run the command "screen-on"
    Then the command succeeds
    And the session marker is absent
    And the TV screen is visible
    And the native TV registration tokens are "webos-test-access-token,webos-test-access-token"
    And the native TV pairing prompt count is 0

  Scenario: Native startup input restoration is followed by native shutdown power-off
    Given a temporary LG Buddy config using input HDMI_2
    And the existing config selects TV platform "lg_webos"
    And LG Buddy session runtime is isolated
    And a native webOS TV on input HDMI_3 with brightness 100
    And a valid native TV access token is stored
    And nm-online succeeds
    And startup delays are disabled
    When I run the command "startup boot"
    Then the command succeeds
    And stdout contains "TV turned on and set to HDMI_2."
    And the TV input is HDMI_2
    And the native TV registration tokens are "webos-test-access-token"
    And the native TV pairing prompt count is 0
    Given reboot detection reports no pending reboot
    When I run the command "shutdown"
    Then the command succeeds
    And stdout contains "TV is on HDMI_2. Turning off for shutdown."
    And the TV is powered off
    And the native TV registration tokens are "webos-test-access-token,webos-test-access-token"
    And the native TV pairing prompt count is 0

  Scenario: Native pre-sleep handling powers off an owned TV without background pairing
    Given a temporary LG Buddy config using input HDMI_2
    And the existing config selects TV platform "lg_webos"
    And LG Buddy session runtime is isolated
    And a native webOS TV on input HDMI_2 with brightness 90
    And a valid native TV access token is stored
    And sleep retry delays are disabled
    When I run the command "sleep-pre"
    Then the command succeeds
    And stdout contains "TV is on HDMI_2. Turning off for sleep."
    And the system marker exists
    And the TV is powered off
    And the native TV registration tokens are "webos-test-access-token"
    And the native TV pairing prompt count is 0
