#!/usr/bin/env python3
# SPDX-License-Identifier: MIT
"""Read fixed M5 KSM registers after a retained, stopped firmware probe.

No recovery acknowledgments or resets. Both ASC CPUs must
be stopped and the measured GPX/GFX power domains must still report ON.
Offsets below come from M5 25G76 firmware, not the Neo diagnostic helper.
--queue11 temporarily selects queue11 for reads and restores its predecessor.
--queue11-before-compute reads only queue11 after successful mode7 setup.
--before-recovery reads slot0 and queue11 after verified mode8 kick admission.
"""
import json
from pathlib import Path
import re
import sys
import time

root = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(root / 'm1n1/proxyclient'))
from m1n1.proxy import Serial

sequence = 0
if sys.argv[1:] not in ([], ['--queue11'], ['--queue11-before-compute'], ['--before-recovery']):
    raise ValueError('Expected --queue11, --queue11-before-compute, or --before-recovery')
before_compute = sys.argv[1:] == ['--queue11-before-compute']
before_recovery = sys.argv[1:] == ['--before-recovery']
select_queue = bool(sys.argv[1:])
with Serial('/dev/cu.kis-01100000-ch-0', 115200, timeout=0.2) as port:
    def command(script):
        global sequence
        sequence += 1
        token = 'M5_KSM_RO_%d' % sequence
        port.write(('stty -echo; echo ' + token + '_BEGIN; ' + script
                    + '; echo ' + token + '_END; stty echo\n').encode())
        received = bytearray()
        deadline = time.monotonic() + 10
        end = ('\n' + token + '_END\n').encode()
        while time.monotonic() < deadline:
            received.extend(port.read(65536))
            clean = bytes(received).replace(b'\r', b'')
            if end in clean:
                body = clean.split(('\n' + token + '_BEGIN\n').encode(), 1)[1]
                # Empty successful commands put END immediately after BEGIN.
                return body.split((token + '_END\n').encode(), 1)[0].rstrip(b'\n')
        raise RuntimeError('M5 read timed out; retain the target and its owners')

    probe_gate = (
        "dmesg | grep -q 'requested probe mode=7' "
        "&& dmesg | grep -q 'M5_INIT transaction result=Ok(())' "
        "&& dmesg | grep -q 'M5_POWER idle policy retired' "
        "&& dmesg | grep -q '1024 untouched outputs, SKSM kick unpublished' "
        "&& ! dmesg | grep -q 'M5_COMPUTE first SKSM kick submitted' "
        "&& ! dmesg | grep -q 'requested probe mode=6' "
        "&& ! dmesg | grep -q 'M5_COMPUTE firmware requested GPU restart'"
        if before_compute else (
        "dmesg | grep -q 'requested probe mode=8' "
        "&& dmesg | grep -q 'M5_INIT transaction result=Ok(())' "
        "&& dmesg | grep -q 'M5_KSM quiescent admission snapshot;' "
        "&& dmesg | grep -Fq 'id=0x41 args=[b, 1, 2, 1]' "
        "&& ! dmesg | grep -q 'M5_COMPUTE firmware requested GPU restart'"
        if before_recovery else
        "dmesg | grep -q 'requested probe mode=6' "
        "&& dmesg | grep -q 'M5_COMPUTE firmware requested GPU restart'"))
    identity = command("grep -q apple,j813 /proc/device-tree/compatible "
        "&& [ $(uname -r) = 7.1.6-aurora-j813-gpu-v9 ] "
        "&& [ -d /sys/module/asahi ] "
        "&& " + probe_gate + " "
        "&& dmesg | grep -q 'M5_INIT stop requests=Ok(())' "
        "&& echo M5_STOPPED_PROBE_VERIFIED")
    if identity.strip() != b'M5_STOPPED_PROBE_VERIFIED':
        raise RuntimeError('Expected the retained M5 stopped probe matching the requested mode')
    print('M5_REQUESTED_QUEUE_PHASE=' + ('before-recovery' if before_recovery
          else 'before-compute' if before_compute else 'after-recovery'), flush=True)

    def read_register(label, address, width):
        raw = command('devmem 0x%x %d' % (address, width)).strip()
        if not re.fullmatch(rb'0x[0-9a-fA-F]+', raw):
            raise RuntimeError('Register read did not return a value: ' + label)
        value = int(raw, 16)
        print(json.dumps(dict(label=label, address=hex(address), width=width,
                              value=hex(value))), flush=True)
        return value

    # Match mailbox.c's exact ASCWrap-v6 RUN and second-clear fields.
    for role, base in [('gfx', 0x582600000), ('gfx1', 0x582e00000)]:
        if read_register(role + '-cpu-control', base + 0x44, 32) & 0x30:
            raise RuntimeError('Firmware CPU stop was not requested')
        if read_register(role + '-cpu-status', base + 0x48, 32) & 0x23 != 0x22:
            raise RuntimeError('Firmware CPU quiescence is not verified; no GPU selector access')
    for label, offset in [('gpx-power', 0xe80000), ('gfx-power', 0xe80110)]:
        if read_register(label, 0x580000000 + offset, 32) & 0xff != 0xff:
            raise RuntimeError('GPU power domain is not fully ON')
    if before_recovery:
        # Exact M5 0xe6a4..0xe6dc reads these producer status registers.
        # Acknowledgments are separate stores at +0x20 (0xea6c..0xea88);
        # do not read or write those acknowledgment registers here.
        for ordinal in range(4):
            read_register('hardware-event-producer-%d' % ordinal,
                          0x580021168 + ordinal * 8, 64)
    # Recovery code 0x6be8..0x6c00 leaves the selected slot and reads its
    # command pointers. Reject another slot rather than changing selection.
    if not before_compute and read_register('selected-slot', 0x580010398, 32) != 0:
        raise RuntimeError('Expected the retained compute slot zero')
    for label, offset in [
            ('fault-mask-0', 0xc0b0), ('fault-mask-1', 0xc0c8),
            ('fault-mask-2', 0xc0d8), ('active-state', 0xc128),
            ('dm0-mask', 0xc160), ('dm1-mask', 0xc168), ('dm2-mask', 0xc170),
            ('command-pointer', 0xc0f0), ('queue-pointer', 0xc0f8),
            ('slot-state', 0xc020), ('slot-time', 0xc028), ('slot-progress', 0xc078)]:
        if not before_compute:
            read_register(label, 0x580000000 + offset, 64)
    # M5 0x11f50 selects qid11 before adding the kick; 0x21790..0x217a0
    # reads queue enable/completion through the same selected-queue view.
    previous_queue = read_register('selected-queue', 0x5800103a8, 64)
    if previous_queue > 0x7f:
        raise RuntimeError('Unexpected selected queue')
    def select(value):
        # Exact M5 0x21790 uses a 64-bit store to this queue selector.
        response = command('devmem 0x5800103a8 64 0x%x' % value).strip()
        if response:
            raise RuntimeError('Queue selector write returned: ' + response.decode(errors='replace')[:240])
        if read_register('queue-selection-readback', 0x5800103a8, 64) != value:
            raise RuntimeError('Queue selector readback mismatch')

    if previous_queue == 11 or select_queue:
        try:
            if previous_queue != 11:
                select(11)
            for label, offset in [('queue-enable', 0x21008),
                                  ('queue-stamp', 0x21020), ('queue-completed', 0x21028)]:
                read_register(label, 0x580000000 + offset, 64)
        finally:
            if previous_queue != 11:
                select(previous_queue)
    else:
        print('M5_QUEUE11_READ_SKIPPED: another queue is selected; selector unchanged')
    # M5 0x6744..0x67b8 scans banks0..7 with this selector and tests
    # fault-info bit0. Before recovery, inspect those same eight banks with
    # both firmware CPUs stopped and restore the selector on every exit.
    if not before_compute:
        previous_bank = read_register('gmmu-selected-bank', 0x58000d800, 64)
        if before_recovery:
            # M5 0x12d6c..0x12d74 and 0x17704..0x1770c also select
            # the global summary view 0x100 with the same 64-bit store.
            if previous_bank not in (*range(8), 0x100):
                raise RuntimeError('Unexpected GMMU bank selection')
            def select_bank(value):
                response = command('devmem 0x58000d800 64 0x%x' % value).strip()
                if response:
                    raise RuntimeError('GMMU selector write returned unexpected output')
                if read_register('gmmu-bank-readback', 0x58000d800, 64) != value:
                    raise RuntimeError('GMMU selector readback mismatch')
            try:
                for bank in range(8):
                    select_bank(bank)
                    read_register('gmmu-bank-%d-fault-info' % bank, 0x58000d8c0, 64)
            finally:
                select_bank(previous_bank)
        else:
            read_register('gmmu-selected-fault-info', 0x58000d8c0, 64)
    print('M5_STOPPED_KSM_READ_COMPLETE')
