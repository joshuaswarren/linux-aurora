# SPDX-License-Identifier: MIT
"""One-shot, reversible program-page replay for a controlled native M5 job.

This is a diagnostic, not a GPU driver. The caller must stop the guest before
publication, arm the matching completion hook, and restore before guest return.
Three pinned 16 KiB program pages may be changed. An additional explicit
dispatch variant changes only five identity fields in the captured descriptor.
A separate +6 native capture pins the program contents; Linux's +5 pages
provide the replacement. Every changed byte is restored through its journal.
"""
import hashlib
import json
from pathlib import Path
import struct


PAGE = 0x4000
PROGRAMS = (
    ('shader', 0x10000000000, 'compute-extra-0',
     'd6374b393ab3acc0fffc6c85a0b6985bc7e61a504d1f7a8f8c78fb168c343e7a'),
    ('pipeline', 0x10000090000, 'compute-extra-2',
     '03907a0ca3f1d61e88fb205fa4dd88b55b59e5408ef0b67239dcbb311c33b8a1'),
    ('stream', 0x100000b0000, 'compute-cdm',
     'b1d96038c73d87a375a9025cd0dd4d0af894764156818aeb3a61951663f54b3f'),
)
BUFFER = 0x1000001c000
PREEMPT = 0x100000e0000


def digest(data):
    return hashlib.sha256(data).hexdigest()


def load_capture(directory, label, address, native):
    directory = Path(directory)
    manifest = json.loads((directory / 'manifest.json').read_text())
    matches = [o for o in manifest['objects'] if o['label'] == label]
    if len(matches) != 1:
        raise ValueError('Replay capture must contain exactly one ' + label)
    item = matches[0]
    if (int(item['address'], 0) != address or item['size'] != PAGE
            or item.get('gpu_context' if native else 'context') != 3
            or (native and (not item.get('stable') or not item.get('mapping_stable')))):
        raise ValueError('Replay capture address, size, context or stability differs')
    path = (directory / item['file']).resolve()
    if path.parent != directory.resolve():
        raise ValueError('Replay capture file escaped its directory')
    data = path.read_bytes()
    if len(data) != PAGE or digest(data) != item['sha256']:
        raise ValueError('Replay capture hash differs: ' + label)
    return data


