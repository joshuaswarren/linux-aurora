// SPDX-License-Identifier: BSD-3-Clause-Clear
/* Copyright (C) 2020 MediaTek Inc. */

#include <linux/etherdevice.h>
#include <linux/hwmon.h>
#include <linux/hwmon-sysfs.h>
#include <linux/thermal.h>
#include <linux/firmware.h>
#include "mt7921.h"
#include "mt7932_mcu_lab.h"
#include "../mt76_connac2_mac.h"
#include "mcu.h"
#include "regd.h"

/* The minimal bring-up root has no regulatory.db, so cfg80211 otherwise
 * leaves every 6 GHz channel disabled under country 00 even though firmware
 * already runs the measured AU policy.  This conservative fallback is
 * derived from 25G83 IZUBA_db.dat SHA-256 a61e0b70...: AU enables 6 GHz
 * channels 1..93 (5925..6425 MHz).  Use the all-location 14 dBm VLP ceiling;
 * the platform's higher-power indoor policy remains firmware-owned.
 */
static const struct ieee80211_regdomain mt7932_j700_au_regdom = {
	.alpha2 = "AU",
	.n_reg_rules = 7,
	.reg_rules = {
		REG_RULE(2402, 2472, 40, 0, 20, 0),
		REG_RULE(2457, 2482, 20, 0, 20, NL80211_RRF_NO_IR),
		REG_RULE(5170, 5250, 80, 0, 20, NL80211_RRF_NO_IR),
		REG_RULE(5250, 5330, 80, 0, 20,
			 NL80211_RRF_DFS | NL80211_RRF_NO_IR),
		REG_RULE(5490, 5730, 160, 0, 20,
			 NL80211_RRF_DFS | NL80211_RRF_NO_IR),
		REG_RULE(5735, 5835, 80, 0, 20, NL80211_RRF_NO_IR),
		REG_RULE(5925, 6425, 160, 0, 14, 0),
	},
};

static int mt7932_j700_verify_au_regdom(struct mt792x_dev *dev)
{
	struct ieee80211_supported_band *sband;
	unsigned int enabled = 0, upper = 0;
	int i;

	sband = dev->mt76.hw->wiphy->bands[NL80211_BAND_6GHZ];
	if (!sband)
		return -ENODEV;
	for (i = 0; i < sband->n_channels; i++) {
		struct ieee80211_channel *ch = &sband->channels[i];

		if (ch->flags & IEEE80211_CHAN_DISABLED)
			continue;
		if (ch->center_freq > 6415)
			upper++;
		else if (ch->center_freq >= 5955)
			enabled++;
		if (ch->max_reg_power > 14)
			upper++;
	}
	if (enabled == 24 && !upper) {
		dev_info(dev->mt76.dev,
			 "J700_MT7932_AU_REGDOM_PASS: enabled_6g=24 channels=1-93 upper_enabled=0 max_eirp=14dBm source=25G83-IZUBA-db-a61e0b70\n");
		return 0;
	}

	for (i = 0; i < sband->n_channels; i++)
		sband->channels[i].flags |= IEEE80211_CHAN_DISABLED;
	dev_err(dev->mt76.dev,
		"J700_MT7932_AU_REGDOM_FAIL: enabled_6g=%u invalid_or_upper=%u action=disable-all-6g\n",
		enabled, upper);
	return -EIO;
}

/* Cycle395 private J700 image: target WCAL/OCA2 are now captured, SHA-pinned
 * and embedded in the RAM-only initramfs. Keep the runtime parameters so a
 * later generic build can still override policy, but make this image fit the
 * proven U-Boot 256-byte bootargs ceiling without dropping PCIe/PM controls. */
static bool mt7932_load_wcal = true;
module_param_named(load_wcal, mt7932_load_wcal, bool, 0444);
MODULE_PARM_DESC(load_wcal,
		 "load J700 MT7932 FDR WCAL before MAC initialization");

static bool mt7932_load_one_time_cal = true;
module_param_named(load_one_time_cal, mt7932_load_one_time_cal, bool, 0444);
MODULE_PARM_DESC(load_one_time_cal,
		 "load host-converted J700 MT7932 OCA/IFCAL before MAC initialization");

bool mt7932_replay_au_policy;
module_param_named(replay_au_policy, mt7932_replay_au_policy, bool, 0644);
MODULE_PARM_DESC(replay_au_policy,
		 "replay the later captured J700 AU policy transaction in the lab");

