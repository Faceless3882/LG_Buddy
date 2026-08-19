#!/bin/bash

set -e

SCRIPT_DIR="$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)"

if [ -r "$SCRIPT_DIR/bin/LG_Buddy_Common" ]; then
    . "$SCRIPT_DIR/bin/LG_Buddy_Common"
else
    echo "LG Buddy common helper not found."
    exit 1
fi

CONFIG_FILE="$(lg_buddy_user_config_path)"
CONFIG_DIR="$(dirname "$CONFIG_FILE")"

prompt_with_default() {
    local prompt="$1"
    local default_value="$2"
    local reply=""

    if [ -n "$default_value" ]; then
        read -p "$prompt [$default_value]: " reply
        echo "${reply:-$default_value}"
    else
        read -p "$prompt: " reply
        echo "$reply"
    fi
}

validate_ip() {
    [[ "$1" =~ ^[0-9]+\.[0-9]+\.[0-9]+\.[0-9]+$ ]]
}

validate_mac() {
    [[ "$1" =~ ^([0-9a-fA-F]{2}:){5}[0-9a-fA-F]{2}$ ]]
}

validate_input() {
    case "$1" in
        HDMI_1|HDMI_2|HDMI_3|HDMI_4) return 0 ;;
        *) return 1 ;;
    esac
}

validate_backend() {
    case "$1" in
        auto|gnome|swayidle) return 0 ;;
        *) return 1 ;;
    esac
}

validate_screen_idle_blank() {
    case "$1" in
        enabled|disabled) return 0 ;;
        *) return 1 ;;
    esac
}

validate_restore_policy() {
    case "$1" in
        marker_only|conservative|aggressive) return 0 ;;
        *) return 1 ;;
    esac
}

validate_system_sleep_wake_policy() {
    case "$1" in
        enabled|disabled) return 0 ;;
        *) return 1 ;;
    esac
}

validate_idle_timeout() {
    lg_buddy_validate_idle_timeout "$1"
}

normalize_restore_policy() {
    case "$1" in
        marker_only|conservative) echo "conservative" ;;
        aggressive) echo "aggressive" ;;
        *) echo "$1" ;;
    esac
}

current_tv_ip=""
current_tv_mac=""
current_input="HDMI_1"
current_tv_platform="$LG_BUDDY_DEFAULT_TV_PLATFORM"
current_screen_backend="$LG_BUDDY_DEFAULT_SCREEN_BACKEND"
current_screen_idle_blank="$LG_BUDDY_DEFAULT_SCREEN_IDLE_BLANK"
current_screen_idle_timeout="$LG_BUDDY_DEFAULT_IDLE_TIMEOUT"
current_screen_restore_policy="$LG_BUDDY_DEFAULT_SCREEN_RESTORE_POLICY"
current_system_sleep_wake_policy="$LG_BUDDY_DEFAULT_SYSTEM_SLEEP_WAKE_POLICY"
current_update_auto_check="$LG_BUDDY_DEFAULT_UPDATE_AUTO_CHECK"
current_update_channel="$LG_BUDDY_DEFAULT_UPDATE_CHANNEL"

if lg_buddy_load_config >/dev/null 2>&1; then
    current_tv_ip="$tv_ip"
    current_tv_mac="$tv_mac"
    current_input="$input"
    current_tv_platform="$tv_platform"
    current_screen_backend="$screen_backend"
    current_screen_idle_blank="$screen_idle_blank"
    current_screen_idle_timeout="$screen_idle_timeout"
    current_screen_restore_policy="$(normalize_restore_policy "$screen_restore_policy")"
    current_system_sleep_wake_policy="$system_sleep_wake_policy"
    current_update_auto_check="$updates_auto_check"
    current_update_channel="$updates_channel"
    echo "Loaded existing configuration from $LG_BUDDY_CONFIG_FILE"
else
    config_load_status=$?
    if [ "$config_load_status" -eq 2 ]; then
        echo "Existing configuration contains an invalid TV platform; fix tvs_primary_platform before rerunning configure.sh."
        exit 1
    fi
fi

