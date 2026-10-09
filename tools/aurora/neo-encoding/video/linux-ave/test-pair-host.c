/* SPDX-License-Identifier: GPL-2.0-only */
/* Execute the actual paired-command implementation against bounded fake rings.
 * This tests protocol ordering and errors, not AVE firmware or hardware.
 */
#include <assert.h>
#include <errno.h>
#include <stdbool.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/types.h>
typedef uint8_t u8;
typedef uint32_t u32;
typedef uint64_t u64;
typedef uint32_t __le32;
typedef int64_t loff_t;
#define __user
#define __force
#define CAP_SYS_RAWIO 1
#define SZ_64K 65536
#define AVE_COMMAND_OFF 0x40000
#define AVE_ARENA_OFF 0x80000
#define NSEC_PER_SEC 1000000000ULL
#define BIT(x) (1U << (x))
#define THIS_MODULE NULL
#undef static_assert
#define static_assert(condition) _Static_assert(condition, #condition)
#define le32_to_cpu(x) (x)
struct channel { unsigned int id; };
struct apple_ave {
    struct { u8 *cpu; size_t size; } ipc;
    u64 ipc_fw_base, commands, replies, transport_acks;
    unsigned int lock;
    u8 mailbox[32];
    struct channel channel[2];
    bool ipc_ready, command_failed;
    u8 response[SZ_64K];
    u32 response_bytes;
};
struct file { struct apple_ave *private_data; };
struct file_operations {
    void *owner; int (*open)(void);
    ssize_t (*write)(struct file *,const char *,size_t,loff_t *);
    ssize_t (*read)(struct file *,char *,size_t,loff_t *);
};
static bool privileged = true;
static u64 now;
static bool capable(int capability) { return privileged; }
static void mutex_lock(unsigned int *lock) { assert(!*lock); *lock=1; }
static void mutex_unlock(unsigned int *lock) { assert(*lock==1); *lock=0; }
static int copy_from_user(void *dst,const void *src,size_t n) { memcpy(dst,src,n); return 0; }
static u64 ktime_get_ns(void) { return now; }
static u64 div_u64(u64 a,u32 b) { return a/b; }
static void usleep_range(unsigned int a,unsigned int b) { now+=100000000; }
static u32 readl(const void *p) { return 0; }
static void writel(u32 value,void *p) {}
static unsigned int hweight32(u32 value) { return (unsigned int)__builtin_popcount(value); }
static u32 get_unaligned_le32(const void *p) { u32 v; memcpy(&v,p,4); return v; }
static u64 get_unaligned_le64(const void *p) { u64 v; memcpy(&v,p,8); return v; }
static void put_unaligned_le32(u32 value,void *p) { memcpy(p,&value,4); }
static void put_unaligned_le64(u64 value,void *p) { memcpy(p,&value,8); }
static int simple_open(void) { return 0; }
static ssize_t ave_command_read(struct file *f,char *data,size_t len,loff_t *pos) { return 0; }
static int ave_fw_offset(struct apple_ave *ave,u64 address,size_t bytes,u32 *offset) {
    u64 d=address-ave->ipc_fw_base;
    if(address<ave->ipc_fw_base || d>ave->ipc.size || bytes>ave->ipc.size-d)return -ERANGE;
    *offset=(u32)d;return 0;
}
enum scenario { NORMAL, REVERSED, COMPLETE_DEPENDENCY, MISSING_VIEW, BAD_ACK,
    DUPLICATE_ACK, BAD_STATUS, DUPLICATE_REPLY, BAD_LENGTH, SECOND_SEND_FAIL };
struct event { u64 address; u32 bytes; };
static struct event queue[2][32];
static unsigned int producer[2],consumer[2],reply_slot;
static enum scenario scenario;
static u32 sent;
static void push(unsigned int ch,u64 address,u32 bytes) {
    assert(producer[ch]<32);queue[ch][producer[ch]++]=(struct event){address,bytes};
}
static void response(struct apple_ave *ave,u32 opcode,u32 kind) {
    u32 offset=0x1000+reply_slot++*256,bytes=opcode==8?80:72;
    u8 *p=ave->ipc.cpu+offset;memset(p,0,256);
    put_unaligned_le32(opcode,p);put_unaligned_le32(7,p+0x10);put_unaligned_le32(kind,p+0x20);
    if(scenario==BAD_STATUS && opcode==8)put_unaligned_le32(1,p+4);
    if(scenario==BAD_LENGTH && opcode==8)bytes=100;
    push(1,ave->ipc_fw_base+offset,bytes);
}
static int ave_ring_send(struct apple_ave *ave,struct channel *ch,u64 address,u32 bytes,u32 flags) {
    if(ch->id==1)return 0; /* Reply acknowledgment. */
    unsigned int slot=(unsigned int)(address-ave->ipc_fw_base-AVE_COMMAND_OFF)/SZ_64K;
    assert(slot<4 && !(sent&BIT(slot)));
    if(scenario==SECOND_SEND_FAIL && slot==1)return -ENOSPC;
    const u8 *cmd=ave->ipc.cpu+(size_t)(address-ave->ipc_fw_base);
    assert(get_unaligned_le64(cmd+0x30)==3000000);
    sent|=BIT(slot);
    push(0,scenario==BAD_ACK?address+1:address,0);
    if(scenario==DUPLICATE_ACK && slot==0)push(0,address,0);
    if(slot==1) {
        unsigned int first=scenario==REVERSED?1:0;
        response(ave,8,0x15+first);
        if(scenario==DUPLICATE_REPLY)response(ave,8,0x15+first);
        if(scenario!=COMPLETE_DEPENDENCY && scenario!=MISSING_VIEW)response(ave,8,0x16-first);
    } else if(slot>=2) {
        if(scenario==COMPLETE_DEPENDENCY && slot==2)response(ave,8,0x16);
        response(ave,9,9);
    }
    return 0;
}
static int ave_ring_receive(struct apple_ave *ave,struct channel *ch,u64 *addr,u32 *bytes,u32 *flags) {
    if(consumer[ch->id]==producer[ch->id])return -EAGAIN;
    struct event e=queue[ch->id][consumer[ch->id]++];*addr=e.address;*bytes=e.bytes;*flags=0;return 0;
}
#include "apple-ave-pair.h"

