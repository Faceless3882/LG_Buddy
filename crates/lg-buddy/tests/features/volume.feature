Feature: Volume
  LG Buddy should expose predictable volume and mute controls for the configured TV.

  Background:
    Given a temporary LG Buddy config using input HDMI_2
    And a mock TV client

  Scenario: Volume prints the current numeric level
    Given the TV volume is 37
    And the TV is unmuted
    When I run the command "volume"
    Then the command succeeds
    And stdout is "37"
    And the TV client received "get_audio_status" exactly 1 times

  Scenario: Volume prints mute instead of the numeric level while muted
    Given the TV volume is 37
    And the TV is muted
    When I run the command "volume"
    Then the command succeeds
    And stdout is "mute"

  Scenario: Volume preserves a TV-reported unknown level
    Given the TV volume is unknown
    And the TV is unmuted
    When I run the command "volume"
    Then the command succeeds
    And stdout is "unknown"

  Scenario: Setting volume also unmutes the TV
    Given the TV volume is 20
    And the TV is muted
    When I run the command "volume 42"
    Then the command succeeds
    And stdout contains "Set volume to 42."
    And the TV volume is 42
    And the TV is unmuted
    And the TV client received "set_volume" exactly 1 times
    And the TV client received "set_mute" exactly 1 times

  Scenario: Stepping volume in either direction also unmutes the TV
    Given the TV volume is 20
    And the TV is muted
    When I run the command "volume up"
    Then the command succeeds
    And the TV volume is 21
    And the TV is unmuted
    Given the TV is muted
    When I run the command "volume down"
    Then the command succeeds
    And the TV volume is 20
    And the TV is unmuted

  Scenario: Mute can be toggled or set explicitly
    Given the TV is unmuted
    When I run the command "volume mute"
    Then the command succeeds
    And the TV is muted
    And the TV client received "get_audio_status" exactly 1 times
    And the TV client received "set_mute" exactly 1 times
    When I run the command "volume mute off"
    Then the command succeeds
    And the TV is unmuted
    And the TV client received "get_audio_status" exactly 1 times
    And the TV client received "set_mute" exactly 2 times
    When I run the command "volume mute on"
    Then the command succeeds
    And the TV is muted
    And the TV client received "get_audio_status" exactly 1 times
    And the TV client received "set_mute" exactly 3 times

  Scenario: Invalid volume is rejected before touching the TV
    Given the TV volume is 20
    When I run the command "volume 101"
    Then the command fails
    And the command exits with status 2
    And stderr contains "invalid volume"
    And stderr contains "volume <0-100>"
    And the TV client did not receive "set_volume"
    And the TV volume is 20

  Scenario: An unmute failure does not replay a successful volume change
    Given the TV volume is 20
    And the TV is muted
    And the TV will fail "set_mute" with status 1 and stderr "mute rejected"
    When I run the command "volume up"
    Then the command fails
    And stderr contains "volume was changed, but unmuting failed"
    And the TV client received "volume_up" exactly 1 times
    And the TV client received "set_mute" exactly 1 times
    And the TV volume is 21
    And the TV is muted

  Scenario: Native webOS exposes the same volume and mute behavior
    Given the existing config selects TV platform "lg_webos"
    And a native webOS TV on input HDMI_2 with brightness 90
    And a valid native TV access token is stored
    When I run the command "volume"
    Then the command succeeds
    And stdout is "20"
    When I run the command "volume mute on"
    Then the command succeeds
    And the TV is muted
    When I run the command "volume up"
    Then the command succeeds
    And the TV volume is 21
    And the TV is unmuted
    When I run the command "volume 19"
    Then the command succeeds
    And the TV volume is 19
    And the TV is unmuted
    Given the TV volume is unknown
    When I run the command "volume"
    Then the command succeeds
    And stdout is "unknown"

  Scenario: Native webOS does not replay volume when unmuting is rejected
    Given the existing config selects TV platform "lg_webos"
    And a native webOS TV on input HDMI_2 with brightness 90
    And a valid native TV access token is stored
    And the TV volume is 20
    And the TV is muted
    And the native webOS TV rejects mute changes
    When I run the command "volume up"
    Then the command fails
    And stderr contains "volume was changed, but unmuting failed"
    And the TV volume is 21
    And the TV is muted
