# SPDX-License-Identifier: MIT
"""Read-only, one-shot TA/3D submission capture for the pinned M5 25G76 guest.

Run in the existing paused HV. Only translated DRAM is read. It never writes
GPU objects, adds MMIO traps, or treats native rendering as Linux proof.
"""
import hashlib
import json
import os
from pathlib import Path
import re
import struct
import sys
import time

sys.path.insert(0, str(Path(__file__).resolve().parent))
from m5_gpu_capture import M5GPUCapture

if (hv.ctx is None or not hv.started or hv.u.adt.model != 'Mac17,3'
        or int(hv.u.adt['/chosen'].chip_id) != 0x8142):
    raise RuntimeError('Pause the existing M5 guest before arming graphics capture')
_m5_graphics_sites = {
    0xfffffe0008a24d30: ('ta', 0, 0xfb8, bytes.fromhex('81de47f9700240f9')),
    0xfffffe000895c5a4: ('fragment', 1, 0x458, bytes.fromhex('812e42f9700240f9')),
}
_m5_graphics_stages = set(os.environ.get('M5_GRAPHICS_CAPTURE_STAGES', 'ta,fragment').split(','))
if not _m5_graphics_stages or not _m5_graphics_stages <= {'ta', 'fragment'}:
    raise ValueError('Invalid graphics capture stages')
_m5_graphics_sites = {pc: site for pc, site in _m5_graphics_sites.items()
                      if site[0] in _m5_graphics_stages}
_m5_graphics_output = Path(os.environ['M5_GPU_TRACE_DIR'])
_m5_graphics_roots = re.findall(r'\buat_global_root_pa=(0x[0-9a-f]+)',
    (_m5_graphics_output / 'console.log').read_text())
if len(set(_m5_graphics_roots)) != 1:
    raise RuntimeError('Graphics capture requires this boot\'s SPTM root receipt')
_m5_graphics_low = int(hv.u.adt['/chosen'].dram_base)
_m5_graphics_high = _m5_graphics_low + int(hv.u.adt['/chosen'].dram_size)
_m5_graphics_gpu = M5GPUCapture(hv.iface.readmem,
    (_m5_graphics_low, _m5_graphics_high - _m5_graphics_low), int(_m5_graphics_roots[0],16))
_m5_graphics_hits = dict.fromkeys(_m5_graphics_sites, 0)
_m5_graphics_bytes = 0
_m5_graphics_priority = int(os.environ.get('M5_GRAPHICS_PRIORITY','2'))
_m5_graphics_exclude = {int(x,0) for x in os.environ.get('M5_GRAPHICS_EXCLUDE_QIDS','').split(',') if x}
if _m5_graphics_priority not in range(4):
    raise ValueError('Invalid graphics priority')


def _m5_graphics_read_cpu(address, size):
    result = bytearray()
    while size:
        length = min(size, 4096 - (address & 4095))
        pa = int(hv.p.hv_translate(address, False, False))
        if not _m5_graphics_low <= pa < pa + length <= _m5_graphics_high:
            raise ValueError('Graphics CPU view is unmapped or outside DRAM')
        data = hv.iface.readmem(pa, length)
        if len(data) != length:
            raise ValueError('Short graphics CPU read')
        result.extend(data)
        address += length
        size -= length
    return bytes(result)


