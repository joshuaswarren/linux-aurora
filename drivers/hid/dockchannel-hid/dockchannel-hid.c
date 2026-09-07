/*
 * SPDX-License-Identifier: GPL-2.0 OR MIT
 *
 * Apple DockChannel HID transport driver
 *
 * Copyright The Asahi Linux Contributors
 */
#include <linux/bitfield.h>
#include <linux/delay.h>
#include <linux/device.h>
#include <linux/dma-mapping.h>
#include <linux/firmware.h>
#include <linux/gpio/consumer.h>
#include <linux/gpio/driver.h>
#include <linux/hid.h>
#include <linux/ktime.h>
#include <linux/mfd/macsmc.h>
#include <linux/module.h>
#include <linux/platform_device.h>
#include <linux/slab.h>
#include <linux/soc/apple/dockchannel.h>
#include <linux/string.h>
#include <linux/unaligned.h>
#include <linux/of.h>
#include "../hid-ids.h"

#define COMMAND_TIMEOUT_MS 1000
#define START_TIMEOUT_MS 2000

#define MAX_INTERFACES 16



/* Data + checksum */
#define MAX_PKT_SIZE (0xffff + 4)

#define DCHID_CHANNEL_CMD 0x11
#define DCHID_CHANNEL_REPORT 0x12

struct dchid_hdr {
	u8 hdr_len;
	u8 channel;
	u16 length;
	u8 seq;
	u8 iface;
	u16 pad;
} __packed;

#define IFACE_COMM 0

#define FLAGS_GROUP GENMASK(7, 6)
#define FLAGS_REQ GENMASK(5, 0)

#define REQ_SET_REPORT 0
#define REQ_GET_REPORT 1

struct dchid_subhdr {
	u8 flags;
	u8 unk;
	u16 length;
	u32 retcode;
} __packed;

#define EVENT_GPIO_CMD	0xa0
#define EVENT_INIT	0xf0
#define EVENT_READY	0xf1

struct dchid_init_hdr {
	u8 type;
	u8 unk1;
	u8 unk2;
	u8 iface;
	char name[16];
	u8 more_packets;
	u8 unkpad;
} __packed;

#define INIT_HID_DESCRIPTOR	0
#define INIT_GPIO_REQUEST	1
#define INIT_TERMINATOR		2
#define INIT_PRODUCT_NAME	7

#define CMD_RESET_INTERFACE 0x40
#define CMD_SEND_FIRMWARE 0x95
#define CMD_ENABLE_INTERFACE 0xb4
#define CMD_ACK_GPIO_CMD 0xa1
#define CMD_INIT_SHARED_BUFFER 0x91
#define CMD_POWER_REQUEST 0x41
#define CMD_QUERY_POWER 0x42

#define DCHID_POWER_METHOD_DIRECT 1
#define DCHID_POWER_METHOD_TRANSACTIONAL 2
#define DCHID_POWER_PHASE_WILL_CHANGE 0
#define DCHID_POWER_PHASE_HAS_CHANGED 1

/*
 * AHTPowerState ordinals, as used by AppleHIDTransportInterface::setPowerGated*
 * in the shipping macOS AppleHIDTransport kext: 0 off, 1 sleep, 2 on, 3 reset.
 */
#define DCHID_POWER_STATE_OFF 0
#define DCHID_POWER_STATE_ON 2

/*
 * J700's shipping MTP_SYS multi-touch personality carries the host half of the
 * power transition as two AppleHIDTransport function sequences.
 *
 * power-sequence is the legacy compact encoding: a NUL-terminated function name
 * followed by enable-delay-ms, disable-delay-ms, enable-type and disable-type
 * bytes.  J700 ships
 *
 *   66 75 6e 63 74 69 6f 6e 2d 61 66 65 2d 72 65 73 65 74 00 | 00 32 00 01
 *   "function-afe-reset"                                       enable  Low,  0ms
 *                                                              disable High, 50ms
 *
 * reset-sequence is the modern dictionary form and needs no field-order
 * assumption at all:
 *
 *   FunctionName        = function-ipd-pwr-seq-control
 *   EnableFunctionType  = Low   EnableDelayUs  = 100000
 *   DisableFunctionType = High  DisableDelayUs = 0
 *
 * AHTFunctionInfo's legacy type table is Low=0, High=1, Input=2, AltFunc=3,
 * Config0=4, Config1=5, Read=6.  The compact form's field order is confirmed
 * against a machine whose ADT ships the same encoding: j493's touch-bar node has
 *
 *   function-display_power_enable \0 00 00 01 00
 *   function-clock_32KHz_enable   \0 0a 00 03 00
 *   function-reset                \0 01 01 01 00
 *
 * and only the enable-first order reads sensibly there, since it puts AltFunc on
 * the enable half of a 32 kHz clock output and High on the release half of an
 * active-low reset.  Applied to J700 that gives Low as the AFE's enable level,
 * which is why this driver drives the raw line and deliberately does not consult
 * the DT active-low flag - J700's line is not active low.
 *
 * AppleHIDTransportInterface runs power-sequence as its chip-power step and
 * reset-sequence as its reset step, so despite the function names the AFE supply
 * on J700 is the pcIO line (SMC gP13) and the AFE reset is the pmIP key.  Power
 * on therefore raises the supply and then releases the reset, waiting the
 * sequence's 100 ms before telling MTP the platform has changed; power off
 * asserts the reset and then removes the supply, holding it for 50 ms.
 */
#define DCHID_AFE_RESET_ENABLE_LEVEL 0
#define DCHID_AFE_RESET_ENABLE_DELAY_US 0
#define DCHID_AFE_RESET_DISABLE_LEVEL 1
#define DCHID_AFE_RESET_DISABLE_DELAY_US 50000

#define SMC_KEY_IPD_PWR_SEQ SMC_KEY(pmIP)
#define DCHID_IPD_RESET_ENABLE_VALUE 0
#define DCHID_IPD_RESET_ENABLE_DELAY_US 100000
#define DCHID_IPD_RESET_DISABLE_VALUE 1
#define DCHID_IPD_RESET_DISABLE_DELAY_US 0

static bool ipd_reset_sequence = true;
module_param(ipd_reset_sequence, bool, 0644);
MODULE_PARM_DESC(ipd_reset_sequence,
		 "Run the J700 multi-touch reset-sequence (function-ipd-pwr-seq-control) during power transitions");

static bool ipd_reset_write = true;
module_param(ipd_reset_write, bool, 0644);
MODULE_PARM_DESC(ipd_reset_write,
		 "Write the pmIP SMC key as part of the reset-sequence; with 0 only its delay is applied");

/*
 * The enable level is the one inference in the contract above that rests on
 * another machine's ADT rather than on J700's own bytes, and the boot chain
 * hands the line over at the disable level, so there is no inherited state to
 * check it against.  Keep both levels switchable so the polarity can be settled
 * by measurement: re-opening the multi-touch node re-runs the whole start
 * sequence, so both can be tried without rebuilding or rebooting.
 */
static int afe_enable_level = DCHID_AFE_RESET_ENABLE_LEVEL;
module_param(afe_enable_level, int, 0644);
MODULE_PARM_DESC(afe_enable_level,
		 "Raw gP13 level driven when the J700 AFE is powered on");

static int afe_disable_level = DCHID_AFE_RESET_DISABLE_LEVEL;
module_param(afe_disable_level, int, 0644);
MODULE_PARM_DESC(afe_disable_level,
		 "Raw gP13 level driven when the J700 AFE is powered off");

static bool power_query = true;
module_param(power_query, bool, 0644);
MODULE_PARM_DESC(power_query,
		 "Ask MTP for the multi-touch power state around the start sequence");

/*
 * Read at interface enumeration, long before anything in userspace can change
 * it, so this one has to be set through modprobe.d or the kernel command line
 * rather than sysfs.
 */
static bool power_request = true;
module_param(power_request, bool, 0644);
MODULE_PARM_DESC(power_request,
		 "Send the 0x41 power request after the 0x40 transaction leaves the interface off");

static bool enable_mtp_iface = true;
module_param(enable_mtp_iface, bool, 0444);
MODULE_PARM_DESC(enable_mtp_iface,
		 "Enable the A18 \"mtp\" control interface that has no DT node (read at enumeration: set it through modprobe.d or the kernel command line, not sysfs)");

/*
 * Off by default.  Registering a shared buffer is a state-changing operation
 * that has previously wedged the comm channel, so it is opted into for a single
 * experiment rather than run on every boot; re-opening the multi-touch node
 * re-runs the start sequence, so it can be armed and fired without a reboot,
 * and a reboot always clears it.
 */
/*
 * MTP's own _mgr_esd_recovery gives the AFE three attempts before it reports
 * "Failed to boot Cumulus in 3 recovery attempts. Giving up", so the host has
 * no reason to keep asking after that many either.  Writable so that raising it
 * re-arms a start that has already given up, which is how a changed parameter
 * gets retried without a reboot.
 */
