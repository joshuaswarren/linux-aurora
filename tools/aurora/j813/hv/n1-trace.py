"""Passive N1 PCIe journal for the exact M5 guest qualification.

Load through hv.run_script before guest entry. It observes the guest's own
config transactions; it never probes powered-down BARs or writes hardware.
This first capture layer records register ordering, not DMA packet contents.
"""
import json
import os
from pathlib import Path
import time

from m1n1.trace import TraceMode
from m1n1.utils import irange


class N1PCIJournal:
    MAX_EVENTS = 100_000
    IDENT = "M5N1Journal"

    def __init__(self, hv, output):
        if int(hv.adt["/chosen"].chip_id) != 0x8142:
            raise RuntimeError("N1 journal requires the M5 target")
        if not getattr(hv, 'j813_kernelcache_verified', False):
            raise RuntimeError("N1 journal requires the verified M5 kernel profile")
        self.hv = hv
        self.apcie = hv.adt["/arm-io/apcie"]
        # The previously enumerated N1 is bus 1, device 0. Observe all eight
        # functions, but arm BAR tracing only after a matching identity read.
        self.ecam = int(self.apcie.get_reg(0)[0]) + (1 << 20)
        self.functions = {}
        self.count = 0
        self.closed = False
        self.output = open(os.open(output, os.O_WRONLY | os.O_CREAT | os.O_EXCL,
                                   0o600), "w", buffering=1)

    def record(self, kind, **fields):
        if self.closed:
            return
        self.count += 1
        self.output.write(json.dumps({"seq": self.count, "kind": kind, **fields}) + "\n")
        if self.count >= self.MAX_EVENTS:
            self.stop()
            self.hv.log("N1 journal limit reached; capture stopped")

    def start(self):
        self.hv.add_tracer(irange(self.ecam, 8 * 0x1000), self.IDENT,
                           TraceMode.SYNC, read=self.config, write=self.config)
        self.hv.log("N1 passive PCIe journal armed; DMA payload capture not enabled")

    def attach_enumerated(self, fn, product, bar_size, prearm=False):
        """Attach after the guest log proves enumeration and BAR sizing.

        25G76 AppleEmbeddedPCIEPort may use PIODMA for config access, which
        bypasses CPU ECAM traps. Read only the already enumerated function's
        identity, command and BAR0; never issue a BAR sizing write or probe
        a powered-down function. The caller supplies the size seen in the
        native guest's PCI enumeration log. prearm permits a passive trap
        on an assigned BAR before memory decoding is enabled; it never
        reads that BAR. Active context reads must recheck live decoding.
        """
        if self.closed or self.hv.ctx is None:
            raise RuntimeError('Attach requires an active journal and paused guest')
        if (fn not in range(8) or product not in (0x1900, 0x1901, 0x1902, 0x1903)
                or bar_size < 16 or bar_size > 0x1000000
                or bar_size & (bar_size - 1)):
            raise RuntimeError('Invalid enumerated N1 function or BAR size')
        base = self.ecam + fn * 0x1000
        identity = self.hv.p.read32(base)
        if identity != (product << 16) | 0x106b:
            raise RuntimeError('Enumerated N1 identity is no longer present')
        command = self.hv.p.read32(base + 4)
        if not command & 2 and not prearm:
            raise RuntimeError('Guest has not enabled N1 memory decoding')
        low = self.hv.p.read32(base + 0x10)
        if low & 1 or low in (0, 0xffffffff, 0xfffffff0):
            raise RuntimeError('Guest has no assigned N1 memory BAR')
        words = {0: identity, 4: command, 0x10: low}
        if low & 6 == 4:
            words[0x14] = self.hv.p.read32(base + 0x14)
        if self.hv.p.read32(base) != identity:
            raise RuntimeError('N1 identity changed during attach')
        state = {'bytes': {}, 'size': bar_size, 'probe': False, 'mapped': None,
                 'prearm': bool(prearm and not command & 2)}
        for reg, value in words.items():
            for i in range(4):
                state['bytes'][reg + i] = (value >> (8 * i)) & 0xff
        if fn in self.functions:
            self.unmap(fn, self.functions[fn])
        self.functions[fn] = state
        self.record('attach-enumerated', fn=fn, product=product, bar_size=bar_size,
                    prearm=state['prearm'], memory_enabled=bool(command & 2))
        self.update(fn, state)
        if state['mapped'] is None:
            raise RuntimeError('Assigned N1 BAR does not fit one ADT PCI window')
        return state['mapped']

    def stop(self):
        self.closed = True
        self.hv.clear_tracers(self.IDENT)
        for fn in range(8):
            self.hv.clear_tracers(f"{self.IDENT}.bar0.{fn}")
        self.output.close()

    def config(self, evt):
        width = 1 << int(evt.flags.WIDTH)
        offset = int(evt.addr) - self.ecam
        if self.closed or width > 8 or not 0 <= offset < 8 * 0x1000:
            return
        fn, reg = divmod(offset, 0x1000)
        write = bool(evt.flags.WRITE)
        value = int(evt.data)
        self.record("config", fn=fn, reg=reg, width=width, write=write,
                    value=value, pc=int(evt.pc))
        if self.closed:
            return
        state = self.functions.setdefault(fn, {"bytes": {}, "size": None,
                                                "probe": False, "mapped": None})
        # A BAR sizing read is a mask, not a newly assigned address.
        if reg == 0x10 and width == 4 and not write and state["probe"]:
            mask = value & 0xfffffff0
            size = ((~mask) & 0xffffffff) + 1
            if mask and size & (size - 1) == 0 and 16 <= size <= 0x1000000:
                state["size"] = size
            return
        if reg == 0x10 and width == 4 and write and value == 0xffffffff:
            state["probe"] = True
            self.unmap(fn, state)
            return
        if reg <= 0x10 < reg + width and write:
            state["probe"] = False
        for i in range(width):
            state["bytes"][reg + i] = (value >> (i * 8)) & 0xff
        if reg <= 4 < reg + width:
            # Once a real command transaction is observed, follow its
            # enable/disable lifecycle rather than retaining the prearm.
            state['prearm'] = False
        self.update(fn, state)

    @staticmethod
    def word(state, offset, size):
        values = [state["bytes"].get(offset + i) for i in range(size)]
        if any(value is None for value in values):
            return None
        return sum(value << (8 * i) for i, value in enumerate(values))

    def unmap(self, fn, state):
        self.hv.clear_tracers(f"{self.IDENT}.bar0.{fn}")
        state["mapped"] = None

    def update(self, fn, state):
        vendor, product = self.word(state, 0, 2), self.word(state, 2, 2)
        command, low = self.word(state, 4, 2), self.word(state, 0x10, 4)
        if vendor != 0x106b or product not in (0x1900, 0x1901, 0x1902, 0x1903):
            self.unmap(fn, state)
            return
        if (command is None or (not command & 2 and not state.get('prearm'))
                or low is None or low & 1):
            self.unmap(fn, state)
            return
        if state["probe"] or state["size"] is None:
            return
        address = low & 0xfffffff0
        if low & 6 == 4:
            high = self.word(state, 0x14, 4)
            if high is None:
                return
            address |= high << 32
        if not address or address & (state["size"] - 1):
            self.unmap(fn, state)
            return
        matches = []
        for window in self.apcie.ranges:
            flags, low_bus, high_bus = map(int, window.bus_addr)
            bus_start = (high_bus << 32) | low_bus
            if (flags >> 24) & 3 not in (2, 3):
                continue
            if bus_start <= address and address + state["size"] <= bus_start + int(window.size):
                matches.append(int(self.apcie.translate((flags << 64) | address)))
        if len(matches) != 1 or not 0 < matches[0] < (1 << 52):
            self.unmap(fn, state)
            return
        start = matches[0]
        span = min(state["size"], 0x10000)
        if state["mapped"] == (start, span):
            return
        self.unmap(fn, state)
        state["mapped"] = (start, span)
        self.record("bar0", fn=fn, product=product, start=start,
                    size=state["size"], traced=span)
        if not self.closed:
            self.hv.add_tracer(irange(start, span), f"{self.IDENT}.bar0.{fn}",
                               TraceMode.ASYNC, read=self.mmio, write=self.mmio,
                               fn=fn, start=start)

    def mmio(self, evt, fn, start):
        self.record("mmio", fn=fn, reg=int(evt.addr) - start,
                    width=1 << int(evt.flags.WIDTH), write=bool(evt.flags.WRITE),
                    value=int(evt.data), pc=int(evt.pc))


if "hv" in globals():
    if hasattr(hv, 'n1_journal') and not hv.n1_journal.closed:
        raise RuntimeError('N1 journal is already active')
    trace_dir = Path(__file__).resolve().parent / "logs" / "n1-traces"
    trace_dir.mkdir(mode=0o700, parents=True, exist_ok=True)
    os.chmod(trace_dir, 0o700)
    n1_journal = N1PCIJournal(hv, trace_dir / f"n1-{time.time_ns()}.jsonl")
    hv.n1_journal = n1_journal
    n1_journal.start()
