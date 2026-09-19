// SPDX-License-Identifier: GPL-2.0-only OR MIT
/*
 * Apple SMC (System Management Controller) MFD driver
 *
 * Copyright The Asahi Linux Contributors
 */

#include <linux/bitfield.h>
#include <linux/ctype.h>
#include <linux/debugfs.h>
#include <linux/seq_file.h>
#include <linux/delay.h>
#include <linux/device.h>
#include <linux/io.h>
#include <linux/ioport.h>
#include <linux/math.h>
#include <linux/mfd/core.h>
#include <linux/mfd/macsmc.h>
#include <linux/notifier.h>
#include <linux/of.h>
#include <linux/of_platform.h>
#include <linux/overflow.h>
#include <linux/platform_device.h>
#include <linux/soc/apple/rtkit.h>
#include <linux/unaligned.h>

#define SMC_ENDPOINT			0x20

/* We don't actually know the true size here but this seem reasonable */
#define SMC_SHMEM_SIZE			0x1000
#define SMC_MAX_SIZE			255

#define SMC_MSG_READ_KEY		0x10
#define SMC_MSG_WRITE_KEY		0x11
#define SMC_MSG_GET_KEY_BY_INDEX	0x12
#define SMC_MSG_GET_KEY_INFO		0x13
#define SMC_MSG_INITIALIZE		0x17
#define SMC_MSG_NOTIFICATION		0x18
#define SMC_MSG_RW_KEY			0x20

#define SMC_DATA			GENMASK_ULL(63, 32)
#define SMC_WSIZE			GENMASK_ULL(31, 24)
#define SMC_SIZE			GENMASK_ULL(23, 16)
#define SMC_ID				GENMASK_ULL(15, 12)
#define SMC_MSG				GENMASK_ULL(7, 0)
#define SMC_RESULT			SMC_MSG

#define SMC_TIMEOUT_MS		500

static const struct mfd_cell apple_smc_devs[] = {
	MFD_CELL_NAME("macsmc-input"),
	MFD_CELL_NAME("macsmc-power"),
	MFD_CELL_OF("macsmc-gpio", NULL, NULL, 0, 0, "apple,smc-gpio"),
	MFD_CELL_OF("macsmc-low-gpio", NULL, NULL, 0, 0, "apple,smc-low-gpio"),
	MFD_CELL_OF("macsmc-hwmon", NULL, NULL, 0, 0, "apple,smc-hwmon"),
	MFD_CELL_OF("macsmc-reboot", NULL, NULL, 0, 0, "apple,smc-reboot"),
	MFD_CELL_OF("macsmc-rtc", NULL, NULL, 0, 0, "apple,smc-rtc"),
};

/* J700 diagnostic gate: expose only the established gPxx GPIO bank. */
static const struct mfd_cell apple_smc_gpio_devs[] = {
	MFD_CELL_OF("macsmc-gpio", NULL, NULL, 0, 0, "apple,smc-gpio"),
};

/* Initial T8152 support is read-only telemetry. Firmware retains fan control. */
static const struct mfd_cell apple_smc_telemetry_devs[] = {
	MFD_CELL_OF("macsmc-hwmon", NULL, NULL, 0, 0, "apple,smc-hwmon"),
};

static int apple_smc_cmd_locked(struct apple_smc *smc, u64 cmd, u64 arg,
				  u64 size, u64 wsize, u32 *ret_data)
{
	u8 result;
	int ret;
	u64 msg;

	lockdep_assert_held(&smc->mutex);

	if (smc->boot_stage != APPLE_SMC_INITIALIZED)
		return -EIO;
	if (smc->atomic_mode)
		return -EIO;

	reinit_completion(&smc->cmd_done);

	smc->msg_id = (smc->msg_id + 1) & 0xf;
	msg = (FIELD_PREP(SMC_MSG, cmd) |
	       FIELD_PREP(SMC_SIZE, size) |
	       FIELD_PREP(SMC_WSIZE, wsize) |
	       FIELD_PREP(SMC_ID, smc->msg_id) |
	       FIELD_PREP(SMC_DATA, arg));

	ret = apple_rtkit_send_message(smc->rtk, SMC_ENDPOINT, msg, NULL, false);
	if (ret) {
		dev_err(smc->dev, "Failed to send command\n");
		return ret;
	}

	if (wait_for_completion_timeout(&smc->cmd_done, msecs_to_jiffies(SMC_TIMEOUT_MS)) <= 0) {
		dev_err(smc->dev, "Command timed out (%llx)", msg);
		return -ETIMEDOUT;
	}

	if (FIELD_GET(SMC_ID, smc->cmd_ret) != smc->msg_id) {
		dev_err(smc->dev, "Command sequence mismatch (expected %d, got %d)\n",
			smc->msg_id, (unsigned int)FIELD_GET(SMC_ID, smc->cmd_ret));
		return -EIO;
	}

	result = FIELD_GET(SMC_RESULT, smc->cmd_ret);
	if (result)
		return -EIO;

	if (ret_data)
		*ret_data = FIELD_GET(SMC_DATA, smc->cmd_ret);

	return FIELD_GET(SMC_SIZE, smc->cmd_ret);
}

