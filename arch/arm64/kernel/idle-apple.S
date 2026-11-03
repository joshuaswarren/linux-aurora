/* SPDX-License-Identifier: GPL-2.0-only */
#include <linux/linkage.h>
#include <asm/assembler.h>

	.text
/*
 * T8140/T8142/T8152 secondaries can resume from WFI with cleared general registers.
 * SP and stack memory survive. Preserve the AAPCS64 callee-saved registers
 * and x18 (which may hold the shadow call stack), including the return PC.
 * This follows the stateless-wait approach used during m1n1 M5 Windows
 * bring-up. It does not configure an Apple deep-sleep state or DVFS policy.
 *
 * IRQ masking/context management remains with cpu_do_idle(). The direct
 * function call tells its compiler that x0-x17 and flags are clobbered.
 */
	.macro apple_m5_wait_save
	stp	x29, x30, [sp, #-112]!
	mov	x29, sp
	stp	x19, x20, [sp, #16]
	stp	x21, x22, [sp, #32]
	stp	x23, x24, [sp, #48]
	stp	x25, x26, [sp, #64]
	stp	x27, x28, [sp, #80]
	str	x18, [sp, #96]
	dsb	sy
	.endm

	.macro apple_m5_wait_restore
	isb
	ldr	x18, [sp, #96]
	ldp	x19, x20, [sp, #16]
	ldp	x21, x22, [sp, #32]
	ldp	x23, x24, [sp, #48]
	ldp	x25, x26, [sp, #64]
	ldp	x27, x28, [sp, #80]
	ldp	x29, x30, [sp], #112
	ret
	.endm

SYM_FUNC_START(apple_m5_cpu_wfi)
	apple_m5_wait_save
	wfi
	apple_m5_wait_restore
SYM_FUNC_END(apple_m5_cpu_wfi)

/* __delay() also uses WFIT, which loses the same state on M5 secondaries. */
SYM_FUNC_START(apple_m5_cpu_wfit)
	apple_m5_wait_save
	.inst	0xd5031020	/* wfit x0 */
	apple_m5_wait_restore
SYM_FUNC_END(apple_m5_cpu_wfit)
	.pushsection .cpuidle.text, "ax"
SYM_FUNC_START(apple_cpu_wfi)
	apple_m5_wait_save
	wfi
	apple_m5_wait_restore
SYM_FUNC_END(apple_cpu_wfi)
	.popsection
