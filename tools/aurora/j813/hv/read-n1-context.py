"""Read a paused, enumerated N1 context through its existing DART mappings.

No DART initialization, mapping, invalidation command or device writes.
Use only with the RID-to-SID association reported by the current guest.
Payload data stays in a private local capture.
"""
import json
import os
from pathlib import Path
import struct
import time

from m1n1.hw.dart import DART


class RAMReader:
    def __init__(self, iface, base, size):
        self.iface, self.base, self.end = iface, base, base + size

    def readmem(self, address, size):
        if not (0 < size <= 0x10000 and self.base <= address
                and address + size <= self.end):
            raise RuntimeError('N1 DMA or page-table read falls outside DRAM bounds')
        return self.iface.readmem(address, size)


def read_context(hv, fn, stream, message_payloads=False, extra_reads=()):
    if (hv.ctx is None or int(hv.u.adt['/chosen'].chip_id) != 0x8142 or
            not getattr(hv, 'j813_kernelcache_verified', False)):
        raise RuntimeError('Requires paused exact M5 guest')
    if fn not in range(8) or not 0 < stream < 256:
        raise RuntimeError('Requires a PCI function and an explicit nonzero native SID')
    journal = hv.n1_journal
    state = journal.functions.get(fn)
    if journal.closed or not state or not state['mapped']:
        raise RuntimeError('Attach the enumerated N1 BAR first')
    product = journal.word(state, 2, 2)
    if product not in (0x1900, 0x1901, 0x1902):
        raise RuntimeError('N1 function is not a control or alpha context')
    # A passive prearm may precede MEM enable, and the device can disappear
    # during centaurid's reset. Refresh identity, command and assigned BAR
    # before any register/payload read; prearmed state is not proof of life.
    bar, span = journal.attach_enumerated(fn, product, state['size'])
    # Native IODeviceTree and guest37: ROM control 1900 becomes runtime
    # control 1901 at function 0; alpha is 1902 at function 1, beta 1903
    # at function 2. The original ROM BAR can be reassigned to beta.
    # Select the layout from the verified product, not 1900 + function.
    status_reg, context_reg = (0xc, 0x10) if product == 0x1902 else (0x8050, 0x8054)
    if context_reg + 8 > span:
        raise RuntimeError('Context registers exceed the enumerated BAR')
    status = hv.p.read32(bar + status_reg)
    if status != 2:
        raise RuntimeError(f'N1 IPC handshake is not complete: status={status:#x}')
    low = hv.p.read32(bar + context_reg)
    high = hv.p.read32(bar + context_reg + 4)
    iova = low | high << 32
    if not iova or iova & 7 or iova >= 1 << 44:
        raise RuntimeError('Invalid N1 context DMA address')
    # BootArgs.phys_base starts the usable loader range, not physical DRAM.
    # Guest30 measured 0x10003084000 there. Firmware-owned DART tables can
    # sit outside that usable range, so use the native DRAM base and size.
    ram_base = int(hv.u.adt['/chosen'].dram_base)
    ram_size = int(hv.u.ba.mem_size_actual)
    if ram_base != 0x10000000000 or not 0 < ram_size <= 0x1000000000:
        raise RuntimeError('Unexpected M5 DRAM geometry')
    if not (ram_base <= int(hv.u.ba.phys_base)
            < int(hv.u.ba.phys_base) + int(hv.u.ba.mem_size)
            <= ram_base + ram_size):
        raise RuntimeError('M5 usable memory exceeds the native DRAM geometry')
    reader = RAMReader(hv.iface, ram_base, ram_size)
    dart = DART.from_adt(hv.u, '/arm-io/dart-apcie0')
    dart.iface = dart.dart.iface = reader
    dart.invalidate_cache()  # Python page-table cache only; no MMIO writes.
    captures = []

    def read(label, address, size):
        if not address or not 0 < size <= 0x4000 or address + size > 1 << 44:
            raise RuntimeError('Invalid bounded N1 DMA request')
        ranges = dart.iotranslate(stream, address, size)
        if sum(length for _, length in ranges) != size or any(pa is None for pa, _ in ranges):
            raise RuntimeError('N1 DMA buffer is not fully mapped')
        # Validate every translation before fetching any payload fragment.
        if any(not reader.base <= pa < pa + length <= reader.end for pa, length in ranges):
            raise RuntimeError('N1 DMA translation is outside DRAM')
        raw = b''.join(reader.readmem(pa, length) for pa, length in ranges)
        captures.append(dict(label=label, iova=address, size=size,
                             ranges=ranges, hex=raw.hex()))
        return raw

    header = read('context', iova, 0x68)
    version, length = struct.unpack_from('<HH', header)
    cr, tr = struct.unpack_from('<HH', header, 0x30)
    cr_slots, tr_slots = struct.unpack_from('<HH', header, 0x44)
    if (version != 768 or length != 0x68 or not 0 < cr <= 512
            or not 0 < tr <= 512 or cr_slots != 16 or tr_slots != 16):
        raise RuntimeError('Native N1 context differs from the known header format')
    for index, count in enumerate((cr, tr, cr, tr)):
        read(f'index-{index}', struct.unpack_from('<Q', header, 0x10 + 8 * index)[0], count * 2)
    read('message-cr', struct.unpack_from('<Q', header, 0x34)[0], cr_slots * 16)
    message_tr = read('message-tr', struct.unpack_from('<Q', header, 0x3c)[0], tr_slots * 16)
    if message_payloads:
        # These are retained transfer descriptors, not necessarily pending
        # commands. Capture only the bounded message format observed in the
        # native ring; keep slot and raw bytes so order is not inferred.
        for slot in range(tr_slots):
            flags, address, cookie = struct.unpack_from('<IQI', message_tr, slot * 16)
            if not flags and not address and not cookie:
                continue
            length = flags >> 8
            if flags & 0xff != 1 or not 0 < length <= 512:
                raise RuntimeError('Unexpected bounded native message descriptor')
            read(f'message-tr-payload-{slot}', address, length)
    # Callers must obtain ring/payload addresses from this guest's native
    # descriptors. Each explicit read retains the same translation and DRAM
    # checks as the context; this never scans or guesses neighboring buffers.
    for label, address, size in extra_reads:
        read(str(label), int(address), int(size))
    directory = Path(__file__).resolve().parent / 'logs/n1-dma'
    directory.mkdir(mode=0o700, parents=True, exist_ok=True)
    os.chmod(directory, 0o700)
    path = directory / f'context-fn{fn}-sid{stream}-{time.time_ns()}.json'
    with open(os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600), 'w') as output:
        json.dump(dict(fn=fn, product=product, stream=stream, dart='/arm-io/dart-apcie0',
                       context_iova=iova, completion_rings=cr, transfer_rings=tr,
                       captures=captures), output, indent=2)
        output.write('\n')
    print('M5_N1_CONTEXT_SAVED_PRIVATE', str(path))
    return path
