/* SPDX-License-Identifier: GPL-2.0-only OR MIT */
/* Copyright The Asahi Linux Contributors */

#ifndef __APPLE_DCP_LIFECYCLE_H__
#define __APPLE_DCP_LIFECYCLE_H__

#include <linux/bitfield.h>
#include <linux/bits.h>
#include <linux/types.h>

#define APPLE_DCP_COPROC_CPU_CONTROL_RUN	BIT(4)
#define APPLE_DCP_COPROC_CPU_CONTROL_STOP_SECOND_CLEAR	BIT(5)
#define APPLE_DCP_COPROC_CPU_STATUS_RUNNING	BIT(0)
#define APPLE_DCP_COPROC_CPU_STATUS_STOPPED	BIT(1)

#define APPLE_DCP_PMGR_PS_ACTUAL	GENMASK(7, 4)
#define APPLE_DCP_PMGR_PS_TARGET	GENMASK(3, 0)
#define APPLE_DCP_PMGR_PS_AUTO_ENABLE	BIT(28)
#define APPLE_DCP_PMGR_PS_RESET		BIT(12)
#define APPLE_DCP_PMGR_PS_DEV_DISABLE	BIT(10)
#define APPLE_DCP_PMGR_PS_FLAGS		GENMASK(9, 8)
#define APPLE_DCP_PMGR_PS_SET_MASK	(APPLE_DCP_PMGR_PS_AUTO_ENABLE | \
					 APPLE_DCP_PMGR_PS_RESET | \
					 APPLE_DCP_PMGR_PS_DEV_DISABLE | \
					 APPLE_DCP_PMGR_PS_FLAGS | \
					 APPLE_DCP_PMGR_PS_TARGET)
#define APPLE_DCP_PMGR_PS_ACTIVE	0xf
#define APPLE_DCP_PMGR_PS_OFF		0x0

enum apple_dcp_coprocessor_start {
	APPLE_DCP_COPROCESSOR_COLD_START,
	APPLE_DCP_COPROCESSOR_ADOPT_AND_RESTART,
	APPLE_DCP_COPROCESSOR_QUIESCED_RESTART,
	APPLE_DCP_COPROCESSOR_IDLE_RESTART,
	APPLE_DCP_COPROCESSOR_ASC_STOPPED_RESTART,
};

enum apple_dcp_loader_handoff {
	APPLE_DCP_LOADER_HANDOFF_NONE,
	APPLE_DCP_LOADER_HANDOFF_RTKIT_QUIESCED,
	APPLE_DCP_LOADER_HANDOFF_RTKIT_IDLE,
	APPLE_DCP_LOADER_HANDOFF_ASC_STOPPED,
};

static inline bool apple_dcp_coprocessor_is_active(u32 cpu_control, u32 cpu_status)
{
	if (cpu_control & APPLE_DCP_COPROC_CPU_CONTROL_RUN ||
	    cpu_status & APPLE_DCP_COPROC_CPU_STATUS_RUNNING)
		return true;

	/* A zero status is not evidence that the coprocessor stopped. */
	return !(cpu_status & APPLE_DCP_COPROC_CPU_STATUS_STOPPED);
}

static inline enum apple_dcp_coprocessor_start
apple_dcp_coprocessor_start_mode(enum apple_dcp_loader_handoff handoff,
				 bool can_adopt,
				 u32 cpu_control, u32 cpu_status)
{
	if (handoff == APPLE_DCP_LOADER_HANDOFF_ASC_STOPPED)
		return APPLE_DCP_COPROCESSOR_ASC_STOPPED_RESTART;
	if (handoff == APPLE_DCP_LOADER_HANDOFF_RTKIT_IDLE)
		return APPLE_DCP_COPROCESSOR_IDLE_RESTART;
	if (handoff == APPLE_DCP_LOADER_HANDOFF_RTKIT_QUIESCED)
		return APPLE_DCP_COPROCESSOR_QUIESCED_RESTART;

	if (can_adopt &&
	    apple_dcp_coprocessor_is_active(cpu_control, cpu_status))
		return APPLE_DCP_COPROCESSOR_ADOPT_AND_RESTART;

	return APPLE_DCP_COPROCESSOR_COLD_START;
}

static inline bool
apple_dcp_needs_adoption(enum apple_dcp_coprocessor_start start_mode)
{
	return start_mode == APPLE_DCP_COPROCESSOR_ADOPT_AND_RESTART;
}

static inline bool
apple_dcp_needs_fresh_boot(enum apple_dcp_coprocessor_start start_mode)
{
	return start_mode == APPLE_DCP_COPROCESSOR_ASC_STOPPED_RESTART;
}

static inline u32 apple_dcp_pmgr_with_target(u32 reg, u32 target)
{
	return (reg & ~APPLE_DCP_PMGR_PS_SET_MASK) |
		FIELD_PREP(APPLE_DCP_PMGR_PS_TARGET, target);
}

static inline u32 apple_dcp_pmgr_with_auto_enable(u32 reg)
{
	return reg | APPLE_DCP_PMGR_PS_AUTO_ENABLE;
}

static inline bool apple_dcp_pmgr_reached_target(u32 reg, u32 target)
{
	return FIELD_GET(APPLE_DCP_PMGR_PS_ACTUAL, reg) == target;
}

static inline u32 apple_dcp_coprocessor_stop_first(u32 control)
{
	return control & ~APPLE_DCP_COPROC_CPU_CONTROL_RUN;
}

static inline u32 apple_dcp_coprocessor_stop_second(u32 control)
{
	return control & ~APPLE_DCP_COPROC_CPU_CONTROL_STOP_SECOND_CLEAR;
}

#endif /* __APPLE_DCP_LIFECYCLE_H__ */
