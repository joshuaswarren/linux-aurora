// SPDX-License-Identifier: GPL-2.0-only OR MIT
/* Explicit, RAM-boot-only M5 HELLO/EPMAP experiment. No initdata or GPU jobs. */
#include <linux/module.h>
#include <linux/of.h>
#include <linux/of_address.h>
#include <linux/platform_device.h>
#include <linux/slab.h>
#include <linux/soc/apple/mailbox.h>
#include <linux/soc/apple/rtkit.h>

struct m5_probe {
	struct apple_mbox *mbox[2];
	struct apple_rtkit *rtk[2];
	bool started[2];
};

static const struct apple_rtkit_ops m5_rtkit_ops;
static bool full_boot;
module_param(full_boot, bool, 0400);
MODULE_PARM_DESC(full_boot, "Complete RTKit startup on a fresh RAM boot; no initdata");

static int m5_validate_firmware_data(struct device *dev)
{
	static const char * const nodes[] = {
		"/reserved-memory/gfx-data", "/reserved-memory/gfx1-data",
	};
	static const u64 sizes[] = { 0x134000, 0x130000 };
	struct device_node *node;
	struct resource res;
	u64 previous_end = 0;
	int i, ret;

	for (i = 0; i < 2; i++) {
		node = of_find_node_by_path(nodes[i]);
		if (!node)
			return -ENODEV;
		ret = of_address_to_resource(node, 0, &res);
		of_node_put(node);
		if (ret)
			return ret;
		if (res.start < 0x10000000000ULL || res.end >= 0x10400000000ULL ||
		    res.start & 0x3fff || resource_size(&res) != sizes[i] ||
		    (previous_end && res.start <= previous_end))
			return -EINVAL;
		previous_end = res.end;
		dev_info(dev, "M5_RTKIT_PROBE role=%d firmware-data=%pa size=%#llx reserved-crash-buffer=%#llx+0x8000\n",
			 i, &res.start, sizes[i], res.start + 0xa0000);
	}
	return 0;
}

