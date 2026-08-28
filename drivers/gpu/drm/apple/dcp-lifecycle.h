/* SPDX-License-Identifier: GPL-2.0-only OR MIT */
/* Copyright The Asahi Linux Contributors */

#ifndef __APPLE_DCP_LIFECYCLE_H__
#define __APPLE_DCP_LIFECYCLE_H__

#include <linux/bits.h>
#include <linux/types.h>

#define APPLE_DCP_COPROC_CPU_CONTROL_RUN	BIT(4)
#define APPLE_DCP_COPROC_CPU_STATUS_RUNNING	BIT(0)
#define APPLE_DCP_COPROC_CPU_STATUS_STOPPED	BIT(1)

static inline bool apple_dcp_coprocessor_is_active(u32 cpu_control, u32 cpu_status)
{
	if (cpu_control & APPLE_DCP_COPROC_CPU_CONTROL_RUN ||
	    cpu_status & APPLE_DCP_COPROC_CPU_STATUS_RUNNING)
		return true;

	/* A zero status is not evidence that the coprocessor stopped. */
	return !(cpu_status & APPLE_DCP_COPROC_CPU_STATUS_STOPPED);
}

#endif /* __APPLE_DCP_LIFECYCLE_H__ */
