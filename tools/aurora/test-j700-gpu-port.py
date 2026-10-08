#!/usr/bin/env python3
"""Run the port's existing host source/model checks and retain every output."""
import json
import os
from pathlib import Path
import subprocess
import sys
import time

root = Path(__file__).resolve().parents[2]
source_root = Path(os.environ.get("AURORA_TEST_SOURCE", root))
out = root / ".port-validation" / os.environ.get("AURORA_TEST_RUN", "current")
out.mkdir(parents=True, exist_ok=True)
tests = sorted((source_root / "drivers/gpu/drm/asahi/tests").glob("test*.py"))
tests += sorted((source_root / "drivers/gpu/drm/apple").glob("test-*.py"))
if len(sys.argv) > 1:
    tests = [p for p in tests if p.name in sys.argv[1:]]
results = []
for path in tests:
    start = time.monotonic()
    log = out / (path.stem + ".log")
    with log.open("w") as stream:
        try:
            result = subprocess.run(
                [sys.executable, str(path)], cwd=root,
                env={**os.environ, "PYTHONDONTWRITEBYTECODE": "1"},
                stdout=stream, stderr=subprocess.STDOUT, timeout=180,
            )
            code = result.returncode
        except subprocess.TimeoutExpired:
            code = 124
            stream.write("\nTest runner timeout after 180 seconds\n")
    results.append(dict(test=str(path.relative_to(source_root)), exit_code=code,
                        seconds=round(time.monotonic() - start, 2), log=str(log)))
    print(f"{'PASS' if code == 0 else 'FAIL'} {path.name} ({code})", flush=True)
    (out / "tests.json").write_text(json.dumps(results, indent=2) + "\n")
failed = sum(item["exit_code"] != 0 for item in results)
print(f"{len(results) - failed}/{len(results)} scripts passed; {failed} failed")
sys.exit(bool(failed))
