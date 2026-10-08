#!/usr/bin/env python3
"""Stage or restore the pinned J700 kernel candidate for the next Linux boot."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess

parser = argparse.ArgumentParser()
parser.add_argument("boot")
parser.add_argument("--restore", action="store_true")
args = parser.parse_args()
assert os.geteuid() == 0
assert Path("/proc/sys/kernel/random/boot_id").read_text().strip() == args.boot
assert b"apple,j700" in Path("/proc/device-tree/compatible").read_bytes().split(b"\0")
for process in ("Xorg", "glcts", "cts-runner", "plasmashell"):
    found = subprocess.run(["pgrep", "-x", process], capture_output=True, text=True)
    assert found.returncode in (0, 1)
    for pid in found.stdout.split():
        try:
            state = Path("/proc", pid, "stat").read_text().rsplit(") ", 1)[1].split()[0]
        except FileNotFoundError:
            continue
        # The campaign supervisor may not yet have reaped an exited server.
        assert state == "Z", (process, pid, state)

directory = Path(__file__).resolve().parent
module = Path("/lib/modules/7.1.6-aurora-neo-gpu1/kernel/drivers/gpu/drm/asahi/asahi.ko")
baseline = "6e8621eb0c4147778d243870907cffccdee5b9b74711516c451fdc9c88937cbe"
candidate = "f8d887c8c95251b98d11a54831fbdad5d6a0b6c73a9e8e0793f45fe9c898d0d0"
digest = lambda path: hashlib.sha256(path.read_bytes()).hexdigest()
backup = directory / ("asahi-backup-" + baseline + ".ko")
before, after = (candidate, baseline) if args.restore else (baseline, candidate)
source = backup if args.restore else directory / "asahi-kernel-visibility.ko"
assert digest(module) == before
assert digest(source) == after
assert subprocess.check_output(["uname", "-r"]).strip() == b"7.1.6-aurora-neo-gpu1"
assert subprocess.check_output(["modinfo", "-F", "vermagic", str(source)]) == subprocess.check_output(
    ["modinfo", "-F", "vermagic", str(module)]
)
if not args.restore:
    if not backup.exists():
        shutil.copy2(module, backup)
    assert digest(backup) == baseline
temporary = module.with_name("asahi.ko.visibility-staging")
with temporary.open("xb") as output:
    output.write(source.read_bytes())
    output.flush()
    os.fsync(output.fileno())
os.chmod(temporary, module.stat().st_mode & 0o777)
os.replace(temporary, module)
assert digest(module) == after
os.sync()
print(json.dumps(dict(staged=True, restore=args.restore, boot=args.boot,
                      next_boot_module_sha256=after, prior_module_sha256=before,
                      backup=str(backup), loaded_module_changed=False)))
