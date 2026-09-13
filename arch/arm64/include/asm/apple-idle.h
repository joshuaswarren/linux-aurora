/* SPDX-License-Identifier: GPL-2.0-only */
#ifndef __ASM_APPLE_IDLE_H
#define __ASM_APPLE_IDLE_H

#include <asm/cputype.h>

static inline bool apple_needs_wait_save(void)
{
	/* T8140 (A18 Pro), E and P core models measured on J700. */
	u32 model = read_cpuid_id() & MIDR_CPU_MODEL_MASK;

	return IS_ENABLED(CONFIG_ARCH_APPLE) &&
	       (model == MIDR_CPU_MODEL(ARM_CPU_IMP_APPLE, 0x060) ||
		model == MIDR_CPU_MODEL(ARM_CPU_IMP_APPLE, 0x061));
}

void apple_cpu_wfi(void);

#endif
