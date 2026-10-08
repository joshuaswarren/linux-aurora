#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-2.0-only
"""Offline restoration regression cases: no hardware access."""
import importlib.util
from pathlib import Path
import json
from unittest.mock import patch

spec = importlib.util.spec_from_file_location('gate', Path(__file__).with_name('pmgr-gate.py'))
gate = importlib.util.module_from_spec(spec)
spec.loader.exec_module(gate)
d = {'base': '0x300700000', 'offset': 0x250}

for before in (0x3ff, 0x0f0023ff, 0x100003ff):
    with patch.object(gate, 'helper', return_value=(0, '')) as helper, \
         patch.object(gate, 'read', side_effect=[before & ~(1 << 28), before]):
        assert gate.restore(d, {'before': before}, 'log') == before
        assert helper.call_args_list[0].args[3] == '0xf'

# Reproduce the old silent-restore bug: a successful helper exit while actual
# remains OFF must fail, before AUTO writes or a successful completion record.
with patch.object(gate, 'helper', return_value=(0, '')) as helper, \
     patch.object(gate, 'read', return_value=0x300):
    try:
        gate.restore(d, {'before': 0x100003ff}, 'log')
    except RuntimeError:
        pass
    else:
        raise AssertionError('OFF readback accepted as restored')
    assert helper.call_count == 1

with patch.object(gate, 'helper', side_effect=RuntimeError('MMIO failure')), \
     patch.object(gate, 'read') as read:
    try:
        gate.restore(d, {'before': 0x3ff}, 'log')
    except RuntimeError:
        pass
    else:
        raise AssertionError('helper failure ignored')
    read.assert_not_called()

for name in ('SIO_CPU', 'DISPEXT0_CPU', 'AOP_CPU', 'DISP_CPU', 'PMP', 'APCIE_PHY_SW'):
    assert gate.denied(name), name
assert not gate.denied('PRORES') and not gate.denied('MSR')
table = {d['name']: d for d in json.loads(gate.TABLE.read_text())}
assert table['AOP_BASE']['parents'] == ['AOP_FILTER']
assert table['AOP_CPU']['device_id'] != table['AOP_CPU']['adt_index']
assert table['DPTX_PHY']['virtual'] and table['DISPEXT0_GPPP']['virtual']
assert 'VENC-CLSTR-V' in table['VENC0_SYS']['parents']
assert not table['PRORES']['virtual'] and not table['MSR']['virtual']
print('ok: explicit hex target, ACTIVE/policy readback, failure propagation, CPU exclusions')
