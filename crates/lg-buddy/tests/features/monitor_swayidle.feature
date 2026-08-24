Feature: swayidle monitor
  LG Buddy should translate delegated swayidle hooks into TV behavior through the monitor loop.

  Scenario: swayidle timeout blanks the configured TV input
    Given a temporary LG Buddy config using input HDMI_2
    And LG Buddy session runtime is isolated
    And a mock TV client
    And the TV is on input HDMI_2
    And the executable PATH is isolated
    And swayidle is installed
    And the backend override is "swayidle"
    And swayidle will emit an idle timeout
    When I run the command "monitor"
    Then the command succeeds
    And the TV client received "get_input"
    And the TV client received "turn_screen_off"
    And the session marker exists
    And the TV screen is blanked

  Scenario: swayidle resume restores a previously blanked TV output
    Given a temporary LG Buddy config using input HDMI_3
    And LG Buddy session runtime is isolated
    And a mock TV client
    And the TV is on input HDMI_3
    And the TV screen is blanked
    And the session marker exists
    And the executable PATH is isolated
    And swayidle is installed
    And the backend override is "swayidle"
    And swayidle will emit a resume event
    When I run the command "monitor"
    Then the command succeeds
    And the TV client received "turn_screen_on"
    And the session marker is absent
    And the TV screen is visible

  Scenario: swayidle resume wakes a TV that was manually powered off after LG Buddy blanked it
    Given a temporary LG Buddy config using input HDMI_3
    And LG Buddy session runtime is isolated
    And a mock TV client
    And the TV is on input HDMI_3
    And the TV screen is blanked
    And the TV is powered off
    And the session marker exists
    And the next input restore attempt powers the TV back on
    And screen wake delays are disabled
    And the executable PATH is isolated
    And swayidle is installed
    And the backend override is "swayidle"
    And swayidle will emit a resume event
    When I run the command "monitor"
    Then the command succeeds
    And stdout contains "Sending initial Wake-on-LAN packet"
    And stdout contains "Wake attempt 1 succeeded."
    And the TV client received "turn_screen_on"
    And the TV client received "set_input"
    And the session marker is absent
    And the TV is powered on
    And the TV screen is visible