static int max_start_attempts = 3;
module_param(max_start_attempts, int, 0644);
MODULE_PARM_DESC(max_start_attempts,
		 "How many times to attempt an interface start before giving up");

static bool shared_buffer;
module_param(shared_buffer, bool, 0644);
MODULE_PARM_DESC(shared_buffer,
		 "Register a DRAM shared buffer with MTP (opcode 0x91) before uploading firmware");

static unsigned int shared_buffer_size = 0x4000;
module_param(shared_buffer_size, uint, 0644);
MODULE_PARM_DESC(shared_buffer_size,
		 "Size of the shared buffer registered with opcode 0x91");

static int shared_buffer_unk1;
module_param(shared_buffer_unk1, int, 0644);
MODULE_PARM_DESC(shared_buffer_unk1, "Second byte of the opcode 0x91 message");

static int shared_buffer_unk2;
module_param(shared_buffer_unk2, int, 0644);
MODULE_PARM_DESC(shared_buffer_unk2, "Third byte of the opcode 0x91 message");

struct dchid_init_block_hdr {
	u16 type;
	u16 length;
} __packed;

#define MAX_GPIO_NAME 32

struct dchid_gpio_request {
	u16 unk;
	u16 id;
	char name[MAX_GPIO_NAME];
} __packed;

struct dchid_gpio_cmd {
	u8 type;
	u8 iface;
	u8 gpio;
	u8 unk;
	u8 cmd;
} __packed;

struct dchid_gpio_ack {
	u8 type;
	u32 retcode;
	u8 cmd[];
} __packed;

#define STM_REPORT_ID		0x10
#define STM_REPORT_SERIAL	0x11
#define STM_REPORT_KEYBTYPE	0x14

struct dchid_stm_id {
	u8 unk;
	u16 vendor_id;
	u16 product_id;
	u16 version_number;
	u8 unk2;
	u8 unk3;
	u8 keyboard_type;
	u8 serial_length;
	/* Serial follows, but we grab it with a different report. */
} __packed;

#define FW_MAGIC 0x46444948
#define FW_VER 1

struct fw_header {
	u32 magic;
	u32 version;
	u32 hdr_length;
	u32 data_length;
	u32 iface_offset;
} __packed;

struct dchid_work {
	struct work_struct work;
	struct dchid_iface *iface;

	struct dchid_hdr hdr;
	u8 data[];
};

struct dchid_iface {
	struct dockchannel_hid *dchid;
	struct hid_device *hid;
	struct workqueue_struct *wq;

	bool creating;
	struct work_struct create_work;

	int index;
	const char *name;
	const struct device_node *of_node;

	uint8_t tx_seq;
	bool deferred;
	bool starting;
	bool open;
	struct completion ready;

	void *hid_desc;
	size_t hid_desc_len;

	struct gpio_desc *gpio;
	char gpio_name[MAX_GPIO_NAME];
	int gpio_id;

	/* Firmware staged for this interface, allocated once and reused. */
	void *fw;
	size_t fw_size;
	void *fw_dma_buf;
	dma_addr_t fw_dma_addr;
	unsigned int start_attempts;

	/* Host-owned AFE supply line (J700 function-afe-reset). */
	struct gpio_desc *afe_reset;
	/* The SMC behind that line, which also owns the pmIP reset key. */
	struct apple_smc *smc;

	struct mutex out_mutex;
	u32 out_flags;
	int out_report;
	u32 retcode;
	void *resp_buf;
	size_t resp_size;
	struct completion out_complete;

	u32 keyboard_layout_id;
};

struct dockchannel_hid {
	struct device *dev;
	struct dockchannel *dc;
	struct device_link *helper_link;

	bool id_ready;
	struct dchid_stm_id device_id;
	char serial[64];

	struct dchid_iface *comm;
	struct dchid_iface *ifaces[MAX_INTERFACES];

	u8 pkt_buf[MAX_PKT_SIZE];

	/* DRAM shared buffer registered with MTP, if any. */
	void *shmem;
	unsigned int shmem_size;

	/* Workqueue to asynchronously create HID devices */
	struct workqueue_struct *new_iface_wq;
};

static ssize_t apple_layout_id_show(struct device *dev,
				       struct device_attribute *attr,
				       char *buf)
{
	struct hid_device *hdev = to_hid_device(dev);
	struct dchid_iface *iface = hdev->driver_data;

	return scnprintf(buf, PAGE_SIZE, "%d\n", iface->keyboard_layout_id);
}

static DEVICE_ATTR_RO(apple_layout_id);

static struct dchid_iface *
dchid_get_interface(struct dockchannel_hid *dchid, int index, const char *name)
{
	struct dchid_iface *iface;

	if (index >= MAX_INTERFACES) {
		dev_err(dchid->dev, "Interface index %d out of range\n", index);
		return NULL;
	}

	if (dchid->ifaces[index])
		return dchid->ifaces[index];

	iface = devm_kzalloc(dchid->dev, sizeof(struct dchid_iface), GFP_KERNEL);
	if (!iface)
		return NULL;

	iface->index = index;
	iface->name = devm_kstrdup(dchid->dev, name, GFP_KERNEL);
	iface->dchid = dchid;
	iface->out_report= -1;
	init_completion(&iface->out_complete);
	init_completion(&iface->ready);
	mutex_init(&iface->out_mutex);
	iface->wq = alloc_ordered_workqueue("dchid-%s", WQ_MEM_RECLAIM, iface->name);
	if (!iface->wq)
		return NULL;

	/* Comm is not a HID subdevice */
	if (!strcmp(name, "comm")) {
		dchid->ifaces[index] = iface;
		return iface;
	}

	iface->of_node = of_get_child_by_name(dchid->dev->of_node, name);
	if (!iface->of_node) {
		/*
		 * A18 MTP advertises a fourth interface named "mtp" alongside
		 * comm, multi-touch and keyboard, and completes its own
		 * enumeration for it ("MTP interface complete enumeration:
		 * SENT").  No Apple laptop DT describes it, because no earlier
		 * machine offers it, so this driver dropped it on the floor and
		 * never sent it CMD_ENABLE_INTERFACE.  Keep it as a
		 * control-only interface - no OF node, no HID device - so it
		 * can at least be enabled.
		 */
		if (enable_mtp_iface &&
		    of_property_read_bool(dchid->dev->of_node, "apple,no-stm") &&
		    !strcmp(name, "mtp")) {
			dev_info(dchid->dev,
				 "Keeping control-only interface %s (no OF node)\n",
				 name);
			dchid->ifaces[index] = iface;
			return iface;
		}

		dev_warn(dchid->dev, "No OF node for subdevice %s, ignoring.", name);
		return NULL;
	}

	dchid->ifaces[index] = iface;
	return iface;
}

static u32 dchid_checksum(void *p, size_t length)
{
	u32 sum = 0;

	while (length >= 4) {
		sum += get_unaligned_le32(p);
		p += 4;
		length -= 4;
	}

	WARN_ON_ONCE(length);
	return sum;
}

static int dchid_send(struct dchid_iface *iface, u32 flags, void *msg, size_t size)
{
	u32 checksum = 0xffffffff;
	size_t wsize = round_down(size, 4);
	size_t tsize = size - wsize;
	int ret;
	struct {
		struct dchid_hdr hdr;
		struct dchid_subhdr sub;
	} __packed h;

	memset(&h, 0, sizeof(h));
	h.hdr.hdr_len = sizeof(h.hdr);
	h.hdr.channel = DCHID_CHANNEL_CMD;
	h.hdr.length = round_up(size, 4) + sizeof(h.sub);
	h.hdr.seq = iface->tx_seq;
	h.hdr.iface = iface->index;
	h.sub.flags = flags;
	h.sub.length = size;

	ret = dockchannel_send(iface->dchid->dc, &h, sizeof(h));
	if (ret < 0)
		return ret;
	checksum -= dchid_checksum(&h, sizeof(h));

	ret = dockchannel_send(iface->dchid->dc, msg, wsize);
	if (ret < 0)
		return ret;
	checksum -= dchid_checksum(msg, wsize);

	if (tsize) {
		u8 tail[4] = {0, 0, 0, 0};

		memcpy(tail, msg + wsize, tsize);
		ret = dockchannel_send(iface->dchid->dc, tail, sizeof(tail));
		if (ret < 0)
			return ret;
		checksum -= dchid_checksum(tail, sizeof(tail));
	}

	ret = dockchannel_send(iface->dchid->dc, &checksum, sizeof(checksum));
	if (ret < 0)
		return ret;

	return 0;
}

/*
 * Once a shared FIFO is registered, MTP stops acknowledging comm commands over
 * DockChannel, which is consistent with the replies moving into the ring the
 * host is then expected to drain.  Show the ring's head and tail and the first
 * bytes of its data area whenever a command goes unanswered, so the framing can
 * be read off real traffic rather than guessed at.
 */
