#!/usr/bin/env python3
"""Compile the real T8140 DPTX callbacks with a recording PHY and sanitizers."""
from pathlib import Path
import re
import subprocess
import tempfile

ROOT = Path(__file__).resolve().parents[5]
SOURCE = ROOT / 'drivers/gpu/drm/apple/dptxep.c'


def function(source, name):
    match = re.search(r'static (?:int|bool)\s+' + name + r'\s*\(', source)
    assert match, name
    start = source.index('{', match.start())
    depth = 1
    end = start + 1
    while depth:
        depth += (source[end] == '{') - (source[end] == '}')
        end += 1
    return source[match.start():end]


def structure(source, name):
    match = re.search(r'struct ' + name + r'\s*\{.*?\}\s*(?:__packed)?;', source, re.S)
    assert match, name
    return match.group(0)


PRELUDE = r'''
#include <assert.h>
#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>
#include <errno.h>
typedef uint8_t u8;
typedef uint32_t u32;
typedef uint64_t u64;
typedef uint32_t __le32;
typedef uint64_t __le64;
#define __packed __attribute__((packed))
#define ARRAY_SIZE(a) (sizeof(a) / sizeof((a)[0]))
#define cpu_to_le32(x) ((u32)(x))
#define le32_to_cpu(x) ((u32)(x))
#define cpu_to_le64(x) ((u64)(x))
#define le64_to_cpu(x) ((u64)(x))
#define dev_info(...) do {} while (0)
#define dev_err(...) do {} while (0)
struct phy { int unused; } phy;
union phy_configure_opts {
    struct {
        u32 link_rate, lanes, voltage[4], pre[4];
        bool ssc, set_rate, set_lanes, set_voltages;
    } dp;
};
struct completion { unsigned done; };
static void complete(struct completion *c) { c->done++; }
struct dptx_port {
    union phy_configure_opts phy_ops;
    struct phy *atcphy;
    u32 lane_count, drive_settings[2], training_settings[4][3];
    struct completion linkcfg_completion;
};
struct device { void *of_node; };
struct apple_dcp { struct device *dev; u32 dptx_phy; };
struct endpoint { struct apple_dcp *dcp; };
struct apple_epic_service { void *cookie; struct endpoint *ep; };
static int native_node;
static bool of_device_is_compatible(void *node, const char *name) {
    return node == &native_node && !strcmp(name, "apple,t8140-dcpext");
}
static int phy_calls, phy_result;
static union phy_configure_opts observed;
static int phy_configure(struct phy *p, union phy_configure_opts *opts) {
    assert(p == &phy);
    phy_calls++;
    observed = *opts;
    return phy_result;
}
'''

