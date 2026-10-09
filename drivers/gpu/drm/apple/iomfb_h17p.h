// SPDX-License-Identifier: GPL-2.0-only OR MIT
/* Copyright The Asahi Linux Contributors */

#ifndef __APPLE_IOMFB_H17P_H__
#define __APPLE_IOMFB_H17P_H__

#include "version_utils.h"

#define DCP_FW h17p
#define DCP_FW_VER DCP_FW_VERSION(26, 6, 0)

#include "iomfb_template.h"

static_assert(sizeof(struct dcp_swap_h17p) == 0x588);
/* The backlight block, at the offsets of the macOS 25G83 capture. */
static_assert(offsetof(struct dcp_swap_h17p, bl_h17p_update) == 0x354);
static_assert(offsetof(struct dcp_swap_h17p, bl_h17p_flags) == 0x358);
static_assert(offsetof(struct dcp_swap_h17p, bl_h17p_d) == 0x35e);
static_assert(offsetof(struct dcp_swap_h17p, unk_1) == 0x4a8);
static_assert(sizeof(struct dcp_surface_h17p) == 0x22c);
static_assert(offsetof(struct dcp_swap_submit_req_h17p, surf) == 0x588);
static_assert(offsetof(struct dcp_swap_submit_req_h17p, surf_iova) == 0xe38);
static_assert(sizeof(struct dcp_swap_submit_req_h17p) == 0xe9c);
static_assert(sizeof(struct dcp_swap_submit_resp_h17p) == 0x0c);

#undef DCP_FW_VER
#undef DCP_FW

#endif /* __APPLE_IOMFB_H17P_H__ */
