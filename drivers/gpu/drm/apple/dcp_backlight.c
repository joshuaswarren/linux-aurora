// SPDX-License-Identifier: GPL-2.0-only OR MIT
/* Copyright (C) The Asahi Linux Contributors */

#include <drm/drm_atomic.h>
#include <drm/drm_crtc.h>
#include <drm/drm_drv.h>
#include <drm/drm_modeset_lock.h>

#include <linux/backlight.h>
#include <linux/bitops.h>
#include <linux/completion.h>
#include <linux/delay.h>
#include <linux/ktime.h>
#include <linux/minmax.h>
#include <linux/module.h>
#include <linux/workqueue.h>
#include "linux/jiffies.h"

#include "dcp.h"
#include "dcp-internal.h"

/*
 * H17P (J700) does not tell us the level iBoot left the panel at (D006
 * reports 0 nits until the AP sets one), so the first swap after power-on
 * applies this.  40 % of the panel's 525 nits.
 */
static unsigned int initial_brightness = 210;
module_param(initial_brightness, uint, 0644);
MODULE_PARM_DESC(initial_brightness,
		 "Brightness in nits to apply at power-on when the firmware cannot report the current one (H17P)");

/*
 * H17P: a brightness change is ramped, one swap per frame, over this many
 * milliseconds rather than applied in a single step.  macOS fades the panel
 * the same way; the firmware itself does not (the swap carries the target
 * nits and the DAC follows it immediately).  0 disables the ramp.
 */
static unsigned int ramp_ms = 250;
module_param(ramp_ms, uint, 0644);
MODULE_PARM_DESC(ramp_ms,
		 "Milliseconds to fade the panel between brightness levels on H17P (0 = step)");

#define RAMP_STEP_MS	16

#define MIN_BRIGHTNESS_PART1	2U
#define MAX_BRIGHTNESS_PART1	99U
#define MIN_BRIGHTNESS_PART2	103U
#define MAX_BRIGHTNESS_PART2	510U

/*
 * lookup for display brightness 2 to 99 nits
 * */
static u32 brightness_part1[] = {
	0x0000000, 0x0810038, 0x0f000bd, 0x143011c,
	0x1850165, 0x1bc01a1, 0x1eb01d4, 0x2140200,
	0x2380227, 0x2590249, 0x2770269, 0x2930285,
	0x2ac02a0, 0x2c402b8, 0x2d902cf, 0x2ee02e4,
	0x30102f8, 0x314030b, 0x325031c, 0x335032d,
	0x345033d, 0x354034d, 0x362035b, 0x3700369,
	0x37d0377, 0x38a0384, 0x3960390, 0x3a2039c,
	0x3ad03a7, 0x3b803b3, 0x3c303bd, 0x3cd03c8,
	0x3d703d2, 0x3e103dc, 0x3ea03e5, 0x3f303ef,
	0x3fc03f8, 0x4050400, 0x40d0409, 0x4150411,
	0x41d0419, 0x4250421, 0x42d0429, 0x4340431,
	0x43c0438, 0x443043f, 0x44a0446, 0x451044d,
	0x4570454, 0x45e045b, 0x4640461, 0x46b0468,
	0x471046e, 0x4770474, 0x47d047a, 0x4830480,
	0x4890486, 0x48e048b, 0x4940491, 0x4990497,
	0x49f049c, 0x4a404a1, 0x4a904a7, 0x4ae04ac,
	0x4b304b1, 0x4b804b6, 0x4bd04bb, 0x4c204c0,
	0x4c704c5, 0x4cc04c9, 0x4d004ce, 0x4d504d3,
	0x4d904d7, 0x4de04dc, 0x4e204e0, 0x4e704e4,
	0x4eb04e9, 0x4ef04ed, 0x4f304f1, 0x4f704f5,
	0x4fb04f9, 0x4ff04fd, 0x5030501, 0x5070505,
	0x50b0509, 0x50f050d, 0x5130511, 0x5160515,
	0x51a0518, 0x51e051c, 0x5210520, 0x5250523,
	0x5290527, 0x52c052a, 0x52f052e, 0x5330531,
	0x5360535, 0x53a0538, 0x53d053b, 0x540053f,
	0x5440542, 0x5470545, 0x54a0548, 0x54d054c,
	0x550054f, 0x5530552, 0x5560555, 0x5590558,
	0x55c055b, 0x55f055e, 0x5620561, 0x5650564,
	0x5680567, 0x56b056a, 0x56e056d, 0x571056f,
	0x5740572, 0x5760575, 0x5790578, 0x57c057b,
	0x57f057d, 0x5810580, 0x5840583, 0x5870585,
	0x5890588, 0x58c058b, 0x58f058d
};

