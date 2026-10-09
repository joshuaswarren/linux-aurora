/* SPDX-License-Identifier: GPL-2.0-only */
#ifndef __ASM_APPLE_IDLE_H
#define __ASM_APPLE_IDLE_H

#include <asm/cputype.h>

static inline bool apple_m5_needs_wait_save(void)
{
	/* T8142 and T8152 can clear GPRs across an idle wait. M6 first Linux
	 * boot reached do_idle with x19-x28 zero after WFI; retain the same
	 * stack-based register save on the measured E and P/M CPU models.
	 */
	u32 model = read_cpuid_id() & MIDR_CPU_MODEL_MASK;

	return IS_ENABLED(CONFIG_ARCH_APPLE) &&
	       (model == MIDR_CPU_MODEL(ARM_CPU_IMP_APPLE, 0x062) ||
		model == MIDR_CPU_MODEL(ARM_CPU_IMP_APPLE, 0x063) ||
		model == MIDR_CPU_MODEL(ARM_CPU_IMP_APPLE, 0x072) ||
		model == MIDR_CPU_MODEL(ARM_CPU_IMP_APPLE, 0x073));
}

void apple_m5_cpu_wfi(void);
void apple_m5_cpu_wfit(u64 deadline);
/* WFI that preserves x18-x30 on cores that lose them (Apple T8140). */
void apple_cpu_wfi(void);

#endif