MAIN = r'''
int main(void) {
    _Static_assert(sizeof(struct dptxport_apcall_training) == 32, "wire header");
    _Static_assert(sizeof(struct dptxport_training_entry) == 12, "wire entry");
    _Static_assert(sizeof(struct dptxport_apcall_max_drive_settings) == 32, "max wire size");
    _Static_assert(offsetof(struct dptxport_training_entry, voltage) == 4, "native voltage");
    _Static_assert(offsetof(struct dptxport_training_entry, preemphasis) == 8, "native preemphasis");
    u8 input[82], output[82], saved[82];
    struct dptxport_apcall_training *req = (void *)(input + 1);
    struct dptxport_apcall_training *reply = (void *)(output + 1);
    struct dptx_port port = {.atcphy = &phy, .lane_count = 4};
    struct device dev = {.of_node = &native_node};
    struct apple_dcp dcp = {.dev = &dev};
    struct endpoint ep = {.dcp = &dcp};
    struct apple_epic_service service = {.cookie = &port, .ep = &ep};
    port.phy_ops.dp.link_rate = 5400;
    port.phy_ops.dp.set_rate = port.phy_ops.dp.set_lanes = true;
    for (u32 count = 0; count <= 4; count++) {
        if (count == 3) continue;
        memset(input, 0, sizeof(input)); memset(output, 0xcc, sizeof(output));
        req->count = count;
        for (u32 i = 0; i < count; i++) {
            req->entries[i].unknown = 0xface0000 + i;
            req->entries[i].voltage = i;
            req->entries[i].preemphasis = 3 - i;
        }
        size_t bytes = 32 + count * 12;
        phy_calls = 0;
        assert(dptxport_call_set_drive_settings(&service, req, bytes, reply, bytes) == 0);
        assert(phy_calls == (count != 0));
        if (count) {
            assert(observed.dp.lanes == count && observed.dp.link_rate == 5400);
            assert(observed.dp.set_voltages && !observed.dp.set_rate && !observed.dp.set_lanes);
            for (u32 i = 0; i < count; i++) {
                assert(observed.dp.voltage[i] == i && observed.dp.pre[i] == 3 - i);
                assert(port.training_settings[i][0] == 0xface0000 + i);
            }
        }
        assert(reply->retcode == 0 && output[0] == 0xcc && output[bytes + 1] == 0xcc);
        req->retcode = count;
        req->count = 0xdead;
        assert(dptxport_call_get_drive_settings(&service, req, 32, reply, bytes) == 0);
        assert(reply->retcode == count && reply->count == 0);
        for (u32 i = 0; i < count; i++) {
            assert(reply->entries[i].unknown == 0xface0000 + i);
            assert(reply->entries[i].voltage == i && reply->entries[i].preemphasis == 3 - i);
        }
    }
    req->count = 4;
    struct dptx_port before = port;
    for (size_t size = 0; size < 80; size++) {
        phy_calls = 0; memset(output, 0xcc, sizeof(output)); memcpy(saved, output, sizeof(saved));
        assert(dptxport_set_native_training(&port, req, size, reply, 80) == -EINVAL);
        assert(dptxport_set_native_training(&port, req, 80, reply, size) == -EINVAL);
        assert(!phy_calls && !memcmp(&before, &port, sizeof(port)) && !memcmp(saved, output, sizeof(saved)));
    }
    for (u32 bad = 3; bad <= 6; bad++) {
        if (bad == 4) continue;
        req->count = bad;
        assert(dptxport_set_native_training(&port, req, 80, reply, 80) == -EINVAL);
    }
    req->count = UINT32_MAX;
    assert(dptxport_set_native_training(&port, req, 80, reply, 80) == -EINVAL);
    req->count = 4;
    for (u32 i = 0; i < 4; i++) {
        req->entries[i].voltage = 4;
        assert(dptxport_set_native_training(&port, req, 80, reply, 80) == -EINVAL);
        req->entries[i].voltage = i;
        req->entries[i].preemphasis = UINT32_MAX;
        assert(dptxport_set_native_training(&port, req, 80, reply, 80) == -EINVAL);
        req->entries[i].preemphasis = 3 - i;
    }
    port.lane_count = 2;
    assert(dptxport_set_native_training(&port, req, 80, reply, 80) == -EINVAL);
    port.lane_count = 4; port.atcphy = NULL;
    assert(dptxport_set_native_training(&port, req, 80, reply, 80) == -ENODEV);
    port.atcphy = &phy; before = port; phy_result = -EIO;
    memcpy(saved, output, sizeof(saved));
    assert(dptxport_set_native_training(&port, req, 80, reply, 80) == -EIO);
    assert(!memcmp(&before, &port, sizeof(port)) && !memcmp(saved, output, sizeof(saved)));
    phy_result = 0;
    req->retcode = 4;
    for (size_t size = 0; size < 80; size++)
        assert(dptxport_get_native_training(&port, req, 32, reply, size) == -EINVAL);
    for (size_t size = 0; size < 32; size++)
        assert(dptxport_get_native_training(&port, req, size, reply, 80) == -EINVAL);
    req->retcode = UINT32_MAX;
    assert(dptxport_get_native_training(&port, req, 32, reply, 80) == -EINVAL);
    req->retcode = 4; port.lane_count = 0;
    assert(dptxport_get_native_training(&port, req, 32, reply, 80) == 0);
    struct dptxport_apcall_max_drive_settings max;
    memset(&max, 0xcc, sizeof(max));
    assert(dptxport_call_get_max_drive_settings(&service, &max, sizeof(max)) == 0);
    assert(max.retcode == 0 && max.max_drive_settings[0] == 0 &&
           max.max_drive_settings[1] == 3 && max.max_drive_settings[2] == 3);
    struct dptxport_apcall_set_active_lane_count lanes = {.lane_count = 2}, lanes_reply;
    assert(dptxport_call_set_active_lane_count(&service, &lanes, sizeof(lanes), &lanes_reply, sizeof(lanes_reply)) == 0);
    assert(port.lane_count == 2 && port.linkcfg_completion.done == 1);
    assert(observed.dp.set_lanes && !observed.dp.set_rate && !observed.dp.set_voltages);
    before = port; phy_result = -EIO; lanes.lane_count = 4;
    assert(dptxport_call_set_active_lane_count(&service, &lanes, sizeof(lanes), &lanes_reply, sizeof(lanes_reply)) == -EIO);
    assert(!memcmp(&before, &port, sizeof(port)));
    phy_result = 0; lanes.lane_count = UINT64_MAX;
    assert(dptxport_call_set_active_lane_count(&service, &lanes, sizeof(lanes), &lanes_reply, sizeof(lanes_reply)) == -EINVAL);
    assert(dptxport_set_native_lanes(&port, 3) == -EINVAL);
    dev.of_node = NULL;
    struct dptxport_apcall_drive_settings old = {.unk5 = 17, .unk7 = 23}, old_reply;
    phy_calls = 0;
    assert(dptxport_call_set_drive_settings(&service, &old, sizeof(old), &old_reply, sizeof(old_reply)) == 0);
    assert(!phy_calls && port.drive_settings[0] == 17 && port.drive_settings[1] == 23);
    assert(dptxport_call_get_drive_settings(&service, &old, sizeof(old), &old_reply, sizeof(old_reply)) == 0);
    assert(old_reply.unk5 == 17 && old_reply.unk6 == 0 && old_reply.unk7 == 23);
    assert(dptxport_call_get_max_drive_settings(&service, &max, sizeof(max)) == 0);
    assert(max.max_drive_settings[0] == 3 && max.max_drive_settings[1] == 3);
    puts("T8140 DPTX native packet, PHY failure, buffer bounds and legacy tests PASS");
    return 0;
}
'''


def main():
    source = SOURCE.read_text()
    structs = ['dptxport_apcall_set_active_lane_count', 'dptxport_apcall_max_drive_settings',
               'dptxport_apcall_drive_settings', 'dptxport_training_entry', 'dptxport_apcall_training']
    functions = ['dptxport_has_native_training', 'dptxport_set_native_training',
                 'dptxport_get_native_training', 'dptxport_set_native_lanes',
                 'dptxport_call_get_max_drive_settings', 'dptxport_call_get_drive_settings',
                 'dptxport_call_set_drive_settings', 'dptxport_call_set_active_lane_count']
    code = PRELUDE + '\n'.join(structure(source, name) for name in structs)
    code += '\n'.join(function(source, name) for name in functions) + MAIN
    with tempfile.TemporaryDirectory() as tmp:
        path = Path(tmp)
        (path / 'test.c').write_text(code)
        subprocess.run(['clang', '-std=gnu11', '-Wall', '-Wextra', '-Werror',
                        '-fsanitize=address,undefined', str(path / 'test.c'),
                        '-o', str(path / 'test')], check=True)
        subprocess.run([str(path / 'test')], check=True)


if __name__ == '__main__':
    main()
