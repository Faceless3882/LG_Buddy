Feature: Updates CLI
  LG Buddy should expose manual update checks without advertising its timer entrypoint.

  Scenario: Updates help describes the public check command
    When I run the command "updates --help"
    Then the command succeeds
    And stdout contains "updates check [--channel stable|prerelease] [--notify]"
    And stdout does not contain "background-check"

  Scenario: Updates check help is available through both public help forms
    When I run the command "updates check --help"
    Then the command succeeds
    And stdout contains "updates check [--channel stable|prerelease] [--notify]"
    And stdout contains "--channel stable|prerelease"
    And stdout contains "--notify"
    And stdout does not contain "background-check"
    When I run the command "help updates check"
    Then the command succeeds
    And stdout contains "updates check [--channel stable|prerelease] [--notify]"

  Scenario: Invalid updates syntax shows scoped usage
    When I run the command "updates check --channel"
    Then the command fails
    And the command exits with status 2
    And stderr contains "missing channel value for `updates check`"
    And stderr contains "updates check [--channel stable|prerelease] [--notify]"
    And stderr does not contain "background-check"

  Scenario: Global help hides the timer entrypoint
    When I run the command "--help"
    Then the command succeeds
    And stdout contains "updates check [--channel stable|prerelease] [--notify]"
    And stdout does not contain "updates background-check"

  Scenario: The hidden background check entrypoint remains operational
    Given a temporary LG Buddy config using input HDMI_2
    And systemd apply actions are skipped
    When I run the command "settings set updates.auto_check disabled"
    Then the command succeeds
    When I run the command "updates background-check"
    Then the command succeeds
    And stdout contains "background: skipped (automatic update checks disabled)"
