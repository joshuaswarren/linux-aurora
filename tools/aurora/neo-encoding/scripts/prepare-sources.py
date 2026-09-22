#!/usr/bin/env python3
"""Verify the pinned archives and unpack each source tree only once."""
import hashlib
import json
from pathlib import Path
import subprocess

shared = Path(__file__).resolve().parents[1]
sources = Path.home() / "neo/sources"
manifest = json.loads((shared / "artifacts/source-manifest.json").read_text())
for component, item in manifest.items():
    archive = shared / "artifacts" / item["archive"]
    digest = hashlib.sha256()
    with archive.open("rb") as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b""):
            digest.update(chunk)
    if archive.stat().st_size != item["bytes"] or digest.hexdigest() != item["sha256"]:
        raise SystemExit(f"Source archive verification failed: {component}")
    destination = sources / component
    stamp = destination / ".neo-source-commit"
    if stamp.exists():
        if stamp.read_text().strip() != item["commit"]:
            raise SystemExit(f"Source revision differs: {component}; preserve this tree")
        print(f"Preserving existing {component} source at {item['commit']}")
        continue
    destination.mkdir(parents=True, exist_ok=True)
    if any(destination.iterdir()):
        raise SystemExit(f"Unmarked nonempty source tree: {destination}; refusing to overwrite")
    subprocess.run(["tar", "-xzf", str(archive), "-C", str(destination)], check=True)
    stamp.write_text(item["commit"] + "\n")
    print(f"Prepared {component} at {item['commit']}")
