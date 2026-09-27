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
	bool adopted;
	bool awaiting_iop_idle;
	bool epmap_only;
	bool firmware_owned_crashlog;
	bool t8152_pmp;
	bool crashlog_endpoint_started;
	bool hello_seen;
	bool epmap_finished;
	bool shutting_down;

	DECLARE_BITMAP(endpoints, APPLE_RTKIT_MAX_ENDPOINTS);
	DECLARE_BITMAP(discovery_endpoints, APPLE_RTKIT_MAX_ENDPOINTS);

	struct apple_rtkit_shmem ioreport_buffer;
	struct apple_rtkit_shmem crashlog_buffer;
	struct apple_rtkit_shmem oslog_buffer;
	struct apple_rtkit_shmem pmp_aux_buffer;

	struct apple_rtkit_shmem syslog_buffer;
	char *syslog_msg_buffer;
	size_t syslog_n_entries;
	size_t syslog_msg_size;

	struct workqueue_struct *wq;
};

void apple_rtkit_crashlog_dump(struct apple_rtkit *rtk, u8 *bfr, size_t size);

#endif
