/* SPDX-License-Identifier: GPL-2.0-only */
#ifndef __ASM_APPLE_IDLE_H
#define __ASM_APPLE_IDLE_H

#include <asm/cputype.h>

static inline bool apple_m5_needs_wait_save(void)
{
	/* MIDRs measured on the T8142 E and P cores respectively. */
	u32 model = read_cpuid_id() & MIDR_CPU_MODEL_MASK;

	return IS_ENABLED(CONFIG_ARCH_APPLE) &&
	       (model == MIDR_CPU_MODEL(ARM_CPU_IMP_APPLE, 0x062) ||
		model == MIDR_CPU_MODEL(ARM_CPU_IMP_APPLE, 0x063));
}

void apple_m5_cpu_wfi(void);
void apple_m5_cpu_wfit(u64 deadline);

#endif
