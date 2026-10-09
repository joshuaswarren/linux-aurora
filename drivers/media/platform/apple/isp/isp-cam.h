// SPDX-License-Identifier: GPL-2.0-only
/* Copyright 2023 Eileen Yoon <eyn@gmx.com> */

#ifndef __ISP_CAM_H__
#define __ISP_CAM_H__

#include "isp-drv.h"

#define ISP_FRAME_RATE_SCALE	256U
#define ISP_FRAME_RATE_MIN	(15U * ISP_FRAME_RATE_SCALE)
#define ISP_FRAME_RATE_MAX	(30U * ISP_FRAME_RATE_SCALE)

int apple_isp_detect_camera(struct apple_isp *isp);

int apple_isp_start_camera(struct apple_isp *isp);
void apple_isp_stop_camera(struct apple_isp *isp);

int apple_isp_start_capture(struct apple_isp *isp);
void apple_isp_stop_capture(struct apple_isp *isp);

void apple_isp_wdt_init(struct apple_isp *isp);
void apple_isp_wdt_stop(struct apple_isp *isp);

#endif /* __ISP_CAM_H__ */
