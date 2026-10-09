#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-2.0-only OR MIT
"""Check the H17P A408 record against a reference layout.

The reference is the record as laid out by the earlier fix-up, which built
a 0x58a-byte swap and shifted every surface and the request tail down by two
bytes before submission.  The current layout must put the same bytes on the
wire for every surface the driver builds (linear, so is_tiled = 0), and the
fix-up must be safe to apply twice.  On H17P the one intended difference
is that each surface keeps the is_premultiplied value plane.c chose (X
formats are scanned out premultiplied); other firmware still forces it 0.
"""
from pathlib import Path
import os
import re
import subprocess
import tempfile

here = Path(__file__).parent


def block(source, marker, opener='{'):
    start = source.index(marker)
    end = source.index(opener, start) + 1
    depth = 1
    while depth:
        depth += (source[end] == '{') - (source[end] == '}')
        end += 1
    if source[end:end + 10].startswith(' __packed;'):
        end += len(' __packed;')
    elif source[end] == ';':
        end += 1
    return source[start:end]


def fw_structs(header, suffix):
    out = []
    for name in ('dcp_swap', 'dcp_surface', 'dcp_swap_submit_req'):
        text = block(header, 'struct DCP_FW_NAME(%s) {' % name)
        out.append(text)
    text = '\n'.join(out)
    return re.sub(r'DCP_FW_NAME\((\w+)\)', r'\1_' + suffix, text)


def fix_function(template, suffix):
    marker = 'static void dcp_h17p_fix_swap_surfaces(struct apple_dcp *dcp)\n{'
    text = block(template, marker)
    text = text.replace('dcp_h17p_fix_swap_surfaces',
                        'fix_' + suffix)
    text = text.replace('struct DCP_FW_NAME(dcp_swap_submit_req)',
                        'struct req_' + suffix)
    text = text.replace('&DCP_FW_UNION(dcp->swap)', '(void *)dcp')
    text = text.replace('dcp->fw_compat', 'test_fw_compat')
    return text.replace('struct apple_dcp *dcp', 'void *dcp')


def git_show(rev, path):
    return subprocess.check_output(
        ['git', '-C', str(here), 'show', rev + ':./' + path], text=True)


plane = (here / 'iomfb_plane.h').read_text()
plane_types = '\n'.join(block(plane, 'struct %s {' % n) for n in (
    'dcp_rect', 'dcp_plane_info', 'dcp_component_types', 'dcp_surface'))

# The commit before the layout correction holds the reference fix-up.
ref_rev = os.environ.get('H17P_REF_REV', '7c58cf537b227')
old_h = git_show(ref_rev, 'iomfb_template.h')
old_c = git_show(ref_rev, 'iomfb_template.c')
new_h = (here / 'iomfb_template.h').read_text()
new_c = (here / 'iomfb_template.c').read_text()

# The reference layout is reproduced from the old 0x58a swap struct.
assert 'u8 h17p_tail[0xda];' in old_h

prelude = r'''
#include <assert.h>
#include <stdbool.h>
#include <stdint.h>
#include <stddef.h>
#include <stdlib.h>
#include <string.h>
#include <stdio.h>
typedef uint8_t u8; typedef uint16_t u16; typedef uint32_t u32;
typedef uint64_t u64; typedef uint64_t __le64;
#define __packed __attribute__((packed))
#define DCP_FW_VER 0x1a0600
#define DCP_FW_VERSION(x, y, z) (((x) << 16) | ((y) << 8) | (z))
#define DCP_SURF_MAX_PLANES 3
#define SWAP_SURFACES 4
#define DCP_SWAP_FLAGS1 0x34865202ULL
#define DCP_SWAP_FLAGS2 4ULL
#define req_old dcp_swap_submit_req_old
#define req_new dcp_swap_submit_req_new
static char dcp_h17p_swap_poke[128];
enum { DCP_FIRMWARE_OTHER, DCP_FIRMWARE_H17P };
static int test_fw_compat = DCP_FIRMWARE_OTHER;
static unsigned long simple_strtoul(const char *c, char **e, int b)
{ return strtoul(c, e, b); }
'''

