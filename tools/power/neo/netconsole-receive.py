#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-2.0-only
"""Retain UDP netconsole packets on a separate machine during Neo experiments."""
import argparse
import datetime
import json
import os
import socket

p = argparse.ArgumentParser(description=__doc__)
p.add_argument('--bind', required=True, help='receiver interface IPv4 address')
p.add_argument('--source', required=True, help='accept only this sender IPv4 address')
p.add_argument('--port', type=int, default=6666)
p.add_argument('--output', required=True, help='new JSONL capture, never overwritten')
a = p.parse_args()
with socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as sock:
    sock.setsockopt(socket.SOL_SOCKET, socket.SO_RCVBUF, 4 * 1024 * 1024)
    sock.bind((a.bind, a.port))
    fd = os.open(a.output, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_DSYNC, 0o600)
    with os.fdopen(fd, 'w', buffering=1) as output:
        while True:
            packet, sender = sock.recvfrom(65535)
            if sender[0] != a.source:
                continue
            output.write(json.dumps({
                'received_utc': datetime.datetime.now(datetime.timezone.utc).isoformat(),
                'sender': sender,
                'text': packet.decode('utf-8', errors='backslashreplace'),
            }) + '\n')
