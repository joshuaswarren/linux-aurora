/* SPDX-License-Identifier: GPL-2.0-only OR MIT */
/*
 * Apple RTKit IPC library
 * Copyright (C) The Asahi Linux Contributors
 */

#ifndef _APPLE_RTKIT_INTERAL_H
#define _APPLE_RTKIT_INTERAL_H

#include <linux/bitfield.h>
#include <linux/bitmap.h>
#include <linux/completion.h>
#include <linux/dma-mapping.h>
#include <linux/io.h>
#include <linux/kernel.h>
#include <linux/module.h>
#include <linux/slab.h>
#include <linux/soc/apple/mailbox.h>
#include <linux/soc/apple/rtkit.h>
#include <linux/workqueue.h>

#define APPLE_RTKIT_APP_ENDPOINT_START 0x20
#define APPLE_RTKIT_MAX_ENDPOINTS 0x100

#include <linux/debugfs.h>
#include <linux/seq_file.h>

struct apple_rtkit {
	void *cookie;
	const struct apple_rtkit_ops *ops;
	struct device *dev;

	struct apple_mbox *mbox;

	struct completion epmap_completion;
	struct completion iop_pwr_ack_completion;
	struct completion ap_pwr_ack_completion;
	struct completion iop_idle_completion;

	int boot_result;
	int version;
	u64 raw_hello;
	u16 hello_min_version;
	u16 hello_max_version;
	u8 epmap_bases_seen;

	unsigned int iop_power_state;
	unsigned int ap_power_state;
	bool crashed;
	bool shutting_down;

	DECLARE_BITMAP(endpoints, APPLE_RTKIT_MAX_ENDPOINTS);
	DECLARE_BITMAP(discovery_endpoints, APPLE_RTKIT_MAX_ENDPOINTS);

	struct apple_rtkit_shmem ioreport_buffer;
	struct apple_rtkit_shmem crashlog_buffer;
	/* Re-reads a firmware-provided crash buffer a while after the request. */
	struct delayed_work crashlog_probe;
	unsigned int crashlog_probe_attempt;
	unsigned int crashlog_probe_elapsed_ms;
	struct apple_rtkit_shmem oslog_buffer;
	/* The AOP logs through oslog, not syslog: on a J700 the MTP emits
	 * decoded "RTKit: syslog message: ..." lines while the AOP emits
	 * only "Unknown oslog message", so every [AOP][ALS]/CT817Hal
	 * diagnostic lands in this buffer and is never read.  Expose it
	 * read-only through debugfs so it can be inspected. */
	struct debugfs_blob_wrapper oslog_blob;
	struct dentry *oslog_dentry;
	struct dentry *endpoints_dentry;

	struct apple_rtkit_shmem syslog_buffer;
	char *syslog_msg_buffer;
	size_t syslog_n_entries;
	size_t syslog_msg_size;
	/* Session inherited from the bootloader: INIT geometry never observed. */
	bool syslog_inherited;

	struct workqueue_struct *wq;
};

void apple_rtkit_crashlog_dump(struct apple_rtkit *rtk, u8 *bfr, size_t size);
bool apple_rtkit_crashlog_present(const u8 *bfr, size_t size);

#endif
