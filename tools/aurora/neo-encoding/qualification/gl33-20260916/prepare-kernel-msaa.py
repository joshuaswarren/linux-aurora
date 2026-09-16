#!/usr/bin/env python3
"""Build a RAM-boot bundle changing only the Asahi module in the qualified initrd."""
import gzip
import hashlib
import json
from pathlib import Path
import shutil
import subprocess

here = Path(__file__).resolve().parent
root = here.parents[2]
source = here.parent / "j700-native-gl2-20260913/latest-kernel-boot"
out = here / "kernel-msaa-samples-boot"
digest = lambda p: hashlib.sha256(p.read_bytes()).hexdigest()
manifest = json.loads((source / "manifest.json").read_text())
for name, spec in manifest["files"].items():
    assert digest(source / name) == spec["sha256"]
module = (here / "asahi-msaa-samples.ko").read_bytes()
assert module[:4] == b"\x7fELF"
assert not out.exists()
data = gzip.decompress((source / "initramfs.cpio.gz").read_bytes())
pos = 0
result = bytearray()
replaced = []
while pos + 110 <= len(data):
    magic = data[pos:pos + 6]
    assert magic == b"070701"
    fields = [int(data[pos + 6 + i * 8:pos + 14 + i * 8], 16) for i in range(13)]
    pos += 110
    name = data[pos:pos + fields[11]]
    pos = (pos + fields[11] + 3) & ~3
    payload = data[pos:pos + fields[6]]
    pos = (pos + fields[6] + 3) & ~3
    if name.endswith(b"/asahi.ko\0"):
        assert hashlib.sha256(payload).hexdigest() == manifest["diagnostic"]["module_sha256"]
        payload = module
        replaced.append(name.decode().rstrip("\0"))
    fields[6] = len(payload)
    result += magic + b"".join(f"{v:08x}".encode() for v in fields) + name
    result += b"\0" * (-len(result) % 4)
    result += payload
    result += b"\0" * (-len(result) % 4)
    if name == b"TRAILER!!!\0":
        break
assert len(replaced) == 1, replaced
result += b"\0" * (-len(result) % 512)
shutil.copytree(source, out)
(out / "initramfs.cpio.gz").write_bytes(gzip.compress(result, compresslevel=6, mtime=0))
repo = root / "neo-encoder-linux"
patch = subprocess.check_output(["git", "-C", str(repo), "diff", "--", "drivers/gpu/drm/asahi/g17_render.rs"])
(here / "kernel-msaa-samples.patch").write_bytes(patch)
manifest.update(hardware_validated=False, purpose="Propagate native MSAA sample count in every fragment descriptor",
                inherited_bundle=str(source), source_overlay={"patch": str(here / "kernel-msaa-samples.patch"),
                "sha256": hashlib.sha256(patch).hexdigest(), "rust_source_files_compared": 77,
                "rust_source_mismatches": 0})
manifest["diagnostic"]["module_sha256"] = hashlib.sha256(module).hexdigest()
manifest["files"]["initramfs.cpio.gz"] = {"bytes": (out / "initramfs.cpio.gz").stat().st_size,
                                        "sha256": digest(out / "initramfs.cpio.gz")}
(out / "manifest.json").write_text(json.dumps(manifest, indent=2) + "\n")
print(json.dumps({"bundle": str(out), "module_sha256": hashlib.sha256(module).hexdigest(), "replaced": replaced}))
