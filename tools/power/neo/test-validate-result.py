#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-2.0-only
"""Offline regression test for validate-result.py against saved Neo fixtures."""
import importlib.util
from pathlib import Path
import subprocess
import sys

FIXTURES = Path('/home/council/dev/dirtyroom_neo_gpu/artifacts/neo-suspend')
BAD = FIXTURES / 'nvme-phy/platform/result.json'
GOOD = (FIXTURES / 'pmp-gate-wait/short-00/result.json',
        FIXTURES / 'dcp-oslog-flush/reference-120/sleep/result.json',
        FIXTURES / 'dcp-oslog-flush/reference-120/result.json')


def load(name):
    spec = importlib.util.spec_from_file_location(name.replace('-', '_'),
                                                  Path(__file__).with_name(name + '.py'))
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def main():
    pm = load('pm-test')
    validator = load('validate-result')
    for path in (BAD, *GOOD):
        assert path.exists(), f'missing fixture {path}'

    try:
        validator.validate(pm, BAD)
    except RuntimeError as error:
        message = str(error)
    else:
        raise AssertionError(f'{BAD} was accepted')
    for expected in ('failed_resume +1', "last_failed_dev='0000:01:00.0'", "last_failed_step='resume'"):
        assert expected in message, f'{expected!r} not in {message!r}'

    for path in GOOD:
        summary = validator.validate(pm, path)
        assert not any(summary['deltas'].values()), (path, summary)
        assert summary['deltas']['failed_resume'] == 0, (path, summary)

    # Unreadable on one side is an error; missing on both sides is tolerated.
    before = {'files': {pm.SUSPEND_STATS + 'fail': '0'}}
    after = {'files': {pm.SUSPEND_STATS + 'fail': {'error': 'EACCES'}}}
    try:
        pm.check_suspend_stats(before, after)
    except RuntimeError:
        pass
    else:
        raise AssertionError('unreadable counter was accepted')
    assert pm.check_suspend_stats({'files': {}}, {'files': {}})['deltas'] == {}

    cli = subprocess.run([sys.executable, str(Path(__file__).with_name('validate-result.py')),
                          str(BAD), *map(str, GOOD)], capture_output=True, text=True)
    lines = cli.stdout.splitlines()
    assert cli.returncode == 1, cli
    assert lines[0].startswith(f'FAIL {BAD}'), lines
    assert all(line.startswith('PASS ') for line in lines[1:]), lines
    assert len(lines) == 1 + len(GOOD), lines

    cli = subprocess.run([sys.executable, str(Path(__file__).with_name('validate-result.py')),
                          *map(str, GOOD)], capture_output=True, text=True)
    assert cli.returncode == 0, cli
    print(f'ok: {BAD.relative_to(FIXTURES)} rejected, {len(GOOD)} known-good fixtures accepted')


if __name__ == '__main__':
    main()
