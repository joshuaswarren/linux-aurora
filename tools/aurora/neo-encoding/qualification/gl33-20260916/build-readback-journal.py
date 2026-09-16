#!/usr/bin/env python3
"""Build a diagnostic Mesa overlay and restore the exact original VM sources."""
import difflib
import hashlib
from pathlib import Path
import subprocess

here = Path(__file__).resolve().parent
root = here.parents[2]
source = root/'mesa-j700/src/gallium/drivers/asahi'
vm_source = '/home/mcruz.guest/neo/sources/mesa-native-gl2-20260913/src/gallium/drivers/asahi'
vm_build = '/home/mcruz.guest/neo/build/mesa-native-gl2-20260913'
target = 'src/gallium/targets/dri/libgallium-26.1.4.so'
normal_sha = '2b7e6eec6ce430f828e945889d4ccaa97a7080af60ede57bb45210773b19dbf2'
names = ['agx_pipe.c', 'agx_batch.c', 'agx_state.h']
digest = lambda p: hashlib.sha256(p.read_bytes()).hexdigest()


def vm(*args, **kwargs):
    return subprocess.run(['limactl', 'shell', 'neo-builder', *args], check=True, **kwargs)


def vm_digest(path):
    return vm('sha256sum', path, capture_output=True, text=True).stdout.split()[0]


def replace_once(text, old, new):
    assert text.count(old) == 1, (old, text.count(old))
    return text.replace(old, new)


def copy(path, name):
    subprocess.run(['limactl', 'copy', str(path), 'neo-builder:'+vm_source+'/'+name], check=True)
    vm('touch', vm_source+'/'+name)


def build(log):
    with (here/log).open('wb') as output:
        vm('ninja', '-C', vm_build, target, stdout=output, stderr=subprocess.STDOUT)


for name in names:
    assert vm_digest(vm_source+'/'+name) == digest(source/name), name
assert vm_digest(vm_build+'/'+target) == normal_sha
assert vm('test', '!', '-e', vm_source+'/readback-journal.h').returncode == 0
overlay = here/'probes/readback-journal-overlay'
overlay.mkdir(exist_ok=False)
candidate = here/'candidates/libgallium-readback-journal.so'
assert not candidate.exists()

header = (source/'agx_state.h').read_text()
header = replace_once(header, 'struct agx_context {\n   struct pipe_context base;',
    '#include "readback-journal.h"\n\nstruct agx_context {\n   struct pipe_context base;\n'
    '   unsigned readback_journal_head;\n'
    '   struct readback_journal_event readback_journal[128];')
batch = (source/'agx_batch.c').read_text()
batch = replace_once(batch, 'static void\nasahi_add_attachment(struct attachments *att, struct agx_resource *rsrc)',
    (here/'probes/readback-journal.c.inc').read_text() +
    '\nstatic void\nasahi_add_attachment(struct agx_batch *batch, uint64_t seqid, struct attachments *att, struct agx_resource *rsrc)')
batch = replace_once(batch, '   assert(att->count < MAX_ATTACHMENTS);',
    '   agx_readback_record(batch->ctx, "attachment", batch, rsrc->bo, agx_map_gpu(rsrc),\n'
    '                       rsrc->layout.size_B - rsrc->layout.level_offsets_B[0], seqid);\n'
    '   assert(att->count < MAX_ATTACHMENTS);')
assert batch.count('asahi_add_attachment(&att,') == 4
batch = batch.replace('asahi_add_attachment(&att,', 'asahi_add_attachment(batch, seqid, &att,')
batch = replace_once(batch,
    'agx_batch_cleanup(struct agx_context *ctx, struct agx_batch *batch, bool reset)\n{',
    'agx_batch_cleanup(struct agx_context *ctx, struct agx_batch *batch, bool reset)\n{\n'
    '   agx_readback_record(ctx, "cleanup", batch, NULL, 0, 0, reset);')
batch = replace_once(batch, '   agx_writer_add(ctx, agx_batch_idx(batch), rsrc->bo->handle);',
    '   agx_writer_add(ctx, agx_batch_idx(batch), rsrc->bo->handle);\n'
    '   agx_readback_record(ctx, "writer-set", batch, rsrc->bo, level, 0, 0);')
batch = replace_once(batch,
    '   if (writer && writer != except &&\n',
    '   if (sync)\n'
    '      agx_readback_record(ctx, "cpu-writer", writer, rsrc->bo, 0, 0, 0);\n\n'
    '   if (writer && writer != except &&\n')
batch = replace_once(batch, '   int ret = dev->ops.submit(dev, &submit, &virt);',
    '   agx_readback_record(ctx, "submit-start", batch, NULL, seqid, cmdbuf.size, 0);\n'
    '   int ret = dev->ops.submit(dev, &submit, &virt);\n'
    '   agx_readback_record(ctx, "submit-return", batch, NULL, seqid, cmdbuf.size, ret);')
batch = replace_once(batch, '   assert(batch->syncobj);\n   int ret = drmSyncobjWait',
    '   assert(batch->syncobj);\n'
    '   agx_readback_record(ctx, "wait-begin", batch, NULL, 0, 0, 0);\n'
    '   int ret = drmSyncobjWait')
batch = replace_once(batch, '   assert(!ret);\n   agx_batch_cleanup(ctx, batch, false);\n}',
    '   agx_readback_record(ctx, "wait-return", batch, NULL, 0, 0, ret);\n'
    '   assert(!ret);\n   agx_batch_cleanup(ctx, batch, false);\n}')

pipe = (here/'probes/agx_pipe-fence-observe.c').read_text()
pipe = replace_once(pipe, 'observe_retired_readback(const void *map,',
    'observe_retired_readback(struct agx_context *ctx, struct agx_resource *rsrc, const void *map,')
pipe = replace_once(pipe, '   memcpy(before, map, size);',
    '   agx_readback_record(ctx, "cpu-map", NULL, rsrc->bo, (uintptr_t)map, size, usage);\n'
    '   memcpy(before, map, size);')
pipe = replace_once(pipe, '      if (fence_changed <= 6) {',
    '      if (fence_changed <= 6) {\n'
    '         agx_readback_dump(ctx, rsrc->bo);')
pipe = replace_once(pipe, '            observe_retired_readback(map,',
    '            observe_retired_readback(ctx, rsrc, map,')

for name, data in [('agx_state.h', header), ('agx_batch.c', batch), ('agx_pipe.c', pipe)]:
    (overlay/name).write_text(data)
    (overlay/(name+'.diff')).write_text(''.join(difflib.unified_diff(
        (source/name).read_text().splitlines(True), data.splitlines(True),
        fromfile='normal/'+name, tofile='diagnostic/'+name)))
try:
    copy(here/'probes/readback-journal.h', 'readback-journal.h')
    for name in names:
        copy(overlay/name, name)
    build('readback-journal-build.log')
    subprocess.run(['limactl', 'copy', 'neo-builder:'+vm_build+'/'+target, str(candidate)], check=True)
    print('JOURNAL_BUILT', digest(candidate), flush=True)
finally:
    for name in names:
        copy(source/name, name)
    vm('rm', vm_source+'/readback-journal.h')
    build('readback-journal-restore-build.log')
    for name in names:
        assert vm_digest(vm_source+'/'+name) == digest(source/name)
    assert vm_digest(vm_build+'/'+target) == normal_sha
    print('NORMAL_SOURCES_AND_BINARY_RESTORED', normal_sha, flush=True)
