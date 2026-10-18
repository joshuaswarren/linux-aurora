#!/usr/bin/env python3
"""Exercise production priority-ring activation, including return to an old ring."""
from pathlib import Path
import subprocess
import tempfile

root = Path(__file__).resolve().parents[4]
queue = root / 'drivers/gpu/drm/asahi/g17/queue/compute.rs'
source = queue.with_suffix('') / 'activation.rs'
with tempfile.TemporaryDirectory(prefix='asahi-compute-activation-') as directory:
    binary = Path(directory) / 'activation'
    harness = Path(directory) / 'test.rs'
    harness.write_text(f'#[path = "{source}"] mod activation;\n')
    subprocess.run(['rustc', '--edition=2021', '--test', str(harness), '-o', str(binary)], check=True)
    subprocess.run([str(binary)], check=True)
    negative = Path(directory) / 'negative.rs'
    negative.write_text(source.read_text().replace(
        'if m5 { ring } else { started | ring }', 'if m5 { started | ring } else { started | ring }'))
    harness.write_text(f'#[path = "{negative}"] mod activation;\n')
    subprocess.run(['rustc', '--edition=2021', '--test', str(harness), '-o', str(binary)], check=True)
    result = subprocess.run([str(binary)], capture_output=True, text=True)
    assert result.returncode != 0
    assert 'returning_to_old_priority_reactivates ... FAILED' in result.stdout
    print('Historical membership negative control fails the return-to-priority test')
    stale = Path(directory) / 'stale_firmware.rs'
    stale.write_text(source.read_text().replace(
        'refresh: m5 && started != 0 && started & ring == 0', 'refresh: false'))
    harness.write_text(f'#[path = "{stale}"] mod activation;\n')
    subprocess.run(['rustc', '--edition=2021', '--test', str(harness), '-o', str(binary)], check=True)
    result = subprocess.run([str(binary)], capture_output=True, text=True)
    assert result.returncode != 0
    assert 'returning_to_old_priority_reactivates ... FAILED' in result.stdout
    print('Skipping the firmware refresh fails despite correct outer-ring reactivation')

# The publication plan is committed only after the outer-ring write succeeds;
# hardware configuration and outer-ring activation must use the same decision.
text = queue.read_text()
assert text.index('host.publish_outer(', text.index('let activation =')) < text.index('self.outer_started = activation.next_started;')
assert 'first || notify_activation' in text
assert 'self.outer_started |= ' not in text
assert 'first, activation.refresh,' in text
assert 'install: (first && !self.graph.m5) || priority_refresh,' in text
assert 'completion_seed: (first && self.graph.m5).then_some(0),' in text