harness = r'''
static unsigned int seed = 1;
static u8 rnd(void) { seed = seed * 1103515245u + 12345u; return seed >> 16; }

int main(void)
{
	static struct req_old o;
	static struct req_new n, twice;
	size_t tail = sizeof(o) - offsetof(struct req_old, surf_iova);

	static_assert(sizeof(o) == 0xe9c, "reference size");
	static_assert(sizeof(n) == 0xe9c, "request size");
	static_assert(offsetof(struct req_new, surf) == 0x588, "surface 0");
	static_assert(offsetof(struct req_new, surf_iova) == 0xe38, "surf_iova");
	static_assert(offsetof(struct req_new, swap_null) == 0xe92, "swap_null");

	for (int iter = 0; iter < 2000; iter++) {
		for (size_t i = 0; i < sizeof(o); i++)
			((u8 *)&o)[i] = rnd();
		/* Bytes the builder never writes and the old memmove cleared. */
		o.swap.h17p_tail[0xd8] = o.swap.h17p_tail[0xd9] = 0;
		for (int s = 0; s < SWAP_SURFACES; s++)
			o.surf[s].base.is_tiled = 0;
		o.h17p_pad_ref[0] = o.h17p_pad_ref[1] = 0;

		memset(&n, 0, sizeof(n));
		memcpy(&n.swap, &o.swap, sizeof(n.swap));
		memcpy(n.surf, o.surf, sizeof(n.surf));
		memcpy(&n.surf_iova, &o.surf_iova, tail - 2);

		if (iter & 1)
			strcpy(dcp_h17p_swap_poke, "58a=1,e38=7");
		else
			dcp_h17p_swap_poke[0] = 0;

		fix_old(&o);
		fix_new(&n);
		if (memcmp(&o, &n, sizeof(n))) {
			for (size_t i = 0; i < sizeof(n); i++)
				if (((u8 *)&o)[i] != ((u8 *)&n)[i]) {
					printf("iter %d: byte %#zx differs: %02x != %02x\n",
					       iter, i, ((u8 *)&o)[i], ((u8 *)&n)[i]);
					return 1;
				}
		}
		twice = n;
		fix_new(&twice);
		if (memcmp(&twice, &n, sizeof(n))) {
			printf("iter %d: fix-up is not idempotent\n", iter);
			return 1;
		}
	}
	/* H17P: identical except that is_premultiplied passes through. */
	test_fw_compat = DCP_FIRMWARE_H17P;
	for (int iter = 0; iter < 2000; iter++) {
		u8 premul[SWAP_SURFACES];

		for (size_t i = 0; i < sizeof(o); i++)
			((u8 *)&o)[i] = rnd();
		o.swap.h17p_tail[0xd8] = o.swap.h17p_tail[0xd9] = 0;
		for (int s = 0; s < SWAP_SURFACES; s++)
			o.surf[s].base.is_tiled = 0;
		o.h17p_pad_ref[0] = o.h17p_pad_ref[1] = 0;

		memset(&n, 0, sizeof(n));
		memcpy(&n.swap, &o.swap, sizeof(n.swap));
		memcpy(n.surf, o.surf, sizeof(n.surf));
		memcpy(&n.surf_iova, &o.surf_iova, tail - 2);
		for (int s = 0; s < SWAP_SURFACES; s++)
			premul[s] = n.surf[s].base.is_premultiplied = rnd() & 1;

		if (iter & 1)
			strcpy(dcp_h17p_swap_poke, "e38=7");
		else
			dcp_h17p_swap_poke[0] = 0;

		fix_old(&o);
		fix_new(&n);
		for (int s = 0; s < SWAP_SURFACES; s++) {
			if (n.surf[s].base.is_premultiplied != premul[s]) {
				printf("iter %d: surface %d is_premultiplied not kept\n", iter, s);
				return 1;
			}
			/* the reference forced it 0; expect the kept value on the wire */
			((u8 *)&o)[offsetof(struct req_new, surf[s].base.is_premultiplied)] = premul[s];
		}
		if (memcmp(&o, &n, sizeof(n))) {
			printf("iter %d: H17P record differs beyond is_premultiplied\n", iter);
			return 1;
		}
		twice = n;
		fix_new(&twice);
		if (memcmp(&twice, &n, sizeof(n))) {
			printf("iter %d: H17P fix-up is not idempotent\n", iter);
			return 1;
		}
	}
	if (offsetof(struct req_new, surf[0].base.is_premultiplied) != 0x58a) {
		puts("surface 0 is_premultiplied is not at 0x58a");
		return 1;
	}
	puts("h17p swap layout: 2000 records identical, fix-up idempotent; "
	     "H17P keeps is_premultiplied (2000 records)");
	return 0;
}
'''

old_structs = fw_structs(old_h, 'old')
new_structs = fw_structs(new_h, 'new')

# The reference request has no explicit pad; its last two bytes are the
# shifted-out tail.  Model them as a trailing array of a wrapper.
src = prelude + plane_types + '\n' + old_structs + '\n' + new_structs + '\n'
src = src.replace('struct dcp_swap_submit_req_old {',
                  'struct dcp_swap_submit_req_old_body {', 1)
src += r'''
struct dcp_swap_submit_req_old {
	struct dcp_swap_old swap;
	struct dcp_surface_old surf[SWAP_SURFACES];
	u64 surf_iova[SWAP_SURFACES];
	u8 rest[sizeof(struct dcp_swap_submit_req_old_body) -
		offsetof(struct dcp_swap_submit_req_old_body, unk_u64_a) - 2];
	u8 h17p_pad_ref[2];
} __packed;
'''
src += fix_function(old_c, 'old') + '\n' + fix_function(new_c, 'new') + '\n'
src += harness

with tempfile.TemporaryDirectory() as d:
    c = Path(d) / 'layout.c'
    exe = Path(d) / 'layout'
    c.write_text(src)
    subprocess.run(['cc', '-std=gnu11', '-Wall', '-Werror',
                    '-Wno-unused-function', '-Wno-address-of-packed-member',
                    '-o', str(exe), str(c)], check=True)
    subprocess.run([str(exe)], check=True)
