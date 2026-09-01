Feature: Updates CLI
  LG Buddy should expose manual checks and assisted installation without advertising its timer entrypoint.

  Scenario: Updates help describes the public check command
    When I run the command "updates --help"
    Then the command succeeds
    And stdout contains "updates check [--notify]"
    And stdout contains "updates install"
    And stdout does not contain "--channel"
    And stdout does not contain "background-check"

  Scenario: Updates check help is available through both public help forms
    When I run the command "updates check --help"
    Then the command succeeds
    And stdout contains "updates check [--notify]"
    And stdout does not contain "--channel"
    And stdout contains "--notify"
    And stdout does not contain "background-check"
    When I run the command "help updates check"
    Then the command succeeds
    And stdout contains "updates check [--notify]"

  Scenario: Removed channel override is rejected with scoped usage
    When I run the command "updates check --channel stable"
    Then the command fails
    And the command exits with status 2
    And stderr contains "unexpected arguments for `updates check`: --channel stable"
    And stderr contains "updates check [--notify]"
    And stderr does not contain "background-check"

  Scenario: Updates install has scoped help and accepts no target arguments
    When I run the command "updates install --help"
    Then the command succeeds
    And stdout contains "updates install"
    And stdout contains "saved updates.channel"
    And stdout does not contain "--channel"
    When I run the command "help updates install"
    Then the command succeeds
    And stdout contains "updates install"
    When I run the command "updates install 1.5.0"
    Then the command fails
    And the command exits with status 2
    And stderr contains "unexpected arguments for `updates install`: 1.5.0"
    And stderr contains "updates install"

  Scenario: Global help hides the timer entrypoint
    When I run the command "--help"
    Then the command succeeds
    And stdout contains "updates check [--notify]"
    And stdout contains "updates install"
    And stdout does not contain "updates background-check"

  Scenario: The hidden background check entrypoint remains operational
    Given a temporary LG Buddy config using input HDMI_2
    And systemd apply actions are skipped
    When I run the command "settings set updates.auto_check disabled"
    Then the command succeeds
    When I run the command "updates background-check"
    Then the command succeeds
    And stdout contains "background: skipped (automatic update checks disabled)"
