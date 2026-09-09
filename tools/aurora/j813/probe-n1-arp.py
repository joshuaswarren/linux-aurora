#!/usr/bin/env python3
"""Probe a peer or DHCP discovery from RAM Linux without printing identities.

Sends three ARP probes with an unspecified sender IP. It does not
configure an address or modify the host interface. Raw output is private.
The DHCP mode sends DISCOVER only; it never requests a lease or sets an IP.
"""
import argparse
import ipaddress
import os
from pathlib import Path
import re
import subprocess
import sys

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('label')
parser.add_argument('--peer', choices=('host', 'router'), default='host')
parser.add_argument('--wire-format', choices=('ethernet', 'prefix2', 'llc', 'dhcp', 'unicast'),
                    help='Use the bounded native packet-layout probe')
parser.add_argument('--fwlog-ack', type=int,
                    help='Acknowledge an already downloaded firmware log just before sending')
parser.add_argument('--firmware-source', action='store_true',
                    help='Use the validated non-MLD firmware station address for the native probe')
args = parser.parse_args()
if not re.fullmatch(r'[A-Za-z0-9_-]+', args.label):
    parser.error('label must contain only letters, digits, underscore or dash')
if args.fwlog_ack is not None and not 0 <= args.fwlog_ack <= 0xffffffff:
    parser.error('firmware log counter must be a u32')
if args.firmware_source and not args.wire_format:
    parser.error('--firmware-source requires --wire-format')
if args.wire_format == 'unicast' and (args.peer != 'host' or args.firmware_source):
    parser.error('unicast requires the host peer and netdev source')
root = Path(__file__).resolve().parent
try:
    query = (['/usr/sbin/ipconfig', 'getifaddr', 'en0'] if args.peer == 'host'
             else ['/usr/sbin/ipconfig', 'getoption', 'en0', 'router'])
    value = subprocess.check_output(
        query, stderr=subprocess.DEVNULL,
        text=True).strip()
    address = ipaddress.IPv4Address(value)
    if address.is_unspecified or address.is_multicast or address.is_loopback:
        raise ValueError('invalid interface address')
except (subprocess.CalledProcessError, ValueError):
    raise SystemExit('Requested Wi-Fi peer address unavailable') from None

host_mac = None
if args.wire_format == 'unicast':
    config = subprocess.check_output(['/sbin/ifconfig', 'en0'], text=True)
    match = re.search(r'\bether\s+([0-9a-f:]{17})', config)
    if not match or 'status: active' not in config:
        raise SystemExit('Host Wi-Fi interface unavailable')
    host_mac = match[1]
    raw_mac = bytes.fromhex(host_mac.replace(':', ''))
    if len(raw_mac) != 6 or raw_mac[0] & 1 or not any(raw_mac):
        raise SystemExit('Host Wi-Fi address invalid')

label = args.label
acknowledge = (f'echo {args.fwlog_ack} > '
               '/sys/kernel/debug/apple_centauri/0000:01:00.0/fwlog_ack || exit 4'
               if args.fwlog_ack is not None else ':')
capture = f'/run/n1-arp-{label}.pcap'
capture_filter = 'arp or ether broadcast' if args.wire_format else 'arp'
if args.wire_format == 'dhcp':
    capture_filter = 'udp and (port 67 or port 68)'
probe = (f'"$packet_probe" {address} {args.wire_format}' if args.wire_format
         else f'busybox arping -b -D -I wlan0 -s 0.0.0.0 -c 3 -w 8 {address}')
if args.firmware_source:
    probe += ' firmware'
if host_mac:
    probe += ' ' + host_mac
