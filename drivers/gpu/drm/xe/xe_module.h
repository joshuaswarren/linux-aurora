/* SPDX-License-Identifier: MIT */
/*
 * Copyright © 2023 Intel Corporation
 */

#ifndef _XE_MODULE_H_
#define _XE_MODULE_H_

#include <linux/types.h>

struct work_struct;

/* egpu-b60: BO classes that keep VRAM placement under xe.np_keep_vram_mask */
#define XE_NP_KEEP_SLPC		BIT(0)
#define XE_NP_KEEP_PT		BIT(1)
#define XE_NP_KEEP_MIGRATE	BIT(2)
#define XE_NP_KEEP_LRC		BIT(3)
#define XE_NP_KEEP_HWSP		BIT(4)
#define XE_NP_KEEP_GUC		BIT(5)
#define XE_NP_KEEP_OTHER	BIT(6)

/* Module modprobe variables */
struct xe_modparam {
	bool force_execlist;
	bool probe_display;
	bool np_kernel_bo_sysmem;
	bool np_sysmem_wc;
	bool np_user_sysmem_wc;
	bool np_user_no_cpu_vram;
	bool np_hide_visible_vram;
	bool np_keep_vram_ext;
	u32 np_keep_vram_mask;
	int force_vram_bar_size;
	int guc_log_level;
	char *guc_firmware_path;
	char *huc_firmware_path;
	char *gsc_firmware_path;
	char *force_probe;
#ifdef CONFIG_PCI_IOV
	unsigned int max_vfs;
#endif
	unsigned int wedged_mode;
	u32 svm_notifier_size;
};

extern struct xe_modparam xe_modparam;

static inline bool xe_np_keep_vram(u32 bit)
{
	return xe_modparam.np_keep_vram_mask & bit;
}

bool xe_destroy_wq_queue(struct work_struct *work);
void xe_destroy_wq_flush(void);

#endif