def _m5_graphics_submit(ctx):
    global _m5_graphics_bytes
    pc = int(ctx.elr)
    name, dm, load_offset, code = _m5_graphics_sites[pc]
    _m5_graphics_hits[pc] += 1
    if _m5_graphics_read_cpu(pc, 8) != code:
        hv.remove_hw_bp(pc)
        raise RuntimeError('M5 graphics capture site differs from pinned 25G76')
    command = _m5_graphics_read_cpu(int(ctx.regs[2]), 21)
    tag, qid, count, priority, stamp, observed_dm = struct.unpack('<IHHBQI', command)
    if (priority != _m5_graphics_priority or qid in _m5_graphics_exclude) and _m5_graphics_hits[pc] < 1024:
        # Exact emulation of this verified LDR prevents repeated-PC traps on
        # this carrier. It changes only the interrupted architectural register.
        ctx.regs[1], = struct.unpack('<Q', _m5_graphics_read_cpu(int(ctx.regs[20])+load_offset,8))
        ctx.elr += 4
        return True
    hv.remove_hw_bp(pc)
    directory = _m5_graphics_output / ('graphics-'+name+'-'+str(time.time_ns()))
    directory.mkdir(mode=0o700)
    manifest = dict(phase='before-native-graphics-channel-publication', stage=name,
        pc=hex(pc), queue_id=qid, priority=priority, stamp=hex(stamp), count=count,
        data_master=observed_dm, objects=[], errors=[], device_memory_read=False,
        excluded_queue_ids=sorted(_m5_graphics_exclude),
        native_instruction_bytes_checked=True, linux_gpu_proven=False)

    def save(label, address, size, space='GPU', context=None):
        global _m5_graphics_bytes
        if not 0 < size <= 0x80000 or _m5_graphics_bytes + 2*size > 16*1024*1024:
            raise ValueError('Graphics capture exceeds its bounded DRAM budget')
        _m5_graphics_bytes += size*2
        region = int(hv.u.adt['/arm-io/sgx'].gpu_region_base)
        if space == 'CPU':
            read = lambda: _m5_graphics_read_cpu(address,size)
            mapping = None
        else:
            read = (lambda: _m5_graphics_gpu.read(address,size)) if context is None else (
                lambda: _m5_graphics_gpu.read_context(address,size,context,region))
            mapping = _m5_graphics_gpu.mapping(address,size,context,region)
        first, second = read(), read()
        filename = label+'.bin'
        (directory/filename).write_bytes(first)
        item = dict(label=label,address=hex(address),size=size,file=filename,
            address_space=space,sha256=hashlib.sha256(first).hexdigest(),stable=first==second)
        if mapping is not None:
            item.update(mapping=mapping, mapping_stable=mapping ==
                _m5_graphics_gpu.mapping(address,size,context,region),gpu_context=context)
        if first != second:
            (directory/(label+'-second.bin')).write_bytes(second)
            item['second_sha256'] = hashlib.sha256(second).hexdigest()
        manifest['objects'].append(item)
        return first

    def attempt(label,address,size,space='GPU',context=None):
        try:
            return save(label,address,size,space,context)
        except (ValueError,RuntimeError) as error:
            manifest['errors'].append(dict(label=label,error=str(error)))
            return None

    try:
        if (tag != 14 or observed_dm != dm or not count or priority != _m5_graphics_priority
                or qid in _m5_graphics_exclude):
            raise ValueError('Expected a measured TA/3D add-kicks command')
        save('add-kicks-command',int(ctx.regs[2]),21,'CPU')
        # Both pinned submitBuffer functions retain their kick queue in x23.
        queue = save('host-kick-queue',int(ctx.regs[23]),0xb0,'CPU')
        queue_id,entries,stride = struct.unpack_from('<III',queue,0xc)
        queue_priority, = struct.unpack_from('<I',queue,0x1c)
        if (queue_id != qid or priority != queue_priority or not 0<entries<=256
                or not 0x180<=stride<=0x1000 or (stamp&255)>=entries):
            raise ValueError('Graphics queue ownership or geometry mismatch')
        cpu_ring,gpu_ring,firmware_ring = struct.unpack_from('<QQQ',queue,0x78)
        manifest.update(entry_count=entries,entry_stride=stride,
            firmware_ring=hex(firmware_ring),gpu_ring=hex(gpu_ring),cpu_ring=hex(cpu_ring))
        entry = save('kick-entry',firmware_ring+(stamp&255)*stride,stride)
        # Native generators read through TA+0x1438 and fragment+0xb60.
        # These bounded views include those inputs, not a sizeof claim.
        attempt('host-command-descriptor',int(ctx.regs[20]),0x1800 if dm==0 else 0x1000,'CPU')
        descriptor,channel = struct.unpack_from('<QQ',entry,0x10)
        raw = save('work-descriptor',descriptor,0x3000)
        selector, = struct.unpack_from('<I',raw,0)
        context, = struct.unpack_from('<I',raw,0xc)
        if selector != dm or not 0<context<256:
            raise ValueError('Graphics descriptor selector/context mismatch')
        manifest.update(gpu_context=context,descriptor=hex(descriptor),channel=hex(channel))
        channel_bytes = save('channel-record',channel,0xc0)
        for label,offset,extent in [('channel-pointers',0,0x100),('channel-items',8,0x1000),
                ('channel-job-list',0x10,0x40),('channel-control',0x9c,0xc0)]:
            pointer, = struct.unpack_from('<Q',channel_bytes,offset)
            if pointer: attempt(label,pointer,extent)
        # These are bounded views of native-owned descriptor references,
        # without asserting that the Neo descriptor layout applies to M5.
        header_views = {}
        for offset in ([0x10,0x20,0x28,0x30] if dm==0 else [0x20,0x28,0x30,0x38]):
            pointer, = struct.unpack_from('<Q',raw,offset)
            if pointer:
                header_views[offset] = attempt('header-ref-%x'%offset,pointer,
                    min(0x1000,0x4000-(pointer&0x3fff)))
        if dm == 0 and os.environ.get('M5_GRAPHICS_CAPTURE_PB_TABLE') == '1':
            hardware_id, = struct.unpack_from('<I', raw, 0x18)
            if hardware_id >= 1024:
                raise ValueError('M5 PB descriptor index exceeds the owned table extent')
            manifest['pb_hardware_id'] = hardware_id
            save('pb-hardware-descriptor', 0xfffffc20015e0000 + hardware_id * 16, 16)
        if os.environ.get('M5_GRAPHICS_CAPTURE_COMPLETION') == '1':
            scheduler = header_views.get(0x10 if dm == 0 else 0x20)
            if scheduler:
                counter, = struct.unpack_from('<Q', scheduler, 0)
                if counter:
                    attempt('completion-scheduler-counter', counter, 64)
            # allocatePagesEnd derives the first scalar from USC pool sizes.
            # It is not the low alias of the adjacent statistics pointer.
            size_offset, statistics_offset = (0x93d, 0x945) if dm == 0 else (0x21d7, 0x21df)
            required_bytes, = struct.unpack_from('<Q', raw, size_offset)
            statistics, = struct.unpack_from('<Q', raw, statistics_offset)
            manifest['usc_accounting'] = dict(required_bytes=required_bytes,
                statistics=hex(statistics), size_offset=hex(size_offset),
                statistics_offset=hex(statistics_offset))
            # Retain a bounded opt-in reproduction of the former address
            # hypothesis. Normal captures never dereference a byte count.
            if required_bytes and os.environ.get('M5_GRAPHICS_PROBE_USC_SIZE_AS_ADDRESS') == '1':
                for candidate_context in sorted({0, context}):
                    attempt('completion-low-context-%d' % candidate_context,
                        required_bytes, 64, context=candidate_context)
            if statistics:
                attempt('completion-high-global', statistics, 64)
            offsets = ([0x8a6, 0x8ae, 0x8fe, 0x934] if dm == 0 else
                [0x2140, 0x2148, 0x2198, 0x21ce])
            for offset in offsets:
                pointer, = struct.unpack_from('<Q', raw, offset)
                if pointer:
                    attempt('completion-tail-%x' % offset, pointer, 64)
        if os.environ.get('M5_GRAPHICS_CAPTURE_OWNERS') == '1':
            from m5_graphics_owners import capture_owners
            manifest['ownership'] = capture_owners(save, raw, channel_bytes, context, name)
        # Native program setup starts in these fixed context-relative pages
        # in our controlled reference process. Missing pages remain explicit.
        for address in [0x10000000000,0x1000001c000,0x10000090000]:
            attempt('program-page-%x'%address,address,0x4000,context=context)
        context_mode=os.environ.get('M5_GRAPHICS_CAPTURE_CONTEXT')
        if dm==1 and context_mode in ('1','map'):
            from m5_context_capture import survey_context
            if context!=int(os.environ.get('M5_GRAPHICS_CONTEXT_ID','3')):
                raise ValueError('Full context capture requires the selected reference context')
            # The controlled workload's program/data window and corresponding
            # GART aliases. No kernel/firmware root or general process scan.
            windows=[(0x1000000000,0x1000000),(0x10000000000,0x1000000)]
            region=int(hv.u.adt['/arm-io/sgx'].gpu_region_base)
            if context_mode=='map':
                parts=[]
                for base,size in windows:
                    for offset in range(0,size,0x400000):
                        part,_tables=survey_context(_m5_graphics_gpu._read_dram,
                            region,context,[(base+offset,0x400000)])
                        parts.append(part)
                if len({part['root_entry'] for part in parts})!=1:
                    raise ValueError('Context root changed between map windows')
                manifest['context_map']=dict(parts=parts,
                    mapped_resource_reads=False,
                    unique_resource_pages=len({m['physical'] for part in parts
                        for m in part['mappings']}))
                return True
            # The table-only survey of the controlled 256x256 triangle found
            # 334 distinct pages, all within the first 4 MiB of these windows.
            # Bound resource capture to 384 pages (12 MiB for two data reads),
            # with the existing 16 MiB aggregate data-view budget still active.
            windows=[(0x1000000000,0x400000),(0x10000000000,0x400000)]
            survey,tables=survey_context(_m5_graphics_gpu._read_dram,region,context,
                windows,max_pages=384)
            manifest['context_image']=survey
            seen={}
            for mapping in survey['mappings']:
                pa=mapping['physical']
                if pa not in seen:
                    label='context-page-%03d'%len(seen)
                    save(label,mapping['address'],0x4000,context=context)
                    seen[pa]=label+'.bin'
                mapping['file']=seen[pa]
            for index,(pa,data) in enumerate(tables.items()):
                filename='context-table-%02d.bin'%index
                (directory/filename).write_bytes(data)
                survey['tables'][index]['file']=filename
                if _m5_graphics_gpu._read_dram(pa,0x4000)!=data:
                    raise ValueError('Context table changed during resource capture')
            root_now=_m5_graphics_gpu._read_dram(survey['root_slot'],8)
            if struct.unpack('<Q',root_now)[0]!=survey['root_entry']:
                raise ValueError('Context root changed during resource capture')
            survey['stable_after_resource_reads']=True
    except (ValueError,RuntimeError) as error:
        manifest['error'] = str(error)
    finally:
        manifest['total_read_bytes'] = _m5_graphics_bytes
        (directory/'manifest.json').write_text(json.dumps(manifest,indent=2)+'\n')
        print('M5_GRAPHICS_CAPTURE',directory,'error='+str(manifest.get('error')))
    return True


for _pc in _m5_graphics_sites:
    hv.add_hw_bp(_pc,_m5_graphics_submit)
print('M5_GRAPHICS_CAPTURE_ARMED priority='+str(_m5_graphics_priority))
