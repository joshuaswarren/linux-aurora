#!/bin/sh
# Run inside the RAM desktop as aurora, with DISPLAY and XAUTHORITY set.
set -eu
case "${1-}" in
    fastfetch)
        fastfetch --logo arch --structure OS:Host:Kernel:Uptime:Packages:Shell:Display:DE:WM:Terminal:CPU:Memory:Swap
        printf '\n  Online CPUs: %s (current RAM boot)\n' "$(getconf _NPROCESSORS_ONLN)"
        task_display=SimpleDRM
        for task_node in /sys/class/drm/card[0-9]*; do
            [ -f "$task_node/dev" ] || continue
            if [ "$(basename "$(readlink "$task_node/device/driver/module")")" = appledrm ]; then
                task_display='Apple DCP / KMS'
                break
            fi
        done
        printf '\n  Display: XFCE on %s; GPU acceleration is not enabled.\n' "$task_display"
        exec bash --noprofile --norc
        ;;
    wifi)
        printf '\033[1;36mApple N1 Wi-Fi | native Linux\033[0m\n\n'
        printf 'Interface: wlan0\nDriver: '
        basename "$(readlink -f /sys/class/net/wlan0/device/driver)"
        printf 'Link: '
        cat /sys/class/net/wlan0/operstate
        if ip -4 -o address show dev wlan0 scope global | grep -q 'inet '; then
            printf 'IPv4: assigned\n'
        fi
        if task_link=$(iw dev wlan0 link 2>/dev/null); then
            printf '%s\n' "$task_link" | sed -n '/signal:/p; /tx bitrate:/p; /rx bitrate:/p'
        else
            printf 'Signal/bitrate statistics: unavailable\n'
        fi
        printf '\nLive Google DNS ping, explicitly through wlan0 (Ctrl-C stops):\n\n'
        # Pin the link information above the live reply area.
        task_rows=$(tput lines)
        if [ "$task_rows" -gt 15 ]; then
            printf '\033[13;%sr\033[13;1H' "$task_rows"
            trap 'printf "\033[r"' EXIT
        fi
        # Keep the local source address out of the presentation's first line.
        ping -n -I wlan0 -i 1 8.8.8.8 | sed -u '1d'
        exit
        ;;
    btop)
        task_config="$HOME/.config/aurora-demo/btop.conf"
        mkdir -p "$(dirname "$task_config")"
        if [ ! -e "$task_config" ]; then
            # One full-height graph makes low CPU activity easier to see.
            # Values remain the kernel's measured utilization on a 0-100% scale.
            cat >"$task_config" <<'EOF'
update_ms = 1000
cpu_graph_upper = "total"
cpu_single_graph = true
graph_symbol_cpu = "block"
EOF
        fi
        exec btop --force-utf --config "$task_config"
        ;;
    '') ;;
    *) exit 2 ;;
esac
for program in fastfetch btop xfce4-terminal; do
    command -v "$program" >/dev/null
done
task_script=$(readlink -f "$0")
task_dir="$HOME/.config/aurora-demo"
mkdir -p "$task_dir"
chmod 700 "$task_dir"
xfce4-terminal --disable-server --hide-menubar --hide-scrollbar \
    --font='DejaVu Sans Mono 13' --geometry=95x30+35+55 \
    --title='M5 / J813 - Fastfetch' --dynamic-title-mode=none \
    --command="sh $task_script fastfetch" >"$task_dir/fastfetch.log" 2>&1 &
xfce4-terminal --disable-server --hide-menubar --hide-scrollbar \
    --font='DejaVu Sans Mono 13' --geometry=95x25+35+890 \
    --title='Apple N1 Wi-Fi - Live Internet Test' --dynamic-title-mode=none \
    --command="sh $task_script wifi" >"$task_dir/wifi.log" 2>&1 &
xfce4-terminal --disable-server --hide-menubar --hide-scrollbar \
    --font='DejaVu Sans Mono 13' --geometry=95x56+1320+55 \
    --title='M5 / J813 - btop' --dynamic-title-mode=none \
    --command="sh $task_script btop" >"$task_dir/btop.log" 2>&1 &
echo J813_DESKTOP_DEMO_WINDOWS_STARTED
