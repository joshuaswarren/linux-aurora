/* SPDX-License-Identifier: GPL-2.0 */
#ifndef __ASM_IRQ_WORK_H
#define __ASM_IRQ_WORK_H

/* arm64 raises IRQ work with a real IPI -- arch_irq_work_raise() sends
 * IPI_IRQ_WORK in arch/arm64/kernel/smp.c -- so this is true here. The
 * asm-generic fallback returns false, which would silently defer IRQ work
 * to the timer tick instead. */
static inline bool arch_irq_work_has_interrupt(void)
{
	return true;
}

#endif /* __ASM_IRQ_WORK_H */
