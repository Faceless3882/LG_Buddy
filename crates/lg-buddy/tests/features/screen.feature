Feature: Screen
  LG Buddy should expose manual screen blanking and restoration through the public CLI.

  Scenario: Screen off blanks the owned TV output and records ownership
    Given a temporary LG Buddy config using input HDMI_2
    And LG Buddy session runtime is isolated
    And a mock TV client
    And the TV is on input HDMI_2
    When I run the command "screen off"
    Then the command succeeds
    And the TV client received "get_input"
    And the TV client received "turn_screen_off"
    And the TV screen is blanked
    And the session marker exists

  Scenario: Screen on restores an output previously blanked by LG Buddy
    Given a temporary LG Buddy config using input HDMI_2
    And LG Buddy session runtime is isolated
    And a mock TV client
    And the TV is on input HDMI_2
    And the TV screen is blanked
    And the session marker exists
    When I run the command "screen on"
    Then the command succeeds
    And the TV client received "turn_screen_on"
    And the TV screen is visible
    And the session marker is absent

  Scenario: Screen help describes the public commands
    When I run the command "screen --help"
    Then the command succeeds
    And stdout contains "screen off"
    And stdout contains "screen on"
    And stdout does not contain "screen-off"
    And stdout does not contain "screen-on"

  Scenario: Global help exposes screen without flat compatibility aliases
    When I run the command "--help"
    Then the command succeeds
    And stdout contains "screen off"
    And stdout contains "screen on"
    And stdout does not contain "screen-off"
    And stdout does not contain "screen-on"

  Scenario: Invalid screen commands show scoped usage
    When I run the command "screen toggle"
    Then the command fails
    And the command exits with status 2
    And stderr contains "unknown screen command `toggle`"
    And stderr contains "screen off"
    And stderr contains "screen on"

  Scenario: Flat screen compatibility aliases remain operational
    Given a temporary LG Buddy config using input HDMI_2
    And LG Buddy session runtime is isolated
    And a mock TV client
    And the TV is on input HDMI_2
    When I run the command "screen-off"
    Then the command succeeds
    And the TV screen is blanked
    And the session marker exists
    When I run the command "screen-on"
    Then the command succeeds
    And the TV screen is visible
    And the session marker is absent