class M5ProgramReplay:
    def __init__(self, gpu, write_phys, sync_phys, context, gpu_region, output):
        if context != 3:
            raise ValueError('Replay requires the controlled context 3')
        self.gpu, self.write_phys, self.sync_phys = gpu, write_phys, sync_phys
        self.context, self.gpu_region = context, gpu_region
        self.output = Path(output)
        self.pages = []
        self.touched = []
        self.active = False
        self.receipt = dict(state='unprepared', gpu_program_only=True,
                            native_shader_add=6, replacement_shader_add=5,
                            pages=[], writes=[])

    def _record(self):
        (self.output / 'program-replay.json').write_text(
            json.dumps(self.receipt, indent=2) + '\n')

    def _mapping(self, address, size=PAGE, context=3):
        mapping = self.gpu.mapping(address, size, context, self.gpu_region)
        if (len(mapping) != 1 or int(mapping[0]['address'], 0) != address
                or mapping[0]['size'] != size):
            raise ValueError('Replay requires one bounded DRAM mapping')
        pa = int(mapping[0]['physical'], 0)
        if (pa % PAGE != address % PAGE
                or not self.gpu.low <= pa < pa + size <= self.gpu.high):
            raise ValueError('Replay physical page is unaligned or outside DRAM')
        return mapping, pa

    def _read(self, address, size=PAGE, context=3):
        data = (self.gpu.read(address, size) if context is None else
                self.gpu.read_context(address, size, context, self.gpu_region))
        if len(data) != size:
            raise ValueError('Short replay program read')
        return data

    def prepare(self, native, linux, workload, variant='linux-pages', descriptor_address=None):
        if self.pages or self.touched:
            raise ValueError('Replay is one-shot')
        if variant not in ('linux-pages', 'shader-body-only', 'shader-record', 'shader-prefix',
                           'shader-prefix-linux-dispatch'):
            raise ValueError('Unknown controlled replay variant')
        self.receipt['variant'] = variant
        result = json.loads(Path(workload).read_text())
        if any(result.get(k) != v for k, v in dict(shader_add=6, expected_add=6,
                queues=1, compute_values_verified=1024, render_pixels_verified=0).items()):
            raise ValueError('Replay baseline lacks the successful compute-only +6 result')
        manifest = json.loads((Path(native) / 'manifest.json').read_text())
        if (manifest.get('error') or manifest.get('completion_error')
                or not manifest.get('completion', {}).get('same_descriptor')
                or manifest.get('compute_context') != 3
                or int(manifest.get('entry_stamp', '0'), 0) != 1):
            raise ValueError('Replay baseline lacks first-stamp matching completion')
        completed = load_capture(native, 'completed-extra-1', BUFFER, True)
        if (struct.unpack_from('<1024I', completed, 0xd00) != tuple(i * 7 for i in range(1024))
                or struct.unpack_from('<1024I', completed, 0x1d00) != tuple(i * 21 + 6 for i in range(1024))):
            raise ValueError('Captured baseline buffers do not prove the +6 workload')
        for name, address, label, sha in PROGRAMS:
            original = load_capture(native, label, address, True)
            replacement = load_capture(linux, 'program-' + name, address, False)
            if digest(replacement) != sha:
                raise ValueError('Linux replay program differs from pinned owned bytes')
            if name == 'shader' and variant == 'shader-body-only':
                replacement = original[:0x3c0] + replacement[0x3c0:0x400] + original[0x400:]
            elif name == 'shader' and variant == 'shader-record':
                replacement = replacement[:0x340] + original[0x340:0x3c0] + replacement[0x3c0:]
            elif name == 'shader' and variant in ('shader-prefix', 'shader-prefix-linux-dispatch'):
                replacement = original[:0x3c0] + replacement[0x3c0:]
            mapping, pa = self._mapping(address)
            if original != self._read(address) or mapping != self._mapping(address)[0]:
                raise ValueError('Native program or translation differs from baseline: ' + name)
            self.pages.append(dict(name=name, address=address, pa=pa, mapping=mapping,
                                   original=original, replacement=replacement, size=PAGE, context=3))
        shader = self.pages[0]
        if shader['original'][0x3c0:0x400] == shader['replacement'][0x3c0:0x400]:
            raise ValueError('Replay must distinguish original and replacement shader bytes')
        # Exact input/output sentinels and the two argument pointers identify
        # this controlled workload independently of queue priority and code.
        buffer_mapping, buffer_pa = self._mapping(BUFFER)
        preempt_mapping, preempt_pa = self._mapping(PREEMPT)
        buffers = self._read(BUFFER)
        preempt = self._read(PREEMPT)
        if (struct.unpack_from('<1024I', buffers, 0xd00) != tuple(i * 7 for i in range(1024))
                or struct.unpack_from('<1024I', buffers, 0x1d00) != (0xdeadbeef,) * 1024
                or struct.unpack_from('<QQ', preempt, 0x14a0) != (BUFFER + 0xd00, BUFFER + 0x1d00)
                or buffer_mapping != self._mapping(BUFFER)[0]
                or preempt_mapping != self._mapping(PREEMPT)[0]):
            raise ValueError('Replay workload buffers or argument pointers differ')
        physical = [p['pa'] for p in self.pages] + [buffer_pa, preempt_pa]
        table_pages = {int(slot, 0) & ~(PAGE - 1)
                       for p in self.pages for part in p['mapping'] for slot, _ in part['walk']}
        if len(set(physical)) != len(physical) or set(physical) & table_pages:
            raise ValueError('Replay program, resource or page-table pages alias')
        if variant == 'shader-prefix-linux-dispatch':
            self._prepare_dispatch(descriptor_address, linux, set(physical) | table_pages)
        for p in self.pages:
            (self.output / ('replay-original-' + p['name'] + '.bin')).write_bytes(p['original'])
            (self.output / ('replay-replacement-' + p['name'] + '.bin')).write_bytes(p['replacement'])
            self.receipt['pages'].append(dict(name=p['name'], address=hex(p['address']),
                physical=hex(p['pa']), mapping=p['mapping'], size=p['size'], context=p['context'],
                original_sha256=digest(p['original']), replacement_sha256=digest(p['replacement'])))
        self.receipt['state'] = 'prepared'
        self._record()

    def _prepare_dispatch(self, address, linux, protected_pages):
        if not isinstance(address, int) or address < 0xffff_fc00_0000_0000:
            raise ValueError('Dispatch replay requires the captured global descriptor')
        original = self._read(address, 0x1000, None)
        manifest = json.loads((Path(linux) / 'manifest.json').read_text())
        matches = [o for o in manifest['objects'] if o['label'] == 'descriptor']
        if len(matches) != 1:
            raise ValueError('Dispatch replay requires one retained Linux descriptor')
        item = matches[0]
        path = (Path(linux) / item['file']).resolve()
        if path.parent != Path(linux).resolve():
            raise ValueError('Dispatch capture file escaped its directory')
        candidate = path.read_bytes()
        if len(candidate) != 0x1000 or digest(candidate) != item['sha256']:
            raise ValueError('Dispatch capture hash differs')
        offsets = [(0x40 + i * 12 + 4, 8) for i in (23, 24, 25)] + [(0xf20, 4), (0xf68, 8)]
        for data in (original, candidate):
            if (struct.unpack_from('<I', data)[0] != 3
                    or struct.unpack_from('<I', data, 0xc)[0] != 3
                    or struct.unpack_from('<4H', data, 0x18) != (37, 39, 39, 40)):
                raise ValueError('Dispatch descriptor profile differs')
            registers = [struct.unpack_from('<IQ', data, 0x40 + i * 12) for i in range(40)]
            low = struct.unpack_from('<Q', data, 0xf68)[0]
            if low > 0xffffffff:
                raise ValueError('Dispatch low identity exceeds the measured field')
            identity = struct.unpack_from('<I', data, 0xf20)[0] << 32 | low
            if registers[23:26] != [(n, identity) for n in (0x1a540, 0x14a9, 0xa351)]:
                raise ValueError('Dispatch identity fields disagree')
        if struct.unpack_from('<Q', candidate, offsets[0][0])[0] != 0x100000001:
            raise ValueError('Linux dispatch identity differs from the controlled probe')
        # Register 0x14028 selects the owned UMA hardware pool, not an
        # execution gate. Preserve it and independently check shared-control
        # +8; native pool IDs vary across submissions and processes.
        pools = [struct.unpack_from('<IQ', data, 0x40 + 29 * 12) for data in (original, candidate)]
        if any(number != 0x14028 or not 0 <= value < 512 for number, value in pools):
            raise ValueError('Dispatch private pool ID exceeds the measured UMA table')
        state, = struct.unpack_from('<Q', original, 0xfb2)
        pool_bytes = self._read(state + 8, 4, None)
        if (struct.unpack('<I', pool_bytes)[0] != pools[0][1]
                or self._read(state + 8, 4, None) != pool_bytes):
            raise ValueError('Dispatch private pool ID disagrees with its native owner')
        self.receipt['preserved_native_private_pool_hw_id'] = pools[0][1]
        # All other non-identity register values and all register numbers agree.
        masked = []
        for data in (original, candidate):
            registers = bytearray(data[0x40:0x220])
            for offset, size in offsets[:3]:
                registers[offset-0x40:offset-0x40+size] = bytes(size)
            registers[29 * 12 + 4:30 * 12] = bytes(8)
            masked.append(registers)
        if masked[0] != masked[1]:
            raise ValueError('Dispatch replay has an unrelated register difference')
        for offset, size in offsets:
            mapping, pa = self._mapping(address + offset, size, None)
            tables = {int(slot, 0) & ~(PAGE - 1) for part in mapping for slot, _ in part['walk']}
            if (pa & ~(PAGE - 1)) in protected_pages | tables:
                raise ValueError('Dispatch fields alias a program, resource or page table')
            self.pages.append(dict(name='dispatch-%x' % offset, address=address+offset,
                pa=pa, mapping=mapping, size=size, context=None,
                original=original[offset:offset+size], replacement=candidate[offset:offset+size]))
        self.receipt.update(gpu_program_only=False, descriptor_identity_fields=5)

    def _write(self, page, data, phase):
        # The caller supplies cache maintenance for the normal cached alias;
        # writes use the carrier's qualified full-DRAM Normal-NC alias.
        self.sync_phys(page['pa'] & ~(PAGE - 1), PAGE)
        self.write_phys(page['pa'], data)
        self.sync_phys(page['pa'] & ~(PAGE - 1), PAGE)
        if self._read(page['address'], page['size'], page['context']) != data:
            raise ValueError('Replay ' + phase + ' readback mismatch: ' + page['name'])
        self.receipt['writes'].append(dict(page=page['name'], phase=phase, verified=True))

    def apply(self):
        if self.receipt['state'] != 'prepared':
            raise ValueError('Replay was not fully prepared')
        # Revalidate every page before the first write; partial preflight must
        # never modify the target. GPU submission has not yet been published.
        for p in self.pages:
            if (self._mapping(p['address'], p['size'], p['context'])[0] != p['mapping']
                    or self._read(p['address'], p['size'], p['context']) != p['original']):
                raise ValueError('Replay pre-publication state changed')
        try:
            for p in self.pages:
                self.touched.append(p)  # include a possibly partial failed write
                self.active = True
                self._write(p, p['replacement'], 'replace')
            self.receipt['state'] = 'applied'
            self._record()
        except Exception as error:
            self.receipt['apply_error'] = str(error)
            self.restore()
            raise

    def restore(self):
        if not self.active:
            return
        # A changed translation invalidates ownership. Halt for investigation
        # rather than writing a page that may have been recycled by the guest.
        for p in self.touched:
            if self._mapping(p['address'], p['size'], p['context'])[0] != p['mapping']:
                self.receipt['state'] = 'restore-blocked-mapping-changed'
                self._record()
                raise ValueError('Replay ownership changed before restoration')
        try:
            for p in reversed(self.touched):
                self._write(p, p['original'], 'restore')
            self.active = False
            self.receipt['state'] = 'restored'
        except Exception as error:
            self.receipt.update(state='restore-failed', restore_error=str(error))
            raise
        finally:
            self._record()