static void dchid_dump_shmem(struct dockchannel_hid *dchid)
{
	u32 *hdr = dchid->shmem;

	if (!dchid->shmem)
		return;

	dev_info(dchid->dev,
		 "Shared FIFO: producer=%u consumer=%u capacity=%u data=%*ph\n",
		 READ_ONCE(hdr[0]), READ_ONCE(hdr[1]), dchid->shmem_size - 8,
		 64, (u8 *)dchid->shmem + 8);
}

static int dchid_cmd(struct dchid_iface *iface, u32 type, u32 req,
		     void *data, size_t size, void *resp_buf, size_t resp_size)
{
	int ret;
	int report_id = *(u8*)data;

	mutex_lock(&iface->out_mutex);

	WARN_ON(iface->out_report != -1);
	iface->out_report = report_id;
	iface->out_flags = FIELD_PREP(FLAGS_GROUP, type) | FIELD_PREP(FLAGS_REQ, req);
	iface->resp_buf = resp_buf;
	iface->resp_size = resp_size;
	reinit_completion(&iface->out_complete);

	ret = dchid_send(iface, iface->out_flags, data, size);
	if (ret < 0)
		goto done;

	if (!wait_for_completion_timeout(&iface->out_complete, msecs_to_jiffies(COMMAND_TIMEOUT_MS))) {
		dev_err(iface->dchid->dev, "output report 0x%x to iface  %d (%s) timed out\n",
			report_id, iface->index, iface->name);
		dchid_dump_shmem(iface->dchid);
		ret = -ETIMEDOUT;
		goto done;
	}

	ret = iface->resp_size;
	if (iface->retcode) {
		dev_err(iface->dchid->dev,
			"output report 0x%x to iface %d (%s) failed with err 0x%x\n",
			report_id, iface->index, iface->name, iface->retcode);
		ret = -EIO;
	}

done:
	iface->tx_seq++;
	iface->out_report = -1;
	iface->out_flags = 0;
	iface->resp_buf = NULL;
	iface->resp_size = 0;
	mutex_unlock(&iface->out_mutex);
	return ret;
}

static int dchid_comm_cmd(struct dockchannel_hid *dchid, void *cmd, size_t size)
{
	return dchid_cmd(dchid->comm, HID_FEATURE_REPORT, REQ_SET_REPORT, cmd, size, NULL, 0);
}

static int dchid_enable_interface(struct dchid_iface *iface)
{
	u8 msg[] = { CMD_ENABLE_INTERFACE, iface->index };

	return dchid_comm_cmd(iface->dchid, msg, sizeof(msg));
}

/*
 * Find the SMC that owns the AFE supply line.
 *
 * The reset half of the J700 contract is keyW('pmIP'), a plain SMC key rather
 * than a GPIO, so there is no gpiod for it and no separate DT phandle to follow.
 * Both functions live on the same controller by construction - the ADT gives
 * them the same provider - so the SMC is reached through the gpiochip that
 * supplied apple,afe-reset-gpios: its parent is the macsmc-gpio MFD child and
 * that device's parent is the SMC itself.  The compatible is checked before the
 * drvdata is trusted, so a DT that wires the line to some other controller fails
 * closed instead of handing back a foreign pointer.
 */
static struct apple_smc *dchid_afe_smc(struct dchid_iface *iface)
{
	struct gpio_chip *gc;
	struct device *smc_dev;

	if (iface->smc)
		return iface->smc;

	if (!iface->afe_reset)
		return NULL;

	gc = gpiod_to_chip(iface->afe_reset);
	if (!gc || !gc->parent || !gc->parent->parent)
		return NULL;

	smc_dev = gc->parent->parent;
	if (!device_is_compatible(smc_dev, "apple,smc"))
		return NULL;

	iface->smc = dev_get_drvdata(smc_dev);
	return iface->smc;
}

static bool dchid_uses_transactional_power(struct dchid_iface *iface)
{
	const char *firmware_name;

	return of_property_read_bool(iface->dchid->dev->of_node, "apple,no-stm") &&
	       !strcmp(iface->name, "multi-touch") &&
	       !of_property_read_string(iface->of_node, "firmware-name",
					&firmware_name) &&
	       !strcmp(firmware_name, "apple/tpmtfw-j700.bin");
}

/*
 * Run the power-sequence half of a multi-touch power transition.
 *
 * On J700 this line is not an MTP-requested GPIO: MTP sends no INIT_GPIO_REQUEST
 * for multi-touch and its firmware does not implement CMD_ACK_GPIO_CMD, because
 * the line is an SMC platform function - function-afe-reset is
 * pKW8('pcIO', 0x10000, 0x130000), i.e. SMC GPIO index 0x13, key gP13 - that
 * only the host can drive.  The index decode is the same one already qualified
 * for this machine's MT7932: its function-reg_on is pKW8('pcIO', 0x800000,
 * 0x80000) and works through smc_gpio line 8.
 *
 * The levels and delays come from the shipping MTP_SYS personality documented
 * above; both are absolute line levels, so the raw accessor is used and the DT
 * active-low flag is deliberately not consulted.  The line is read back on both
 * sides of the write so a boot log shows what the platform actually did rather
 * than only what was asked for.
 */
static int dchid_afe_power_sequence(struct dchid_iface *iface, bool enable)
{
	int level = !!(enable ? afe_enable_level : afe_disable_level);
	u32 delay_us = enable ? DCHID_AFE_RESET_ENABLE_DELAY_US
			      : DCHID_AFE_RESET_DISABLE_DELAY_US;
	int before, after, ret;

	if (!iface->afe_reset) {
		dev_err(iface->dchid->dev, "No AFE supply line for %s\n",
			iface->name);
		return -ENODEV;
	}

	before = gpiod_get_raw_value_cansleep(iface->afe_reset);

	ret = gpiod_direction_output_raw(iface->afe_reset, level);
	if (ret) {
		dev_err(iface->dchid->dev,
			"Failed to drive the %s AFE supply line to %d: %d\n",
			iface->name, level, ret);
		return ret;
	}

	after = gpiod_get_raw_value_cansleep(iface->afe_reset);

	dev_info(iface->dchid->dev,
		 "AFE power sequence: %s enable=%d gP13=%d before=%d after=%d delay_us=%u\n",
		 iface->name, enable, level, before, after, delay_us);

	if (delay_us)
		fsleep(delay_us);

	return 0;
}

/*
 * Run the reset-sequence half of a multi-touch power transition.
 *
 * function-ipd-pwr-seq-control is keyW('pmIP'), so it is an ordinary SMC key
 * write rather than a GPIO operation.  Nothing in Linux has ever driven it, and
 * without it the Cumulus AFE is left holding whatever reset state the boot chain
 * gave it while MTP runs its HBPP bootload against it - which is exactly the
 * "AFE Chip Boot Failure for HBPP[0]: No HINT_L" the firmware reports.
 *
 * The write is intentionally not fatal.  A boot that cannot reach the SMC still
 * applies the sequence's delay and still produces MTP's verdict, and the log
 * line below says precisely which of the two happened, so one boot distinguishes
 * "the pmIP edge was missing" from "only the settle was missing".
 */
static void dchid_ipd_reset_sequence(struct dchid_iface *iface, bool enable)
{
	u8 value = enable ? DCHID_IPD_RESET_ENABLE_VALUE
			  : DCHID_IPD_RESET_DISABLE_VALUE;
	u32 delay_us = enable ? DCHID_IPD_RESET_ENABLE_DELAY_US
			      : DCHID_IPD_RESET_DISABLE_DELAY_US;
	int before = -1, after = -1, ret = 0;
	const char *how = "no-smc";
	struct apple_smc *smc;
	u8 read_value;

	if (!ipd_reset_sequence)
		return;

	smc = dchid_afe_smc(iface);
	if (smc) {
		if (!apple_smc_read_u8(smc, SMC_KEY_IPD_PWR_SEQ, &read_value))
			before = read_value;

		if (!ipd_reset_write) {
			how = "read-only";
		} else {
			ret = apple_smc_write_u8(smc, SMC_KEY_IPD_PWR_SEQ, value);
			how = ret ? "write-failed" : "written";
			if (!apple_smc_read_u8(smc, SMC_KEY_IPD_PWR_SEQ,
					       &read_value))
				after = read_value;
		}
	}

	dev_info(iface->dchid->dev,
		 "IPD reset sequence: %s enable=%d pmIP=%s value=%u before=%d after=%d delay_us=%u ret=%d\n",
		 iface->name, enable, how, value, before, after, delay_us, ret);

	if (delay_us)
		fsleep(delay_us);
}

/*
 * AppleHIDTransportInterface runs power-sequence as its chip-power step and
 * reset-sequence as its reset step, so the supply is raised before the reset is
 * released and asserted again before the supply is removed.
 */