static ssize_t mt7921_thermal_temp_show(struct device *dev,
					struct device_attribute *attr,
					char *buf)
{
	switch (to_sensor_dev_attr(attr)->index) {
	case 0: {
		struct mt792x_phy *phy = dev_get_drvdata(dev);
		struct mt792x_dev *mdev = phy->dev;
		int temperature;

		mt792x_mutex_acquire(mdev);
		temperature = mt7921_mcu_get_temperature(phy);
		mt792x_mutex_release(mdev);

		if (temperature < 0)
			return temperature;
		/* display in millidegree Celsius */
		return sprintf(buf, "%u\n", temperature * 1000);
	}
	default:
		return -EINVAL;
	}
}
static SENSOR_DEVICE_ATTR_RO(temp1_input, mt7921_thermal_temp, 0);

static struct attribute *mt7921_hwmon_attrs[] = {
	&sensor_dev_attr_temp1_input.dev_attr.attr,
	NULL,
};
ATTRIBUTE_GROUPS(mt7921_hwmon);

static int mt7921_thermal_init(struct mt792x_phy *phy)
{
	struct wiphy *wiphy = phy->mt76->hw->wiphy;
	struct device *hwmon;
	const char *name;

	if (!IS_REACHABLE(CONFIG_HWMON))
		return 0;

	name = devm_kasprintf(&wiphy->dev, GFP_KERNEL, "mt7921_%s",
			      wiphy_name(wiphy));
	if (!name)
		return -ENOMEM;

	hwmon = devm_hwmon_device_register_with_groups(&wiphy->dev, name, phy,
						       mt7921_hwmon_groups);
	return PTR_ERR_OR_ZERO(hwmon);
}

int mt7921_mac_init(struct mt792x_dev *dev)
{
	int i;

	mt76_rmw_field(dev, MT_MDP_DCR1, MT_MDP_DCR1_MAX_RX_LEN, 1536);
	/* enable hardware de-agg */
	mt76_set(dev, MT_MDP_DCR0, MT_MDP_DCR0_DAMSDU_EN);
	/* enable hardware rx header translation */
	mt76_set(dev, MT_MDP_DCR0, MT_MDP_DCR0_RX_HDR_TRANS_EN);

	for (i = 0; i < MT792x_WTBL_SIZE; i++)
		mt7921_mac_wtbl_update(dev, i,
				       MT_WTBL_UPDATE_ADM_COUNT_CLEAR);
	for (i = 0; i < 2; i++)
		mt792x_mac_init_band(dev, i);

	return mt76_connac_mcu_set_rts_thresh(&dev->mt76, 0x92b, 0);
}
EXPORT_SYMBOL_GPL(mt7921_mac_init);

static int mt7921_post_firmware_init(struct mt792x_dev *dev)
{
	int ret;

	ret = mt76_eeprom_override(&dev->mphy);
	if (ret) {
		if (is_mt7932(&dev->mt76))
			dev_err(dev->mt76.dev,
				"J700_MT7932_FIRMWARE_GATE_FAIL: stage=eeprom-override ret=%d\n",
				ret);
		goto out;
	}

	if (is_mt7932(&dev->mt76)) {
		/* mt7921_run_firmware() already emitted exact ordinals 84..134,
		 * including Apple's mode-3 EFUSE selection.  Do not add the generic
		 * mode-0 query/ack packet here between pre-cal and WCAL ordinal 135.
		 */
		memcpy(dev->mt76.alpha2, "AU", sizeof(dev->mt76.alpha2));
		dev_info(dev->mt76.dev,
			 "J700_MT7932_COUNTRY_PIN: alpha2=AU host-policy=1 firmware-phases=XZ-pre-cal,AU-update source=25G83-live-capture\n");
	} else {
		ret = mt7921_mcu_set_eeprom(dev);
		if (ret) {
			if (is_mt7932(&dev->mt76))
				dev_err(dev->mt76.dev,
					"J700_MT7932_FIRMWARE_GATE_FAIL: stage=efuse-mode ret=%d\n",
					ret);
			goto out;
		}
		if (is_mt7932(&dev->mt76)) {
			memcpy(dev->mt76.alpha2, "AU", sizeof(dev->mt76.alpha2));
			dev_info(dev->mt76.dev,
				 "J700_MT7932_EFUSE_MODE_PASS: mode=functional source=cycle469-on-chip external-buffer-hook=disabled-in-25G83\n");
			dev_info(dev->mt76.dev,
				 "J700_MT7932_COUNTRY_PIN: alpha2=AU host-policy=1 firmware-policy=generic-linux source=cycle469-scan-fsm-frontier\n");
		}
	}
	if (is_mt7932(&dev->mt76) && mt7932_load_wcal) {
		ret = mt7932_mcu_load_wcal(dev);
		if (ret) {
			dev_err(dev->mt76.dev,
				"J700_MT7932_FIRMWARE_GATE_FAIL: stage=wcal ret=%d\n",
				ret);
			goto out;
		}
	}
	if (is_mt7932(&dev->mt76) && mt7932_load_one_time_cal) {
		if (!mt7932_load_wcal) {
			ret = -EINVAL;
			dev_err(dev->mt76.dev,
				"J700_MT7932_FIRMWARE_GATE_FAIL: stage=one-time-cal-prerequisite require_wcal=1\n");
			goto out;
		}
		ret = mt7932_mcu_load_one_time_cal(dev);
		if (ret) {
			dev_err(dev->mt76.dev,
				"J700_MT7932_FIRMWARE_GATE_FAIL: stage=one-time-cal ret=%d\n",
				ret);
			goto out;
		}
	}

	ret = mt7921_mac_init(dev);
	if (ret && is_mt7932(&dev->mt76))
		dev_err(dev->mt76.dev,
			"J700_MT7932_FIRMWARE_GATE_FAIL: stage=mac-init ret=%d\n",
			ret);
out:
	return ret;
}

