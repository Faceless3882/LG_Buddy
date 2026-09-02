Feature: Detect backend
  LG Buddy should resolve the correct screen backend from the environment it sees.

  Scenario: GNOME is preferred when available
    Given a temporary LG Buddy config using input HDMI_2
    And the executable PATH is isolated
    And GNOME Shell is available
    And swayidle is installed
    When I run the command "detect-backend"
    Then the command succeeds
    And stdout is "gnome"

  Scenario: swayidle is selected when GNOME and native Wayland are unavailable
    Given a temporary LG Buddy config using input HDMI_2
    And the executable PATH is isolated
    And swayidle is installed
    When I run the command "detect-backend"
    Then the command succeeds
    And stdout is "swayidle"

  Scenario: Backend override wins
    Given a temporary LG Buddy config using input HDMI_2
    And the executable PATH is isolated
    And GNOME Shell is available
    And the backend override is "gnome"
    When I run the command "detect-backend"
    Then the command succeeds
    And stdout is "gnome"

  Scenario: Missing GNOME idle monitor is reported explicitly when no fallback exists
    Given a temporary LG Buddy config using input HDMI_2
    And the executable PATH is isolated
    And GNOME Shell is available
    And GNOME idle monitor is unavailable
    When I run the command "detect-backend"
    Then the command fails
    And stderr contains "org.gnome.Mutter.IdleMonitor"
    And stderr contains "native Wayland unavailable"