static int dchid_platform_power_sequence(struct dchid_iface *iface, int state)
{
	int ret;

	switch (state) {
	case DCHID_POWER_STATE_OFF:
		dchid_ipd_reset_sequence(iface, false);
		return dchid_afe_power_sequence(iface, false);
	case DCHID_POWER_STATE_ON:
		ret = dchid_afe_power_sequence(iface, true);
		if (ret < 0)
			return ret;
		dchid_ipd_reset_sequence(iface, true);
		return 0;
	default:
		return 0;
	}
}

static int dchid_reset_interface(struct dchid_iface *iface, int state)
{
	u8 msg[] = {
		CMD_RESET_INTERFACE,
		DCHID_POWER_METHOD_DIRECT,
		iface->index,
		state,
		DCHID_POWER_PHASE_WILL_CHANGE,
		0, 0, 0, 0,
	};
	ktime_t started, settled;
	int ret;

	if (!dchid_uses_transactional_power(iface))
		return dchid_comm_cmd(iface->dchid, msg, 4);

	msg[1] = DCHID_POWER_METHOD_TRANSACTIONAL;
	ret = dchid_comm_cmd(iface->dchid, msg, sizeof(msg));
	if (ret < 0)
		return ret;

	/*
	 * The platform sequences belong between the two phases: will-change lets
	 * MTP quiesce the interface, the host then puts the AFE into the new
	 * state, and has-changed tells MTP the platform is ready, which is when
	 * it runs its own AFE bootload.  How much settle actually elapsed before
	 * that bootload is the whole question here, so it is measured rather
	 * than assumed.
	 */
	started = ktime_get();
	ret = dchid_platform_power_sequence(iface, state);
	if (ret < 0)
		return ret;
	settled = ktime_get();

	msg[4] = DCHID_POWER_PHASE_HAS_CHANGED;
	ret = dchid_comm_cmd(iface->dchid, msg, sizeof(msg));

	dev_info(iface->dchid->dev,
		 "Power transition %s state=%d: platform=%lldus has-changed=%lldus ret=%d\n",
		 iface->name, state, ktime_us_delta(settled, started),
		 ktime_us_delta(ktime_get(), settled), ret);

	return ret;
}

static int dchid_send_firmware(struct dchid_iface *iface, void *firmware, size_t size)
{
	struct {
		u8 cmd;
		u8 unk1;
		u8 unk2;
		u8 iface;
		u64 addr;
		u32 size;
	} __packed msg = {
		.cmd = CMD_SEND_FIRMWARE,
		.unk1 = 2,
		.unk2 = 0,
		.iface = iface->index,
		.size = size,
	};
	/*
	 * A start can be retried, so this buffer is allocated once and reused.
	 * It used to be a fresh dmam_alloc_coherent() per attempt, which leaked
	 * the whole image every time an interface failed to come up.
	 */
	if (!iface->fw_dma_buf) {
		iface->fw_dma_buf = dmam_alloc_coherent(iface->dchid->dev, size,
						        &iface->fw_dma_addr,
						        GFP_KERNEL);
		if (IS_ERR_OR_NULL(iface->fw_dma_buf)) {
			void *err = iface->fw_dma_buf;

			iface->fw_dma_buf = NULL;
			return err ? PTR_ERR(err) : -ENOMEM;
		}
	}

	msg.addr = iface->fw_dma_addr;
	memcpy(iface->fw_dma_buf, firmware, size);
	wmb();

	return dchid_comm_cmd(iface->dchid, &msg, sizeof(msg));
}

static int dchid_get_firmware(struct dchid_iface *iface, void **firmware, size_t *size)
{
	int ret;
	const char *fw_name;
	const struct firmware *fw;
	struct fw_header *hdr;
	u8 *fw_data;

	ret = of_property_read_string(iface->of_node, "firmware-name", &fw_name);
	if (ret) {
		/* Firmware is only for some devices */
		*firmware = NULL;
		*size = 0;
		return 0;
	}

	/* Already parsed by an earlier start attempt. */
	if (iface->fw) {
		*firmware = iface->fw;
		*size = iface->fw_size;
		return 0;
	}

	ret = request_firmware(&fw, fw_name, iface->dchid->dev);
	if (ret)
		return ret;

	hdr = (struct fw_header *)fw->data;

	if (hdr->magic != FW_MAGIC || hdr->version != FW_VER ||
		hdr->hdr_length < sizeof(*hdr) || hdr->hdr_length > fw->size ||
		(hdr->hdr_length + (size_t)hdr->data_length) > fw->size ||
		hdr->iface_offset >= hdr->data_length) {
		dev_warn(iface->dchid->dev, "%s: invalid firmware header\n",
			 fw_name);
		ret = -EINVAL;
		goto done;
	}

	fw_data = devm_kmemdup(iface->dchid->dev, fw->data + hdr->hdr_length,
			       hdr->data_length, GFP_KERNEL);
	if (!fw_data) {
		ret = -ENOMEM;
		goto done;
	}

	if (hdr->iface_offset)
		fw_data[hdr->iface_offset] = iface->index;

	iface->fw = fw_data;
	iface->fw_size = hdr->data_length;
	*firmware = fw_data;
	*size = hdr->data_length;

done:
	release_firmware(fw);
	return ret;
}

static int dchid_request_gpio(struct dchid_iface *iface)
{
	char prop_name[MAX_GPIO_NAME + 16];

	if (iface->gpio)
		return 0;

	dev_info(iface->dchid->dev, "Requesting GPIO %s#%d: %s\n",
		 iface->name, iface->gpio_id, iface->gpio_name);

	snprintf(prop_name, sizeof(prop_name), "apple,%s", iface->gpio_name);

	iface->gpio = devm_gpiod_get_index(iface->dchid->dev, prop_name, 0, GPIOD_OUT_LOW);

	if (IS_ERR_OR_NULL(iface->gpio)) {
		dev_err(iface->dchid->dev, "Failed to request GPIO %s-gpios\n", prop_name);
		iface->gpio = NULL;
		return -1;
	}

	return 0;
}

/*
 * Report the platform state the boot chain handed over, before anything in Linux
 * touches it.  At this point MTP still reports multi-touch as unpowered, so the
 * values logged here are the inherited "off" state of both halves of the
 * contract and say whether either write below actually changes anything.
 */
static void dchid_report_platform_state(struct dchid_iface *iface)
{
	struct apple_smc_key_info info;
	struct apple_smc *smc;
	u8 value;
	int ret;

	dev_info(iface->dchid->dev,
		 "AFE supply line claimed for %s: direction=%d raw=%d\n",
		 iface->name, gpiod_get_direction(iface->afe_reset),
		 gpiod_get_raw_value_cansleep(iface->afe_reset));

	smc = dchid_afe_smc(iface);
	if (!smc) {
		dev_warn(iface->dchid->dev,
			 "No SMC behind the %s AFE supply line; the reset-sequence cannot run\n",
			 iface->name);
		return;
	}

	ret = apple_smc_get_key_info(smc, SMC_KEY_IPD_PWR_SEQ, &info);
	if (ret) {
		dev_warn(iface->dchid->dev,
			 "SMC key pmIP is not present for %s: %d\n",
			 iface->name, ret);
		return;
	}

	ret = apple_smc_read_u8(smc, SMC_KEY_IPD_PWR_SEQ, &value);
	dev_info(iface->dchid->dev,
		 "IPD reset key pmIP for %s: type=%p4ch size=%u flags=0x%02x value=%d\n",
		 iface->name, &info.type_code, info.size, info.flags,
		 ret ? ret : value);
}

/*
 * Claim the host-owned AFE supply line described by the parent node's
 * apple,afe-reset-gpios.  MTP never asks for it on J700, so it cannot come from
 * an INIT_GPIO_REQUEST the way it does on the STM platforms.  GPIOD_ASIS keeps
 * the line untouched until dchid_afe_power_sequence() drives it as part of a
 * real power transition.
 */
static int dchid_request_afe_reset(struct dchid_iface *iface)
{
	struct gpio_desc *gpio;

	if (iface->afe_reset)
		return 0;

	gpio = devm_gpiod_get(iface->dchid->dev, "apple,afe-reset", GPIOD_ASIS);
	if (IS_ERR(gpio)) {
		dev_err(iface->dchid->dev,
			"Failed to request apple,afe-reset-gpios for %s: %ld\n",
			iface->name, PTR_ERR(gpio));
		return PTR_ERR(gpio);
	}

	iface->afe_reset = gpio;
	dchid_report_platform_state(iface);
	return 0;
}

static bool dchid_adopt_preinitialized_ipd_functions(struct dchid_iface *iface)
{
	/*
	 * J700 no longer describes the trackpad reset as an SMC GPIO.  Its ADT
	 * uses an eight-byte pcIO platform-key write plus a separate pmIP power
	 * sequence, both of which iBoot has already applied before the live MTP
	 * handoff.  Keep this adoption path deliberately narrower than the GPIO
	 * fallback: an unknown interface or request must still fail closed.
	 */
	return dchid_uses_transactional_power(iface) &&
	       of_property_read_bool(iface->of_node,
				     "apple,adopt-preinitialized-ipd-functions") &&
	       !strcmp(iface->gpio_name, "afe-reset");
}

