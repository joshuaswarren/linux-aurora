#!/usr/bin/env python3
"""Exercise the actual ACK handler with the native nested-call wire layout."""
from pathlib import Path
import subprocess
import tempfile

root = next(candidate for parent in Path(__file__).resolve().parents
            for candidate in (parent, parent / 'linux')
            if (candidate / 'drivers/gpu/drm/apple/iomfb.c').is_file())
source = (root / 'drivers/gpu/drm/apple/iomfb.c').read_text()


def function(declaration):
    start = source.index(declaration)
    opening = source.index('{', start)
    end, depth = opening + 1, 1
    while depth:
        depth += (source[end] == '{') - (source[end] == '}')
        end += 1
    return source[start:end]


harness = r'''
#include <assert.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>
typedef uint8_t u8;
enum dcp_context_id { DCP_CONTEXT_CB = 0, DCP_CONTEXT_CMD = 2 };
struct apple_dcp;
typedef void (*dcp_callback_t)(struct apple_dcp *, void *, void *);
struct dcp_channel {
    dcp_callback_t callbacks[4];
    void *cookies[4], *output[4];
    u8 depth;
};
struct apple_dcp { struct dcp_channel cb, cmd; void *dev; };
static unsigned warnings, completions;
#define WARN_ON(condition) ((condition) ? ++warnings : 0)
#define dev_warn(...) (++warnings)
static struct dcp_channel *dcp_get_channel(struct apple_dcp *dcp,
                                          enum dcp_context_id context) {
    if (context == DCP_CONTEXT_CB) return &dcp->cb;
    if (context == DCP_CONTEXT_CMD) return &dcp->cmd;
    return NULL;
}
''' + function('static u8 dcp_pop_depth(') + '\n' + function('static void dcpep_handle_ack(') + r'''
static uint32_t *expected;
static void complete(struct apple_dcp *dcp, void *out, void *cookie) {
    (void)dcp;
    assert(out == expected && cookie == expected);
    assert(((uint32_t *)out)[0] == 2560 && ((uint32_t *)out)[1] == 1664);
    completions++;
}
int main(void) {
    /* Native nested A445 command starts at CMD+0x40, has a12-byte header
     * and4-byte input. Its ACK has context0 and offset0; the unrelated
     * callback header at CB+0 must not be used to locate the response.
     */
    uint32_t command[64] = {0}, callback[64] = {0};
    struct apple_dcp dcp = {0};
    expected = command + (0x40 + 12 + 4) / 4;
    expected[0] = 2560; expected[1] = 1664;
    callback[3] = 0; callback[4] = 1819308905;
    dcp.cb.depth = 2;
    dcp.cb.output[0] = callback + 3;
    dcp.cb.output[1] = expected;
    dcp.cb.callbacks[1] = complete;
    dcp.cb.cookies[1] = expected;
    dcpep_handle_ack(&dcp, DCP_CONTEXT_CB);
    assert(completions == 1 && dcp.cb.depth == 1);
    assert(dcp.cb.output[0] == callback + 3);
    assert(!dcp.cb.output[1] && !dcp.cb.callbacks[1] && !dcp.cb.cookies[1]);
    dcp.cmd.depth = 1;
    dcp.cmd.output[0] = expected;
    dcp.cmd.callbacks[0] = complete;
    dcp.cmd.cookies[0] = expected;
    dcpep_handle_ack(&dcp, DCP_CONTEXT_CMD);
    assert(completions == 2 && dcp.cmd.depth == 0);
    dcpep_handle_ack(&dcp, DCP_CONTEXT_CMD);
    dcpep_handle_ack(&dcp, (enum dcp_context_id)99);
    assert(completions == 2 && dcp.cmd.depth == 0 && warnings == 2);
    puts("DCP ACK regression: nested/main responses and empty/invalid contexts pass");
    return 0;
}
'''
with tempfile.TemporaryDirectory(prefix='m5-dcp-ack-') as directory:
    path = Path(directory)
    (path / 'test.c').write_text(harness)
    subprocess.run(['clang', '-std=c11', '-Wall', '-Wextra', '-Werror',
                    '-fsanitize=address,undefined', str(path / 'test.c'),
                    '-o', str(path / 'test')], check=True)
    subprocess.run([str(path / 'test')], check=True)