static int apple_smc_cmd(struct apple_smc *smc, u64 cmd, u64 arg,
			 u64 size, u64 wsize, u32 *ret_data)
{
	guard(mutex)(&smc->mutex);

	return apple_smc_cmd_locked(smc, cmd, arg, size, wsize, ret_data);
}

static int apple_smc_rw_locked(struct apple_smc *smc, smc_key key,
				const void *wbuf, size_t wsize,
				void *rbuf, size_t rsize)
{
	u64 smc_size, smc_wsize;
	u32 rdata;
	int ret;
	u64 cmd;

	lockdep_assert_held(&smc->mutex);

	if (rsize > SMC_MAX_SIZE)
		return -EINVAL;
	if (wsize > SMC_MAX_SIZE)
		return -EINVAL;

	if (rsize && wsize) {
		cmd = SMC_MSG_RW_KEY;
		memcpy_toio(smc->shmem.iomem, wbuf, wsize);
		smc_size = rsize;
		smc_wsize = wsize;
	} else if (wsize && !rsize) {
		cmd = SMC_MSG_WRITE_KEY;
		memcpy_toio(smc->shmem.iomem, wbuf, wsize);
		/*
		 * Setting size to the length we want to write and wsize to 0
		 * looks silly but that's how the SMC protocol works ¯\_(ツ)_/¯
		 */
		smc_size = wsize;
		smc_wsize = 0;
	} else if (!wsize && rsize) {
		cmd = SMC_MSG_READ_KEY;
		smc_size = rsize;
		smc_wsize = 0;
	} else {
		return -EINVAL;
	}

	ret = apple_smc_cmd_locked(smc, cmd, key, smc_size, smc_wsize, &rdata);
	if (ret < 0)
		return ret;

	if (rsize) {
		/*
		 * Small data <= 4 bytes is returned as part of the reply
		 * message which is sent over the mailbox FIFO. Everything
		 * bigger has to be copied from SRAM which is mapped as
		 * Device memory.
		 */
		if (rsize <= 4)
			memcpy(rbuf, &rdata, rsize);
		else
			memcpy_fromio(rbuf, smc->shmem.iomem, rsize);
	}

	return ret;
}

int apple_smc_read(struct apple_smc *smc, smc_key key, void *buf, size_t size)
{
	guard(mutex)(&smc->mutex);

	return apple_smc_rw_locked(smc, key, NULL, 0, buf, size);
}
EXPORT_SYMBOL(apple_smc_read);

int apple_smc_write(struct apple_smc *smc, smc_key key, const void *buf, size_t size)
{
	guard(mutex)(&smc->mutex);

	return apple_smc_rw_locked(smc, key, buf, size, NULL, 0);
}
EXPORT_SYMBOL(apple_smc_write);

int apple_smc_rw(struct apple_smc *smc, smc_key key, const void *wbuf, size_t wsize,
		 void *rbuf, size_t rsize)
{
	guard(mutex)(&smc->mutex);

	return apple_smc_rw_locked(smc, key, wbuf, wsize, rbuf, rsize);
}
EXPORT_SYMBOL(apple_smc_rw);

int apple_smc_get_key_by_index(struct apple_smc *smc, int index, smc_key *key)
{
	int ret;

	ret = apple_smc_cmd(smc, SMC_MSG_GET_KEY_BY_INDEX, index, 0, 0, key);

	*key = swab32(*key);
	return ret;
}
EXPORT_SYMBOL(apple_smc_get_key_by_index);

