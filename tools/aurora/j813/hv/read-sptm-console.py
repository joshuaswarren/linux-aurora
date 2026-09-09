"""Read the bounded SPTM call ring from a paused M5 guest controller.

Use hv.run_script with this file. This reads resident RAM only. The result
is the tail of the early console, not a complete log or a successful boot.
"""
import ctypes as C
from pathlib import Path
import re

from tools.sptm.seed import _m1n1_symbol_offset


class Table(C.Structure):
    _fields_ = [("valid", C.c_uint8), ("entry_va", C.c_uint64),
                ("caller_mask", C.c_uint64)]


class SKState(C.Structure):
    _fields_ = [
        ("active", C.c_uint8), ("tables", Table * 2),
        ("dart_held", C.c_uint8 * 16), ("bootstrap_done", C.c_uint8),
        ("returns", C.c_uint64), ("final_cursor", C.c_uint64),
        ("last_cmd", C.c_uint64), ("last_cmd_x0", C.c_uint64),
        ("last_cmd_x1", C.c_uint64), ("panicked", C.c_uint8),
        ("calls", C.c_uint64), ("unknown_calls", C.c_uint64),
        ("retypes", C.c_uint64), ("logged", C.c_uint64),
        ("yields", C.c_uint64), ("ring", (C.c_uint64 * 5) * 64),
        ("ring_idx", C.c_uint64),
    ]


def console_tail(hv):
    if int(hv.adt["/chosen"].chip_id) != 0x8142 or hv.ctx is None:
        raise RuntimeError("Requires the paused M5 guest controller")
    source = Path(__file__).resolve().parent / "m1n1/src/sptm/sk.c"
    text = source.read_text()
    for name, expected in [("SK_MAX_TABLES", 2), ("SK_MAX_DARTS", 16),
                           ("SK_RING_SLOTS", 64)]:
        match = re.search(r"^#define\s+" + name + r"\s+(\d+)\s*$", text, re.M)
        if not match or int(match[1]) != expected:
            raise RuntimeError("SPTM ring ABI changed; revalidate the decoder")
    assert C.sizeof(SKState) == 2736 and SKState.ring.offset == 168
    address = hv.u.base + _m1n1_symbol_offset("g_sk")
    state = SKState.from_buffer_copy(hv.iface.readmem(address, C.sizeof(SKState)))
    chars = []
    for index in range(max(0, state.ring_idx - 64), state.ring_idx):
        x16, x0, *_ = state.ring[index % 64]
        if x16 == 0x2d and x0 < 256:
            chars.append(x0)
    print("SPTM_CONSOLE_TAIL", bytes(chars).decode("utf-8", errors="replace"))
    print("SPTM_RING_TOTAL_CALLS", state.ring_idx)


if "hv" in globals():
    console_tail(hv)
