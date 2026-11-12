/* SPDX-License-Identifier: GPL-2.0-only OR MIT */
/*
 * Software camera indicator: pure state and policy.
 *
 * The kernel draws a small opaque square on a swap layer that userspace
 * cannot address. This is NOT the secure indicator Apple's firmware draws
 * outside the OS: root who can run its own kernel defeats it. It remains
 * until the display coprocessor's own request exists on Linux.
 */
#ifndef DCP_INDICATOR_H
#define DCP_INDICATOR_H

#include <linux/types.h>
#include <linux/errno.h>
#include <drm/drm_mode.h>

/* The panel level below which the square is treated as not visible. */
#define DCP_IND_MIN_NITS 10
/* Per-channel distance from the ideal ramp that still counts as identity. */
#define DCP_IND_LUT_TOLERANCE 512

enum dcp_ind_state {
	DCP_IND_OFF,
	DCP_IND_PENDING,
	DCP_IND_LIT,
	DCP_IND_FAILED,
};

struct dcp_indicator {
	enum dcp_ind_state state;
};

/* The request for the next stream: only from OFF, one owner at a time. */
static inline int dcp_ind_begin(struct dcp_indicator *ind)
{
	if (ind->state != DCP_IND_OFF)
		return -EBUSY;
	ind->state = DCP_IND_PENDING;
	return 0;
}

/* The first present that carried the square, or its failure. */
static inline void dcp_ind_presented(struct dcp_indicator *ind, bool ok)
{
	if (ind->state != DCP_IND_PENDING)
		return;
	ind->state = ok ? DCP_IND_LIT : DCP_IND_FAILED;
}

/* A later present failed: the panel no longer shows a known frame. */
static inline void dcp_ind_present_failed(struct dcp_indicator *ind)
{
	if (ind->state == DCP_IND_PENDING || ind->state == DCP_IND_LIT)
		ind->state = DCP_IND_FAILED;
}

static inline void dcp_ind_end(struct dcp_indicator *ind)
{
	ind->state = DCP_IND_OFF;
}

/* The kernel owns the layer: an ISP stream is, or is about to be, running. */
static inline bool dcp_ind_active(const struct dcp_indicator *ind)
{
	return ind->state != DCP_IND_OFF;
}

/*
 * True only while the square was presented and the panel can show it: lit and
 * at or above the floor. Turning the pipe off marks the present failed, so
 * the state itself covers a disabled pipe. The camera stream stops the moment
 * this turns false.
 */
static inline bool dcp_ind_visible(const struct dcp_indicator *ind,
				   bool panel_lit, u32 nits)
{
	return ind->state == DCP_IND_LIT && panel_lit && nits >= DCP_IND_MIN_NITS;
}

/*
 * The reserved plane accepts no userspace framebuffer. A disabled plane, or
 * the kernel's own framebuffer, is allowed.
 */
static inline bool dcp_ind_plane_ok(bool reserved_plane, bool fb_is_indicator,
				    bool has_fb)
{
	if (!reserved_plane)
		return true;
	return !has_fb || fb_is_indicator;
}

/* A colour transform could hide the square: none may change while it runs. */
static inline bool dcp_ind_color_change_ok(const struct dcp_indicator *ind,
					   bool color_mgmt_changed)
{
	return !(dcp_ind_active(ind) && color_mgmt_changed);
}

static inline u32 dcp_ind_ramp_distance(u16 got, unsigned int i, unsigned int n)
{
	u32 want = (u32)((u64)i * 0xffff / (n - 1));

	return got > want ? got - want : want - got;
}

/* A missing table is identity; so is a ramp within the tolerance. */
static inline bool dcp_ind_lut_identity(const struct drm_color_lut *lut,
					unsigned int n)
{
	unsigned int i;

	if (!lut)
		return true;
	if (n < 2)
		return false;
	for (i = 0; i < n; i++) {
		if (dcp_ind_ramp_distance(lut[i].red, i, n) > DCP_IND_LUT_TOLERANCE ||
		    dcp_ind_ramp_distance(lut[i].green, i, n) > DCP_IND_LUT_TOLERANCE ||
		    dcp_ind_ramp_distance(lut[i].blue, i, n) > DCP_IND_LUT_TOLERANCE)
			return false;
	}
	return true;
}

/* Entries are sign-magnitude S31.32: 1 << 32 is exactly 1.0. */
static inline bool dcp_ind_ctm_identity(const struct drm_color_ctm *ctm)
{
	unsigned int i;

	if (!ctm)
		return true;
	for (i = 0; i < 9; i++)
		if (ctm->matrix[i] != ((i % 4 == 0) ? 1ULL << 32 : 0))
			return false;
	return true;
}

#endif
