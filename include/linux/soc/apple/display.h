/* SPDX-License-Identifier: GPL-2.0-only OR MIT */
#ifndef _LINUX_SOC_APPLE_DISPLAY_H_
#define _LINUX_SOC_APPLE_DISPLAY_H_

struct device;

/* Initial native display admission, before starting retained GPU firmware. */
int apple_drm_t6030_link_device(struct device *consumer);
int apple_drm_t6031_link_device(struct device *consumer);
int apple_drm_t8132_link_device(struct device *consumer);

/*
 * Software camera indicator (interim, not the secure indicator): a
 * kernel-owned overlay on the internal panel. set(true) returns 0 only once
 * a present carried it to the panel. lit() is true only while it is up and
 * the panel is on above the backlight floor.
 */
int apple_dcp_camera_indicator_set(struct device *dcp, bool on);
bool apple_dcp_camera_indicator_lit(struct device *dcp);

#endif
