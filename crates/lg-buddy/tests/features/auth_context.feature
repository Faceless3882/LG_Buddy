Feature: TV auth context
  LG Buddy should derive one consistent user-owned auth context for TV helper calls.

  Scenario: Screen off uses the config-owned auth context by default
    Given a temporary LG Buddy config using input HDMI_2
    And LG Buddy session runtime is isolated
    And a mock TV client
    And the inherited user environment is cleared
    And the TV is on input HDMI_2
    When I run the command "screen off"
    Then the command succeeds
    And the TV helper uses the expected auth context

  Scenario: sleep-pre honors an explicit shared auth key file override
    Given a temporary LG Buddy config using input HDMI_3
    And LG Buddy session runtime is isolated
    And a mock TV client
    And the inherited user environment is cleared
    And the TV is on input HDMI_3
    And sleep retry delays are disabled
    And the TV auth key file override is "shared-auth.sqlite"
    When I run the command "sleep-pre"
    Then the command succeeds
    And the TV helper uses the expected auth context
