#!/usr/bin/env python3
"""Read native M5 regressions after encoding; run as root in the RAM desktop."""
import json
from pathlib import Path
import re
import subprocess

def run(args):
    return subprocess.check_output(args, text=True, stderr=subprocess.STDOUT)

status = dict(l.split('=', 1) for l in Path('/sys/kernel/debug/281100000.video-codec/ave-status').read_text().splitlines() if '=' in l)
assert status['encoder_owned'] == '1' and status['command_failed'] == '0'
assert status['commands'] == status['replies'] == status['transport_acks']
assert int(status['v4l2_frames']) >= 20
assert Path('/sys/class/net/wlan0/carrier').read_text().strip() == '1'
addresses = json.loads(run(['ip', '-j', 'addr', 'show', 'dev', 'wlan0']))
assert any(a['family'] == 'inet' for iface in addresses for a in iface['addr_info'])
ping = run(['/usr/sbin/runuser', '-u', 'aurora', '--', 'ping', '-I', 'wlan0', '-c', '5', '-W', '2', '8.8.8.8'])
assert re.search(r'5 packets transmitted, 5 received', ping)
hci = run(['hciconfig', 'hci0'])
assert 'UP RUNNING' in hci
errors = re.findall(r'errors:(\d+)', hci)
assert len(errors) == 2 and errors == ['0', '0']
inputs = Path('/proc/bus/input/devices').read_text()
names = re.findall(r'^N: Name="(.+)"$', inputs, re.M)
keyboard = 'Apple MTP keyboard' in names
trackpad = 'Apple MTP multi-touch' in names
assert keyboard and trackpad
for name in ('xfce4-session', 'xfwm4', 'xfce4-panel', 'xfdesktop'):
    run(['pgrep', '-u', 'aurora', '-x', name])
kernel = run(['dmesg'])
fatal = [l for l in kernel.splitlines() if re.search(
    r'Kernel panic|Unable to handle kernel|\bBUG:|Synchronous External Abort|dart.*(?:translation fault|ERROR)', l, re.I)]
assert not fatal, 'Kernel or DART failure in this boot'
result = dict(passed=True, kernel=run(['uname', '-r']).strip(),
    online_cpus=Path('/sys/devices/system/cpu/online').read_text().strip(),
    wifi_ipv4_present=True, wifi_ping_target='8.8.8.8', wifi_ping_sent=5,
    wifi_ping_received=5, ping_user='aurora', bluetooth_up=True,
    bluetooth_transport_errors_rx=0, bluetooth_transport_errors_tx=0,
    bluetooth_pairing_or_audio_retested=False, keyboard_registered=bool(keyboard),
    trackpad_registered=bool(trackpad), physical_input_retested=False,
    xfce_running=True, gpu_acceleration_qualified=False,
    firmware_commands=int(status['commands']), firmware_replies=int(status['replies']),
    v4l2_frames=int(status['v4l2_frames']), command_failed=False,
    kernel_or_dart_fault=False)
print(json.dumps(result, indent=2))