/*
 * Ask MTP what power state it thinks the interface is in.
 *
 * Opcode 0x42 is the power query of the 0x41/0x42 pair this firmware exposes
 * for multi-touch; it echoes the request plus a state byte and was already
 * exercised on this machine without side effects.  It answers the one question
 * the driver otherwise cannot: our 0x40 transaction is acknowledged, but that
 * only proves MTP parsed it, not that MTP considers the interface powered.
 */
static void dchid_query_power_state(struct dchid_iface *iface, const char *when)
{
	u8 msg[] = { CMD_QUERY_POWER, DCHID_POWER_METHOD_DIRECT, iface->index, 0 };
	u8 resp[16] = {};
	int ret;

	if (!power_query || !dchid_uses_transactional_power(iface))
		return;

	ret = dchid_cmd(iface->dchid->comm, HID_FEATURE_REPORT, REQ_SET_REPORT,
			msg, sizeof(msg), resp, sizeof(resp));

	dev_info(iface->dchid->dev, "Power query %s %s: ret=%d resp=%*ph\n",
		 iface->name, when, ret, (int)sizeof(resp), resp);
}

/*
 * Ask MTP to power the interface on with the other half of its power pair.
 *
 * The 0x40 transaction is acknowledged, yet 0x42 reports the interface in the
 * same state before and after it, so being parsed is not the same as being
 * applied.  Opcode 0x41 is this firmware's "Power request", takes exactly four
 * bytes and exists only for multi-touch, and is the one power opcode the driver
 * has never sent.  A firmware that refuses it just returns an error, which is
 * logged next to the 0x42 result either way.
 */
static void dchid_power_request(struct dchid_iface *iface, int state)
{
	u8 msg[] = {
		CMD_POWER_REQUEST,
		DCHID_POWER_METHOD_DIRECT,
		iface->index,
		state,
	};
	int ret;

	if (!power_request || !dchid_uses_transactional_power(iface))
		return;

	ret = dchid_comm_cmd(iface->dchid, msg, sizeof(msg));
	dev_info(iface->dchid->dev, "Power request %s state=%d: ret=%d\n",
		 iface->name, state, ret);
}

/*
 * Register a DRAM shared buffer with MTP.
 *
 * The wire format is m1n1's InitBufMsg, decoded from a macOS trace: the same
 * shape as CMD_SEND_FIRMWARE without its interface byte, fifteen bytes in all,
 * which matches the "exactly 15 bytes" this firmware's comm_cr.c enforces.  MTP
 * confirms a successful registration in its own log as
 *
 *   hal.c:170: Registering DRAM Shared Buffer #N. Addr=<addr>, Size=<size>
 *
 * with the address shifted right by four.
 *
 * This is the transport half A18 MTP appears to want and this driver has never
 * spoken: its bootloader gives up on the AFE within one RTKit log slot, with no
 * measurable elapsed time and no bus traffic, which is a refused precondition
 * rather than a part that failed to answer.
 */
static int dchid_register_shared_buffer(struct dchid_iface *iface)
{
	struct dockchannel_hid *dchid = iface->dchid;
	struct {
		u8 cmd;
		u8 unk1;
		u8 unk2;
		u64 addr;
		u32 size;
	} __packed msg = {
		.cmd = CMD_INIT_SHARED_BUFFER,
		.unk1 = shared_buffer_unk1,
		.unk2 = shared_buffer_unk2,
		.size = shared_buffer_size,
	};
	dma_addr_t addr;
	void *buf;
	int ret;

	BUILD_BUG_ON(sizeof(msg) != 15);

	if (!shared_buffer || dchid->shmem)
		return 0;

	buf = dmam_alloc_coherent(dchid->dev, shared_buffer_size, &addr,
				  GFP_KERNEL);
	if (IS_ERR_OR_NULL(buf)) {
		dev_err(dchid->dev, "Failed to allocate the %u-byte shared buffer\n",
			shared_buffer_size);
		return buf ? PTR_ERR(buf) : -ENOMEM;
	}

	/*
	 * The buffer starts with the FIFO's own head and tail, which MTP reads
	 * as soon as it owns the registration, so they have to be zero before
	 * the address goes out.  dma_alloc_coherent() already zeroes, but say so
	 * rather than depend on it.
	 */
	memset(buf, 0, sizeof(u32) * 2);
	msg.addr = addr;
	wmb();

	/*
	 * Claim it before sending, not after.  MTP registers the buffer as soon
	 * as it parses the message - it says so in its own log - but does not
	 * acknowledge the command, so waiting for an ack that never comes used
	 * to leave this unset and register a second buffer on the next attempt.
	 * Registering twice takes the coprocessor down:
	 *
	 *   hal.c:170: Registering DRAM Shared Buffer #1. Addr=..., Size=16384
	 *   RTKit: co-processor has crashed
	 *   ESR = 0x96000044  FAR = 0x1010108eb80
	 *
	 * so this must happen exactly once for the life of the device.
	 */
	dchid->shmem = buf;
	dchid->shmem_size = shared_buffer_size;

	ret = dchid_comm_cmd(dchid, &msg, sizeof(msg));
	dev_info(dchid->dev,
		 "Shared buffer: iova=%pad size=%u unk=%d,%d ret=%d\n",
		 &addr, shared_buffer_size, shared_buffer_unk1,
		 shared_buffer_unk2, ret);

	/*
	 * A missing acknowledgement is not a failure here: the registration has
	 * demonstrably taken effect by the time the command times out, and
	 * failing the start would only send us round again.
	 */
	return 0;
}

static int dchid_start_interface(struct dchid_iface *iface)
{
	void *fw;
	size_t size;
	int ret;

	if (iface->starting) {
		dev_warn(iface->dchid->dev, "Interface %s is already starting", iface->name);
		return -EINPROGRESS;
	}

	/*
	 * A timed-out start releases the latch so it can be retried, but the
	 * consumer of an input node reopens it as fast as it is refused, and
	 * every attempt re-uploads the image and power-cycles the AFE - which
	 * also disturbs the keyboard, since they share the SCM bus.  Stop after
	 * as many attempts as MTP's own _mgr_esd_recovery gives the part before
	 * it gives up, and then fail fast and quietly.
	 */
	if (iface->start_attempts >= max_start_attempts) {
		dev_warn_once(iface->dchid->dev,
			      "Interface %s failed to start %u times, not retrying\n",
			      iface->name, iface->start_attempts);
		return -EIO;
	}

	dev_info(iface->dchid->dev, "Starting interface %s (attempt %u)\n",
		 iface->name, ++iface->start_attempts);

	iface->starting = true;

	/* Look to see if we need firmware */
	ret = dchid_get_firmware(iface, &fw, &size);
	if (ret < 0)
		goto err;

	/*
	 * J700 owns the AFE reset itself instead of being asked for it, so the
	 * line has to be claimed before the power transitions below.
	 */
	if (dchid_uses_transactional_power(iface)) {
		ret = dchid_request_afe_reset(iface);
		if (ret < 0)
			goto err;
	}

	/* If we need a GPIO, make sure we have it. */
	if (iface->gpio_id) {
		if (dchid_adopt_preinitialized_ipd_functions(iface)) {
			dev_info(iface->dchid->dev,
				 "J700_MTP_IPD_ADOPT: interface=%s id=%u request=%s pcIO=live pmIP=live\n",
				 iface->name, iface->gpio_id, iface->gpio_name);
		} else {
			ret = dchid_request_gpio(iface);
			if (ret < 0)
				goto err;
		}
	}

	/* Only multi-touch has firmware */
	if (fw && size) {

		/*
		 * The buffer registration is a transport-level operation, so it
		 * goes in before the image the transport has to carry.
		 */
		ret = dchid_register_shared_buffer(iface);
		if (ret < 0)
			goto err;

		/* Send firmware to the device */
		dev_info(iface->dchid->dev, "Sending firmware for %s\n", iface->name);
		ret = dchid_send_firmware(iface, fw, size);
		if (ret < 0) {
			dev_err(iface->dchid->dev, "Failed to send %s firmwareS", iface->name);
			goto err;
		}

		/* After loading firmware, multi-touch needs a reset */
		dev_info(iface->dchid->dev, "Resetting %s\n", iface->name);
		dchid_query_power_state(iface, "before-cycle");
		ret = dchid_reset_interface(iface, DCHID_POWER_STATE_OFF);
		if (ret < 0)
			goto err;
		ret = dchid_reset_interface(iface, DCHID_POWER_STATE_ON);
		if (ret < 0)
			goto err;
		dchid_query_power_state(iface, "after-cycle");
		dchid_power_request(iface, DCHID_POWER_STATE_ON);
		dchid_query_power_state(iface, "after-request");
	}

	return 0;

err:
	iface->starting = false;
	return ret;
}

