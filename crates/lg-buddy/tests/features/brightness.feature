Feature: Brightness
  LG Buddy should provide a manual OLED brightness control for the configured TV.

  Scenario: Brightness sets the TV OLED brightness when the TV is reachable
    Given a temporary LG Buddy config using input HDMI_2
    And a mock TV client
    And the TV backlight is 72
    And the TV is reachable over ping
    And the brightness dialog returns 65
    When I run the command "brightness"
    Then the command succeeds
    And stdout contains "Set OLED pixel brightness to 65%."
    And the TV client received "get_picture_settings"
    And the TV client received "set_settings"
    And the TV brightness is 65

  Scenario: Brightness exits cleanly when the dialog is cancelled
    Given a temporary LG Buddy config using input HDMI_2
    And a mock TV client
    And the TV backlight is 44
    And the TV is reachable over ping
    And the brightness dialog is cancelled
    When I run the command "brightness"
    Then the command succeeds
    And the TV client received "get_picture_settings"
    And the TV client did not receive "set_settings"
    And the TV brightness is 44

  Scenario: Brightness get prints the current OLED brightness
    Given a temporary LG Buddy config using input HDMI_2
    And a mock TV client
    And the TV backlight is 58
    When I run the command "brightness get"
    Then the command succeeds
    And stdout is "58"
    And the TV client received "get_picture_settings"
    And the TV client did not receive "set_settings"

  Scenario: Brightness set updates OLED brightness without opening a dialog
    Given a temporary LG Buddy config using input HDMI_2
    And a mock TV client
    And the TV backlight is 44
    When I run the command "brightness set 66"
    Then the command succeeds
    And stdout contains "Set OLED pixel brightness to 66%."
    And the TV client received "set_settings"
    And the TV brightness is 66

  Scenario: Brightness set rejects invalid values before touching the TV
    Given a temporary LG Buddy config using input HDMI_2
    And a mock TV client
    And the TV backlight is 44
    When I run the command "brightness set 101"
    Then the command fails
    And the command exits with status 2
    And stderr contains "invalid OLED brightness"
    And stderr contains "brightness set <0-100>"
    And the TV client did not receive "get_picture_settings"
    And the TV client did not receive "set_settings"
    And the TV brightness is 44

  Scenario: Brightness fails when the TV is unreachable
    Given a temporary LG Buddy config using input HDMI_2
    And a mock TV client
    And the TV is unreachable over ping
    And the brightness error dialog is available
    When I run the command "brightness"
    Then the command fails
    And the command exits with status 1
    And stderr contains "TV is not reachable"
    And the TV client did not receive "set_settings"

  Scenario: Brightness help describes the public commands
    When I run the command "brightness --help"
    Then the command succeeds
    And stdout contains "brightness get"
    And stdout contains "brightness set <0-100>"

  Scenario: Global help exposes the brightness family
    When I run the command "--help"
    Then the command succeeds
    And stdout contains "brightness"
    And stdout contains "brightness get"
    And stdout contains "brightness set <0-100>"

  Scenario: Invalid brightness commands show scoped usage
    When I run the command "brightness show"
    Then the command fails
    And the command exits with status 2
    And stderr contains "unknown brightness command `show`"
    And stderr contains "brightness get"
    And stderr contains "brightness set <0-100>"