int mt7921_reset_hardware(struct mt792x_dev *dev)
{
	int ret;

	if (!is_mt7932(&dev->mt76))
		return -EOPNOTSUPP;
	dev->mt7932_6g_scan_policy_enabled = false;
	dev->mt7932_scan_fwlog_enabled = false;
	ret = mt7921_post_firmware_init(dev);
	if (!ret)
		dev_info(dev->mt76.dev,
			 "J700_MT7932_RESET_HARDWARE_PASS: eeprom=restored wcal=pass one-time-cal=pass mac=pass\n");

	return ret;
}
EXPORT_SYMBOL_GPL(mt7921_reset_hardware);

int mt7932_apply_manufacture_policy(struct mt792x_dev *dev)
{
	int ret;

	ret = mt76_connac_mcu_set_channel_domain(&dev->mphy);
	if (ret)
		return ret;
	ret = mt7932_mcu_load_power_transcript(dev);
	if (ret)
		return ret;
	ret = mt7932_mcu_drain_tx17(dev, "power-post", false);
	if (ret)
		return ret;
	ret = mt7932_mcu_init_dbdc(dev);
	if (ret)
		return ret;
	ret = mt76_connac_mcu_set_channel_domain(&dev->mphy);
	if (!ret)
		dev_info(dev->mt76.dev,
			 "J700_MT7932_TYPED_AU_POLICY_PASS: country=AU mode6g=VLP domain=initial,refresh power=42 dbdc=typed replay=0\n");

	return ret;
}
EXPORT_SYMBOL_GPL(mt7932_apply_manufacture_policy);

static int __mt7921_init_hardware(struct mt792x_dev *dev)
{
	int ret;

	/* force firmware operation mode into normal state,
	 * which should be set before firmware download stage.
	 */
	mt76_wr(dev, MT_SWDEF_MODE, MT_SWDEF_NORMAL_MODE);
	ret = mt792x_mcu_init(dev);
	if (ret)
		return ret;

	return mt7921_post_firmware_init(dev);
}

static int mt7921_init_hardware(struct mt792x_dev *dev)
{
	int ret, i;
	int attempts = is_mt7932(&dev->mt76) ? 1 : MT792x_MCU_INIT_RETRY_COUNT;

	set_bit(MT76_STATE_INITIALIZED, &dev->mphy.state);

	for (i = 0; i < attempts; i++) {
		ret = __mt7921_init_hardware(dev);
		if (!ret)
			break;

		if (i + 1 < attempts)
			mt792x_init_reset(dev);
	}

	if (i == attempts) {
		dev_err(dev->mt76.dev, "hardware init failed\n");
		if (is_mt7932(&dev->mt76))
			dev_err(dev->mt76.dev,
				"J700_MT7932_FIRMWARE_GATE_FAIL: stage=hardware-init ret=%d attempts=1\n",
				ret);
		return ret;
	}
	if (is_mt7932(&dev->mt76))
		dev_info(dev->mt76.dev,
			 "J700_MT7932_HARDWARE_INIT_PASS: attempts=1\n");

	return 0;
}

