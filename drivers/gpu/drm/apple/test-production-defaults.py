#!/usr/bin/env python3
"""Keep qualified DCP ABI choices fixed and hot paths free of delay loops."""

from pathlib import Path

root = Path(__file__).parent
iomfb = (root / "iomfb.c").read_text()
internal = (root / "iomfb_internal.h").read_text()
template = (root / "iomfb_template.c").read_text()
dcp = (root / "dcp.c").read_text()
backlight = (root / "dcp_backlight.c").read_text()

assert "#define DCP_CLAMSHELL_STATE 1U" in internal
assert "dcp_clamshell_state" not in iomfb + internal + template
assert "dcp_send_modeset" not in iomfb + internal + template
assert "dcp_park_display" not in iomfb + internal + template
assert "complete_set_digital_out_mode(dcp, NULL, cookie);\n#else" in template

vblank = dcp[dcp.index("static void dcp_delayed_vblank("):]
vblank = vblank[:vblank.index("\n}")]
assert "mdelay" not in vblank
assert "to_delayed_work" in vblank
assert "msleep" not in backlight
assert "mod_delayed_work(system_wq, &dcp->bl_update_wq" in template

print("PASS DCP production defaults: fixed H17P ABI and no busy waits in vblank/brightness work")