if [ "${LG_BUDDY_NONINTERACTIVE:-0}" = "1" ]; then
    tv_ip="${LG_BUDDY_TV_IP:-$current_tv_ip}"
    tv_mac="${LG_BUDDY_TV_MAC:-$current_tv_mac}"
    input="${LG_BUDDY_INPUT:-$current_input}"
    screen_backend="${LG_BUDDY_SCREEN_BACKEND:-$current_screen_backend}"
    screen_idle_blank="$current_screen_idle_blank"
    screen_idle_timeout="${LG_BUDDY_SCREEN_IDLE_TIMEOUT:-$current_screen_idle_timeout}"
    screen_restore_policy="${LG_BUDDY_SCREEN_RESTORE_POLICY:-$current_screen_restore_policy}"
    system_sleep_wake_policy="${LG_BUDDY_SYSTEM_SLEEP_WAKE_POLICY:-$current_system_sleep_wake_policy}"
    update_auto_check="$current_update_auto_check"
    update_channel="$current_update_channel"
    if [ -z "${LG_BUDDY_SYSTEM_SLEEP_WAKE_POLICY:-}" ] && [ -n "${LG_BUDDY_DISABLE_SLEEP_WAKE:-}" ]; then
        case "$LG_BUDDY_DISABLE_SLEEP_WAKE" in
            [Yy]*|1|true|TRUE|True|yes|YES|Yes) system_sleep_wake_policy="disabled" ;;
            *) system_sleep_wake_policy="enabled" ;;
        esac
    fi

    validate_ip "$tv_ip" || {
        echo "LG_BUDDY_TV_IP must be set to a valid IPv4 address in non-interactive mode."
        exit 1
    }
    validate_mac "$tv_mac" || {
        echo "LG_BUDDY_TV_MAC must be set to a valid MAC address in non-interactive mode."
        exit 1
    }
    validate_input "$input" || {
        echo "LG_BUDDY_INPUT must be one of HDMI_1, HDMI_2, HDMI_3, or HDMI_4."
        exit 1
    }
    validate_backend "$screen_backend" || {
        echo "LG_BUDDY_SCREEN_BACKEND must be one of auto, gnome, or swayidle."
        exit 1
    }
    validate_screen_idle_blank "$screen_idle_blank" || {
        echo "screen_idle_blank must be one of enabled or disabled."
        exit 1
    }
    validate_restore_policy "$screen_restore_policy" || {
        echo "LG_BUDDY_SCREEN_RESTORE_POLICY must be one of conservative or aggressive (legacy marker_only is also accepted)."
        exit 1
    }
    screen_restore_policy="$(normalize_restore_policy "$screen_restore_policy")"
    validate_idle_timeout "$screen_idle_timeout" || {
        echo "LG_BUDDY_SCREEN_IDLE_TIMEOUT must be a positive integer."
        exit 1
    }
    screen_idle_timeout="$(lg_buddy_normalize_idle_timeout "$screen_idle_timeout")"
    validate_system_sleep_wake_policy "$system_sleep_wake_policy" || {
        echo "LG_BUDDY_SYSTEM_SLEEP_WAKE_POLICY must be one of enabled or disabled."
        exit 1
    }

    echo "Using non-interactive configuration from environment."