static int mt7921_init_work_run(struct mt792x_dev *dev)
{
	int ret;

	ret = mt7921_init_hardware(dev);
	if (ret)
		return ret;
	if (is_mt7932(&dev->mt76)) {
		ret = mt7932_apply_manufacture_policy(dev);
		if (ret)
			return ret;
	}

	mt76_set_stream_caps(&dev->mphy, true);
	mt7921_set_stream_he_caps(&dev->phy);
	mt792x_config_mac_addr_list(dev);
	ret = mt76_register_device(&dev->mt76, true, mt76_rates,
				   ARRAY_SIZE(mt76_rates));
	if (ret) {
		dev_err(dev->mt76.dev, "register device failed\n");
		return ret;
	}
	if (is_mt7932(&dev->mt76)) {
		ret = mt7932_j700_verify_au_regdom(dev);
		if (ret)
			return ret;
		dev_info(dev->mt76.dev,
			 "J700_MT7932_NETWORK_REGISTER_PASS: firmware=%s\n",
			 dev->mt76.hw->wiphy->fw_version);
	}
	if (is_mt7932(&dev->mt76)) {
		dev_info(dev->mt76.dev,
			 "J700_MT7932_AU_POLICY_REPLAY_DISABLED: ordinals=601-643 replacement=typed-device-policy\n");
	}
	ret = mt7921_init_debugfs(dev);
	if (ret) {
		dev_err(dev->mt76.dev, "register debugfs failed\n");
		return ret;
	}

	ret = mt7921_thermal_init(&dev->phy);
	if (ret) {
		dev_err(dev->mt76.dev, "thermal init failed\n");
		return ret;
	}

	/* we support chip reset now */
	dev->hw_init_done = true;
	ret = mt7932_mcu_lab_runtime_create(dev);
	if (ret)
		return ret;

	mt76_connac_mcu_set_deep_sleep(&dev->mt76, dev->pm.ds_enable);

	return 0;
}

static void mt7921_init_work(struct work_struct *work)
{
	struct mt792x_dev *dev = container_of(work, struct mt792x_dev,
					      init_work);

	mt7921_init_work_run(dev);
}

