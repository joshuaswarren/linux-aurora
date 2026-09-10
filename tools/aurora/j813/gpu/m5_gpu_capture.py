# SPDX-License-Identifier: MIT
"""Read-only M5 firmware graph capture through the recorded SPTM global root."""
import hashlib
import json
from pathlib import Path
import struct

from m1n1.agx.g17p_trace import G17PUATReader


class M5GPUCapture:
    VERSION = 0x0c8aa322039e04c0
    PAGE_SIZE = 0x4000
    MAX_CAPTURE = 0x100000

    def __init__(self, read_phys, dram, global_root):
        self.read_phys = read_phys
        self.low, size = dram
        self.high = self.low + size
        self.global_root = global_root
        if global_root % self.PAGE_SIZE or not self.low <= global_root < self.high:
            raise ValueError('Invalid recorded M5 SPTM global root')
        self.walker = G17PUATReader(self._read_dram, 0, read_root=self._root_selector)

    def _root_selector(self, address, size):
        # There is no user half in this explicit global view. Address 8 is
        # the walker's root-selector slot, not a physical memory access.
        if address != 8 or size != 8:
            raise ValueError('Firmware pointer lost its global root-select bit')
        return struct.pack('<Q', self.global_root | 1)

    def _read_dram(self, address, size):
        if (size <= 0 or size > self.PAGE_SIZE or
                not self.low <= address < address + size <= self.high):
            raise ValueError('GPU capture attempted a non-DRAM access')
        # This full-RAM Normal-NC alias is installed by the qualified M5
        # carrier's sptm_emul_init. No mappings or cache state are modified.
        data = self.read_phys(address | 0xa0000000000, size)
        if len(data) != size:
            raise ValueError('Short M5 GPU DRAM read')
        return data

    def read(self, pointer, size):
        if not 0 < size <= self.MAX_CAPTURE:
            raise ValueError('Invalid GPU capture extent')
        return self.walker.read(pointer, size)

    def capture(self, role, init_message, output, phase):
        if role not in ('gfx-asc', 'gfx1-asc') or (init_message >> 48) & 0xff != 0x81:
            raise ValueError('Expected an M5 initialization doorbell')
        output = Path(output)
        output.mkdir(mode=0o700, parents=True, exist_ok=False)
        objects, total = [], 0

        def save(label, pointer, size):
            nonlocal total
            total += size
            if total > self.MAX_CAPTURE:
                raise ValueError('M5 GPU capture exceeds its byte budget')
            physical, walk = self.walker.translate(0, pointer)
            data = self.read(pointer, size)
            if data != self.read(pointer, size):
                raise ValueError('M5 initialization object changed during capture: ' + label)
            name = label + '.bin'
            (output / name).write_bytes(data)
            objects.append(dict(label=label, pointer=hex(pointer), physical=hex(physical),
                                global_root=hex(self.global_root), walk=walk[1:], size=size,
                                sha256=hashlib.sha256(data).hexdigest(), file=name))
            return data

        pointer = init_message & ((1 << 44) - 1)
        root = save('root', pointer, 0xc8 if role == 'gfx1-asc' else 0xb8)
        word = lambda data, offset: struct.unpack_from('<Q', data, offset)[0]
        if word(root, 0) != self.VERSION or struct.unpack_from('<I', root, 0x30)[0] != 0x030e4000:
            raise ValueError('M5 firmware version/geometry differs from the verified reference')
        main_address = word(root, 0x18)
        main = save('main-config', main_address, 0x600)
        hardware = word(main, 0)
        expected_main_offset = 0x22a80 if role == 'gfx1-asc' else 0x1e5c0
        if main_address - hardware != expected_main_offset:
            raise ValueError('M5 main configuration left the measured shared cluster')
        if role == 'gfx-asc':
            if [word(main, 0x254 + i * 8) - hardware for i in range(5)] != [0x2740, 0x3380, 0x4400, 0xbc80, 0xbd00]:
                raise ValueError('M5 hardware bundle views changed')
        save('shared-cluster', hardware, 0x28000)
        save('region-a', word(root, 8), 0x4000)
        save('region-c', word(root, 0x20), 0x1000)
        save('status-a', word(root, 0xa8), 0x80)
        if role == 'gfx-asc':
            status_b = word(root, 0xb0)
            if word(root, 0xa8) - status_b != 0xeb00:
                raise ValueError('M5 primary status object extent changed')
            save('status-b', status_b, 0xeb00)
        else:
            for i, offset in enumerate((0xb8, 0xc0)):
                if word(root, offset):
                    save('secondary-extra-' + str(i), word(root, offset), 0x80)
        manifest = dict(format='aurora-m5-gpu-init-v1', role=role, phase=phase,
                        version=hex(self.VERSION), uat_virtual_address_bits=43,
                        firmware_pointer_masked_to_40_bits=False, total_bytes=total,
                        objects=objects, linux_runtime_admitted=False)
        (output / 'manifest.json').write_text(json.dumps(manifest, indent=2) + '\n')
        return manifest
