/* SPDX-License-Identifier: GPL-2.0-only */
#include <linux/linkage.h>
#include <asm/assembler.h>

/* Dedicated MRS sites let the diagnostic recover ONLY its own UNDEFs.
 * x0 is the result. Normal function-entry annotations include kernel BTI.
 */
	.macro neo_read name, reg
SYM_FUNC_START(neo_read_\name)
	.global neo_site_\name
neo_site_\name:
	mrs x0, \reg
	ret
SYM_FUNC_END(neo_read_\name)
	.endm

neo_read cyc, S3_5_C15_C5_0
neo_read acc, S3_5_C15_C4_0
neo_read override, S3_5_C15_C6_0
neo_read debug, MDSCR_EL1
neo_read pmcr0, S3_1_C15_C0_0
neo_read pmcr1, S3_1_C15_C1_0
neo_read dbgpower, DBGPRCR_EL1
neo_read oslock, OSLSR_EL1
neo_read amx, S3_4_C15_C1_4
