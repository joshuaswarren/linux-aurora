/* SPDX-License-Identifier: GPL-2.0-only OR MIT */
/*
 * Apple mailbox message format
 *
 * Copyright The Asahi Linux Contributors
 */

#ifndef _APPLE_MAILBOX_H_
#define _APPLE_MAILBOX_H_

#include <linux/bits.h>
#include <linux/device.h>
#include <linux/types.h>

/* encodes a single 96bit message sent over the single channel */
struct apple_mbox_msg {
	u64 msg0;
	u32 msg1;
};

/**
 * enum apple_mbox_ascwrap_v6_role - firmware role owned by an ASCWrap v6
 * mailbox provider
 * @APPLE_MBOX_ASCWRAP_V6_ROLE_NONE: not an ASCWrap v6 provider
 * @APPLE_MBOX_ASCWRAP_V6_ROLE_GFX: primary GPU firmware role
 * @APPLE_MBOX_ASCWRAP_V6_ROLE_GFX1: secondary GPU firmware role
 */
enum apple_mbox_ascwrap_v6_role {
	APPLE_MBOX_ASCWRAP_V6_ROLE_NONE,
	APPLE_MBOX_ASCWRAP_V6_ROLE_GFX,
	APPLE_MBOX_ASCWRAP_V6_ROLE_GFX1,
};

/*
 * Missing pieces of the provider-owned ASCWrap v6 CPU lifecycle.  Consumers
 * must treat any non-zero mask as a hard pre-CPU-start gate.
 */
#define APPLE_MBOX_ASCWRAP_V6_MISSING_SAFE_STOP	BIT(0)
#define APPLE_MBOX_ASCWRAP_V6_MISSING_SAFE_RESET	BIT(1)

/**
 * struct apple_mbox_ascwrap_v6_lifecycle - provider-owned lifecycle facts
 * @role: firmware role owned by this provider
 * @wrapper_start: CPU physical base of the provider's ASCWrap window
 * @wrapper_size: size of the provider's ASCWrap window
 * @iop_vbar_start: CPU physical base of the provider's IOP VBAR window
 * @iop_vbar_size: size of the provider's IOP VBAR window
 * @missing: APPLE_MBOX_ASCWRAP_V6_MISSING_* execution gates
 *
 * This is deliberately a capability/query surface, not an invitation for a
 * mailbox consumer to map either resource.  The apple-mailbox provider owns
 * both mappings.  CPU start remains forbidden until the provider can also
 * perform the complete matching stop/reset sequence.
 */
struct apple_mbox_ascwrap_v6_lifecycle {
	enum apple_mbox_ascwrap_v6_role role;
	u64 wrapper_start;
	u64 wrapper_size;
	u64 iop_vbar_start;
	u64 iop_vbar_size;
	u32 missing;
};

struct apple_mbox {
	struct device *dev;
	void __iomem *regs;
	void __iomem *wrapper_regs;
	void __iomem *iop_vbar_regs;
	const struct apple_mbox_hw *hw;
	struct apple_mbox_ascwrap_v6_lifecycle ascwrap_v6;
	bool active;

	int irq_recv_not_empty;
	int irq_send_empty;

	spinlock_t rx_lock;
	spinlock_t tx_lock;

	struct completion tx_empty;

	/** Receive callback for incoming messages */
	void (*rx)(struct apple_mbox *mbox, struct apple_mbox_msg msg, void *cookie);
	void *cookie;
};

struct apple_mbox *apple_mbox_get(struct device *dev, int index);
struct apple_mbox *apple_mbox_get_byname(struct device *dev, const char *name);

int apple_mbox_start(struct apple_mbox *mbox);
void apple_mbox_stop(struct apple_mbox *mbox);
int apple_mbox_poll(struct apple_mbox *mbox);
int apple_mbox_send(struct apple_mbox *mbox, struct apple_mbox_msg msg,
		    bool atomic);

int apple_mbox_ascwrap_v6_get_lifecycle(
	struct apple_mbox *mbox,
	struct apple_mbox_ascwrap_v6_lifecycle *lifecycle);
int apple_mbox_ascwrap_v6_require_safe_cpu_lifecycle(struct apple_mbox *mbox);

#endif