static u8 *packet(size_t *length) {
    const u32 sizes[]={AVE_PAIR_PROCESS_BYTES,AVE_PAIR_PROCESS_BYTES,72,72};
    *length=20+2*AVE_PAIR_PROCESS_BYTES+144;u8 *data=calloc(1,*length);assert(data);
    put_unaligned_le32(AVE_PAIR_MAGIC,data);size_t offset=20;
    for(unsigned int i=0;i<4;i++) {
        put_unaligned_le32(sizes[i],data+4+i*4);u8 *p=data+offset;
        put_unaligned_le32(i<2?8:9,p);put_unaligned_le64(100+i,p+8);
        put_unaligned_le64(7,p+0x10);put_unaligned_le32(i<2?0x15+i:9,p+0x20);
        if(i<2) {put_unaligned_le32(i,p+0xac84);put_unaligned_le32(i,p+0xad34);}
        offset+=sizes[i];
    }
    return data;
}
static void reset(struct apple_ave *ave,enum scenario next) {
    memset(ave,0,sizeof(*ave));ave->ipc.cpu=calloc(1,0x100000);assert(ave->ipc.cpu);
    ave->ipc.size=0x100000;ave->ipc_fw_base=0xffffffff82000000;ave->ipc_ready=true;
    ave->channel[1].id=1;scenario=next;now=1;sent=reply_slot=0;
    memset(producer,0,sizeof(producer));memset(consumer,0,sizeof(consumer));privileged=true;
}
int main(void) {
    unsigned int checks=0;size_t bytes;u8 *data=packet(&bytes);struct apple_ave ave;
    struct file file={&ave};loff_t pos=0;
    assert(ave_pair_fops.write==ave_pair_write);
    for(enum scenario s=NORMAL;s<=SECOND_SEND_FAIL;s++) {
        reset(&ave,s);ssize_t got=ave_pair_write(&file,(const char *)data,bytes,&pos);
        if(s<=COMPLETE_DEPENDENCY) {
            assert(got==(ssize_t)bytes && !ave.command_failed && sent==15);
            assert(ave.commands==4 && ave.transport_acks==4 && ave.replies==4);
            assert(ave.response_bytes==400);
        } else {
            assert(got<0 && ave.command_failed);
            assert(got==(s==MISSING_VIEW?-ETIMEDOUT:s==SECOND_SEND_FAIL?-ENOSPC:-EPROTO));
        }
        assert(!ave.lock);free(ave.ipc.cpu);checks++;
    }
    for(unsigned int kind=0;kind<7;kind++) {
        reset(&ave,NORMAL);u8 *bad=malloc(bytes);assert(bad);memcpy(bad,data,bytes);size_t count=bytes;
        if(kind==0)count--;
        if(kind==1)put_unaligned_le32(0,bad);
        if(kind==2)put_unaligned_le32(0,bad+4);
        if(kind==3)put_unaligned_le64(8,bad+20+AVE_PAIR_PROCESS_BYTES+0x10);
        if(kind==4)put_unaligned_le64(100,bad+20+AVE_PAIR_PROCESS_BYTES+8);
        if(kind==5)put_unaligned_le32(0,bad+20+AVE_PAIR_PROCESS_BYTES+0xac84);
        if(kind==6)privileged=false;
        assert(ave_pair_write(&file,(const char *)bad,count,&pos)==(kind==6?-EPERM:-EINVAL));
        assert(!sent && !ave.command_failed && !ave.lock);free(bad);free(ave.ipc.cpu);checks++;
    }
    free(data);printf("AVE_PAIR_HOST_TESTS %u passed; ordering, completion dependency and fault paths; hardware untested\n",checks);
    return 0;
}