static u32 brightness_part12[] = { 0x58f058d, 0x59d058f };

/*
 * lookup table for display brightness 103.3 to 510 nits
 * */
static u32 brightness_part2[] = {
	0x59d058f, 0x5b805ab, 0x5d105c5, 0x5e805dd,
	0x5fe05f3, 0x6120608, 0x625061c, 0x637062e,
	0x6480640, 0x6580650, 0x6680660, 0x677066f,
	0x685067e, 0x693068c, 0x6a00699, 0x6ac06a6,
	0x6b806b2, 0x6c406be, 0x6cf06ca, 0x6da06d5,
	0x6e506df, 0x6ef06ea, 0x6f906f4, 0x70206fe,
	0x70c0707, 0x7150710, 0x71e0719, 0x7260722,
	0x72f072a, 0x7370733, 0x73f073b, 0x7470743,
	0x74e074a, 0x7560752, 0x75d0759, 0x7640760,
	0x76b0768, 0x772076e, 0x7780775, 0x77f077c,
	0x7850782, 0x78c0789, 0x792078f, 0x7980795,
	0x79e079b, 0x7a407a1, 0x7aa07a7, 0x7af07ac,
	0x7b507b2, 0x7ba07b8, 0x7c007bd, 0x7c507c2,
	0x7ca07c8, 0x7cf07cd, 0x7d407d2, 0x7d907d7,
	0x7de07dc, 0x7e307e1, 0x7e807e5, 0x7ec07ea,
	0x7f107ef, 0x7f607f3, 0x7fa07f8, 0x7fe07fc
};


static int dcp_get_brightness(struct backlight_device *bd)
{
	struct apple_dcp *dcp = bl_get_data(bd);

	return dcp->brightness.nits;
}

/*
 * IEEE-754 binary64 bit pattern of a small non-negative integer, built
 * without touching the FPU: the H17P swap record carries the brightness as
 * a double in nits.
 */
u64 dcp_nits_to_double_bits(u32 nits)
{
	unsigned int msb;
	u64 mant;

	if (!nits)
		return 0;

	msb = fls(nits) - 1;
	mant = ((u64)nits << (52 - msb)) & ((1ULL << 52) - 1);

	return ((u64)(1023 + msb) << 52) | mant;
}

u32 dcp_h17p_initial_nits(struct apple_dcp *dcp)
{
	u32 max = min(dcp->brightness.maximum, MAX_BRIGHTNESS_PART2 - 1);

	return clamp_t(u32, initial_brightness, 1, max);
}

#define SCALE_FACTOR (1 << 10)

static u32 interpolate(int val, int min, int max, u32 *tbl, size_t tbl_size)
{
	u32 frac;
	u64 low, high;
	u32 interpolated = (tbl_size - 1) * ((val - min) * SCALE_FACTOR) / (max - min);

	size_t index = interpolated / SCALE_FACTOR;

	if (WARN(index + 1 >= tbl_size, "invalid index %zu for brightness %u\n", index, val))
		return tbl[tbl_size / 2];

	frac = interpolated & (SCALE_FACTOR - 1);
	low = tbl[index];
	high = tbl[index + 1];

	return ((frac * high) + ((SCALE_FACTOR - frac) * low)) / SCALE_FACTOR;
}

static u32 calculate_dac(struct apple_dcp *dcp, int val)
{
	u32 dac;

	if (val <= MIN_BRIGHTNESS_PART1)
		return 16 * brightness_part1[0];
	else if (val == MAX_BRIGHTNESS_PART1)
		return 16 * brightness_part1[ARRAY_SIZE(brightness_part1) - 1];
	else if (val == MIN_BRIGHTNESS_PART2)
		return 16 * brightness_part2[0];
	else if (val >= MAX_BRIGHTNESS_PART2)
		return brightness_part2[ARRAY_SIZE(brightness_part2) - 1];

	if (val < MAX_BRIGHTNESS_PART1) {
		dac = interpolate(val, MIN_BRIGHTNESS_PART1, MAX_BRIGHTNESS_PART1,
				  brightness_part1, ARRAY_SIZE(brightness_part1));
	} else if (val > MIN_BRIGHTNESS_PART2) {
		dac = interpolate(val, MIN_BRIGHTNESS_PART2, MAX_BRIGHTNESS_PART2,
				  brightness_part2, ARRAY_SIZE(brightness_part2));
	} else {
		dac = interpolate(val, MAX_BRIGHTNESS_PART1, MIN_BRIGHTNESS_PART2,
				  brightness_part12, ARRAY_SIZE(brightness_part12));
	}

	return 16 * dac;
}