int apple_smc_get_key_info(struct apple_smc *smc, smc_key key, struct apple_smc_key_info *info)
{
	u8 key_info[6];
	int ret;

	ret = apple_smc_cmd(smc, SMC_MSG_GET_KEY_INFO, key, 0, 0, NULL);
	if (ret >= 0 && info) {
		memcpy_fromio(key_info, smc->shmem.iomem, sizeof(key_info));
		info->size = key_info[0];
		info->type_code = get_unaligned_be32(&key_info[1]);
		info->flags = key_info[5];
	}
	return ret;
}
EXPORT_SYMBOL(apple_smc_get_key_info);

int apple_smc_enter_atomic(struct apple_smc *smc)
{
	guard(mutex)(&smc->mutex);

	/*
	 * Disable notifications since this is called before shutdown and no
	 * notification handler will be able to handle the notification
	 * using atomic operations only. Also ignore any failure here
	 * because we're about to shut down or reboot anyway.
	 * We can't use apple_smc_write_flag here since that would try to lock
	 * smc->mutex again.
	 */
	const u8 flag = 0;

	apple_smc_rw_locked(smc, SMC_KEY(NTAP), &flag, sizeof(flag), NULL, 0);

	smc->atomic_mode = true;

	return 0;
}
EXPORT_SYMBOL(apple_smc_enter_atomic);

int apple_smc_write_atomic(struct apple_smc *smc, smc_key key, const void *buf, size_t size)
{
	guard(spinlock_irqsave)(&smc->lock);
	u8 result;
	int ret;
	u64 msg;

	if (size > SMC_MAX_SIZE || size == 0)
		return -EINVAL;

	if (smc->boot_stage != APPLE_SMC_INITIALIZED)
		return -EIO;
	if (!smc->atomic_mode)
		return -EIO;

	memcpy_toio(smc->shmem.iomem, buf, size);
	smc->msg_id = (smc->msg_id + 1) & 0xf;
	msg = (FIELD_PREP(SMC_MSG, SMC_MSG_WRITE_KEY) |
	       FIELD_PREP(SMC_SIZE, size) |
	       FIELD_PREP(SMC_ID, smc->msg_id) |
	       FIELD_PREP(SMC_DATA, key));
	smc->atomic_pending = true;

	ret = apple_rtkit_send_message(smc->rtk, SMC_ENDPOINT, msg, NULL, true);
	if (ret < 0) {
		dev_err(smc->dev, "Failed to send command (%d)\n", ret);
		return ret;
	}

	while (smc->atomic_pending) {
		ret = apple_rtkit_poll(smc->rtk);
		if (ret < 0) {
			dev_err(smc->dev, "RTKit poll failed (%llx)", msg);
			return ret;
		}
		udelay(100);
	}

	if (FIELD_GET(SMC_ID, smc->cmd_ret) != smc->msg_id) {
		dev_err(smc->dev, "Command sequence mismatch (expected %d, got %d)\n",
			smc->msg_id, (unsigned int)FIELD_GET(SMC_ID, smc->cmd_ret));
		return -EIO;
	}

	result = FIELD_GET(SMC_RESULT, smc->cmd_ret);
	if (result)
		return -EIO;

	return FIELD_GET(SMC_SIZE, smc->cmd_ret);
}
EXPORT_SYMBOL(apple_smc_write_atomic);

static void apple_smc_rtkit_crashed(void *cookie, const void *bfr, size_t bfr_len)
{
	struct apple_smc *smc = cookie;

	smc->boot_stage = APPLE_SMC_ERROR_CRASHED;
	dev_err(smc->dev, "SMC crashed! Your system will reboot in a few seconds...\n");
}

static int apple_smc_rtkit_shmem_setup(void *cookie, struct apple_rtkit_shmem *bfr)
{
	struct apple_smc *smc = cookie;
	size_t bfr_end;

	if (!bfr->iova) {
		dev_err(smc->dev, "RTKit wants a RAM buffer\n");
		return -EIO;
	}

	if (check_add_overflow(bfr->iova, bfr->size - 1, &bfr_end))
		return -EFAULT;

	if (bfr->iova < smc->sram->start || bfr->iova > smc->sram->end ||
	    bfr_end > smc->sram->end) {
		dev_err(smc->dev, "RTKit buffer request outside SRAM region: [0x%llx, 0x%llx]\n",
			(unsigned long long)bfr->iova,
			(unsigned long long)bfr_end);
		return -EFAULT;
	}

	bfr->iomem = smc->sram_base + (bfr->iova - smc->sram->start);
	bfr->is_mapped = true;

	return 0;
}

