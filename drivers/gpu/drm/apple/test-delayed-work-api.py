#!/usr/bin/env python3
"""Keep DCP delayed-work initialization, scheduling, and cancellation typed."""
from pathlib import Path

root = Path(__file__).parent
dcp = (root / "dcp.c").read_text()
iomfb = (root / "iomfb.c").read_text()
template = (root / "iomfb_template.c").read_text()

assert "INIT_DELAYED_WORK(&dcp->bl_update_wq" in dcp
assert "cancel_delayed_work_sync(&dcp->vblank_wq)" in dcp
assert "INIT_WORK(&dcp->bl_update_wq" not in dcp
assert "cancel_work_sync(&dcp->vblank_wq)" not in dcp
assert "schedule_work(&dcp->vblank_wq)" not in iomfb + template
assert "warned_busy" not in iomfb
assert "dev_err_ratelimited(dcp->dev," in iomfb

print("PASS DCP delayed-work API: typed initialization, scheduling, and synchronous cancellation")
