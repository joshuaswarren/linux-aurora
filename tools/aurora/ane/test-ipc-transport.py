#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-2.0-only
"""Compile the actual kernel IPC helpers with ASAN/UBSAN and RAM fixtures.

Kernel types, endian conversions and DMA barriers are supplied by a small
host shim. The fixture models observed ownership and allocation exchanges,
including malformed/stale replies; it never opens a hardware device.
"""
from pathlib import Path
import os
import subprocess
import tempfile

HEADER = Path(__file__).resolve().parents[3] / 'drivers/soc/apple/apple-ane-ipc.h'
SHIM = r'''
#include <assert.h>
#include <stdbool.h>
#include <stdint.h>
#include <errno.h>
#include <stdio.h>
#include <string.h>
typedef uint64_t u64;
typedef uint32_t u32;
typedef uint8_t u8;
typedef uint64_t __le64;
static unsigned writes, reads;
#define READ_ONCE(x) (*(volatile __typeof__(x) *)&(x))
#define WRITE_ONCE(x, v) (*(volatile __typeof__(x) *)&(x) = (v))
#define cpu_to_le64(x) ((u64)(x))
#define le64_to_cpu(x) ((u64)(x))
#define dma_wmb() (++writes)
#define dma_rmb() (++reads)
static inline uint16_t get_unaligned_le16(const void *p) { uint16_t v; memcpy(&v, p, 2); return v; }
static inline uint32_t get_unaligned_le32(const void *p) { uint32_t v; memcpy(&v, p, 4); return v; }
static inline uint64_t get_unaligned_le64(const void *p) { uint64_t v; memcpy(&v, p, 8); return v; }
'''
TEST = r'''
int main(void)
{
    struct apple_ane_ipc_slot slot = {.pointer = 1};
    struct apple_ane_io_transaction tx = {0};
    const u64 command = 0x10000384300ULL;
    u64 metadata = 99, tag = 0;
    u32 length = 0, size = 0;
    unsigned before;
    unsigned char event[96] = {0};
    bool completed = false;
    u32 stage = 99;

    assert(sizeof(slot) == 64);
    memset(slot.reserved, 0xa5, sizeof(slot.reserved));
    assert(apple_ane_io_publish(&tx, &slot, command, 12) == 0);
    assert(tx.pending && slot.pointer == command && slot.length == 12 && slot.metadata == 12);
    assert(writes == 2 && reads == 0);
    assert(apple_ane_io_publish(&tx, &slot, command, 12) == -EBUSY);
    assert(apple_ane_io_poll(&tx, &length, &metadata) == -EAGAIN);
    assert(reads == 0);
    slot.pointer = command | 1;
    slot.metadata = 0;
    assert(apple_ane_io_poll(&tx, &length, &metadata) == 0);
    assert(length == 12 && metadata == 0 && !tx.pending && reads == 1);
    for (unsigned i = 0; i < sizeof(slot.reserved); i++) assert(slot.reserved[i] == 0xa5);

    assert(apple_ane_io_publish(&tx, &slot, command, 100) == 0);
    slot.pointer = (command + 64) | 1;
    assert(apple_ane_io_poll(&tx, &length, &metadata) == -EPROTO && tx.pending);
    slot.pointer = command | 1;
    slot.length = 101;
    assert(apple_ane_io_poll(&tx, &length, &metadata) == -EPROTO && tx.pending);
    slot.length = 7;
    assert(apple_ane_io_poll(&tx, &length, &metadata) == -EPROTO);
    slot.length = 100;
    slot.metadata = 0xabcdef;
    assert(apple_ane_io_poll(&tx, &length, &metadata) == 0 && metadata == 0xabcdef);
    assert(apple_ane_io_poll(&tx, &length, &metadata) == -EINVAL);

    before = writes;
    assert(apple_ane_io_publish(&tx, &slot, 0, 12) == -EINVAL);
    assert(apple_ane_io_publish(&tx, &slot, command + 2, 12) == -EINVAL);
    assert(apple_ane_io_publish(&tx, &slot, command, 7) == -EINVAL);
    assert(apple_ane_io_publish(&tx, &slot, command, 0x40001) == -EINVAL);
    assert(apple_ane_io_publish(&tx, &slot, (1ULL << 42) - 4, 12) == -EINVAL);
    slot.pointer = 3;
    assert(apple_ane_io_publish(&tx, &slot, command, 12) == -EPROTO);
    assert(writes == before && !tx.pending);

    slot.pointer = 0;
    slot.length = 0x10000;
    slot.metadata = 0x4c4f47;
    assert(apple_ane_alloc_request(&slot, 0x100000, &size, &tag) == 0);
    assert(size == 0x10000 && tag == 0x4c4f47);
    assert(apple_ane_alloc_reply(&slot, size + 1, tag, 0x100012e8000ULL, 0xa) == -EPROTO);
    assert(apple_ane_alloc_reply(&slot, size, tag + 1, 0x100012e8000ULL, 0xa) == -EPROTO);
    assert(apple_ane_alloc_reply(&slot, size, tag, 0x100012e8001ULL, 0xa) == -EINVAL);
    assert(apple_ane_alloc_reply(&slot, size, tag, 0x100012e8000ULL, 0) == -EINVAL);
    assert(writes == before && slot.pointer == 0 && slot.length == size);
    assert(apple_ane_alloc_reply(&slot, size, tag, 0x100012e8000ULL, 0xa) == 0);
    assert(slot.pointer == 0x100012e8001ULL && slot.length == 0 && slot.metadata == 0xa);
    assert(writes == before + 2);
    assert(apple_ane_alloc_request(&slot, 0x100000, &size, &tag) == -EAGAIN);
    assert(apple_ane_alloc_reply(&slot, 0x10000, 0x4c4f47, 0x100012e8000ULL, 0xa) == -EBUSY);
    slot.pointer = command;
    assert(apple_ane_alloc_request(&slot, 0x100000, &size, &tag) == -EOPNOTSUPP);
    slot.pointer = 0;
    slot.length = 0x100001;
    assert(apple_ane_alloc_request(&slot, 0x100000, &size, &tag) == -EOVERFLOW);
    slot.length = 0;
    assert(apple_ane_alloc_request(&slot, 0x100000, &size, &tag) == -EOVERFLOW);
    /* Reconstructed begin/end metadata from completion-capture-1; timestamps
     * are omitted because they are not used to accept a completion. */
    event[1] = 0xfe;
    event[5] = 3;
    event[8] = 9;
    assert(apple_ane_work_event(event, 96, 9, &completed) == 0 && !completed);
    event[0] = 1;
    event[0x1c] = 1;
    assert(apple_ane_work_event(event, 96, 9, &completed) == 0 && completed);
    assert(apple_ane_work_event(event, 96, 8, &completed) == -ESTALE);
    assert(apple_ane_work_event(event, 95, 9, &completed) == -EINVAL);
    event[6] = 1;
    assert(apple_ane_work_event(event, 96, 9, &completed) == -EIO);
    event[6] = 0;
    event[0x1c] = 2;
    assert(apple_ane_work_event(event, 96, 9, &completed) == -EPROTO);
    event[0x1c] = 1;
    event[0x10] = 1;
    assert(apple_ane_work_event(event, 96, 9, &completed) == -EPROTO);
    event[0x10] = 0;
    event[4] = 1;
    assert(apple_ane_work_event(event, 96, 9, &completed) == -EOPNOTSUPP);
    event[4] = 0;
    event[0] = 4;
    assert(apple_ane_work_event(event, 96, 9, &completed) == -EOPNOTSUPP);
    assert(apple_ane_profile_event(NULL, 96, 9, 0) == -EINVAL);
    assert(apple_ane_profile_event(event, 96, 9, 3) == -EINVAL);
    assert(apple_ane_session_event(NULL,96,9,&stage)==-EINVAL);
    assert(apple_ane_session_event(event,96,0,&stage)==-EINVAL);
    assert(apple_ane_session_event(event,96,9,&stage)==0 && stage==1);
    assert(apple_ane_session_event(event,96,8,&stage)==-ESTALE);
    event[0x1c]=3;
    assert(apple_ane_session_event(event,96,9,&stage)==-EPROTO);
    event[0x31]=0xff;
    assert(apple_ane_session_event(event,96,9,&stage)==0 && stage==3);
    puts("ANE_IPC_TRANSPORT_HOST_PASS actual C helpers, ownership, allocation, bounds, stale replies, matched work events");
    return 0;
}
'''


def main():
    source = '\n'.join(line for line in HEADER.read_text().splitlines()
                       if not line.startswith('#include '))
    with tempfile.TemporaryDirectory(prefix='ane-ipc-transport-') as tmp:
        root = Path(tmp)
        (root / 'test.c').write_text(SHIM + source + TEST)
        subprocess.run([os.environ.get('CC', 'clang'), '-std=gnu11', '-Wall', '-Wextra',
                        '-Werror', '-fsanitize=address,undefined', '-fno-omit-frame-pointer',
                        str(root / 'test.c'), '-o', str(root / 'test')], check=True)
        subprocess.run([str(root / 'test')], check=True)


if __name__ == '__main__':
    main()
