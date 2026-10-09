// SPDX-License-Identifier: GPL-2.0-only OR MIT
/* Copyright 2021 Alyssa Rosenzweig */

/*
 * This file is intended to be included multiple times with IOMFB_VER
 * defined to declare DCP firmware version dependent structs.
 */

#ifdef DCP_FW_VER

#include <drm/drm_crtc.h>

#include <linux/types.h>

#include "iomfb.h"
#include "iomfb_plane.h"
#include "plane.h"
#include "version_utils.h"

struct DCP_FW_NAME(dcp_swap) {
	/* IOMobileFramebuffer timestamp types 1 through 7, in wire order. */
	u64 timestamp[7];
	u64 unk_38;

	u64 flags1;
	u64 flags2;

#if DCP_FW_VER >= DCP_FW_VERSION(26, 0, 0)
	/*
	 * H17P inserts 0x48 bytes between flags2 and the swap ID.  Live macOS
	 * 25G83 traces put the value A406 returned as the swap id at +0x98, in
	 * 1919 of 1919 swaps across two independent captures; at the pre-H17P
	 * +0x50 it matches in 0 of 1919.  Every following field lines up once
	 * this shift is applied: surf_ids +0x9c, src_rect +0xac, surf_flags
	 * +0xec, dst_rect +0x10c, swap_enabled/completed +0x14c/+0x150,
	 * bg_color +0x154.  The two u32s the firmware does keep in this gap
	 * (at +0x50 and +0x78) are small counters we do not model yet.
	 */
	u8 h17p_pre_swap_id[0x48];
#endif

	u32 swap_id;

	u32 surf_ids[SWAP_SURFACES];
	struct dcp_rect src_rect[SWAP_SURFACES];
	u32 surf_flags[SWAP_SURFACES];
	u32 surf_unk[SWAP_SURFACES];
	struct dcp_rect dst_rect[SWAP_SURFACES];
	u32 swap_enabled;
	u32 swap_completed;

	u32 bg_color;
	u8 unk_110[0x1b8];
	u32 unk_2c8;
	u8 unk_2cc[0x14];
	u32 unk_2e0;
#if DCP_FW_VER < DCP_FW_VERSION(13, 2, 0)
	u16 unk_2e2;
#else
	u8 unk_2e2[3];
#endif
	u64 bl_unk;
	u32 bl_value; // min value is 0x10000000
	u8  bl_power; // constant 0x40 for on
#if DCP_FW_VER >= DCP_FW_VERSION(26, 0, 0)
	/*
	 * H17P moved the backlight block and changed its shape.  In the macOS
	 * 25G83 capture (223 swaps) exactly one A408 changes the brightness,
	 * and the firmware answers it with "brightness_changed: brightness
	 * value: 5957818" (= 90.909 nits in 16.16) and "send_brightness: dbv:
	 * 1402".  That swap carries, at +0x354, a u8 1 and at +0x358 the bytes
	 * 01 01 01 00 01 00, followed by a run of IEEE-754 doubles at an 8-byte
	 * stride from +0x35e: [0] = 90.909 (the nits), [2] = [3] = 477.38
	 * (slowly varying in later swaps, ambient light by the look of it),
	 * [4] = 1.0 and [17] = 1.0.  Every other swap leaves the block zero
	 * apart from the bl_h17p_d[2]/[3] pair.  The old bl_unk/bl_value/
	 * bl_power slots above are zero in all of them.
	 */
	u8 unk_33c[0x18];
	u8 bl_h17p_update;	/* +0x354 */
	u8 unk_355[3];
	u8 bl_h17p_flags[6];	/* +0x358: 01 01 01 00 01 00 on an update */
	__le64 bl_h17p_d[18];	/* +0x35e: doubles, [0] = nits */
	u8 unk_3ee[0xba];
	u64 unk_1;
#else
	u8 unk_2f3[0x2d];
#if DCP_FW_VER >= DCP_FW_VERSION(13, 2, 0)
	u8 unk_320[0x13f];
	u64 unk_1;
#endif
#endif
#if DCP_FW_VER >= DCP_FW_VERSION(26, 0, 0)
	/*
	 * The swap record is 0x588 bytes: the first surface starts there with
	 * the usual is_tiled/is_tearing_allowed/is_premultiplied header, and
	 * the request carries two trailing pad bytes instead (see
	 * dcp_swap_submit_req).  Byte 0x58a selects premultiplied blending:
	 * with it set, an alpha 0x80 frame gives a different blend CRC from
	 * the straight-alpha one, while alpha 0x00 and 0xff are unchanged.
	 */
	u8 h17p_tail[0xd8];
#endif
} __packed;

