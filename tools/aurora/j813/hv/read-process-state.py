"""Bounded, read-only PID lookup for the hash-verified 25G76 M5 kernel."""
import struct


def lookup_process(hv, pid):
    if (hv.ctx is None or hv.ctx.spsr.M == 0 or
            int(hv.u.adt['/chosen'].chip_id) != 0x8142 or
            not getattr(hv, 'j813_kernelcache_verified', False) or
            not 0 < pid <= 999999):
        raise RuntimeError('PID lookup requires a paused exact M5 kernel mapping')
    dram = int(hv.u.adt['/chosen'].dram_base)
    end = dram + int(hv.u.ba.mem_size_actual)
    def read(address, size):
        if not 0 < size <= 0x1000 or address >> 48 != 0xffff:
            raise RuntimeError('Invalid bounded kernel data read')
        points = {address, address + size - 1}
        points.update(range((address + 0x4000) & ~0x3fff, address + size, 0x4000))
        for point in points:
            pa = hv.p.hv_translate(point, False, False)
            if not dram <= pa < end:
                raise RuntimeError('Process data is not mapped to DRAM')
        return hv.readmem(address, size)
    canonical = lambda p: (p & 0xffffffffffff) | 0xffff000000000000
    # Exact proc_find at bd8d39c: hash32 and SMR bucket encoding. We only
    # traverse while all guest CPUs are stopped; no SMR/refcount writes.
    encoded, = struct.unpack('<Q', read(0xfffffe000c737e80, 8))
    mask = 0xffffffffffffffff >> ((encoded >> 48) & 63)
    if not 0 < mask <= 0xffff or mask & (mask + 1):
        raise RuntimeError('Unexpected native process hash geometry')
    value = pid ^ (pid >> 16)
    value = (value * 0x7feb352d) & 0xffffffff
    value ^= value >> 15
    value = (value * 0x846ca68b) & 0xffffffff
    value ^= value >> 16
    node, = struct.unpack('<Q', read(canonical(encoded) + (value & mask) * 8, 8))
    seen = set()
    while node:
        node = canonical(node)
        if node in seen or len(seen) >= 128:
            raise RuntimeError('Process hash cycle or traversal limit')
        seen.add(node)
        found_pid, = struct.unpack('<I', read(node - 0x40, 4))
        if found_pid == pid:
            proc = node - 0xa0
            # Exact proc_name at bd90474 uses proc + 0x5cc.
            name = read(proc + 0x5cc, 32).split(b'\0')[0].decode('utf-8', 'replace')
            return dict(pid=pid, proc_va=hex(proc), name=name, hash_nodes=len(seen))
        node, = struct.unpack('<Q', read(node, 8))
    return dict(pid=pid, present=False, hash_nodes=len(seen))