static bool apple_smc_rtkit_recv_early(void *cookie, u8 endpoint, u64 message)
{
	struct apple_smc *smc = cookie;

	if (endpoint != SMC_ENDPOINT) {
		dev_warn(smc->dev, "Received message for unknown endpoint 0x%x\n", endpoint);
		return false;
	}

	if (smc->boot_stage == APPLE_SMC_BOOTING) {
		int ret;

		smc->shmem.iova = message;
		smc->shmem.size = SMC_SHMEM_SIZE;
		ret = apple_smc_rtkit_shmem_setup(smc, &smc->shmem);
		if (ret < 0) {
			smc->boot_stage = APPLE_SMC_ERROR_NO_SHMEM;
			dev_err(smc->dev, "Failed to initialize shared memory (%d)\n", ret);
		} else {
			smc->boot_stage = APPLE_SMC_INITIALIZED;
		}
		complete(&smc->init_done);
	} else if (FIELD_GET(SMC_MSG, message) == SMC_MSG_NOTIFICATION) {
		/* Handle these in the RTKit worker thread */
		return false;
	} else {
		smc->cmd_ret = message;
		if (smc->atomic_pending)
			smc->atomic_pending = false;
		else
			complete(&smc->cmd_done);
	}

	return true;
}

static void apple_smc_rtkit_recv(void *cookie, u8 endpoint, u64 message)
{
	struct apple_smc *smc = cookie;

	if (endpoint != SMC_ENDPOINT) {
		dev_warn(smc->dev, "Received message for unknown endpoint 0x%x\n", endpoint);
		return;
	}

	if (FIELD_GET(SMC_MSG, message) != SMC_MSG_NOTIFICATION) {
		dev_warn(smc->dev, "Received unknown message from worker: 0x%llx\n", message);
		return;
	}

	blocking_notifier_call_chain(&smc->event_handlers, FIELD_GET(SMC_DATA, message), NULL);
}

static const struct apple_rtkit_ops apple_smc_rtkit_ops = {
	.crashed = apple_smc_rtkit_crashed,
	.recv_message = apple_smc_rtkit_recv,
	.recv_message_early = apple_smc_rtkit_recv_early,
	.shmem_setup = apple_smc_rtkit_shmem_setup,
};

static void apple_smc_rtkit_shutdown(void *data)
{
	struct apple_smc *smc = data;

	/* Shut down SMC firmware, if it's not completely wedged */
	if (apple_rtkit_is_running(smc->rtk))
		apple_rtkit_quiesce(smc->rtk);
}

static void apple_smc_disable_notifications(void *data)
{
	struct apple_smc *smc = data;

	apple_smc_write_flag(smc, SMC_KEY(NTAP), false);
}

static int apple_smc_j700_log_function_key(struct apple_smc *smc,
					    smc_key key, const char *name)
{
	struct apple_smc_key_info info;
	int ret;

	ret = apple_smc_get_key_info(smc, key, &info);
	if (ret < 0)
		return dev_err_probe(smc->dev, ret,
				     "Failed to read J700 %s key metadata\n", name);

	dev_info(smc->dev,
		 "J700_SMC_FUNCTION_KEY: name=%s key=%08x size=%u type=%08x flags=%02x readable=%u writable=%u function=%u\n",
		 name, key, info.size, info.type_code, info.flags,
		 !!(info.flags & APPLE_SMC_READABLE),
		 !!(info.flags & APPLE_SMC_WRITABLE),
		 !!(info.flags & APPLE_SMC_FUNCTION));

	return 0;
}

#ifdef CONFIG_DEBUG_FS
/*
 * Read-only view of the firmware's own key table.  The hwmon child only
 * instantiates the sensors named in the DT, so walking the table is the only
 * way to see which keys a given machine actually implements.  Metadata is
 * always safe to enumerate; values are read only for keys the firmware marks
 * readable and not as functions, so that listing never executes anything.
 */