static int dchid_start(struct hid_device *hdev)
{
	struct dchid_iface *iface = hdev->driver_data;

	if (iface->keyboard_layout_id) {
		int ret = device_create_file(&hdev->dev, &dev_attr_apple_layout_id);
		if (ret) {
			dev_warn(iface->dchid->dev, "Failed to create apple_layout_id: %d", ret);
			iface->keyboard_layout_id = 0;
		}
	}

	return 0;
};

static void dchid_stop(struct hid_device *hdev)
{
	struct dchid_iface *iface = hdev->driver_data;

	if (iface->keyboard_layout_id)
		device_remove_file(&hdev->dev, &dev_attr_apple_layout_id);
}

static int dchid_open(struct hid_device *hdev)
{
	struct dchid_iface *iface = hdev->driver_data;
	int ret;

	if (!completion_done(&iface->ready)) {
		ret = dchid_start_interface(iface);
		if (ret < 0)
			return ret;

		if (!wait_for_completion_timeout(&iface->ready, msecs_to_jiffies(START_TIMEOUT_MS))) {
			dev_err(iface->dchid->dev, "iface %s start timed out\n", iface->name);
			/*
			 * Nothing else ever clears the latch that
			 * dchid_start_interface() sets, so a single timed-out
			 * start otherwise turns every later open into an
			 * "already starting" refusal for the lifetime of the
			 * boot.  Release it so the next open runs the start
			 * sequence again.
			 */
			iface->starting = false;
			return -ETIMEDOUT;
		}
	}

	iface->open = true;
	return 0;
}

static void dchid_close(struct hid_device *hdev)
{
	struct dchid_iface *iface = hdev->driver_data;

	iface->open = false;
}

static int dchid_parse(struct hid_device *hdev)
{
	struct dchid_iface *iface = hdev->driver_data;

	return hid_parse_report(hdev, iface->hid_desc, iface->hid_desc_len);
}

/* Note: buf excludes report number! For ease of fetching strings/etc. */
static int dchid_get_report_cmd(struct dchid_iface *iface, u8 reportnum, void *buf, size_t len)
{
	int ret = dchid_cmd(iface, HID_FEATURE_REPORT, REQ_GET_REPORT, &reportnum, 1, buf, len);

	return ret <= 0 ? ret : ret - 1;
}

/* Note: buf includes report number! */
static int dchid_set_report(struct dchid_iface *iface, void *buf, size_t len)
{
	return dchid_cmd(iface, HID_OUTPUT_REPORT, REQ_SET_REPORT, buf, len, NULL, 0);
}

static int dchid_raw_request(struct hid_device *hdev,
				unsigned char reportnum, __u8 *buf, size_t len,
				unsigned char rtype, int reqtype)
{
	struct dchid_iface *iface = hdev->driver_data;

	switch (reqtype) {
	case HID_REQ_GET_REPORT:
		buf[0] = reportnum;
		return dchid_cmd(iface, rtype, REQ_GET_REPORT, &reportnum, 1, buf + 1, len - 1);
	case HID_REQ_SET_REPORT:
		return dchid_set_report(iface, buf, len);
	default:
		return -EIO;
	}

	return 0;
}

static struct hid_ll_driver dchid_ll = {
	.start = &dchid_start,
	.stop = &dchid_stop,
	.open = &dchid_open,
	.close = &dchid_close,
	.parse = &dchid_parse,
	.raw_request = &dchid_raw_request,
};

static void dchid_create_interface_work(struct work_struct *ws)
{
	struct dchid_iface *iface = container_of(ws, struct dchid_iface, create_work);
	struct dockchannel_hid *dchid = iface->dchid;
	struct hid_device *hid;
	int ret;

	if (iface->hid) {
		dev_warn(dchid->dev, "Interface %s already created!\n",
			 iface->name);
		return;
	}

	dev_info(dchid->dev, "New interface %s\n", iface->name);

	/* Start the interface. This is not the entire init process, as firmware is loaded later on device open. */
	ret = dchid_enable_interface(iface);
	if (ret < 0) {
		dev_warn(dchid->dev, "Failed to enable %s: %d\n", iface->name, ret);
		return;
	}

	iface->deferred = false;

	hid = hid_allocate_device();
	if (IS_ERR(hid))
		return;

	snprintf(hid->name, sizeof(hid->name), "Apple MTP %s", iface->name);
	snprintf(hid->phys, sizeof(hid->phys), "%s.%d (%s)",
		 dev_name(dchid->dev), iface->index, iface->name);
	strscpy(hid->uniq, dchid->serial, sizeof(hid->uniq));

	hid->ll_driver = &dchid_ll;
	hid->bus = BUS_HOST;
	hid->vendor = dchid->device_id.vendor_id;
	hid->product = dchid->device_id.product_id;
	hid->version = dchid->device_id.version_number;
	hid->type = HID_TYPE_OTHER;
	if (!strcmp(iface->name, "multi-touch")) {
		hid->type = HID_TYPE_SPI_MOUSE;
	} else if (!strcmp(iface->name, "keyboard")) {
		u32 country_code = 0;

		hid->type = HID_TYPE_SPI_KEYBOARD;

		/*
		 * We have to get the country code from the device tree, since the
		 * device provides no reliable way to get this info.
		 */
		if (!of_property_read_u32(iface->of_node, "hid-country-code", &country_code))
			hid->country = country_code;

		of_property_read_u32(iface->of_node, "apple,keyboard-layout-id",
			&iface->keyboard_layout_id);
	}

	hid->dev.parent = iface->dchid->dev;
	hid->driver_data = iface;

	iface->hid = hid;

	ret = hid_add_device(hid);
	if (ret < 0) {
		iface->hid = NULL;
		hid_destroy_device(hid);
		dev_warn(iface->dchid->dev, "Failed to register hid device %s", iface->name);
	}
}

static int dchid_create_interface(struct dchid_iface *iface)
{
	if (iface->creating)
		return -EBUSY;

	iface->creating = true;
	INIT_WORK(&iface->create_work, dchid_create_interface_work);
	return queue_work(iface->dchid->new_iface_wq, &iface->create_work);
}

static void dchid_handle_descriptor(struct dchid_iface *iface, void *hid_desc, size_t desc_len)
{
	if (iface->hid) {
		dev_warn(iface->dchid->dev, "Tried to initialize already started interface %s!\n",
			 iface->name);
		return;
	}

	iface->hid_desc = devm_kmemdup(iface->dchid->dev, hid_desc, desc_len, GFP_KERNEL);
	if (!iface->hid_desc)
		return;

	iface->hid_desc_len = desc_len;
}

static void dchid_handle_ready(struct dockchannel_hid *dchid, void *data, size_t length)
{
	struct dchid_iface *iface;
	u8 *pkt = data;
	u8 index;
	int i, ret;

	if (length < 2) {
		dev_err(dchid->dev, "Bad length for ready message: %zu\n", length);
		return;
	}

	index = pkt[1];

	if (index >= MAX_INTERFACES) {
		dev_err(dchid->dev, "Got ready notification for bad iface %d\n", index);
		return;
	}

	iface = dchid->ifaces[index];
	if (!iface) {
		dev_err(dchid->dev, "Got ready notification for unknown iface %d\n", index);
		return;
	}

	dev_info(dchid->dev, "Interface %s is now ready\n", iface->name);
	iface->start_attempts = 0;
	complete_all(&iface->ready);

	/* When STM is ready, grab global device info */
	if (!strcmp(iface->name, "stm")) {
		ret = dchid_get_report_cmd(iface, STM_REPORT_ID, &dchid->device_id,
					   sizeof(dchid->device_id));
		if (ret < sizeof(dchid->device_id)) {
			dev_warn(iface->dchid->dev, "Failed to get device ID from STM!\n");
			/* Fake it and keep going. Things might still work... */
			memset(&dchid->device_id, 0, sizeof(dchid->device_id));
			dchid->device_id.vendor_id = HOST_VENDOR_ID_APPLE;
		}
		ret = dchid_get_report_cmd(iface, STM_REPORT_SERIAL, dchid->serial,
					   sizeof(dchid->serial) - 1);
		if (ret < 0) {
			dev_warn(iface->dchid->dev, "Failed to get serial from STM!\n");
			dchid->serial[0] = 0;
		}

		dchid->id_ready = true;
		for (i = 0; i < MAX_INTERFACES; i++) {
			if (!dchid->ifaces[i] || !dchid->ifaces[i]->deferred)
				continue;
			dchid_create_interface(dchid->ifaces[i]);
		}
	}
}