result_marker = 'DHCP_PROBE_RESULT' if args.wire_format == 'dhcp' else 'ARP_PROBE_RESULT'
command = f'''umask 077
packet_probe=/run/n1-packet-probe
test -x "$packet_probe" || packet_probe=/bin/n1-packet-probe
test ! -e {capture} || exit 2
timeout -s INT 15 /bin/tcpdump -i wlan0 -p -n -s 0 -U -Z root -w {capture} '{capture_filter}' > /run/n1-arp-{label}-capture.log 2>&1 &
capture_pid=$!
trap 'kill "$capture_pid" 2>/dev/null || true' EXIT HUP INT TERM
ready=0
for try in 1 2 3 4 5; do
    if grep -q 'listening on wlan0' /run/n1-arp-{label}-capture.log; then ready=1; break; fi
    kill -0 "$capture_pid" 2>/dev/null || break
    sleep 1
done
test "$ready" = 1 || exit 3
{acknowledge}
printf 'N1_TX_BEFORE '
head -n 1 /sys/kernel/debug/apple_centauri/0000:01:00.0/data_status
{probe} > /run/n1-arp-{label}-client.log 2>&1
arp_rc=$?
wait "$capture_pid" || true
trap - EXIT HUP INT TERM
printf '{result_marker} rc=%s\\n' "$arp_rc"
cat /run/n1-arp-{label}-client.log
cat /run/n1-arp-{label}-capture.log
printf 'N1_TX_AFTER '
head -n 1 /sys/kernel/debug/apple_centauri/0000:01:00.0/data_status
'''
log = root / 'logs' / f'linux-n1-arp-{label}.log'
fd = os.open(log, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
with os.fdopen(fd, 'wb') as output:
    result = subprocess.run(
        [sys.executable, str(root / 'linux-command.py'), command, '--timeout', '25'],
        stdout=output, stderr=subprocess.STDOUT, timeout=30)
text = log.read_text(errors='replace')
probe_rc = None
sent = None
tx = {}
for line in text.splitlines():
    if re.fullmatch(r'(?:ARP|DHCP)_PROBE_RESULT rc=[0-9]+', line.strip()):
        print(line.strip())
        probe_rc = int(line.strip().rsplit('=', 1)[1])
    elif line.startswith('N1_PACKET_PROBE '):
        print(line)
    elif line.startswith('N1_DHCP_PROBE '):
        print(line)
    elif re.fullmatch(r'Received [0-9]+ (?:response|request)\(s\).*', line.strip()):
        # BusyBox summary contains counters only; omit optional trailing text.
        print(re.match(r'Received [0-9]+ (?:response|request)\(s\)', line.strip())[0])
    elif line.startswith(('N1_TX_BEFORE stage=', 'N1_TX_AFTER stage=')):
        phase, status = line.split(' ', 1)
        tx[phase] = {key: int(value) for key, value in
                     re.findall(r'\b(stage|submitted|completed|failed|tx_bad)=([0-9]+)\b', status)}
        if phase == 'N1_TX_AFTER':
            print(status)
    if line.startswith(('N1_PACKET_PROBE ', 'N1_DHCP_PROBE ')):
        match = re.search(r'\bsent=([0-9]+)\b', line)
        if match:
            sent = int(match[1])
print('ARP_PROBE_LOG_SAVED_PRIVATE')
if result.returncode:
    raise SystemExit(result.returncode)
if probe_rc is None or len(tx) != 2:
    raise SystemExit('Probe result or transmit accounting missing')
before, after = tx['N1_TX_BEFORE'], tx['N1_TX_AFTER']
delta = {key: after[key] - before[key]
         for key in ('submitted', 'completed', 'failed', 'tx_bad')}
print('N1_TX_DELTA ' + ' '.join(f'{key}={value}' for key, value in delta.items()))
# These aggregate counters are necessary evidence, not per-packet radio proof.
# In particular, do not classify an unopened TX queue as a failed network test.
if (before['stage'] != 2 or after['stage'] != 2 or
        any(value < 0 for value in delta.values()) or
        delta['failed'] or delta['tx_bad'] or
        (sent is not None and (delta['submitted'] < sent or delta['completed'] < sent))):
    raise SystemExit('Probe invalid: transmit accounting did not pass')
raise SystemExit(probe_rc)