#define SMC_DBG_MAX_VALUE 32

static void apple_smc_key_str(smc_key key, char out[5])
{
	out[0] = (key >> 24) & 0xff;
	out[1] = (key >> 16) & 0xff;
	out[2] = (key >> 8) & 0xff;
	out[3] = key & 0xff;
	out[4] = '\0';
}

/* Same decode as the hwmon child, kept local so this stays debug-only. */
static int apple_smc_dbg_f32(u32 fval, long *p)
{
	int exp = (fval >> 23) & 0xff;
	u64 val;

	if (exp == 0xff)
		return -ENODATA;

	val = fval & GENMASK(22, 0);
	if (exp)
		val |= BIT(23);
	else
		exp = 1;
	exp -= 127 + 23;

	val *= 1000;
	if (exp > 40)
		return -ERANGE;
	else if (exp < -63)
		val = 0;
	else if (exp < 0)
		val >>= -exp;
	else
		val <<= exp;

	*p = (fval & BIT(31)) ? -(long)val : (long)val;
	return 0;
}

static void apple_smc_dbg_value(struct seq_file *s, struct apple_smc *smc,
				smc_key key, const struct apple_smc_key_info *info)
{
	u8 buf[SMC_DBG_MAX_VALUE];
	char type[5];
	int ret, i;

	if (!(info->flags & APPLE_SMC_READABLE) || (info->flags & APPLE_SMC_FUNCTION))
		return;
	if (!info->size || info->size > SMC_DBG_MAX_VALUE)
		return;

	ret = apple_smc_read(smc, key, buf, info->size);
	if (ret < 0) {
		seq_printf(s, " err=%d", ret);
		return;
	}

	seq_puts(s, " ");
	for (i = 0; i < info->size; i++)
		seq_printf(s, "%02x", buf[i]);

	apple_smc_key_str(info->type_code, type);
	if (!strcmp(type, "flt ") && info->size == 4) {
		long v;

		if (!apple_smc_dbg_f32(get_unaligned_le32(buf), &v))
			seq_printf(s, " = %ld.%03ld", v / 1000, abs(v % 1000));
	} else if (!strcmp(type, "flag") && info->size == 1) {
		seq_printf(s, " = %s", buf[0] ? "true" : "false");
	} else if (!strncmp(type, "ui", 2) || !strncmp(type, "si", 2)) {
		u64 v = 0;

		/* Integers are big endian on the wire. */
		for (i = 0; i < info->size && i < 8; i++)
			v = (v << 8) | buf[i];
		seq_printf(s, " = %llu", v);
	} else if (!strcmp(type, "ioft") && info->size == 8) {
		u64 v = get_unaligned_be64(buf);

		seq_printf(s, " = %llu.%03llu", v >> 16, ((v & 0xffff) * 1000) >> 16);
	} else if (!strncmp(type, "ch8", 3)) {
		seq_puts(s, " = \"");
		for (i = 0; i < info->size; i++)
			seq_putc(s, isprint(buf[i]) ? buf[i] : '.');
		seq_puts(s, "\"");
	}
}

static int apple_smc_dbg_walk(struct seq_file *s, bool values)
{
	struct apple_smc *smc = s->private;
	char key_str[5], type_str[5];
	unsigned int i;

	seq_printf(s, "# %u keys, first %c%c%c%c\n", smc->key_count,
		   (smc->first_key >> 24) & 0xff, (smc->first_key >> 16) & 0xff,
		   (smc->first_key >> 8) & 0xff, smc->first_key & 0xff);
	seq_puts(s, "# index key  type size flags\n");

	for (i = 0; i < smc->key_count; i++) {
		struct apple_smc_key_info info;
		smc_key key;
		int ret;

		if (signal_pending(current))
			return -EINTR;

		ret = apple_smc_get_key_by_index(smc, i, &key);
		if (ret < 0) {
			seq_printf(s, "%5u <index error %d>\n", i, ret);
			continue;
		}

		apple_smc_key_str(key, key_str);
		ret = apple_smc_get_key_info(smc, key, &info);
		if (ret < 0) {
			seq_printf(s, "%5u %-4s <info error %d>\n", i, key_str, ret);
			continue;
		}

		apple_smc_key_str(info.type_code, type_str);
		seq_printf(s, "%5u %-4s %-4s %4u %c%c%c", i, key_str, type_str,
			   info.size,
			   (info.flags & APPLE_SMC_READABLE) ? 'r' : '-',
			   (info.flags & APPLE_SMC_WRITABLE) ? 'w' : '-',
			   (info.flags & APPLE_SMC_FUNCTION) ? 'f' : '-');

		if (values)
			apple_smc_dbg_value(s, smc, key, &info);

		seq_putc(s, '\n');
	}

	return 0;
}