static int drm_crtc_set_brightness(struct apple_dcp *dcp)
{
	struct drm_atomic_state *state;
	struct drm_crtc_state *crtc_state;
	struct drm_modeset_acquire_ctx ctx;
	struct drm_crtc *crtc = &dcp->crtc->base;
	int ret = 0;

	drm_modeset_acquire_init(&ctx, DRM_MODESET_ACQUIRE_INTERRUPTIBLE);
	ret = drm_modeset_lock(&crtc->mutex, &ctx);
	if (ret == -EDEADLK) {
		drm_modeset_backoff(&ctx);
		return -EDEADLK;
	} else if (ret == -ERESTARTSYS) {
		return -ERESTARTSYS;
	}

	if (!dcp->brightness.update)
		goto done;

	state = drm_atomic_state_alloc(crtc->dev);
	if (!state) {
		ret = -ENOMEM;
		goto done;
	}

	state->acquire_ctx = &ctx;
retry:
	crtc_state = drm_atomic_get_crtc_state(state, crtc);
	if (IS_ERR(crtc_state)) {
		ret = PTR_ERR(crtc_state);
		goto fail;
	}

	crtc_state->color_mgmt_changed |= true;

	/*
	 * The brightness travels inside a swap, and on H17P a swap without a
	 * surface is refused (iomfb_flush keeps the last surface on the panel
	 * rather than risk the firmware's null-fetch NMI).  A commit that only
	 * touches the CRTC therefore never reaches the firmware unless the
	 * compositor happens to draw a frame -- which it does not under
	 * swaylock: swayidle's dim to 0 went out on the lock screen's last
	 * frame and the restore on the next touch sat in sysfs forever while
	 * the panel stayed dark (hb118, 2026-09-19 18:58).  Carry the
	 * primary plane's current state, so the flush re-submits the surface
	 * that is already on the panel together with the new level.
	 */
	if (crtc->primary) {
		struct drm_plane_state *plane_state =
			drm_atomic_get_plane_state(state, crtc->primary);

		if (IS_ERR(plane_state)) {
			ret = PTR_ERR(plane_state);
			goto fail;
		}
	}

	ret = drm_atomic_commit(state);
	if (ret == -EDEADLK) {
		drm_atomic_state_clear(state);
		drm_modeset_backoff(&ctx);
		goto retry;
	}

fail:
	if (ret == -EDEADLK) {
		drm_atomic_state_clear(state);
		drm_modeset_backoff(&ctx);
		goto retry;
	}
	drm_atomic_state_put(state);
done:
	drm_modeset_drop_locks(&ctx);

	return ret;
}

int dcp_backlight_update(struct apple_dcp *dcp)
{
	/*
	 * Do not actively try to change brightness if no mode is set.
	 * TODO: should this be reflected the in backlight's power property?
	 *       defer this hopefully until it becomes irrelevant due to proper
	 *       drm integrated backlight handling
	 */
	if (!dcp->valid_mode)
		return 0;

	/* Wait 1 vblank cycle in the hope an atomic swap has already updated
	 * the brightness */
	msleep((1001 + 23) / 24); // 42ms for 23.976 fps

	return drm_crtc_set_brightness(dcp);
}

/*
 * One step of the H17P fade.  The level in flight is interpolated on the
 * clock between the anchor and the target so a slow swap (or a compositor
 * that is already flipping at full rate) stretches nothing: the fade ends
 * ramp_ms after it started, give or take one frame.  Each step goes out
 * as its own swap carrying the current surface (see
 * drm_crtc_set_brightness), exactly as a single-step change did.
 */
