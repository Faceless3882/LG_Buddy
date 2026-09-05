Feature: Native webOS TV platform
  LG Buddy should expose the same product behavior through its native webOS platform,
  pairing when the user is setting up or actively controlling the TV without delaying
  shutdown, suspend, or network teardown when no stored credential is available.

  Scenario: Fresh configuration defaults to and pairs the native platform
    Given an empty temporary LG Buddy config path
    And a native webOS26 TV on firmware 43.21.60 on input HDMI_2 with brightness 90
    When I accept the default TV platform during initial configuration
    Then the command succeeds
    And stdout contains "TV Platform:         lg_webos"
    And stdout contains "pairing required; accept the prompt on the TV"
    And config.env contains "tvs_primary_platform=lg_webos"
    And a valid native TV access token is stored
    And the native TV connection count is 1
    And the native TV registration tokens are "none"
    And the native TV pairing prompt count is 1
    When I run the command "brightness get"
    Then the command succeeds
    And stdout is "90"
    And the native TV connection count is 2
    And the native TV registration tokens are "none,webos-test-access-token"
    And the native TV pairing prompt count is 1

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

  Scenario: An ordinary command pairs when the token is missing
    Given a temporary LG Buddy config using input HDMI_2
    And the existing config selects TV platform "lg_webos"
    And a native webOS TV on input HDMI_2 with brightness 90
    When I run the command "brightness get"
    Then the command succeeds
    And stdout is "90"
    And a valid native TV access token is stored
    And the native TV connection count is 1
    And the native TV registration tokens are "none"
    And the native TV pairing prompt count is 1

  Scenario: An ordinary command repairs a stale token
    Given a temporary LG Buddy config using input HDMI_2
    And the existing config selects TV platform "lg_webos"
    And a native webOS TV on input HDMI_2 with brightness 90
    And a stale native TV access token is stored
    When I run the command "brightness get"
    Then the command succeeds
    And stdout is "90"
    And a valid native TV access token is stored
    And the native TV connection count is 2
    And the native TV registration tokens are "stale-cucumber-access-token,none"
    And the native TV pairing prompt count is 2

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

  Scenario: Foreground opt-in reuses a valid token before persisting the platform
    Given a temporary LG Buddy config using input HDMI_2
    And the existing config selects TV platform "bscpylgtv"
    And a native webOS TV on input HDMI_2 with brightness 90
    And a valid native TV access token is stored
    When I run the command "settings set tv.platform lg_webos"
    Then the command succeeds
    And stdout contains "using stored access token"
    And stdout contains "native webOS preflight succeeded: power_state=Active"
    And config.env contains "tvs_primary_platform=lg_webos"
    And a valid native TV access token is stored
    And the native TV connection count is 1
    And the native TV registration tokens are "webos-test-access-token"
    And the native TV pairing prompt count is 0

  Scenario: Unsetting native platform restores the missing-value compatibility default
    Given a temporary LG Buddy config using input HDMI_2
    And the existing config selects TV platform "lg_webos"
    When I run the command "settings unset tv.platform"
    Then the command succeeds
    And stdout contains "tv.platform unset"
    And config.env does not contain "tvs_primary_platform="
    When I run the command "settings get tv.platform"
    Then the command succeeds
    And stdout is "bscpylgtv"

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

  Scenario: Native brightness writes use the Luna bridge on available webOS24
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

  Scenario: Native brightness writes use the Luna bridge on affected webOS26 firmware
    Given a temporary LG Buddy config using input HDMI_2
    And the existing config selects TV platform "lg_webos"
    And a native webOS26 TV on firmware 43.21.60 on input HDMI_2 with brightness 90
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
    When I run the command "screen off"
    Then the command succeeds
    And the session marker exists
    And the TV screen is blanked
    And the native TV registration tokens are "webos-test-access-token"
    And the native TV pairing prompt count is 0
    When I run the command "screen on"
    Then the command succeeds
    And the session marker is absent
    And the TV screen is visible
    And the native TV registration tokens are "webos-test-access-token,webos-test-access-token"
    And the native TV pairing prompt count is 0

  Scenario: Native restoration verifies visibility after an ambiguous fallback acknowledgement
    Given a temporary LG Buddy config using input HDMI_2
    And the existing config selects TV platform "lg_webos"
    And LG Buddy session runtime is isolated
    And a native webOS TV on input HDMI_2 with brightness 90
    And a valid native TV access token is stored
    When I run the command "screen off"
    Then the command succeeds
    And the session marker exists
    And the TV screen is blanked
    Given the native webOS TV interrupts the first restore session and acknowledges input without unblanking
    And screen wake delays are disabled
    When I run the command "screen on"
    Then the command succeeds
    And stdout contains "Screen visibility could not be verified. Falling back to full wake."
    And stdout does not contain "Screen unblank failed."
    And stdout contains "LG Buddy Screen Restore Failure Context:"
    And stdout contains "operations: direct_unblank=failed kind=screen_not_visible"
    And stdout contains "input_attempt_1=failed kind=screen_not_visible"
    And stdout contains "recovery_unblank_1=succeeded"
    And stdout contains "input_retry_1=succeeded"
    And stdout contains "marker_after=absent"
    And the session marker is absent
    And the TV screen is visible
    And the native TV registration tokens are "webos-test-access-token,webos-test-access-token,webos-test-access-token"
    And the native TV pairing prompt count is 0

  Scenario: Native restoration retires a stale marker when the TV is already visible
    Given a temporary LG Buddy config using input HDMI_2
    And the existing config selects TV platform "lg_webos"
    And LG Buddy session runtime is isolated
    And a native webOS TV on input HDMI_2 with brightness 90
    And a valid native TV access token is stored
    And the session marker exists
    When I run the command "screen on"
    Then the command succeeds
    And stdout contains "Screen unblank succeeded. Clearing wake state."
    And stdout does not contain "Sending initial Wake-on-LAN packet"
    And the session marker is absent
    And the TV screen is visible
    And the native TV connection count is 1
    And the native TV pairing prompt count is 0

  Scenario: Native GNOME inactivity follows the LG Buddy timeout
    Given a temporary LG Buddy config using input HDMI_2
    And the existing config selects TV platform "lg_webos"
    And the idle timeout is 1 seconds
    And LG Buddy session runtime is isolated
    And a native webOS TV on input HDMI_2 with brightness 90
    And a valid native TV access token is stored
    And the executable PATH is isolated
    And GNOME Shell is available
    And GNOME emits no ScreenSaver signals
    And GNOME idle monitor will report idletimes "1000, 1000, 1000, 1000, 1000, 1000, 0"
    And GNOME monitor stays open for 1.8 seconds
    When I run the command "monitor"
    Then the command succeeds
    And stdout contains "Using GNOME backend."
    And the session marker is absent
    And the TV screen is visible
    And the native TV registration tokens are "webos-test-access-token,webos-test-access-token"
    And the native TV pairing prompt count is 0

  Scenario: Native GNOME inactivity powers off an owned blank screen after the grace period
    Given a temporary LG Buddy config using input HDMI_2
    And the existing config selects TV platform "lg_webos"
    And the idle timeout is 1 seconds
    And the timed power-off grace is 0.2 seconds
    And LG Buddy session runtime is isolated
    And a native webOS TV on input HDMI_2 with brightness 90
    And a valid native TV access token is stored
    And the executable PATH is isolated
    And GNOME Shell is available
    And GNOME emits no ScreenSaver signals
    And GNOME idle monitor will report idletimes "1000"
    And GNOME monitor stays open for 1.5 seconds
    When I run the command "monitor"
    Then the command succeeds
    And stdout contains "Timed power-off deadline reached"
    And the session marker exists
    And the TV is powered off
    And the native TV registration tokens are "webos-test-access-token,webos-test-access-token"
    And the native TV pairing prompt count is 0

  Scenario: Native power on restoration is followed by native power off
    Given a temporary LG Buddy config using input HDMI_2
    And the existing config selects TV platform "lg_webos"
    And LG Buddy session runtime is isolated
    And a native webOS TV on input HDMI_3 with brightness 100
    And a valid native TV access token is stored
    And nm-online succeeds
    And startup delays are disabled
    When I run the command "power on"
    Then the command succeeds
    And stdout contains "TV turned on and set to HDMI_2."
    And the TV input is HDMI_2
    And the native TV registration tokens are "webos-test-access-token"
    And the native TV pairing prompt count is 0
    Given reboot detection reports no pending reboot
    When I run the command "power off"
    Then the command succeeds
    And stdout contains "TV is on HDMI_2. Turning off for shutdown."
    And the TV is powered off
    And the native TV registration tokens are "webos-test-access-token,webos-test-access-token"
    And the native TV pairing prompt count is 0

  Scenario: Native startup does not delay boot when the token is missing
    Given a temporary LG Buddy config using input HDMI_2
    And the existing config selects TV platform "lg_webos"
    And LG Buddy session runtime is isolated
    And a native webOS TV on input HDMI_3 with brightness 100
    And the system marker exists
    When I run the command "startup boot"
    Then the command succeeds
    And the command completes within 1 seconds
    And stdout contains "No stored native TV credential; skipping unattended TV control."
    And no native TV access token is stored
    And the system marker is absent
    And the TV input is HDMI_3
    And the native TV connection count is 0
    And the native TV pairing prompt count is 0

  Scenario: Native resume retires ownership when the stored token is stale
    Given a temporary LG Buddy config using input HDMI_2
    And the existing config selects TV platform "lg_webos"
    And LG Buddy session runtime is isolated
    And a native webOS TV on input HDMI_3 with brightness 100
    And a stale native TV access token is stored
    And the system marker exists
    And nm-online succeeds
    And startup delays are disabled
    When I run the command "startup auto"
    Then the command succeeds
    And stdout contains "Stored TV authentication is unavailable; skipping unattended TV control."
    And the system marker is absent
    And the native TV access token is "stale-cucumber-access-token"
    And the TV input is HDMI_3
    And the native TV registration tokens are "stale-cucumber-access-token"
    And the native TV pairing prompt count is 1

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

  Scenario: Native shutdown is immediate and does not pair when the token is missing
    Given a temporary LG Buddy config using input HDMI_2
    And the existing config selects TV platform "lg_webos"
    And a native webOS TV on input HDMI_2 with brightness 90
    And reboot detection reports no pending reboot
    When I run the command "shutdown"
    Then the command succeeds
    And the command completes within 1 seconds
    And stdout contains "No stored native TV credential; skipping unattended TV control."
    And no native TV access token is stored
    And the TV is powered on
    And the native TV connection count is 0
    And the native TV pairing prompt count is 0

  Scenario: Native pre-sleep does not pair or retry when the token is missing
    Given a temporary LG Buddy config using input HDMI_2
    And the existing config selects TV platform "lg_webos"
    And LG Buddy session runtime is isolated
    And a native webOS TV on input HDMI_2 with brightness 90
    When I run the command "sleep-pre"
    Then the command succeeds
    And the command completes within 1 seconds
    And no native TV access token is stored
    And the system marker is absent
    And the TV is powered on
    And the native TV connection count is 0
    And the native TV pairing prompt count is 0

  Scenario: Native NetworkManager pre-down does not pair or retry when the token is missing
    Given a temporary LG Buddy config using input HDMI_2
    And the existing config selects TV platform "lg_webos"
    And LG Buddy session runtime is isolated
    And a native webOS TV on input HDMI_2 with brightness 90
    And mock system logind reports PreparingForSleep=true
    When I run the command "nm-pre-down"
    Then the command succeeds
    And the command completes within 1 seconds
    And no native TV access token is stored
    And the system marker is absent
    And the TV is powered on
    And the native TV connection count is 0
    And the native TV pairing prompt count is 0

  Scenario: Native shutdown does not repair a stale token
    Given a temporary LG Buddy config using input HDMI_2
    And the existing config selects TV platform "lg_webos"
    And a native webOS TV on input HDMI_2 with brightness 90
    And a stale native TV access token is stored
    And reboot detection reports no pending reboot
    When I run the command "shutdown"
    Then the command succeeds
    And the native TV access token is "stale-cucumber-access-token"
    And the TV is powered on
    And the native TV registration tokens are "stale-cucumber-access-token"
    And the native TV pairing prompt count is 1

  Scenario: Native pre-sleep bounds a stalled TV response and uses the fallback
    Given a temporary LG Buddy config using input HDMI_2
    And the existing config selects TV platform "lg_webos"
    And LG Buddy session runtime is isolated
    And a native webOS TV on input HDMI_2 with brightness 90
    And a valid native TV access token is stored
    And the native webOS TV stalls its first TV response
    And sleep retry delays are disabled
    When I run the command "sleep-pre"
    Then the command succeeds
    And the command completes within 5 seconds
    And stdout contains "Could not query TV input. Attempting power_off fallback."
    And the system marker exists
    And the TV is powered off
    And the native TV registration tokens are "webos-test-access-token,webos-test-access-token"
    And the native TV pairing prompt count is 0

  Scenario: Native NetworkManager pre-down enters the suspend rail through system logind
    Given a temporary LG Buddy config using input HDMI_2
    And the existing config selects TV platform "lg_webos"
    And LG Buddy session runtime is isolated
    And a native webOS TV on input HDMI_2 with brightness 90
    And a valid native TV access token is stored
    And the native webOS TV stalls its first TV response
    And mock system logind reports PreparingForSleep=true
    And sleep retry delays are disabled
    When I run the command "nm-pre-down"
    Then the command succeeds
    And the command completes within 5 seconds
    And stdout contains "logind is preparing for sleep; running pre-sleep TV handling before network teardown."
    And stdout contains "Could not query TV input. Attempting power_off fallback."
    And the system marker exists
    And the TV is powered off
    And the native TV registration tokens are "webos-test-access-token,webos-test-access-token"
    And the native TV pairing prompt count is 0
