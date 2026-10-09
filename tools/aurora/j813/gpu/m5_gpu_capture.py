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
    MAX_CAPTURE = 0x400000

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

    def read_context(self, pointer, size, context, gpu_region):
        """Read a command's recorded context, retaining every DRAM guard."""
        if not 0 <= context < 256 or not 0 < size <= self.MAX_CAPTURE:
            raise ValueError('Invalid M5 context capture request')
        # The root pair is in the ADT's existing iBoot-owned gpu-region table.
        # Only the firmware-global view uses the separate SPTM root receipt.
        reader = G17PUATReader(self._read_dram, gpu_region)
        return reader.read(pointer, size, context=context)

    def mapping(self, pointer, size, context=None, gpu_region=None):
        """Record bounded DRAM translations, without reading mapped data."""
        if not 0 < size <= self.MAX_CAPTURE:
            raise ValueError('Invalid M5 mapping extent')
        if context is not None and (not 0 <= context < 256 or gpu_region is None):
            raise ValueError('Invalid M5 mapping context')
        reader = self.walker if context is None else G17PUATReader(self._read_dram, gpu_region)
        result = []
        offset = 0
        while offset < size:
            address = pointer + offset
            length = min(size - offset, self.PAGE_SIZE - (address & (self.PAGE_SIZE - 1)))
            physical, walk = reader.translate(context or 0, address)
            if not self.low <= physical < physical + length <= self.high:
                raise ValueError('M5 mapping attempted a non-DRAM view')
            result.append(dict(address=hex(address), physical=hex(physical), size=length,
                               walk=[[hex(slot), hex(entry)] for slot, entry in walk]))
            offset += length
        return result

    def capture(self, role, init_message, output, phase):
        if role not in ('gfx-asc', 'gfx1-asc') or (init_message >> 48) & 0xff != 0x81:
            raise ValueError('Expected an M5 initialization doorbell')
        output = Path(output)
        output.mkdir(mode=0o700, parents=True, exist_ok=False)
        objects, unmapped_views, total = [], [], 0

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
        cluster = save('shared-cluster', hardware, 0x28000)
        if struct.unpack_from('<I', cluster, 0xe90)[0] != 0x8142:
            raise ValueError('Hardware-data chip identity differs from M5')
        # M5 provisions four completion queues, including ordinals 1 and 3
        # which the older Neo profile leaves absent. The native descriptors
        # independently specify 0x800 entries of 0x40 bytes each.
        for ordinal in range(4):
            offset = 0x2614 + ordinal * 0x20
            low, high, count, entry_size, recorded_ordinal = struct.unpack_from('<QQIIQ', cluster, offset)
            if count != 0x800 or entry_size != 0x40 or recorded_ordinal != ordinal or not low:
                raise ValueError('M5 completion queue descriptor changed')
            save('completion-' + str(ordinal), high, count * entry_size)
        # 25G76 AGXQOSManager::init allocates and clears exactly 0xe40 bytes
        # (host native VAs 0xfffffe0008a0d748 and 0xfffffe0008a0d934).
        save('qos', word(cluster, 0x26ac), 0xe40)
        # AGXFirmware::allocFirmwareData descriptor __const 0x71398d8:
        # AON PTD is 0xe4 bytes. AGXArmFirmware::initPowerAndPerformanceData
        # stores its GPU address at policy+0x6460 (cluster+0xb6a0).
        save('aon-ptd', word(cluster, 0xb6a0), 0xe4)
        save('region-a', word(root, 8), 0x4000)
        save('region-c', word(root, 0x20), 0x1000)
        save('status-a', word(root, 0xa8), 0x80)
        if role == 'gfx-asc':
            status_b = word(root, 0xb0)
            if word(root, 0xa8) - status_b != 0xeb00:
                raise ValueError('M5 primary status object extent changed')
            save('status-b', status_b, 0xeb00)
            save('status-config', status_b + 0xe440, 0x6a8)
            # Bounded mapped-DRAM observations of the external ownership
            # graph. These are capture views, not inferred allocation sizes.
            # Every leaf still goes through _read_dram and is read twice.
            state_grid = word(main, 0x20) - 0xc0
            if status_b != state_grid + 0x340:
                raise ValueError('M5 primary channel/status relationship changed')
            save('private-cluster-view', state_grid, 0x178000)
            for label, offset in (('sentinel-page', 0x2d0),
                                  ('pb-descriptor-page', 0x2e0),
                                  ('uma-descriptor-page', 0x2f0)):
                # Native can publish these future descriptor addresses
                # before creating their mappings. Preserve that distinction
                # instead of substituting zero pages or aborting the graph.
                pointer = word(main, offset)
                try:
                    self.walker.translate(0, pointer)
                except RuntimeError as error:
                    unmapped_views.append(dict(label=label, pointer=hex(pointer),
                                               reason=str(error)))
                else:
                    save(label, pointer, 0x4000)
            status = (output / 'status-b.bin').read_bytes()
            fwctl = word(status, 0x48e0)
            if word(status, 0x48e8) != fwctl + 0x40:
                raise ValueError('M5 firmware control state/ring relationship changed')
            save('fwctl-page', fwctl, 0x4000)
        else:
            for i, offset in enumerate((0xb8, 0xc0)):
                if word(root, offset):
                    save('secondary-extra-' + str(i), word(root, offset), 0x80)
            # Secondary +0xb8 aliases the primary's independent config
            # allocation. Its descriptor size is 0x6a8, rounded to 0x6c0.
            save('status-config', word(root, 0xb8), 0x6a8)
        manifest = dict(format='aurora-m5-gpu-init-v1', role=role, phase=phase,
                        version=hex(self.VERSION), uat_virtual_address_bits=43,
                        firmware_pointer_masked_to_40_bits=False, total_bytes=total,
                        objects=objects, unmapped_views=unmapped_views,
                        linux_runtime_admitted=False)
        (output / 'manifest.json').write_text(json.dumps(manifest, indent=2) + '\n')
        return manifest