/* Information describing a surface */
struct DCP_FW_NAME(dcp_surface) {
	struct dcp_surface base;
#if DCP_FW_VER < DCP_FW_VERSION(13, 2, 0)
	u8 padding[7];
#else
	u8 padding[47];
#endif
} __packed;

/* Prototypes */

struct DCP_FW_NAME(dcp_swap_submit_req) {
	struct DCP_FW_NAME(dcp_swap) swap;
	struct DCP_FW_NAME(dcp_surface) surf[SWAP_SURFACES];
	u64 surf_iova[SWAP_SURFACES];
#if DCP_FW_VER >= DCP_FW_VERSION(13, 2, 0)
	u64 unk_u64_a[SWAP_SURFACES];
/* H17P (26.x) carries no surf2 block: the trailing u8 null-flag run in a
 * captured A408 record is 8 bytes (swap_null + surf_null[4] + three tail
 * bools), where a surf2-bearing layout would add five or six more. */
#if DCP_FW_VER < DCP_FW_VERSION(26, 0, 0)
	struct DCP_FW_NAME(dcp_surface) surf2[5];
	u64 surf2_iova[5];
#endif
#endif
	u8 unkbool;
	u64 unkdouble;
#if DCP_FW_VER >= DCP_FW_VERSION(13, 2, 0)
	u64 unkU64;
	u8 unkbool2;
#endif
	u32 clear; // or maybe switch to default fb?
#if DCP_FW_VER >= DCP_FW_VERSION(13, 2, 0)
	u32 unkU32Ptr;
#endif
	u8 swap_null;
	u8 surf_null[SWAP_SURFACES];
#if DCP_FW_VER >= DCP_FW_VERSION(13, 2, 0)
#if DCP_FW_VER < DCP_FW_VERSION(26, 0, 0)
	u8 surf2_null[5];
#endif
#endif
	u8 unkoutbool_null;
#if DCP_FW_VER >= DCP_FW_VERSION(13, 2, 0)
	u8 unkU32Ptr_null;
	u8 unkU32out_null;
#endif
#if DCP_FW_VER < DCP_FW_VERSION(26, 0, 0)
	u8 padding[1];
#else
	u8 h17p_pad[2];
#endif
} __packed;

struct DCP_FW_NAME(dcp_swap_submit_resp) {
	u8 unkoutbool;
#if DCP_FW_VER >= DCP_FW_VERSION(13, 2, 0)
	u32 unkU32out;
#endif
	u32 ret;
	u8 padding[3];
} __packed;

/*
 * A406 swap_start / A473 set_power_state.  H17P shrank both requests: the
 * 0x10-byte struct dcp_iouserclient became a bare u64 handle and the swap_start
 * response no longer echoes it.  Sizes are the literal wire headers from macOS
 * 25G83 on this machine (0x10/0x8 across 1919 swaps, and the sole OOBCMD call).
 */
#if DCP_FW_VER >= DCP_FW_VERSION(26, 0, 0)
struct DCP_FW_NAME(dcp_swap_start_req) {
	u32 swap_id;
	u64 client;			/* IOUserClient kernel VA */
	u8 client_null;
	u8 padding[3];
} __packed;

struct DCP_FW_NAME(dcp_swap_start_resp) {
	u32 swap_id;
	u32 ret;
} __packed;

struct DCP_FW_NAME(dcp_set_power_state_req) {
	u64 unklong;
	u32 unkint;			/* widened from u8 on H17P */
	u8 unkint_null;
	u8 padding[3];
} __packed;

static_assert(sizeof(struct DCP_FW_NAME(dcp_swap_start_req)) == 0x10);
static_assert(sizeof(struct DCP_FW_NAME(dcp_swap_start_resp)) == 0x8);
static_assert(sizeof(struct DCP_FW_NAME(dcp_set_power_state_req)) == 0x10);
#else
struct DCP_FW_NAME(dcp_swap_start_req) {
	u32 swap_id;
	struct dcp_iouserclient client;
	u8 swap_id_null;
	u8 client_null;
	u8 padding[2];
} __packed;

struct DCP_FW_NAME(dcp_swap_start_resp) {
	u32 swap_id;
	struct dcp_iouserclient client;
	u32 ret;
} __packed;

struct DCP_FW_NAME(dcp_set_power_state_req) {
	u64 unklong;
	u8 unkbool;
	u8 unkint_null;
	u8 padding[2];
} __packed;
#endif

/*
 * D201 map_piodma reply.  H17P declares a 16-byte output, not 20: writing the
 * pre-H17P struct into it overruns the record by four bytes and puts every
 * field where the firmware does not look, so the display engine ends up with a
 * null base and DMAs to address 0.
 */