static void dchid_handle_init(struct dockchannel_hid *dchid, void *data, size_t length)
{
	struct dchid_init_hdr *hdr = data;
	struct dchid_iface *iface;
	struct dchid_init_block_hdr *blk;

	if (length < sizeof(*hdr))
		return;

	iface = dchid_get_interface(dchid, hdr->iface, hdr->name);
	if (!iface)
		return;

	data += sizeof(*hdr);
	length -= sizeof(*hdr);

	while (length >= sizeof(*blk)) {
		blk = data;
		data += sizeof(*blk);
		length -= sizeof(*blk);

		if (blk->length > length)
			break;

		switch (blk->type) {
		case INIT_HID_DESCRIPTOR:
			dchid_handle_descriptor(iface, data, blk->length);
			break;

		case INIT_GPIO_REQUEST: {
			struct dchid_gpio_request *req = data;

			if (sizeof(*req) > length)
				break;

			if (iface->gpio_id) {
				dev_err(dchid->dev,
					"Cannot request more than one GPIO per interface!\n");
				break;
			}

			strscpy(iface->gpio_name, req->name, MAX_GPIO_NAME);
			iface->gpio_id = req->id;
			dev_info(dchid->dev,
				 "Interface %s requested reset GPIO id=%u name=%s\n",
				 iface->name, iface->gpio_id, iface->gpio_name);
			break;
		}

		case INIT_TERMINATOR:
			break;

		case INIT_PRODUCT_NAME: {
			char *product = data;

			if (product[blk->length - 1] != 0) {
				dev_warn(dchid->dev, "Unterminated product name for %s\n",
					 iface->name);
			} else {
				dev_info(dchid->dev, "Product name for %s: %s\n",
					 iface->name, product);
			}
			break;
		}

		default:
			dev_warn(dchid->dev, "Unknown init packet %d for %s\n",
				 blk->type, iface->name);
			break;
		}

		data += blk->length;
		length -= blk->length;

		if (blk->type == INIT_TERMINATOR)
			break;
	}

	if (hdr->more_packets)
		return;

	/*
	 * A control-only interface has no HID descriptor and no OF node, so it
	 * gets the enable message on its own rather than through the HID
	 * device creation path.
	 */
	if (!iface->of_node) {
		int ret = dchid_enable_interface(iface);

		dev_info(dchid->dev, "Enabled control-only interface %s: %d\n",
			 iface->name, ret);
		return;
	}

	/* We need to enable STM first, since it'll give us the device IDs */
	if (iface->dchid->id_ready || !strcmp(iface->name, "stm")) {
		dchid_create_interface(iface);
	} else {
		iface->deferred = true;
	}
}

static void dchid_handle_gpio(struct dockchannel_hid *dchid, void *data, size_t length)
{
	struct dchid_gpio_cmd *cmd = data;
	struct dchid_iface *iface;
	u32 retcode = 0xe000f00d; /* Give it a random Apple-style error code */
	struct dchid_gpio_ack *ack;

	if (length < sizeof(*cmd))
		return;

	if (cmd->iface >= MAX_INTERFACES || !(iface = dchid->ifaces[cmd->iface])) {
		dev_err(dchid->dev, "Got GPIO command for bad inteface %d\n", cmd->iface);
		goto err;
	}

	dev_info(dchid->dev, "GPIO command received: %s#%d cmd=%d\n",
		 iface->name, cmd->gpio, cmd->cmd);

	/*
	 * J700's AFE reset is an eight-byte pcIO platform-key operation rather
	 * than a conventional GPIO.  A dedicated diagnostic may acknowledge the
	 * pulse without touching SMC so we can establish whether iBoot's inherited
	 * IPD state is already sufficient.  This is deliberately gated by the same
	 * exact interface contract as startup adoption and by a second DT opt-in;
	 * it must never turn an unknown GPIO request into a successful no-op.
	 */
	if (dchid_adopt_preinitialized_ipd_functions(iface) &&
	    of_property_read_bool(iface->of_node,
				  "apple,ack-preinitialized-afe-pulse")) {
		if (cmd->gpio != iface->gpio_id) {
			dev_err(dchid->dev,
				"Got adopted IPD command for bad GPIO %s#%d\n",
				iface->name, cmd->gpio);
			goto err;
		}

		if (cmd->cmd != 3) {
			dev_err(dchid->dev,
				"Unknown adopted IPD GPIO command %d\n", cmd->cmd);
			goto err;
		}

		dev_info(dchid->dev,
			 "J700_MTP_AFE_ACK_ONLY: interface=%s id=%u cmd=%u pcIO_writes=0\n",
			 iface->name, iface->gpio_id, cmd->cmd);
		retcode = 0;
		goto ack;
	}

	if (dchid_request_gpio(iface) < 0)
		goto err;

	if (!iface->gpio || cmd->gpio != iface->gpio_id) {
		dev_err(dchid->dev, "Got GPIO command for bad GPIO %s#%d\n",
			iface->name, cmd->gpio);
		goto err;
	}

	dev_info(dchid->dev, "GPIO command: %s#%d: %d\n", iface->name, cmd->gpio, cmd->cmd);

	switch (cmd->cmd) {
	case 3: {
		u32 pulse_us = 10000;

		/*
		 * Older platforms never described the timing, so preserve their
		 * historical 10-ms default.  J700's shipping MTP_SYS config gives
		 * an exact 50-ms AFE disable delay and opts in through its interface
		 * child instead of carrying another platform guess here.
		 */
		of_property_read_u32(iface->of_node, "apple,reset-pulse-us",
				     &pulse_us);
		if (!pulse_us || pulse_us > USEC_PER_SEC) {
			dev_err(dchid->dev,
				"Invalid reset pulse for %s: %u us\n",
				iface->name, pulse_us);
			break;
		}

		dev_info(dchid->dev,
			 "GPIO pulse: %s#%d assert_us=%u\n",
			 iface->name, cmd->gpio, pulse_us);
		gpiod_set_value_cansleep(iface->gpio, 1);
		fsleep(pulse_us);
		gpiod_set_value_cansleep(iface->gpio, 0);
		retcode = 0;
		break;
	}
	default:
		dev_err(dchid->dev, "Unknown GPIO command %d\n", cmd->cmd	);
		break;
	}

err:
	/* Keep the firmware-visible acknowledgement format identical on errors. */
	;
ack:
	/* Ack it */
	ack = kzalloc(sizeof(*ack) + length, GFP_KERNEL);
	if (!ack)
		return;

	ack->type = CMD_ACK_GPIO_CMD;
	ack->retcode = retcode;
	memcpy(ack->cmd, data, length);

	if (dchid_comm_cmd(dchid, ack, sizeof(*ack) + length) < 0)
		dev_err(dchid->dev, "Failed to ACK GPIO command\n");

	kfree(ack);
}

static void dchid_handle_event(struct dockchannel_hid *dchid, void *data, size_t length)
{
	u8 *p = data;
	switch (*p) {
	case EVENT_INIT:
		dchid_handle_init(dchid, data, length);
		break;
	case EVENT_READY:
		dchid_handle_ready(dchid, data, length);
		break;
	case EVENT_GPIO_CMD:
		dchid_handle_gpio(dchid, data, length);
		break;
	default:
		/*
		 * A18 MTP drives its own AFE recovery and logs "IPD power cycle
		 * using reset request" before each retry.  If any part of that
		 * is a request aimed at the host, it arrives here, and this
		 * switch used to drop it without a trace.  Never stay silent
		 * about an unhandled comm event again.
		 */
		dev_warn(dchid->dev, "Unhandled comm event 0x%02x (%zu bytes): %*ph\n",
			 *p, length, (int)min(length, (size_t)32), data);
		break;
	}
}

static void dchid_handle_report(struct dchid_iface *iface, void *data, size_t length)
{
	struct dockchannel_hid *dchid = iface->dchid;

	if (!iface->of_node) {
		/* Control-only interface: log what it sends, do not route it. */
		dev_info(dchid->dev, "Control interface %s report (%zu bytes): %*ph\n",
			 iface->name, length, (int)min(length, (size_t)32), data);
		return;
	}

	if (!iface->hid) {
		dev_warn(dchid->dev, "Report received but %s is not initialized!\n", iface->name);
		return;
	}

	if (!iface->open)
		return;

	hid_input_report(iface->hid, HID_INPUT_REPORT, data, length, 1);
}

static void dchid_packet_work(struct work_struct *ws)
{
	struct dchid_work *work = container_of(ws, struct dchid_work, work);
	struct dchid_subhdr *shdr = (void *)work->data;
	struct dockchannel_hid *dchid = work->iface->dchid;
	int type = FIELD_GET(FLAGS_GROUP, shdr->flags);
	u8 *payload = work->data + sizeof(*shdr);

	if (shdr->length + sizeof(*shdr) > work->hdr.length) {
		dev_err(dchid->dev, "Bad sub header length (%hu > %zu)\n",
			shdr->length, work->hdr.length - sizeof(*shdr));
		return;
	}

	switch (type) {
	case HID_INPUT_REPORT:
		if (work->hdr.iface == IFACE_COMM)
			dchid_handle_event(dchid, payload, shdr->length);
		else
			dchid_handle_report(work->iface, payload, shdr->length);
		break;
	default:
		dev_err(dchid->dev, "Received unknown packet type %d\n", type);
		break;
	}

	kfree(work);
}

