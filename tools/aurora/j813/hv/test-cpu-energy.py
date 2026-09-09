"""Verify the optional energy-restore patch against the exact M5 kernel."""
import array
import os
from pathlib import Path
import runpy
from unittest.mock import patch

# Reuse the production-branch extraction and exact-kernel/hash checks.
with patch.dict(os.environ, {'M5_SKIP_CPU_ENERGY_RESTORE': '0'}):
    base = runpy.run_path(str(Path(__file__).with_name('test-nvme-cq.py')))
run, hv, original, va = (base[k] for k in ('run', 'hv', 'original', 'va'))
address = 0xfffffe000983cce4
with patch.dict(os.environ, {'M5_SKIP_CPU_ENERGY_RESTORE': '1',
                            'M5_NVME_CQ_DEPTH_64': '0'}):
    output = run(hv, array.array('I', original), va, len(original), '__TEXT_EXEC')
    before, after = array.array('I', original), array.array('I', output)
    changed = [i for i, (a, b) in enumerate(zip(before, after)) if a != b]
    assert changed == [(address - va) // 4]
    assert before[changed[0]] == 0x940003b2
    assert after[changed[0]] == 0xd503201f
    # Both the call target and surrounding setup are required to match.
    for addr in (address, address - 16, address + 12):
        altered = array.array('I', original)
        altered[(addr - va) // 4] ^= 1
        try:
            run(hv, altered, va, len(original), '__TEXT_EXEC')
        except RuntimeError as error:
            assert 'signature mismatch' in str(error)
        else:
            raise AssertionError('Changed energy-restore code was accepted')
    hv.u.adt['/chosen'].chip_id = 0x8140
    untouched = array.array('I', original)
    assert run(hv, untouched, va, len(original), '__TEXT_EXEC') is None
    assert untouched.tobytes() == original
    hv.u.adt['/chosen'].chip_id = 0x8142
with patch.dict(os.environ, {'M5_SKIP_CPU_ENERGY_RESTORE': '0',
                            'M5_NVME_CQ_DEPTH_64': '0'}):
    assert run(hv, array.array('I', original), va, len(original), '__TEXT_EXEC') == original
with patch.dict(os.environ, {'M5_SKIP_CPU_ENERGY_RESTORE': 'invalid',
                            'M5_NVME_CQ_DEPTH_64': '0'}):
    try:
        run(hv, array.array('I', original), va, len(original), '__TEXT_EXEC')
    except RuntimeError as error:
        assert 'must be 0 or 1' in str(error)
    else:
        raise AssertionError('Invalid energy-restore control was accepted')
print('M5 CPU energy restore: one call omitted, signature guards, opt-out and chip isolation passed')