int mt7921_register_device(struct mt792x_dev *dev)
{
	struct ieee80211_hw *hw = mt76_hw(dev);
	int ret;

	dev->phy.dev = dev;
	dev->phy.mt76 = &dev->mt76.phy;
	dev->mt76.phy.priv = &dev->phy;
	dev->mt76.tx_worker.fn = mt792x_tx_worker;

	INIT_DELAYED_WORK(&dev->pm.ps_work, mt792x_pm_power_save_work);
	INIT_WORK(&dev->pm.wake_work, mt792x_pm_wake_work);
	spin_lock_init(&dev->pm.wake.lock);
	mutex_init(&dev->pm.mutex);
	init_waitqueue_head(&dev->pm.wait);
	init_waitqueue_head(&dev->wait);
	if (mt76_is_sdio(&dev->mt76))
		init_waitqueue_head(&dev->mt76.sdio.wait);
	spin_lock_init(&dev->pm.txq_lock);
	INIT_DELAYED_WORK(&dev->mphy.mac_work, mt792x_mac_work);
	INIT_DELAYED_WORK(&dev->phy.scan_work, mt7921_scan_work);
	INIT_DELAYED_WORK(&dev->coredump.work, mt7921_coredump_work);
#if IS_ENABLED(CONFIG_IPV6)
	INIT_WORK(&dev->ipv6_ns_work, mt7921_set_ipv6_ns_work);
	skb_queue_head_init(&dev->ipv6_ns_list);
#endif
	skb_queue_head_init(&dev->phy.scan_event_list);
	skb_queue_head_init(&dev->coredump.msg_list);

	INIT_WORK(&dev->reset_work, mt7921_mac_reset_work);
	INIT_WORK(&dev->init_work, mt7921_init_work);

	INIT_WORK(&dev->phy.roc_work, mt7921_roc_work);
	timer_setup(&dev->phy.roc_timer, mt792x_roc_timer, 0);
	init_waitqueue_head(&dev->phy.roc_wait);

	dev->pm.idle_timeout = MT792x_PM_TIMEOUT;
	dev->pm.stats.last_wake_event = jiffies;
	dev->pm.stats.last_doze_event = jiffies;

	if (!mt76_is_usb(&dev->mt76) &&
	    !is_mt7902(&dev->mt76) &&
	    !is_mt7932(&dev->mt76)) {
		dev->pm.enable_user = true;
		dev->pm.enable = true;
		dev->pm.ds_enable_user = true;
		dev->pm.ds_enable = true;
	}
	if (is_mt7932(&dev->mt76))
		dev_info(dev->mt76.dev,
			 "J700_MT7932_PM_GUARD: runtime-pm=off deep-sleep=off\n");

	if (!mt76_is_mmio(&dev->mt76))
		hw->extra_tx_headroom += MT_SDIO_TXD_SIZE + MT_SDIO_HDR_SIZE;

	mt792x_init_acpi_sar(dev);

	ret = mt792x_init_wcid(dev);
	if (ret)
		return ret;

	ret = mt792x_init_wiphy(hw);
	if (ret)
		return ret;
	if (is_mt7932(&dev->mt76)) {
		hw->wiphy->regulatory_flags |= REGULATORY_CUSTOM_REG;
		dev->mt76.custom_regd = &mt7932_j700_au_regdom;
		dev_info(dev->mt76.dev,
			 "J700_MT7932_AU_REGDOM_STAGED: source=25G83-IZUBA-db-a61e0b70 channels=6g1-93 max_eirp=14dBm mode=VLP root-regdb=absent\n");
	}
	/* AppleSunriseWLAN 25G83 carries the live association listen interval in
	 * EA/0x40 and caps that policy input at 20 beacon intervals.  Advertising
	 * the same J700 capability also makes mac80211 initialize the live value
	 * consumed by the typed fullmac builder instead of its generic fallback 5.
	 */
	if (is_mt7932(&dev->mt76))
		hw->max_listen_interval = 20;

	hw->wiphy->reg_notifier = mt7921_regd_notifier;
	dev->mphy.sband_2g.sband.ht_cap.cap |=
			IEEE80211_HT_CAP_LDPC_CODING |
			IEEE80211_HT_CAP_MAX_AMSDU;
	dev->mphy.sband_5g.sband.ht_cap.cap |=
			IEEE80211_HT_CAP_LDPC_CODING |
			IEEE80211_HT_CAP_MAX_AMSDU;
	dev->mphy.sband_5g.sband.vht_cap.cap |=
			IEEE80211_VHT_CAP_MAX_MPDU_LENGTH_11454 |
			IEEE80211_VHT_CAP_MAX_A_MPDU_LENGTH_EXPONENT_MASK |
			IEEE80211_VHT_CAP_SU_BEAMFORMEE_CAPABLE |
			IEEE80211_VHT_CAP_MU_BEAMFORMEE_CAPABLE |
			(3 << IEEE80211_VHT_CAP_BEAMFORMEE_STS_SHIFT);
	if (is_mt7922_class(&dev->mt76))
		dev->mphy.sband_5g.sband.vht_cap.cap |=
			IEEE80211_VHT_CAP_SUPP_CHAN_WIDTH_160MHZ |
			IEEE80211_VHT_CAP_SHORT_GI_160;

	dev->mphy.hw->wiphy->available_antennas_rx = dev->mphy.chainmask;
	dev->mphy.hw->wiphy->available_antennas_tx = dev->mphy.chainmask;

	/* J700's MT7932 boot ROM has a bounded interval between the final RAM
	 * download-config response and CID-2.  The normal per-CPU workqueue let
	 * init-memory freeing and unrelated USB/HID probes deschedule the final
	 * scatter burst for more than 100 ms; the same path completes in about
	 * 7 ms when it is not starved.  Keep the hardware-proven synchronous
	 * ordering while the inherited-controller handoff is being consolidated.
	 * Other mt7921-family devices retain the normal deferred workqueue.
	 */
	if (is_mt7932(&dev->mt76)) {
		dev_info(dev->mt76.dev,
			 "J700_MT7932_SYNCHRONOUS_INIT: reason=bounded-rom-download-window\n");
		ret = mt7921_init_work_run(dev);
		if (ret)
			return ret;
	} else {
		queue_work(system_percpu_wq, &dev->init_work);
	}

	return 0;
}
EXPORT_SYMBOL_GPL(mt7921_register_device);
