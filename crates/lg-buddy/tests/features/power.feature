Feature: Power
  LG Buddy should expose manual TV power control through the public CLI.

  Scenario: Power on restores the configured input using cold-boot behavior
    Given a temporary LG Buddy config using input HDMI_2
    And LG Buddy session runtime is isolated
    And a mock TV client
    And nm-online succeeds
    And startup delays are disabled
    When I run the command "power on"
    Then the command succeeds
    And nm-online was invoked with "-q -t 60"
    And the TV input is HDMI_2
    And the TV client received "set_input"

  Scenario: Power off uses shutdown ownership behavior
    Given a temporary LG Buddy config using input HDMI_3
    And a mock TV client
    And the TV is on input HDMI_3
    And reboot detection reports no pending reboot
    When I run the command "power off"
    Then the command succeeds
    And the TV client received "get_input"
    And the TV client received "power_off"
    And the TV is powered off

  Scenario: Power help describes the public commands
    When I run the command "power --help"
    Then the command succeeds
    And stdout contains "power on"
    And stdout contains "power off"
    And stdout does not contain "startup"
    And stdout does not contain "shutdown"

  Scenario: Global help exposes power without lifecycle compatibility aliases
    When I run the command "--help"
    Then the command succeeds
    And stdout contains "power on"
    And stdout contains "power off"
    And stdout does not contain "startup [mode]"
    And stdout does not contain "shutdown        "

  Scenario: Invalid power commands show scoped usage
    When I run the command "power standby"
    Then the command fails
    And stderr contains "unknown power command `standby`"
    And stderr contains "power on"
    And stderr contains "power off"