#if DCP_FW_VER >= DCP_FW_VERSION(26, 0, 0)
/*
 * H17P's D201 reply is { index, dva, pad }, not { vaddr, dva }.  Decoded from
 * the macOS 25G83 capture, which answers e.g. buffer 6 with
 *   06000000 0040b10000010000 00000000   -> index 6, dva 0x100_00b14000
 * With the old layout the DVA lands four bytes late, the firmware reads a
 * shifted value and DMAs to it, and the coprocessor dies in
 * apiodma_irq_AxiErr().
 */
struct DCP_FW_NAME(dcp_map_buf_resp) {
	u32 buffer;
	u64 dva;
	u32 unk;
} __packed;
static_assert(sizeof(struct DCP_FW_NAME(dcp_map_buf_resp)) == 0x10);
#else
struct DCP_FW_NAME(dcp_map_buf_resp) {
	u64 vaddr;
	u64 dva;
	u32 ret;
} __packed;
#endif

struct DCP_FW_NAME(dc_swap_complete_resp) {
	u32 swap_id;			/* +0x000 */
	u8 unkbool;			/* +0x004 */
#if DCP_FW_VER >= DCP_FW_VERSION(26, 0, 0)
	/*
	 * H17P: swap_complete_ap_gated(u32, bool, SwapCompleteData const *,
	 * SwapInfoBlob const *, u32, bool) is 0x730 bytes on the wire, 89 more
	 * than the pre-H17P record.  Byte-exact against 1919 D590 callbacks
	 * captured from macOS 25G83 on this machine: the SwapCompleteData grew
	 * from 8 to 0x22, swap_info became eight 0xe0-byte SwapInfoBlobs rather
	 * than one 0x6c5 blob, and the tail gained a flag and three pad bytes.
	 * Only swap_info_count of those blobs are populated (always 1 in the
	 * captures); the rest are uninitialised DCP buffer memory.
	 */
	u32 hol_swap_ids[4];		/* +0x005, zero in 1919/1919 */
	u32 hol_arg;			/* +0x015 */
	u32 hol_count;			/* +0x019 */
	u8 hol_valid;			/* +0x01d */
	u64 displayed_data;		/* +0x01e, unaligned */
	u8 notify_displayed;		/* +0x026 */
	u8 swap_info[8][0xe0];		/* +0x027, blob[0]+0x38 == swap_id */
	u32 swap_info_count;		/* +0x727, 1 in 1919/1919, max 8 */
	u8 completion_flag;		/* +0x72b */
	u8 swap_data_null;		/* +0x72c, 0 => data at +0x005 valid */
	u8 padding[3];			/* +0x72d, to sizeof == 0x730 */
#else
	u64 swap_data;
#if DCP_FW_VER < DCP_FW_VERSION(13, 2, 0)
	u8 swap_info[0x6c4];
#else
	u8 swap_info[0x6c5];
#endif
	u32 unkint;
	u8 swap_info_null;
#endif
} __packed;

struct DCP_FW_NAME(dcp_map_reg_req) {
	char obj[4];
	u32 index;
	u32 flags;
#if DCP_FW_VER >= DCP_FW_VERSION(13, 2, 0)
	u8 unk_u64_null;
#endif
	u8 addr_null;
	u8 length_null;
#if DCP_FW_VER >= DCP_FW_VERSION(13, 2, 0)
	u8 padding[1];
#else
	u8 padding[2];
#endif
} __packed;

struct DCP_FW_NAME(dcp_map_reg_resp) {
#if DCP_FW_VER >= DCP_FW_VERSION(13, 2, 0)
	u64 dva;
#endif
	u64 addr;
	u64 length;
	u32 ret;
} __packed;


struct apple_dcp;

int DCP_FW_NAME(iomfb_modeset)(struct apple_dcp *dcp,
			       struct drm_crtc_state *crtc_state);
void DCP_FW_NAME(iomfb_flush)(struct apple_dcp *dcp, struct drm_crtc *crtc, struct drm_atomic_state *state);
void DCP_FW_NAME(iomfb_poweron)(struct apple_dcp *dcp);
void DCP_FW_NAME(iomfb_poweroff)(struct apple_dcp *dcp);
void DCP_FW_NAME(iomfb_sleep)(struct apple_dcp *dcp);
void DCP_FW_NAME(iomfb_start)(struct apple_dcp *dcp);
void DCP_FW_NAME(iomfb_shutdown)(struct apple_dcp *dcp);
#if DCP_FW_VER >= DCP_FW_VERSION(26, 0, 0)
int DCP_FW_NAME(iomfb_present_brightness)(struct apple_dcp *dcp);
#endif

#endif