static void dcp_backlight_ramp_work(struct work_struct *work)
{
	struct apple_dcp *dcp = container_of(to_delayed_work(work),
					     struct apple_dcp,
					     brightness.ramp_work);
	struct drm_modeset_acquire_ctx ctx;
	int from, target, level;
	s64 elapsed;
	int ret;

	drm_modeset_acquire_init(&ctx, 0);
	ret = drm_modeset_lock(&dcp->crtc->base.mutex, &ctx);
	if (ret) {
		drm_modeset_acquire_fini(&ctx);
		schedule_delayed_work(&dcp->brightness.ramp_work,
				      msecs_to_jiffies(RAMP_STEP_MS));
		return;
	}

	from = dcp->brightness.ramp_from;
	target = dcp->brightness.target_nits;
	elapsed = ktime_ms_delta(ktime_get(), dcp->brightness.ramp_start);

	if (!ramp_ms || !dcp->valid_mode || elapsed >= ramp_ms)
		level = target;
	else
		level = from + (int)(((s64)(target - from) * elapsed) / ramp_ms);

	/* never stall on rounding: always move at least one nit per step */
	if (level == dcp->brightness.nits && level != target)
		level += target > level ? 1 : -1;

	dcp->brightness.nits = level;
	dcp->brightness.dac = calculate_dac(dcp, level);
	dcp->brightness.update = true;

	drm_modeset_drop_locks(&ctx);
	drm_modeset_acquire_fini(&ctx);

	if (dcp->valid_mode)
		drm_crtc_set_brightness(dcp);

	if (level != target)
		schedule_delayed_work(&dcp->brightness.ramp_work,
				      msecs_to_jiffies(RAMP_STEP_MS));
}

static void dcp_backlight_ramp_cancel(void *data)
{
	struct apple_dcp *dcp = data;

	cancel_delayed_work_sync(&dcp->brightness.ramp_work);
}

static int dcp_set_brightness(struct backlight_device *bd)
{
	int ret = 0;
	struct apple_dcp *dcp = bl_get_data(bd);
	struct drm_modeset_acquire_ctx ctx;
	int brightness = backlight_get_brightness(bd);

	drm_modeset_acquire_init(&ctx, DRM_MODESET_ACQUIRE_INTERRUPTIBLE);
	ret = drm_modeset_lock(&dcp->crtc->base.mutex, &ctx);
	if (ret == -EDEADLK) {
		drm_modeset_backoff(&ctx);
		return -EDEADLK;
	} else if (ret == -ERESTARTSYS) {
		return -ERESTARTSYS;
	}

	/*
	 * H17P takes the level in nits and does the DAC mapping itself; on the
	 * older firmware nits is only ever what the DCP reported back.
	 *
	 * On H17P the new level is not applied here: the ramp worker walks
	 * brightness.nits from wherever it is now to the target, and the
	 * backlight core keeps reporting the target in `brightness` while
	 * `actual_brightness` (dcp_get_brightness) follows the fade.  A
	 * request arriving mid-fade simply re-anchors the ramp at the
	 * current level, so a burst of sysfs writes (a slider drag, gsd's
	 * ambient updates) never snaps or overshoots.
	 */
	if (dcp->fw_compat == DCP_FIRMWARE_H17P) {
		dcp->brightness.target_nits = brightness;
		dcp->brightness.ramp_from = dcp->brightness.nits;
		dcp->brightness.ramp_start = ktime_get();
		drm_modeset_drop_locks(&ctx);

		mod_delayed_work(system_wq, &dcp->brightness.ramp_work, 0);
		return 0;
	}

	dcp->brightness.dac = calculate_dac(dcp, brightness);
	dcp->brightness.update = true;

	drm_modeset_drop_locks(&ctx);

	return dcp_backlight_update(dcp);
}

static const struct backlight_ops dcp_backlight_ops = {
	.options = BL_CORE_SUSPENDRESUME,
	.get_brightness = dcp_get_brightness,
	.update_status = dcp_set_brightness,
};

int dcp_backlight_register(struct apple_dcp *dcp)
{
	struct device *dev = dcp->dev;
	struct backlight_device *bl_dev;
	int ret;
	struct backlight_properties props = {
		.type = BACKLIGHT_PLATFORM,
		.brightness = dcp->brightness.nits,
		.scale = BACKLIGHT_SCALE_LINEAR,
	};
	props.max_brightness = min(dcp->brightness.maximum, MAX_BRIGHTNESS_PART2 - 1);

	INIT_DELAYED_WORK(&dcp->brightness.ramp_work, dcp_backlight_ramp_work);
	dcp->brightness.target_nits = dcp->brightness.nits;
	ret = devm_add_action_or_reset(dev, dcp_backlight_ramp_cancel, dcp);
	if (ret)
		return ret;

	bl_dev = devm_backlight_device_register(dev, "apple-panel-bl", dev, dcp,
						&dcp_backlight_ops, &props);
	if (IS_ERR(bl_dev))
		return PTR_ERR(bl_dev);

	dcp->brightness.bl_dev = bl_dev;
	dcp->brightness.dac = calculate_dac(dcp, dcp->brightness.nits);

	return 0;
}
