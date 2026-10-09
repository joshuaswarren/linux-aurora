#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-2.0-only
"""Exercise the real mapped-buffer and asynchronous-consumer C routines."""
import importlib.util
from pathlib import Path
import subprocess
import tempfile


def extract(source, declaration):
    start = source.index(declaration)
    end = source.index('{', start) + 1
    depth = 1
    while depth:
        depth += (source[end] == '{') - (source[end] == '}')
        end += 1
    return source[start:end]


def main():
    here = Path(__file__).resolve().parent
    spec = importlib.util.spec_from_file_location('transport_test', here / 'test-ipc-transport.py')
    transport = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(transport)
    header = transport.HEADER.read_text()
    source = transport.HEADER.with_name('apple-ane-fw-test.c').read_text()
    slot = header[header.index('struct apple_ane_ipc_slot {'):header.index('struct apple_ane_io_transaction')]
    code = transport.SHIM + r'''
#include <stdlib.h>
#define ANE_IPC 0x3d0000
#define BIT(x) (1U << (x))
#define BIT_ULL(x) (1ULL << (x))
#define dev_info(...) ((void)0)
#define dev_err(...) ((void)0)
struct ane_buffer { struct ane_buffer *next; void *cpu; u64 iova; u32 size; };
struct ane_fw_test {
    struct ane_buffer *arena, *buffers; void *doorbell;
    u32 event_head; u64 work_token;
    bool work_pending, work_begin, work_end, profile_events, work_profile;
    bool session_mode; u32 work_events;
};
#define list_for_each_entry(pos, head, ignored) for (pos = *(head); pos; pos = pos->next)
static unsigned doorbells;
static void writel(u32 value, void *p) {
    assert(value == BIT(6)); assert(writes >= 2); doorbells++; memcpy(p, &value, 4);
}
'''
    code += slot + extract(header, 'static inline int apple_ane_work_event(')
    code += extract(header, 'static inline int apple_ane_profile_event(')
    code += extract(header, 'static inline int apple_ane_session_event(')
    code += extract(source, 'static void *ane_buffer_pointer(')
    code += extract(source, 'static int ane_service_work(')
    code += r'''
static void put32(void *p, u32 x) { memcpy(p, &x, 4); }
static void put64(void *p, u64 x) { memcpy(p, &x, 8); }
int main(void) {
    u8 payload[0x100] = {0}; u32 bell = 0;
    struct ane_buffer event = {.cpu=payload, .iova=0x10001314000ULL, .size=sizeof(payload)};
    struct ane_buffer arena = {.cpu=calloc(1,0x400000), .iova=0x10000000000ULL,
                               .size=0x400000, .next=&event};
    struct ane_fw_test ane = {.arena=&arena, .buffers=&arena, .doorbell=&bell,
                              .work_token=9, .work_pending=true};
    assert(arena.cpu);
    assert(ane_buffer_pointer(&ane,event.iova,96)==payload);
    assert(ane_buffer_pointer(&ane,event.iova+sizeof(payload)-96,96)==payload+sizeof(payload)-96);
    assert(!ane_buffer_pointer(&ane,event.iova+sizeof(payload)-95,96));
    assert(!ane_buffer_pointer(&ane,event.iova-4,96));
    assert(!ane_buffer_pointer(&ane,0x401800000ULL,96));
    assert(!ane_buffer_pointer(&ane,(1ULL<<42)-4,96));
    assert(!ane_buffer_pointer(&ane,event.iova,0));
    struct apple_ane_ipc_slot *ring=arena.cpu+ANE_IPC+0xb000;
    assert(ane_service_work(&ane)==0 && !doorbells && !ane.event_head);
    put32(payload,0xfe00); put32(payload+4,0x300); put64(payload+8,9);
    ring[0]=(struct apple_ane_ipc_slot){.pointer=event.iova,.length=96};
    // Every invalid publication is left owned by firmware with no doorbell.
    for(unsigned test=0;test<9;test++) {
        struct apple_ane_ipc_slot saved=ring[0];
        u8 backup[96];memcpy(backup,payload,96);
        switch(test) {
        case 0: ring[0].pointer=0; break;
        case 1: ring[0].pointer|=2; break;
        case 2: ring[0].pointer=0x401800000ULL; break;
        case 3: ring[0].length=95; break;
        case 4: ring[0].metadata=1; break;
        case 5: put64(payload+8,8); break;
        case 6: payload[6]=1; break;
        case 7: put32(payload+0x1c,1);break;
        case 8: ane.work_pending=false;break;
        }
        struct apple_ane_ipc_slot changed=ring[0];
        assert(ane_service_work(&ane)<0);
        assert(!memcmp(&changed,&ring[0],sizeof(changed)));
        assert(!doorbells && !ane.work_begin && !ane.work_end && !ane.event_head);
        ring[0]=saved;memcpy(payload,backup,96);ane.work_pending=true;
    }
    assert(ane_service_work(&ane)==0);
    assert(ane.work_begin && !ane.work_end && ane.event_head==1 && doorbells==1);
    assert(ring[0].pointer==(event.iova|1) && ring[0].length==96 && !ring[0].metadata);
    // A duplicate begin is rejected, then the corresponding end is accepted.
    ring[1]=(struct apple_ane_ipc_slot){.pointer=event.iova,.length=96};
    assert(ane_service_work(&ane)==-EPROTO && doorbells==1);
    put32(payload,0xfe01);
    assert(ane_service_work(&ane)==-EPROTO && doorbells==1);
    put32(payload,0xfe01);put32(payload+0x1c,1);
    assert(ane_service_work(&ane)==0 && ane.work_end && ane.event_head==2 && doorbells==2);
    assert(ring[1].pointer==(event.iova|1) && ring[1].length==96 && !ring[1].metadata);
    ring[2]=(struct apple_ane_ipc_slot){.pointer=event.iova,.length=96};
    assert(ane_service_work(&ane)==-EPROTO && doorbells==2);
    // Real Linux's second pair is fe02/fe03; phase lives in the stage field.
    ane.work_token=2;ane.work_begin=false;ane.work_end=false;
    put64(payload+8,2);put32(payload,0xfe02);put32(payload+0x1c,0);
    assert(ane_service_work(&ane)==0 && ane.work_begin && doorbells==3);
    ring[3]=(struct apple_ane_ipc_slot){.pointer=event.iova,.length=96};
    put32(payload,0xfe03);put32(payload+0x1c,1);
    assert(ane_service_work(&ane)==0 && ane.work_end && doorbells==4);
    // Real Linux's third job reuses handles fe00/fe01, with a fresh token.
    ane.work_token=3;ane.work_begin=false;ane.work_end=false;
    ring[4]=(struct apple_ane_ipc_slot){.pointer=event.iova,.length=96};
    put64(payload+8,2);put32(payload,0xfe00);put32(payload+0x1c,0);
    assert(ane_service_work(&ane)==-ESTALE && doorbells==4);
    put64(payload+8,3);
    assert(ane_service_work(&ane)==0 && ane.work_begin && doorbells==5);
    ring[5]=(struct apple_ane_ipc_slot){.pointer=event.iova,.length=96};
    put32(payload,0xfe01);put32(payload+0x1c,1);
    assert(ane_service_work(&ane)==0 && ane.work_end && doorbells==6);
    // Tiny Llama: three jobs each have begin/profile/end, not three completions.
    memset(ring,0,64*sizeof(*ring)); memset(payload,0,sizeof(payload));
    ane.event_head=0;ane.profile_events=true;doorbells=0;
    for(unsigned job=0;job<3;job++) {
        ane.work_token=job+1;ane.work_begin=false;ane.work_end=false;ane.work_profile=false;
        put32(payload+4,0x300);put64(payload+8,job+1);
        unsigned at=job*3;
        ring[at]=(struct apple_ane_ipc_slot){.pointer=event.iova,.length=96};
        put32(payload,job*0x10001);put32(payload+0x1c,3);put64(payload+0x30,0xff00);
        assert(ane_service_work(&ane)==-EPROTO && doorbells==at); // before begin
        put32(payload,0xfe00+(job%2)*2);put32(payload+0x1c,0);put64(payload+0x30,0);
        assert(ane_service_work(&ane)==0 && ane.work_begin && !ane.work_end);
        ring[at+1]=(struct apple_ane_ipc_slot){.pointer=event.iova,.length=96};
        put32(payload,0xfe01+(job%2)*2);put32(payload+0x1c,1);
        assert(ane_service_work(&ane)==-EPROTO && doorbells==at+1); // missing profile
        put32(payload,job*0x10001);put32(payload+0x1c,3);put64(payload+0x30,0xff00);
        for(unsigned test=0;test<10;test++) {
            u8 backup[96];memcpy(backup,payload,96);
            switch(test) {
            case 0: put32(payload,0x30003); break;
            case 1: put32(payload+4,0x301); break;
            case 2: payload[6]=1; break;
            case 3: put64(payload+8,job+2); break;
            case 4: put64(payload+0x10,1); break;
            case 5: put32(payload+0x18,1); break;
            case 6: put64(payload+0x20,1); break;
            case 7: put64(payload+0x30,0xff01); break;
            case 8: ane.profile_events=false; break;
            case 9: ane.work_profile=true; break;
            }
            assert(ane_service_work(&ane)<0 && doorbells==at+1);
            assert(ring[at+1].pointer==event.iova && !ane.work_end);
            memcpy(payload,backup,96);ane.profile_events=true;ane.work_profile=false;
        }
        assert(ane_service_work(&ane)==0 && ane.work_profile && !ane.work_end);
        ring[at+2]=(struct apple_ane_ipc_slot){.pointer=event.iova,.length=96};
        assert(ane_service_work(&ane)==-EPROTO && doorbells==at+2); // duplicate profile
        put32(payload,0xfe01+(job%2)*2);put32(payload+0x1c,1);put64(payload+0x30,0);
        assert(ane_service_work(&ane)==0 && ane.work_end && doorbells==at+3);
    }
    ring[9]=(struct apple_ane_ipc_slot){.pointer=event.iova,.length=96};
    assert(ane_service_work(&ane)==-EOVERFLOW && doorbells==9);
    // A runtime session crosses the 64-slot boundary repeatedly. Opaque
    // handles are unrelated to producer/consumer index and are never used
    // as pointers or completion identities. The current token still matches.
    memset(ring,0,64*sizeof(*ring));memset(payload,0,sizeof(payload));
    ane.event_head=0;ane.work_events=0;ane.session_mode=true;doorbells=0;
    for(unsigned job=0;job<256;job++) {
        ane.work_token=job+1;ane.work_begin=false;ane.work_end=false;ane.work_profile=false;
        for(unsigned phase=0;phase<3;phase++) {
            unsigned at=(job*3+phase)%64;
            assert(ane.event_head==at);
            ring[at]=(struct apple_ane_ipc_slot){.pointer=event.iova,.length=96};
            put32(payload,0xf1230000U+job*71+phase);put32(payload+4,0x300);
            put64(payload+8,job+1);put32(payload+0x1c,phase==1?3:phase==2?1:0);
            put64(payload+0x30,phase==1?0xff00:0);
            u8 backup[96];memcpy(backup,payload,96);
            put64(payload+8,job+2);
            assert(ane_service_work(&ane)==-ESTALE && ring[at].pointer==event.iova);
            memcpy(payload,backup,96);
            assert(ane_service_work(&ane)==0);
            assert(ring[at].pointer==(event.iova|1) && ring[at].length==96);
            assert(ane.work_end==(phase==2));
        }
        assert(ane.work_events==3*(job+1) && doorbells==3*(job+1));
    }
    free(arena.cpu);
    puts("ANE_LINUX_WORK_C_TESTS_PASS: conv/tiny phases, 256-job ring wrap, ownership, malformed/stale events");
    return 0;
}
'''
    with tempfile.TemporaryDirectory(prefix='ane-work-c-test-') as temporary:
        cfile, executable = Path(temporary) / 'test.c', Path(temporary) / 'test'
        cfile.write_text(code)
        subprocess.run(['clang', '-std=gnu11', '-Wall', '-Wextra', '-Werror',
                        '-fsanitize=address,undefined', '-g', str(cfile), '-o', str(executable)], check=True)
        subprocess.run([str(executable)], check=True)


if __name__ == '__main__':
    main()