static void dchid_handle_ack(struct dchid_iface *iface, struct dchid_hdr *hdr, void *data)
{
	struct dchid_subhdr *shdr = (void *)data;
	u8 *payload = data + sizeof(*shdr);

	if (shdr->length + sizeof(*shdr) > hdr->length) {
		dev_err(iface->dchid->dev, "Bad sub header length (%hu > %zu)\n",
			shdr->length, hdr->length - sizeof(*shdr));
		return;
	}
	if (shdr->flags != iface->out_flags) {
		dev_err(iface->dchid->dev,
			"Received unexpected flags 0x%x on ACK channel (expFected 0x%x)\n",
			shdr->flags, iface->out_flags);
		return;
	}

	if (shdr->length < 1) {
		dev_err(iface->dchid->dev, "Received length 0 output report ack\n");
		return;
	}
	if (iface->tx_seq != hdr->seq) {
		dev_err(iface->dchid->dev, "Received ACK with bad seq (expected %d, got %d)\n",
			iface->tx_seq, hdr->seq);
		return;
	}
	if (iface->out_report != payload[0]) {
		dev_err(iface->dchid->dev, "Received ACK with bad report (expected %d, got %d\n",
			iface->out_report, payload[0]);
		return;
	}

	if (iface->resp_buf && iface->resp_size)
		memcpy(iface->resp_buf, payload + 1, min((size_t)shdr->length - 1, iface->resp_size));

	iface->resp_size = shdr->length;
	iface->out_report = -1;
	iface->retcode = shdr->retcode;
	complete(&iface->out_complete);
}

static void dchid_handle_packet(void *cookie, size_t avail)
{
	struct dockchannel_hid *dchid = cookie;
	struct dchid_hdr hdr;
	struct dchid_work *work;
	struct dchid_iface *iface;
	u32 checksum;

	if (dockchannel_recv(dchid->dc, &hdr, sizeof(hdr)) != sizeof(hdr)) {
		dev_err(dchid->dev, "Read failed (header)\n");
		return;
	}

	if (hdr.hdr_len != sizeof(hdr)) {
		dev_err(dchid->dev, "Bad header length %d\n", hdr.hdr_len);
		goto done;
	}

	if (dockchannel_recv(dchid->dc, dchid->pkt_buf, hdr.length + 4) != (hdr.length + 4)) {
		dev_err(dchid->dev, "Read failed (body)\n");
		goto done;
	}

	checksum = dchid_checksum(&hdr, sizeof(hdr));
	checksum += dchid_checksum(dchid->pkt_buf, hdr.length + 4);

	if (checksum != 0xffffffff) {
		dev_err(dchid->dev, "Checksum mismatch (iface %d): 0x%08x != 0xffffffff\n",
			hdr.iface, checksum);
		goto done;
	}


	if (hdr.iface >= MAX_INTERFACES) {
		dev_err(dchid->dev, "Bad iface %d\n", hdr.iface);
	}

	iface = dchid->ifaces[hdr.iface];

	if (!iface) {
		dev_err(dchid->dev, "Received packet for uninitialized iface %d\n", hdr.iface);
		goto done;
	}

	switch (hdr.channel) {
		case DCHID_CHANNEL_CMD:
			dchid_handle_ack(iface, &hdr, dchid->pkt_buf);
			goto done;
		case DCHID_CHANNEL_REPORT:
			break;
		default:
			dev_warn(dchid->dev, "Unknown channel 0x%x, treating as report...\n",
				 hdr.channel);
			break;
	}

	work = kzalloc(sizeof(*work) + hdr.length, GFP_KERNEL);
	if (!work)
		return;

	work->hdr = hdr;
	work->iface = iface;
	memcpy(work->data, dchid->pkt_buf, hdr.length);
	INIT_WORK(&work->work, dchid_packet_work);

	queue_work(iface->wq, &work->work);

done:
	dockchannel_await(dchid->dc, dchid_handle_packet, dchid, sizeof(struct dchid_hdr));
}

static int dockchannel_hid_probe(struct platform_device *pdev)
{
	struct device *dev = &pdev->dev;
	struct dockchannel_hid *dchid;
	struct device_node *child, *helper;
	struct platform_device *helper_pdev;
	struct property *prop;
	int ret;

	ret = dma_set_mask_and_coherent(&pdev->dev, DMA_BIT_MASK(64));
	if (ret)
		return ret;

	dchid = devm_kzalloc(dev, sizeof(*dchid), GFP_KERNEL);
	if (!dchid) {
		return -ENOMEM;
	}

	dchid->dev = dev;

	/*
	 * First make sure all the GPIOs are available, in cased we need to defer.
	 * This is necessary because MTP will request them by name later, and by then
	 * it's too late to defer the probe.
	 */

	for_each_child_of_node(dev->of_node, child) {
		for_each_property_of_node(child, prop) {
			size_t len = strlen(prop->name);
			struct gpio_desc *gpio;

			if (len < 12 || strncmp("apple,", prop->name, 6) ||
			    strcmp("-gpios", prop->name + len - 6))
				continue;

			gpio = fwnode_gpiod_get_index(&child->fwnode, prop->name, 0, GPIOD_ASIS,
						      prop->name);
			if (IS_ERR_OR_NULL(gpio)) {
				if (PTR_ERR(gpio) == -EPROBE_DEFER) {
					of_node_put(child);
					return -EPROBE_DEFER;
				}
			} else {
				gpiod_put(gpio);
			}
		}
	}

	/*
	 * Make sure we also have the MTP coprocessor available, and
	 * defer probe if the helper hasn't probed yet.
	 */
	helper = of_parse_phandle(dev->of_node, "apple,helper-cpu", 0);
	if (!helper) {
		dev_err(dev, "Missing apple,helper-cpu property");
		return -EINVAL;
	}

	helper_pdev = of_find_device_by_node(helper);
	of_node_put(helper);
	if (!helper_pdev) {
		dev_err(dev, "Failed to find helper device");
		return -EINVAL;
	}

	dchid->helper_link = device_link_add(dev, &helper_pdev->dev,
					     DL_FLAG_AUTOREMOVE_CONSUMER);
	put_device(&helper_pdev->dev);
	if (!dchid->helper_link) {
		dev_err(dev, "Failed to link to helper device");
		return -EINVAL;
	}

	if (dchid->helper_link->supplier->links.status != DL_DEV_DRIVER_BOUND)
		return -EPROBE_DEFER;

	/* Now it is safe to begin initializing */
	dchid->dc = dockchannel_init(pdev);
	if (IS_ERR_OR_NULL(dchid->dc)) {
		return PTR_ERR(dchid->dc);
	}
	dchid->new_iface_wq = alloc_workqueue("dchid-new", WQ_MEM_RECLAIM, 0);
	if (!dchid->new_iface_wq)
		return -ENOMEM;

	dchid->comm = dchid_get_interface(dchid, IFACE_COMM, "comm");
	if (!dchid->comm) {
		dev_err(dchid->dev, "Failed to initialize comm interface");
		return -EIO;
	}

	/*
	 * M4-generation MTP firmware no longer advertises the STM interface that
	 * older machines use to return the common HID identity.  Waiting for STM
	 * in that topology leaves every real interface deferred forever.  The
	 * per-interface descriptors are still delivered by MTP, so use the only
	 * identity value needed to enumerate them and let the DT opt in to the
	 * no-STM protocol variant.
	 */
	if (of_property_read_bool(dev->of_node, "apple,no-stm")) {
		dchid->device_id.vendor_id = HOST_VENDOR_ID_APPLE;
		dchid->id_ready = true;
		dev_info(dev, "No STM interface; using fallback Apple HID identity\n");
	}

	dev_info(dchid->dev, "Initialized, awaiting packets\n");
	dockchannel_await(dchid->dc, dchid_handle_packet, dchid, sizeof(struct dchid_hdr));

	return 0;
}

static void dockchannel_hid_remove(struct platform_device *pdev)
{
	BUG_ON(1);
}

static const struct of_device_id dockchannel_hid_of_match[] = {
	{ .compatible = "apple,dockchannel-hid" },
	{},
};
MODULE_DEVICE_TABLE(of, dockchannel_hid_of_match);
MODULE_FIRMWARE("apple/tpmtfw-*.bin");

static struct platform_driver dockchannel_hid_driver = {
	.driver = {
		.name = "dockchannel-hid",
		.of_match_table = dockchannel_hid_of_match,
	},
	.probe = dockchannel_hid_probe,
	.remove = dockchannel_hid_remove,
};
module_platform_driver(dockchannel_hid_driver);

MODULE_DESCRIPTION("Apple DockChannel HID transport driver");
MODULE_AUTHOR("Hector Martin <marcan@marcan.st>");
MODULE_LICENSE("Dual MIT/GPL");