else
    echo "Scanning for LG TV on local network..."

    DETECTED_IPS="$(ip neigh show | grep -iE "a8:23:fe|fc:f1:52|f8:b9:5a|c4:36:6c|50:c7:bf|40:b0:76" | awk '{print $1}')"
    IP_COUNT="$(printf '%s\n' "$DETECTED_IPS" | sed '/^$/d' | wc -l)"
    SUGGESTED_IP=""

    if [ "$IP_COUNT" -eq 1 ]; then
        SUGGESTED_IP="$DETECTED_IPS"
        read -p "Found LG device at $SUGGESTED_IP. Use this address? [Y/n]: " USE_IT
        case "$USE_IT" in
            [Nn]*) SUGGESTED_IP="" ;;
        esac
    elif [ "$IP_COUNT" -gt 1 ]; then
        echo "Found multiple LG devices:"
        i=1
        while IFS= read -r ip; do
            echo "  $i) $ip"
            ((i++))
        done <<< "$DETECTED_IPS"
        read -p "Enter the number of your TV, or press Enter to type manually: " CHOICE
        if [[ "$CHOICE" =~ ^[0-9]+$ ]] && [ "$CHOICE" -ge 1 ] && [ "$CHOICE" -le "$IP_COUNT" ]; then
            SUGGESTED_IP="$(echo "$DETECTED_IPS" | sed -n "${CHOICE}p")"
        fi
    fi

    if [ -n "$SUGGESTED_IP" ]; then
        tv_ip="$SUGGESTED_IP"
    else
        while true; do
            tv_ip="$(prompt_with_default "Enter your TV's IP address (e.g. 192.168.1.100)" "$current_tv_ip")"
            if validate_ip "$tv_ip"; then
                break
            fi
            echo "  Invalid format. Expected: 192.168.1.100"
        done
    fi

    if ! ping -c 1 -W 2 "$tv_ip" &>/dev/null; then
        echo "  Warning: TV not responding at $tv_ip (may be in standby). Continuing."
    fi

    DETECTED_MAC="$(ip neigh show "$tv_ip" | awk 'NR==1{print $5}')"
    if [ -n "$DETECTED_MAC" ]; then
        read -p "Detected TV MAC $DETECTED_MAC. Use this address? [Y/n]: " USE_DETECTED_MAC
        case "$USE_DETECTED_MAC" in
            [Nn]*) DETECTED_MAC="" ;;
        esac
    fi

    if [ -n "$DETECTED_MAC" ]; then
        tv_mac="$DETECTED_MAC"
    else
        while true; do
            tv_mac="$(prompt_with_default "Enter your TV's MAC address (e.g. aa:bb:cc:dd:ee:ff)" "$current_tv_mac")"
            if validate_mac "$tv_mac"; then
                break
            fi
            echo "  Invalid format. Expected: aa:bb:cc:dd:ee:ff"
        done
    fi

    echo "Which HDMI input is your PC connected to?"
    echo "  1) HDMI_1"
    echo "  2) HDMI_2"
    echo "  3) HDMI_3"
    echo "  4) HDMI_4"

    case "$current_input" in
        HDMI_1) default_hdmi_choice="1" ;;
        HDMI_2) default_hdmi_choice="2" ;;
        HDMI_3) default_hdmi_choice="3" ;;
        HDMI_4) default_hdmi_choice="4" ;;
        *) default_hdmi_choice="1" ;;
    esac

    while true; do
        HDMI_CHOICE="$(prompt_with_default "Enter number (1-4)" "$default_hdmi_choice")"
        case "$HDMI_CHOICE" in
            1) input="HDMI_1"; break ;;
            2) input="HDMI_2"; break ;;
            3) input="HDMI_3"; break ;;
            4) input="HDMI_4"; break ;;
            *) echo "  Please enter a number between 1 and 4." ;;
        esac
    done

    case "$current_screen_idle_blank" in
        enabled) default_idle_blank_choice="Y" ;;
        disabled) default_idle_blank_choice="n" ;;
        *) default_idle_blank_choice="Y" ;;
    esac

    while true; do
        IDLE_BLANK_CHOICE="$(prompt_with_default "Enable automatic idle blank/restore? (Y/n)" "$default_idle_blank_choice")"
        case "$IDLE_BLANK_CHOICE" in
            ""|[Yy]*|1|true|TRUE|True|yes|YES|Yes) screen_idle_blank="enabled"; break ;;
            [Nn]*|0|false|FALSE|False|no|NO|No) screen_idle_blank="disabled"; break ;;
            *) echo "  Please answer yes or no." ;;
        esac
    done

    if [ "$screen_idle_blank" = "enabled" ]; then
        echo "Choose the screen idle backend:"
        echo "  1) auto"
        echo "  2) gnome"
        echo "  3) swayidle"

        case "$current_screen_backend" in
            auto) default_backend_choice="1" ;;
            gnome) default_backend_choice="2" ;;
            swayidle) default_backend_choice="3" ;;
            *) default_backend_choice="1" ;;
        esac

        while true; do
            BACKEND_CHOICE="$(prompt_with_default "Enter number (1-3)" "$default_backend_choice")"
            case "$BACKEND_CHOICE" in
                1) screen_backend="auto"; break ;;
                2) screen_backend="gnome"; break ;;
                3) screen_backend="swayidle"; break ;;
                *) echo "  Please enter a number between 1 and 3." ;;
            esac
        done

        while true; do
            screen_idle_timeout="$(prompt_with_default "Enter idle timeout in seconds" "$current_screen_idle_timeout")"
            if validate_idle_timeout "$screen_idle_timeout"; then
                screen_idle_timeout="$(lg_buddy_normalize_idle_timeout "$screen_idle_timeout")"
                break
            fi
            echo "  Please enter a positive number of seconds."
        done

        echo "Choose how aggressively LG Buddy should restore the display:"
        echo "  1) conservative  (only restore when LG Buddy knows it blanked or powered off the TV)"
        echo "  2) aggressive    (restore on wake/activity even without prior LG Buddy ownership)"

        case "$current_screen_restore_policy" in
            conservative|marker_only) default_restore_policy_choice="1" ;;
            aggressive) default_restore_policy_choice="2" ;;
            *) default_restore_policy_choice="1" ;;
        esac

        while true; do
            RESTORE_POLICY_CHOICE="$(prompt_with_default "Enter number (1-2)" "$default_restore_policy_choice")"
            case "$RESTORE_POLICY_CHOICE" in
                1) screen_restore_policy="conservative"; break ;;
                2) screen_restore_policy="aggressive"; break ;;
                *) echo "  Please enter a number between 1 and 2." ;;
            esac
        done
    else
        screen_backend="$current_screen_backend"
        screen_idle_timeout="$current_screen_idle_timeout"
        screen_restore_policy="$current_screen_restore_policy"
    fi

    system_sleep_wake_policy="$current_system_sleep_wake_policy"
    update_auto_check="$current_update_auto_check"
    update_channel="$current_update_channel"
