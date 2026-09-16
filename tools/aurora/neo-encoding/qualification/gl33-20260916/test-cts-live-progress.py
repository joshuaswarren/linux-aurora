#!/usr/bin/env python3
"""Execute the exact remote observer with synthetic filesystem/process inputs."""
import contextlib
import io
import json
from pathlib import Path
import runpy
import tempfile
from types import SimpleNamespace
from unittest.mock import patch

HERE = Path(__file__).resolve().parent
observer = runpy.run_path(str(HERE/'cts-live-progress.py'))

with tempfile.TemporaryDirectory() as directory:
    home = Path(directory)
    root = home/'test-boot'/'test-campaign'
    root.mkdir(parents=True)
    (home/'boot').write_text('test-boot\n')
    (root/'process.json').write_text('{"pid": 123}')
    (root/'main.qpa').write_text('#beginSession\n#endSession\n')
    (root/'kernel-stream.log').write_text(
        'asahi: G17PMARK rtkit-boot role=gfx ok=false\n')
    (root/'receipt.json').write_text('{"kernel_fault": false, "exit_code": 1}')
    namespace = {}
    exec((HERE/'cts_faults.py').read_text() + '\n' + observer['REMOTE'], namespace)
    def fixture_path(name):
        name = str(name)
        if name == '/proc/sys/kernel/random/boot_id':
            return home/'boot'
        if name == '/home/mcruz/gl30-cts-20260913':
            return home
        return Path(name)
    namespace['Path'] = fixture_path
    for missing in (False, True):
        if missing:
            (root/'kernel-stream.log').unlink()
        output = io.StringIO()
        with patch.object(namespace['subprocess'], 'run',
                          return_value=SimpleNamespace(stdout='')), contextlib.redirect_stdout(output):
            namespace['inspect']('test-boot', 'test-campaign', {})
        result = json.loads(output.getvalue())
        audit = result['kernel_fault_audit']
        assert audit['fault_detected'] is (None if missing else True)
        assert audit['evidence_complete'] is (not missing)
        assert result['receipt']['kernel_fault'] is False
        assert len(result['fault_lines']) == (0 if missing else 1)
print('2 remote observer regressions passed; no hardware execution')
