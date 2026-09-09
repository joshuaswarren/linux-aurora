#!/usr/bin/env python3
"""Send a bounded command to the already booted Linux DockChannel shell.

This is a plain serial terminal, not an m1n1 proxy client. Do not use while
boot-linux.py or another terminal owns the Neo KIS channel.
"""
import argparse
import re
import sys
import time
import uuid
import serial

parser=argparse.ArgumentParser()
parser.add_argument('--device',default='/dev/cu.kis-00100000-ch-0')
parser.add_argument('--timeout',type=float,default=60)
parser.add_argument('command')
args=parser.parse_args()
marker='NEO_CMD_'+uuid.uuid4().hex
command=args.command+'; task_result=$?; printf "\\n'+marker+'=%s\\n" "$task_result"\n'
deadline=time.monotonic()+args.timeout
data=bytearray()
with serial.Serial(args.device,115200,timeout=0.2,write_timeout=10) as port:
    # End a partial prompt without invoking any proxy operation.
    port.write(b'\n')
    time.sleep(0.2)
    port.write(command.encode())
    port.flush()
    while time.monotonic()<deadline:
        chunk=port.read(65536)
        if not chunk:
            continue
        sys.stdout.buffer.write(chunk)
        sys.stdout.flush()
        data.extend(chunk)
        normalized=bytes(data).replace(b'\r',b'')
        # A wrapped terminal echo may put the printf template at line start.
        # Match the completed numeric result, wherever it occurs in the stream.
        result=re.search(b'\n'+marker.encode()+rb'=(\d+)\n',normalized)
        if result:
            raise SystemExit(int(result.group(1)))
raise SystemExit('Timed out waiting for Linux command completion')