fi

echo ""
echo "Configuration to apply:"
echo "  TV IP:               $tv_ip"
echo "  TV MAC:              $tv_mac"
echo "  PC Input:            $input"
echo "  TV Platform:         $current_tv_platform"
echo "  Screen Idle Blank:   $screen_idle_blank"
echo "  Screen Backend:      $screen_backend"
echo "  Screen Idle Timeout: $screen_idle_timeout"
echo "  Screen Restore:      $screen_restore_policy"
echo "  System Sleep/Wake:   $system_sleep_wake_policy"
echo "  Update Checks:       $update_auto_check"
echo "  Update Channel:      $update_channel"
echo "  Config File:         $CONFIG_FILE"
echo ""

if [ "${LG_BUDDY_NONINTERACTIVE:-0}" != "1" ]; then
    read -p "Apply this configuration? [Y/n]: " CONFIRM
    case "$CONFIRM" in
        [Nn]*)
            echo "Aborted. Re-run configure.sh to try again."
            exit 1
            ;;
    esac
fi

mkdir -p "$CONFIG_DIR"
chmod 700 "$CONFIG_DIR"

cat >"$CONFIG_FILE" <<EOF
# LG Buddy configuration
tvs_primary_ip=$tv_ip
tvs_primary_mac=$tv_mac
tvs_primary_input=$input
tvs_primary_platform=$current_tv_platform
screen_idle_blank=$screen_idle_blank
screen_backend=$screen_backend
screen_idle_timeout=$screen_idle_timeout
screen_restore_policy=$screen_restore_policy
system_sleep_wake_policy=$system_sleep_wake_policy
updates_auto_check=$update_auto_check
updates_channel=$update_channel
EOF

chmod 600 "$CONFIG_FILE"
echo "Configuration written to $CONFIG_FILE"

if [ -f "$HOME/.config/systemd/user/LG_Buddy_screen.service" ]; then
    if [ "${LG_BUDDY_SKIP_SYSTEMD_ACTIONS:-0}" = "1" ]; then
        echo "Skipping LG_Buddy_screen.service reload because LG_BUDDY_SKIP_SYSTEMD_ACTIONS=1."
    else
        systemctl --user daemon-reload
        if systemctl --user is-active --quiet LG_Buddy_screen.service || systemctl --user is-enabled --quiet LG_Buddy_screen.service; then
            systemctl --user restart LG_Buddy_screen.service
            echo "Restarted LG_Buddy_screen.service to pick up the new configuration."
        fi
    fi
fi
