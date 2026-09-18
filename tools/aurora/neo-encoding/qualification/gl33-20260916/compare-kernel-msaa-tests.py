#!/usr/bin/env python3
"""Compare the real descriptor tests with the unchanged source revision."""
import ast
import json
from pathlib import Path
import re
import subprocess
import tempfile

out = Path(__file__).resolve().parent
repo = out.parents[2] / "neo-encoder-linux"
relative = "drivers/gpu/drm/asahi/g17_render.rs"
runner = repo / "drivers/gpu/drm/asahi/tests/test_g17_render_samples.py"
assign = next(n for n in ast.parse(runner.read_text()).body
              if isinstance(n, ast.Assign) and any(isinstance(t, ast.Name) and t.id == "source" for t in n.targets))
prefix = ast.literal_eval(assign.value)
results = {}
with tempfile.TemporaryDirectory(prefix="g17-msaa-compare-") as tmp:
    tmp = Path(tmp)
    baseline = tmp / "baseline.rs"
    baseline.write_bytes(subprocess.check_output(["git", "-C", str(repo), "show", "HEAD:" + relative]))
    for label, source in [("baseline", baseline), ("candidate", repo / relative)]:
        entry, binary = tmp / (label + "-entry.rs"), tmp / label
        entry.write_text(prefix + '#[path = "' + str(source) + '"] mod g17_render;\n')
        subprocess.run(["rustc", "-Awarnings", "--edition=2021", "--test", str(entry), "-o", str(binary)], check=True)
        run = subprocess.run([str(binary)], capture_output=True, text=True)
        (out / ("kernel-msaa-" + label + "-all.log")).write_text(run.stdout + run.stderr)
        results[label] = {
            "exit_code": run.returncode,
            "summary": next(l for l in run.stdout.splitlines() if l.startswith("test result:")),
            "failed": re.findall(r"^test (\S+) \.\.\. FAILED$", run.stdout, re.M),
        }
assert results["baseline"]["failed"] == results["candidate"]["failed"], results
candidate = (out / "kernel-msaa-candidate-all.log").read_text()
assert "fragment_sample_count_matches_tile_configuration_on_every_submission ... ok" in candidate
(out / "kernel-msaa-regressions.json").write_text(json.dumps(results, indent=2) + "\n")
print(json.dumps(results, indent=2))