static int apple_smc_keys_show(struct seq_file *s, void *unused)
{
	return apple_smc_dbg_walk(s, false);
}
DEFINE_SHOW_ATTRIBUTE(apple_smc_keys);

static int apple_smc_values_show(struct seq_file *s, void *unused)
{
	return apple_smc_dbg_walk(s, true);
}
DEFINE_SHOW_ATTRIBUTE(apple_smc_values);

static void apple_smc_debugfs_remove(void *data)
{
	debugfs_remove_recursive(data);
}

static void apple_smc_debugfs_init(struct apple_smc *smc)
{
	struct dentry *dir;

	dir = debugfs_create_dir("macsmc", NULL);
	if (IS_ERR(dir))
		return;

	debugfs_create_file("keys", 0400, dir, smc, &apple_smc_keys_fops);
	debugfs_create_file("values", 0400, dir, smc, &apple_smc_values_fops);
	devm_add_action_or_reset(smc->dev, apple_smc_debugfs_remove, dir);
}
#else
static void apple_smc_debugfs_init(struct apple_smc *smc) { }
#endif

static int apple_smc_probe(struct platform_device *pdev)
{
	struct device *dev = &pdev->dev;
	struct apple_smc *smc;
	bool j700_gpio_only;
	bool j700_read_only;
	__be32 count;
	int ret;

	j700_read_only = of_property_read_bool(dev->of_node,
					       "apple,j700-read-only-diagnostics");
	j700_gpio_only = of_property_read_bool(dev->of_node,
					      "apple,j700-gpio-only");
	if (j700_read_only && j700_gpio_only)
		return dev_err_probe(dev, -EINVAL,
				     "J700 SMC diagnostic modes are mutually exclusive\n");

	smc = devm_kzalloc(dev, sizeof(*smc), GFP_KERNEL);
	if (!smc)
		return -ENOMEM;

	mutex_init(&smc->mutex);
	smc->dev = &pdev->dev;
	smc->sram_base = devm_platform_get_and_ioremap_resource(pdev, 1, &smc->sram);
	if (IS_ERR(smc->sram_base))
		return dev_err_probe(dev, PTR_ERR(smc->sram_base), "Failed to map SRAM region");

	smc->rtk = devm_apple_rtkit_init(dev, smc, NULL, 0, &apple_smc_rtkit_ops);
	if (IS_ERR(smc->rtk))
		return dev_err_probe(dev, PTR_ERR(smc->rtk), "Failed to initialize RTKit");

	smc->boot_stage = APPLE_SMC_BOOTING;
	ret = apple_rtkit_wake(smc->rtk);
	if (ret)
		return dev_err_probe(dev, ret, "Failed to wake up SMC");

	ret = devm_add_action_or_reset(dev, apple_smc_rtkit_shutdown, smc);
	if (ret)
		return ret;

	ret = apple_rtkit_start_ep(smc->rtk, SMC_ENDPOINT);
	if (ret)
		return dev_err_probe(dev, ret, "Failed to start SMC endpoint");

	init_completion(&smc->init_done);
	init_completion(&smc->cmd_done);

	ret = apple_rtkit_send_message(smc->rtk, SMC_ENDPOINT,
				       FIELD_PREP(SMC_MSG, SMC_MSG_INITIALIZE), NULL, false);
	if (ret)
		return dev_err_probe(dev, ret, "Failed to send init message");

	if (wait_for_completion_timeout(&smc->init_done, msecs_to_jiffies(SMC_TIMEOUT_MS)) == 0) {
		dev_err(dev, "Timed out initializing SMC");
		return -ETIMEDOUT;
	}

	if (smc->boot_stage != APPLE_SMC_INITIALIZED) {
		dev_err(dev, "SMC failed to boot successfully, boot stage=%d\n", smc->boot_stage);
		return -EIO;
	}

	dev_set_drvdata(&pdev->dev, smc);
	BLOCKING_INIT_NOTIFIER_HEAD(&smc->event_handlers);

	ret = apple_smc_read(smc, SMC_KEY(#KEY), &count, sizeof(count));
	if (ret >= 0 && ret != sizeof(count))
		ret = -EINVAL;
	if (ret < 0)
		return dev_err_probe(smc->dev, ret, "Failed to get key count");
	smc->key_count = be32_to_cpu(count);

	apple_smc_debugfs_init(smc);

	/*
	 * J700's shipping multi-touch contract names two SMC functions, but the
	 * first hardware gate must not execute them.  Probe only their key
	 * metadata and deliberately skip NTAP plus every MFD child, since both
	 * paths can issue unrelated writes.  This remains opt-in and has no effect
	 * on the normal macsmc path or any existing platform DT.
	 */
	if (j700_read_only || j700_gpio_only) {
		ret = apple_smc_j700_log_function_key(smc, SMC_KEY(pcIO),
						      "pcIO");
		if (ret)
			return ret;
		ret = apple_smc_j700_log_function_key(smc, SMC_KEY(pmIP),
						      "pmIP");
		if (ret)
			return ret;

		if (j700_read_only) {
			dev_info(smc->dev,
				 "J700_SMC_READ_ONLY_READY: keys=%u key_writes=0 notifications=0 children=0\n",
				 smc->key_count);
			return 0;
		}

		/*
		 * A J700 pKW8 function with a 0x80000 mask was proven on the
		 * machine through existing gP08.  The AFE function's 0x10000 mask
		 * therefore maps to gP01.  Register only that established GPIO
		 * provider and leave notifications plus unrelated children off.
		 */
		ret = devm_mfd_add_devices(smc->dev, PLATFORM_DEVID_NONE,
					   apple_smc_gpio_devs,
					   ARRAY_SIZE(apple_smc_gpio_devs),
					   NULL, 0, NULL);
		if (ret)
			return dev_err_probe(smc->dev, ret,
					     "Failed to register J700 SMC GPIO provider");

		dev_info(smc->dev,
			 "J700_SMC_GPIO_ONLY_READY: keys=%u notifications=0 children=macsmc-gpio\n",
			 smc->key_count);
		return 0;
	}

	/* T8152's key-read interface is qualified; notifications, GPIO and power
	 * functions are not. The board DT exposes temperature and fan inputs only.
	 */
	if (of_device_is_compatible(dev->of_node, "apple,t8152-smc"))
		return devm_mfd_add_devices(smc->dev, PLATFORM_DEVID_NONE,
					    apple_smc_telemetry_devs,
					    ARRAY_SIZE(apple_smc_telemetry_devs),
					    NULL, 0, NULL);

	/* Enable notifications */
	apple_smc_write_flag(smc, SMC_KEY(NTAP), true);
	ret = devm_add_action_or_reset(dev, apple_smc_disable_notifications, smc);
	if (ret)
		return ret;

	ret = devm_mfd_add_devices(smc->dev, PLATFORM_DEVID_NONE,
				   apple_smc_devs, ARRAY_SIZE(apple_smc_devs),
				   NULL, 0, NULL);
	if (ret)
		return dev_err_probe(smc->dev, ret, "Failed to register sub-devices");


	return 0;
}

static const struct of_device_id apple_smc_of_match[] = {
	{ .compatible = "apple,t8152-smc" },
	{ .compatible = "apple,t8103-smc" },
	{ .compatible = "apple,smc" },
	{},
};
MODULE_DEVICE_TABLE(of, apple_smc_of_match);

static struct platform_driver apple_smc_driver = {
	.driver = {
		.name = "macsmc",
		.of_match_table = apple_smc_of_match,
	},
	.probe = apple_smc_probe,
};
module_platform_driver(apple_smc_driver);

MODULE_AUTHOR("Hector Martin <marcan@marcan.st>");
MODULE_AUTHOR("Sven Peter <sven@kernel.org>");
MODULE_LICENSE("Dual MIT/GPL");
MODULE_DESCRIPTION("Apple SMC driver");