static int m5_probe(struct platform_device *pdev)
{
	static const u64 wrapper[] = { 0x582600000, 0x582e00000 };
	static const u64 vbar[] = { 0x582050000, 0x582850000 };
	static const int start_order[] = { 1, 0 };
	struct device *dev = &pdev->dev;
	struct apple_mbox_ascwrap_v6_lifecycle life;
	struct apple_rtkit_epmap_snapshot snap;
	struct m5_probe *probe;
	int i, ordinal, ret;

	if (!of_machine_is_compatible("apple,j813") ||
	    !of_device_is_compatible(dev->of_node, "apple,agx-t8142"))
		return -ENODEV;
	ret = m5_validate_firmware_data(dev);
	if (ret)
		return dev_err_probe(dev, ret, "M5 firmware DATA reservations changed\n");
	probe = kzalloc(sizeof(*probe), GFP_KERNEL);
	if (!probe)
		return -ENOMEM;
	for (i = 0; i < 2; i++) {
		probe->mbox[i] = apple_mbox_get(dev, i);
		if (IS_ERR(probe->mbox[i])) {
			ret = PTR_ERR(probe->mbox[i]);
			goto before_start;
		}
		ret = apple_mbox_ascwrap_v6_get_lifecycle(probe->mbox[i], &life);
		if (ret)
			goto before_start;
		if (life.role != APPLE_MBOX_ASCWRAP_V6_ROLE_GFX + i ||
		    life.wrapper_start != wrapper[i] || life.wrapper_size != 0x88000 ||
		    life.iop_vbar_start != vbar[i] || life.iop_vbar_size != 8 ||
		    life.missing) {
			ret = -EINVAL;
			goto before_start;
		}
		ret = apple_mbox_ascwrap_v6_require_safe_cpu_lifecycle(probe->mbox[i]);
		if (ret)
			goto before_start;
	}
	for (i = 0; i < 2; i++) {
		probe->rtk[i] = full_boot ?
			apple_rtkit_init_g17p(dev, probe, NULL, i, &m5_rtkit_ops) :
			apple_rtkit_init_epmap_only(dev, probe, NULL, i, &m5_rtkit_ops);
		if (IS_ERR(probe->rtk[i])) {
			ret = PTR_ERR(probe->rtk[i]);
			probe->rtk[i] = NULL;
			goto before_start;
		}
	}
	/* Retain the consumer, callbacks and transports until the RAM test ends
	 * in reboot, including failed startup. No unload can free a live owner. */
	__module_get(THIS_MODULE);
	platform_set_drvdata(pdev, probe);
	dev_info(dev, "M5_RTKIT_PROBE receivers=2 full-boot=%d\n", full_boot);
	/* Native M5 25G76 starts GFX1 before GFX (trace sequences 26, 79). */
	for (ordinal = 0; ordinal < 2; ordinal++) {
		i = start_order[ordinal];
		ret = apple_mbox_ascwrap_v6_start_cpu(probe->mbox[i]);
		if (ret)
			goto stop;
		probe->started[i] = true;
		dev_info(dev, "M5_RTKIT_PROBE role=%d cpu-start-requested\n", i);
		ret = full_boot ? apple_rtkit_boot(probe->rtk[i]) :
			apple_rtkit_discover_epmap(probe->rtk[i]);
		if (ret)
			goto stop;
		if (full_boot) {
			if (!apple_rtkit_is_running(probe->rtk[i]) ||
			    apple_rtkit_is_crashed(probe->rtk[i])) {
				ret = -EIO;
				goto stop;
			}
			dev_info(dev, "M5_RTKIT_BOOT role=%d running=1 crashed=0 ep20=%d ep21=%d\n", i,
				 apple_rtkit_has_endpoint(probe->rtk[i], 0x20),
				 apple_rtkit_has_endpoint(probe->rtk[i], 0x21));
			continue;
		}
		ret = apple_rtkit_get_epmap_snapshot(probe->rtk[i], &snap);
		if (ret)
			goto stop;
		dev_info(dev, "M5_RTKIT_EPMAP role=%d hello=%#llx version=%u..%u endpoints=%016llx:%016llx:%016llx:%016llx\n",
			 i, snap.raw_hello, snap.hello_min_version, snap.hello_max_version,
			 snap.endpoints[0], snap.endpoints[1], snap.endpoints[2], snap.endpoints[3]);
	}
	dev_info(dev, "M5_RTKIT_PROBE both-roles-complete full-boot=%d no-initdata no-submission\n", full_boot);
stop:
	dev_info(dev, "M5_RTKIT_PROBE discovery-result=%d\n", ret);
	for (ordinal = 1; ordinal >= 0; ordinal--) {
		i = start_order[ordinal];
		if (probe->started[i])
			dev_info(dev, "M5_RTKIT_PROBE role=%d stop-request-result=%d\n", i,
				 apple_mbox_ascwrap_v6_stop_cpu(probe->mbox[i]));
	}
	dev_info(dev, "M5_RTKIT_PROBE owners retained until reboot\n");
	return 0;

before_start:
	for (i = 1; i >= 0; i--)
		if (probe->rtk[i])
			apple_rtkit_free(probe->rtk[i]);
	kfree(probe);
	return dev_err_probe(dev, ret, "M5 RTKit pre-start validation failed\n");
}

/* No MODULE_DEVICE_TABLE: this experiment must be loaded explicitly. */
static const struct of_device_id m5_match[] = {
	{ .compatible = "apple,agx-t8142" },
	{}
};

static struct platform_driver m5_driver = {
	.probe = m5_probe,
	.driver = {
		.name = "m5-gpu-rtkit-probe",
		.of_match_table = m5_match,
		.suppress_bind_attrs = true,
	},
};
module_platform_driver(m5_driver);
MODULE_LICENSE("Dual MIT/GPL");
MODULE_DESCRIPTION("M5 GPU firmware HELLO/EPMAP RAM-boot experiment");
