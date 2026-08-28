/* SPDX-License-Identifier: GPL-2.0-only OR MIT */
/* Copyright The Asahi Linux Contributors */

#ifndef __APPLE_IOMFB_SURFACE_H__
#define __APPLE_IOMFB_SURFACE_H__

#include <linux/errno.h>
#include <linux/kernel.h>
#include <linux/types.h>

#define APPLE_DCP_MAX_SURFACES 4
#define APPLE_DCP_SCANOUT_PITCH_ALIGN 64

struct apple_dcp_surface_order {
	u32 count;
	u32 id[APPLE_DCP_MAX_SURFACES];
};

static inline u32 apple_dcp_linear_pitch(u32 width, u32 bpp)
{
	return ALIGN(DIV_ROUND_UP(width * bpp, 8),
		     APPLE_DCP_SCANOUT_PITCH_ALIGN);
}

static inline int
apple_dcp_surface_order_init(struct apple_dcp_surface_order *order,
			     const u32 *ids, u32 count)
{
	static const u32 legacy_order[] = { 2, 1 };
	u32 seen = 0;
	u32 i;

	if (!order)
		return -EINVAL;

	if (!ids) {
		ids = legacy_order;
		count = ARRAY_SIZE(legacy_order);
	}
	if (!count || count > APPLE_DCP_MAX_SURFACES)
		return -EINVAL;

	for (i = 0; i < count; i++) {
		if (ids[i] >= APPLE_DCP_MAX_SURFACES || (seen & BIT(ids[i])))
			return -EINVAL;
		seen |= BIT(ids[i]);
		order->id[i] = ids[i];
	}
	order->count = count;

	return 0;
}

#endif /* __APPLE_IOMFB_SURFACE_H__ */
