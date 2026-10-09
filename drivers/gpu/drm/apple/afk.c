// SPDX-License-Identifier: GPL-2.0-only OR MIT
/* Copyright 2022 Sven Peter <sven@svenpeter.dev> */

#include <linux/bitfield.h>
#include <linux/debugfs.h>
#include <linux/dma-mapping.h>
#include <linux/kconfig.h>
#include <linux/of_platform.h>
#include <linux/slab.h>
#include <linux/unaligned.h>
#include <linux/workqueue.h>
#include <linux/soc/apple/rtkit.h>

#include "afk.h"
#include "trace.h"

struct afk_receive_message_work {
	struct apple_dcp_afkep *ep;
	u64 message;
	struct work_struct work;
};

#define RBEP_TYPE GENMASK(63, 48)

enum rbep_msg_type {
	RBEP_INIT = 0x80,
	RBEP_INIT_ACK = 0xa0,
	RBEP_GETBUF = 0x89,
	RBEP_GETBUF_ACK = 0xa1,
	RBEP_INIT_TX = 0x8a,
	RBEP_INIT_RX = 0x8b,
	RBEP_START = 0xa3,
	RBEP_START_ACK = 0x86,
	RBEP_SEND = 0xa2,
	RBEP_RECV = 0x85,
	RBEP_SHUTDOWN = 0xc0,
	RBEP_SHUTDOWN_ACK = 0xc1,
};

/*
 * Which endpoints use the AFKEP V2 command/response transmit path.
 *
 * Only the DisplayPort endpoints do.  The first attempt gated this on the
 * firmware alone, which also rerouted 0x20 (system) and 0x23 (disp/dispext) --
 * endpoints whose INIT and transmit paths work under the classic path -- and
 * broke the internal panel: "DCP did not answer INIT" followed by
 * "Inherited DCP IOP OFF not acknowledged" and a failed component bind.
 * Keep the blast radius to the endpoints that actually need it.
 */
static bool afk_endpoint_is_v2(struct apple_dcp_afkep *ep)
{
	if (!ep || ep->dcp->fw_compat != DCP_FIRMWARE_H17P)
		return false;

	switch (ep->endpoint) {
	case DPTX_CONTROL_ENDPOINT:
	case DPAVSERV_ENDPOINT:
	case DPTX_ENDPOINT:
	case DPTX_PORT_ENDPOINT_H17P:
		return true;
	default:
		return false;
	}
}

/*
 * The H17P system endpoint (0x20) also receives the compact V2 EPIC header,
 * with its service channel at +2 and type/category at +16/+17.  Its outgoing
 * commands still use the established system endpoint path; only the DP
 * endpoints have a validated V2 command/reply exchange.  Decode 0x20's RX
 * header and reports without changing its INIT or transmit sequence.
 *
 * The disp endpoint (0x23) appears to use the same compact header: read
 * with the wide one, the external DCP's first message after the ring
 * set-up (its dispext-service announce on older firmware) comes out as
 * "expected announce but got 0x0 on channel 0" on every boot.  Its
 * service is a do-nothing handshake (ibootep.c), so decoding its RX
 * header cannot change what the driver sends.
 */
static bool afk_endpoint_has_v2_rx(struct apple_dcp_afkep *ep)
{
	if (afk_endpoint_is_v2(ep))
		return true;
	if (!ep || ep->dcp->fw_compat != DCP_FIRMWARE_H17P)
		return false;

	return ep->endpoint == SYSTEM_ENDPOINT ||
	       ep->endpoint == DISP0_ENDPOINT;
}


#define BLOCK_SHIFT 6

#define GETBUF_SIZE GENMASK(31, 16)
#define GETBUF_TAG GENMASK(15, 0)
#define GETBUF_ACK_DVA GENMASK(47, 0)

#define INITRB_OFFSET GENMASK(47, 32)
#define INITRB_SIZE GENMASK(31, 16)
#define INITRB_TAG GENMASK(15, 0)

#define SEND_WPTR GENMASK(31, 0)

/* H17P/V2 helper used by afk_shutdown() before it is defined below. */
static int afk_service_close_v2(struct apple_epic_service *service);

static void afk_send(struct apple_dcp_afkep *ep, u64 message)
{
	dcp_send_message(ep->dcp, ep->endpoint, message);
}

struct apple_dcp_afkep *afk_init(struct apple_dcp *dcp, u32 endpoint,
				 const struct apple_epic_service_ops *ops)
{
	struct apple_dcp_afkep *afkep;
	int ret;

	afkep = devm_kzalloc(dcp->dev, sizeof(*afkep), GFP_KERNEL);
	if (!afkep)
		return ERR_PTR(-ENOMEM);

	afkep->ops = ops;
	afkep->dcp = dcp;
	afkep->endpoint = endpoint;
	afkep->wq = alloc_ordered_workqueue("apple-dcp-afkep%02x",
					    WQ_MEM_RECLAIM, endpoint);
	if (!afkep->wq) {
		ret = -ENOMEM;
		goto out_free_afkep;
	}

	// TODO: devm_ for wq

	init_completion(&afkep->started);
	init_completion(&afkep->stopped);
	spin_lock_init(&afkep->lock);

	return afkep;

out_free_afkep:
	devm_kfree(dcp->dev, afkep);
	return ERR_PTR(ret);
}

void afk_shutdown(struct apple_dcp_afkep *afkep)
{
	int ret;

	/*
	 * H17P/V2: tell the firmware we are done with each service we OPENed
	 * before tearing the endpoint down.  Best effort; a failure here must
	 * not stop the shutdown.
	 */
	if (afk_endpoint_is_v2(afkep)) {
		u32 i;

		for (i = 0; i < afkep->num_channels; i++) {
			if (afkep->services[i].enabled &&
			    afkep->services[i].opened) {
				afk_service_close_v2(&afkep->services[i]);
				afkep->services[i].opened = false;
			}
		}
	}

	afk_send(afkep, FIELD_PREP(RBEP_TYPE, RBEP_SHUTDOWN));

	ret = wait_for_completion_timeout(&afkep->stopped, msecs_to_jiffies(1000));
	if (ret <= 0) {
		dev_err(afkep->dcp->dev, "Timed out shutting down AFK endpoint %02x", afkep->endpoint);
	}

	destroy_workqueue(afkep->wq);
}

int afk_start(struct apple_dcp_afkep *ep)
{
	int ret;

	reinit_completion(&ep->started);
	ret = dcp_start_rtkit_endpoint(ep->dcp, ep->endpoint);
	if (ret)
		return ret;
	afk_send(ep, FIELD_PREP(RBEP_TYPE, RBEP_INIT));

	ret = wait_for_completion_timeout(&ep->started, msecs_to_jiffies(1000));
	if (ret <= 0)
		return -ETIMEDOUT;
	else
		return 0;
}

static void afk_getbuf(struct apple_dcp_afkep *ep, u64 message)
{
	u32 size = FIELD_GET(GETBUF_SIZE, message) << BLOCK_SHIFT;
	u16 tag = FIELD_GET(GETBUF_TAG, message);
	u64 reply;

	trace_afk_getbuf(ep, size, tag);
	dev_info(ep->dcp->dev, "AFK[ep:%02x]: getbuf size %#x tag %#x\n",
		 ep->endpoint, size, tag);

	if (ep->bfr) {
		dev_err(ep->dcp->dev,
			"Got GETBUF message but buffer already exists\n");
		return;
	}

	ep->bfr = dmam_alloc_coherent(ep->dcp->dev, size, &ep->bfr_dma,
				      GFP_KERNEL);
	if (!ep->bfr) {
		dev_err(ep->dcp->dev, "Failed to allocate %d bytes buffer\n",
			size);
		return;
	}

	ep->bfr_size = size;
	ep->bfr_tag = tag;

	reply = FIELD_PREP(RBEP_TYPE, RBEP_GETBUF_ACK);
	reply |= FIELD_PREP(GETBUF_ACK_DVA, ep->bfr_dma);
	afk_send(ep, reply);

	dev_info(ep->dcp->dev,
		 "J700_AFK_GETBUF_ACK: ep:%02x dva=%pad size=%#zx tag=%#x reply=%#llx\n",
		 ep->endpoint, &ep->bfr_dma, ep->bfr_size, ep->bfr_tag, reply);
}

static void afk_init_rxtx(struct apple_dcp_afkep *ep, u64 message,
			  struct afk_ringbuffer *bfr)
{
	u32 base = FIELD_GET(INITRB_OFFSET, message) << BLOCK_SHIFT;
	u32 size = FIELD_GET(INITRB_SIZE, message) << BLOCK_SHIFT;
	u16 tag = FIELD_GET(INITRB_TAG, message);
	u32 bufsz, hdrsz, end, block;

	if (tag != ep->bfr_tag) {
		dev_err(ep->dcp->dev, "AFK[ep:%02x]: expected tag 0x%x but got 0x%x\n",
			ep->endpoint, ep->bfr_tag, tag);
		return;
	}

	if (bfr->ready) {
		dev_err(ep->dcp->dev, "AFK[ep:%02x]: buffer is already initialized\n",
			ep->endpoint);
		return;
	}

	if (!ep->bfr || size < sizeof(__le32) || base >= ep->bfr_size) {
		dev_err(ep->dcp->dev,
			"AFK[ep:%02x]: requested base 0x%x >= max size 0x%lx\n",
			ep->endpoint, base, ep->bfr_size);
		return;
	}

	end = base + size;
	if (end > ep->bfr_size) {
		dev_err(ep->dcp->dev,
			"AFK[ep:%02x]: requested end 0x%x > max size 0x%lx\n",
			ep->endpoint, end, ep->bfr_size);
		return;
	}

	bfr->hdr = ep->bfr + base;
	dma_rmb();
	bufsz = le32_to_cpu(READ_ONCE(*(__le32 *)bfr->hdr));
	hdrsz = size - bufsz;
	block = hdrsz / AFK_RB_BLOCKS;
	if (bufsz >= size || hdrsz % AFK_RB_BLOCKS ||
	    block < AFK_RB_BLOCK_MIN || !is_power_of_2(block) ||
	    !IS_ALIGNED(bufsz, block) || bufsz < 2 * block) {
		dev_err(ep->dcp->dev,
			"AFK[ep:%02x]: ring bufsz %#x with size %#x gives no valid header (base %#x, msg %#llx)\n",
			ep->endpoint, bufsz, size, base, message);
		return;
	}
	bfr->block = block;

	bfr->buf = (u8 *)bfr->hdr + hdrsz;
	bfr->bufsz = bufsz;
	bfr->ready = true;

	if (ep->rxbfr.ready && ep->txbfr.ready)
		afk_send(ep, FIELD_PREP(RBEP_TYPE, RBEP_START));
}

/*
 * H17P / AFKEP-V2 raw TX ring write.
 *
 * Same wrap-around discipline as afk_send_epic() (duplicate queue entry at the
 * tail and a second copy before a payload placed at offset zero), but the
 * caller has already framed the whole V2 packet, so we only prepend the queue
 * entry.  Unlike the V1 envelope the queue-entry channel/type are left zero:
 * on V2 the interface id and message type live inside the payload headers.
 */
static int afk_ring_write_v2(struct apple_dcp_afkep *ep, const u8 *pkt,
			     size_t len)
{
	u32 rptr, wptr;
	struct afk_qe *hdr, *hdr2;
	unsigned long flags;
	size_t total_size;
	int ret;

	spin_lock_irqsave(&ep->lock, flags);

	dma_rmb();
	rptr = le32_to_cpu(*afk_rb_rptr(&ep->txbfr));
	wptr = le32_to_cpu(*afk_rb_wptr(&ep->txbfr));
	total_size = sizeof(*hdr) + len;
	hdr = hdr2 = NULL;

	if (wptr < rptr) {
		if (wptr + total_size > rptr) {
			ret = -ENOMEM;
			goto out;
		}
		hdr = ep->txbfr.buf + wptr;
		wptr += sizeof(*hdr);
	} else {
		if (wptr + sizeof(*hdr) > ep->txbfr.bufsz) {
			ret = -ENOMEM;
			goto out;
		}
		if (wptr + total_size > ep->txbfr.bufsz) {
			if (sizeof(*hdr) > rptr) {
				ret = -ENOMEM;
				goto out;
			}
			hdr = ep->txbfr.buf + wptr;
			hdr2 = ep->txbfr.buf;
			wptr = sizeof(*hdr);
			if (wptr + len > rptr) {
				ret = -ENOMEM;
				goto out;
			}
		} else {
			hdr = ep->txbfr.buf + wptr;
			wptr += sizeof(*hdr);
		}
	}

	hdr->magic = cpu_to_le32(QE_MAGIC);
	hdr->size = cpu_to_le32(len);
	hdr->channel = cpu_to_le32(0);
	hdr->type = cpu_to_le32(0);
	if (hdr2)
		memcpy(hdr2, hdr, sizeof(*hdr));

	memcpy(ep->txbfr.buf + wptr, pkt, len);
	wptr += len;
	wptr = ALIGN(wptr, ep->txbfr.block);
	if (wptr == ep->txbfr.bufsz)
		wptr = 0;

	*afk_rb_wptr(&ep->txbfr) = cpu_to_le32(wptr);
	afk_send(ep, FIELD_PREP(RBEP_TYPE, RBEP_SEND) |
			     FIELD_PREP(SEND_WPTR, wptr));
	ret = 0;

out:
	spin_unlock_irqrestore(&ep->lock, flags);
	return ret;
}

/* H17P/V2: OPEN (0x12) or CLOSE (0x13) report for one service. */
static int afk_service_report_v2(struct apple_epic_service *service, u8 type)
{
	struct apple_dcp_afkep *ep = service->ep;
	u8 pkt[EPIC_V2_REPORT_HDR] = { 0 };
	unsigned long flags;

	spin_lock_irqsave(&service->lock, flags);
	pkt[0] = service->seq++;
	spin_unlock_irqrestore(&service->lock, flags);

	put_unaligned_le16(service->channel, pkt + 2);
	put_unaligned_le32(EPIC_V2_REPORT_HDR - 8, pkt + 4);
	pkt[16] = type;
	pkt[17] = EPIC_V2_CAT_REPORT;
	pkt[18] = 1; /* native open/close report flag */

	return afk_ring_write_v2(ep, pkt, sizeof(pkt));
}

static int afk_service_open_v2(struct apple_epic_service *service)
{
	return afk_service_report_v2(service, EPIC_V2_TYPE_OPEN);
}

static int afk_service_close_v2(struct apple_epic_service *service)
{
	return afk_service_report_v2(service, EPIC_V2_TYPE_CLOSE);
}

#if IS_ENABLED(CONFIG_DRM_APPLE_DEBUG)
static void afk_populate_service_debugfs(struct apple_epic_service *srv);
static void afk_remove_service_debugfs(struct apple_epic_service *srv);
#else
static void afk_populate_service_debugfs(struct apple_epic_service *srv)
{
}
static void afk_remove_service_debugfs(struct apple_epic_service *srv)
{
}
#endif

static const struct apple_epic_service_ops *
afk_match_service(struct apple_dcp_afkep *ep, const char *name)
{
	const struct apple_epic_service_ops *ops;

	if (!name[0])
		return NULL;
	if (!ep->ops)
		return NULL;

	for (ops = ep->ops; ops->name[0]; ops++) {
		if (strcmp(ops->name, name))
			continue;

		return ops;
	}

	return NULL;
}

static struct apple_epic_service *afk_epic_find_service(struct apple_dcp_afkep *ep,
						 u32 channel)
{
    for (u32 i = 0; i < ep->num_channels; i++)
        if (ep->services[i].enabled && ep->services[i].channel == channel)
            return &ep->services[i];

    return NULL;
}

static void afk_recv_handle_init(struct apple_dcp_afkep *ep, u32 channel,
				 u8 *payload, size_t payload_size)
{
	char name[32];
	s64 epic_unit = -1;
	u32 ch_idx;
	const char *service_name = name;
	const char *epic_name = NULL, *epic_class = NULL;
	const struct apple_epic_service_ops *ops;
	struct dcp_parse_ctx ctx;
	u8 *props;
	size_t props_size;

	WARN_ON(afk_epic_find_service(ep, channel));

	if (payload_size < sizeof(name)) {
		dev_err(ep->dcp->dev, "AFK[ep:%02x]: payload too small: %lx\n",
			ep->endpoint, payload_size);
		return;
	}
	props = payload + sizeof(name);
	props_size = payload_size - sizeof(name);

	if (ep->num_channels >= AFK_MAX_CHANNEL) {
		dev_err(ep->dcp->dev, "AFK[ep:%02x]: too many enabled services!\n",
			ep->endpoint);
		return;
	}

	strscpy(name, payload, sizeof(name));

	/*
	 * in DCP firmware 13.2 DCP reports interface-name as name which starts
	 * with "dispext%d" using -1 s ID for "dcp". In the 12.3 firmware
	 * EPICProviderClass was used. If the init call has props parse them and
	 * use EPICProviderClass to match the service.
	 */
	if (props_size > 36) {
		int ret = parse(props, props_size, &ctx);
		if (ret) {
			dev_err(ep->dcp->dev,
				"AFK[ep:%02x]: Failed to parse service init props for %s\n",
				ep->endpoint, name);
			return;
		}
		ret = parse_epic_service_init(&ctx, &epic_name, &epic_class, &epic_unit);
		if (ret) {
			{
				/* name the service we could not register */
				const u8 *b = props;
				char buf[128];
				int n = 0;
				size_t k, run = 0;

				for (k = 0; k < props_size && n < 100; k++) {
					if (b[k] >= 0x20 && b[k] < 0x7f) {
						run++;
						continue;
					}
					if (run >= 4)
						n += scnprintf(buf + n,
							       sizeof(buf) - n,
							       "%.*s ", (int)run,
							       b + k - run);
					run = 0;
				}
				dev_err(ep->dcp->dev,
					"AFK[ep:%02x]: unregistered service props: %s\n",
					ep->endpoint, buf);
			}
			dev_err(ep->dcp->dev,
				"AFK[ep:%02x]: failed to extract init props: %d\n",
				ep->endpoint, ret);
			return;
		}
		service_name = epic_class;
	} else {
            service_name = name;
        }

	if (ep->match_epic_name)
		ops = afk_match_service(ep, epic_name);
	else
		ops = afk_match_service(ep, service_name);

	if (!ops) {
		dev_err(ep->dcp->dev,
			"AFK[ep:%02x]: unable to match service %s on channel %d\n",
			ep->endpoint, service_name, channel);
		goto free;
	}

	ch_idx = ep->num_channels++;
	spin_lock_init(&ep->services[ch_idx].lock);
	ep->services[ch_idx].enabled = true;
	ep->services[ch_idx].torndown = false;
	ep->services[ch_idx].ops = ops;
	ep->services[ch_idx].ep = ep;
	ep->services[ch_idx].channel = channel;
	ep->services[ch_idx].cmd_tag = 0;
	/* H17P/V2: a freshly (re-)announced service is not yet OPENed. */
	ep->services[ch_idx].opened = false;
	ep->services[ch_idx].seq = 0;
	ops->init(&ep->services[ch_idx], epic_name, epic_class, epic_unit);
	dev_info(ep->dcp->dev, "AFK[ep:%02x]: new service %s on channel %d\n",
		 ep->endpoint, service_name, channel);

	afk_populate_service_debugfs(&ep->services[ch_idx]);

free:
	kfree(epic_name);
	kfree(epic_class);
}

static void afk_recv_handle_teardown(struct apple_dcp_afkep *ep, u32 channel)
{
	struct apple_epic_service *service;
	const struct apple_epic_service_ops *ops;
	unsigned long flags;

	service = afk_epic_find_service(ep, channel);
	if (!service) {
		dev_warn(ep->dcp->dev, "AFK[ep:%02x]: teardown for disabled channel %u\n",
			 ep->endpoint, channel);
		return;
	}

	afk_remove_service_debugfs(service);

	// TODO: think through what locking is necessary
	spin_lock_irqsave(&service->lock, flags);
	/*
	 * teardown must not disable the service since since it may be sent as
	 * side effect of a COMMAND which for which a reply is expected.
	 * Seen with DCP's "av" endpoint during the close afk_service_call.
	 */
	service->torndown = true;
	ops = service->ops;
	spin_unlock_irqrestore(&service->lock, flags);

	if (ops->teardown)
		ops->teardown(service);
}

static void afk_recv_handle_reply(struct apple_dcp_afkep *ep, u32 channel,
				  u16 tag, void *payload, size_t payload_size)
{
	struct epic_cmd *cmd = payload;
	struct apple_epic_service *service;
	unsigned long flags;
	u8 idx = tag & 0xff;
	void *rxbuf, *txbuf;
	dma_addr_t rxbuf_dma, txbuf_dma;
	size_t rxlen, txlen;

	service = afk_epic_find_service(ep, channel);
	if (!service) {
		dev_warn(ep->dcp->dev, "AFK[ep:%02x]: command reply on disabled channel %u\n",
			 ep->endpoint, channel);
		return;
	}

	if (payload_size < sizeof(*cmd)) {
		dev_err(ep->dcp->dev,
			"AFK[ep:%02x]: command reply on channel %d too small: %ld\n",
			ep->endpoint, channel, payload_size);
		return;
	}

	if (idx >= MAX_PENDING_CMDS) {
		dev_err(ep->dcp->dev,
			"AFK[ep:%02x]: command reply on channel %d out of range: %d\n",
			ep->endpoint, channel, idx);
		return;
	}

	spin_lock_irqsave(&service->lock, flags);
	if (service->cmds[idx].done) {
		dev_err(ep->dcp->dev,
			"AFK[ep:%02x]: command reply on channel %d already handled\n",
			ep->endpoint, channel);
		spin_unlock_irqrestore(&service->lock, flags);
		return;
	}

	if (tag != service->cmds[idx].tag) {
		dev_err(ep->dcp->dev,
			"AFK[ep:%02x]: command reply on channel %d has invalid tag: expected 0x%04x != 0x%04x\n",
			ep->endpoint, channel, tag, service->cmds[idx].tag);
		spin_unlock_irqrestore(&service->lock, flags);
		return;
	}

	service->cmds[idx].done = true;
	service->cmds[idx].retcode = le32_to_cpu(cmd->retcode);
	if (service->cmds[idx].free_on_ack) {
		/* defer freeing until we're no longer in atomic context */
		rxbuf = service->cmds[idx].rxbuf;
		txbuf = service->cmds[idx].txbuf;
		rxlen = service->cmds[idx].rxlen;
		txlen = service->cmds[idx].txlen;
		rxbuf_dma = service->cmds[idx].rxbuf_dma;
		txbuf_dma = service->cmds[idx].txbuf_dma;
		bitmap_release_region(service->cmd_map, idx, 0);
	} else {
		rxbuf = txbuf = NULL;
		rxlen = txlen = 0;
	}
	if (service->cmds[idx].completion)
		complete(service->cmds[idx].completion);

	spin_unlock_irqrestore(&service->lock, flags);

	if (rxbuf && rxlen)
		dma_free_coherent(ep->dcp->dev, rxlen, rxbuf, rxbuf_dma);
	if (txbuf && txlen)
		dma_free_coherent(ep->dcp->dev, txlen, txbuf, txbuf_dma);
}

struct epic_std_service_ap_call {
	__le32 unk0;
	__le32 unk1;
	__le32 type;
	__le32 len;
	__le32 magic;
	u8 _unk[48];
} __attribute__((packed));

static void afk_recv_handle_std_service(struct apple_dcp_afkep *ep, u32 channel,
					u32 type, struct epic_hdr *ehdr,
					struct epic_sub_hdr *eshdr,
					void *payload, size_t payload_size)
{
	struct apple_epic_service *service = afk_epic_find_service(ep, channel);

	if (!service) {
		dev_warn(ep->dcp->dev,
			 "AFK[ep:%02x]: std service notify on disabled channel %u\n",
			 ep->endpoint, channel);
		return;
	}
	if (service->torndown) {
		dev_warn(ep->dcp->dev,
			 "AFK[ep:%02x]: std service notify on torn down service "
			 "(chan:%u)\n", ep->endpoint, channel);
		return;
	}

	if (type == EPIC_TYPE_NOTIFY && eshdr->category == EPIC_CAT_NOTIFY) {
		struct epic_std_service_ap_call *call = payload;
		size_t call_size;
		void *reply;
		int ret;

		if (payload_size < sizeof(*call))
			return;

		call_size = le32_to_cpu(call->len);
		if (payload_size < sizeof(*call) + call_size)
			return;

		if (!service->ops->call)
			return;
		reply = kzalloc(payload_size, GFP_KERNEL);
		if (!reply)
			return;

		ret = service->ops->call(service, le32_to_cpu(call->type),
					 payload + sizeof(*call), call_size,
					 reply + sizeof(*call), call_size);
		if (ret) {
			kfree(reply);
			return;
		}

		memcpy(reply, call, sizeof(*call));
		afk_send_epic(ep, channel, le16_to_cpu(eshdr->tag),
			      EPIC_TYPE_NOTIFY_ACK, EPIC_CAT_REPLY,
			      EPIC_SUBTYPE_STD_SERVICE, reply, payload_size);
		kfree(reply);

		return;
	}

	if (type == EPIC_TYPE_NOTIFY && eshdr->category == EPIC_CAT_REPORT) {
		if (service->ops->report)
			service->ops->report(service, le16_to_cpu(eshdr->type),
					     payload, payload_size);
		return;
	}

	dev_err(ep->dcp->dev,
		"AFK[ep:%02x]: channel %d received unhandled standard service message: %x / %x\n",
		ep->endpoint, channel, type, eshdr->category);
	print_hex_dump(KERN_INFO, "AFK: ", DUMP_PREFIX_NONE, 16, 1, payload,
				   payload_size, true);
}

/*
 * H17P/V2 response to a command we sent (category 2).  The response body --
 * struct epic_service_call followed by its inline data -- sits at offset 32
 * of the packet; copy it into the caller's output buffer and complete the
 * command.  Matched by the single tag byte at +25 (tag == slot index + 1).
 */
static void afk_recv_handle_reply_v2(struct apple_dcp_afkep *ep,
				     struct apple_epic_service *service,
				     const u8 *data, size_t data_size)
{
	unsigned long flags;
	u32 status;
	u8 tag;
	int idx;

	if (data_size < EPIC_V2_HDR) {
		dev_err(ep->dcp->dev,
			"AFK[ep:%02x]: V2 response too small: %zx\n",
			ep->endpoint, data_size);
		return;
	}

	tag = data[25];
	status = get_unaligned_le32(data + 28);

	if (tag < 1 || tag > MAX_PENDING_CMDS) {
		dev_err(ep->dcp->dev,
			"AFK[ep:%02x]: V2 response with bad tag %u on if:%u\n",
			ep->endpoint, tag, service->channel);
		return;
	}
	idx = tag - 1;

	spin_lock_irqsave(&service->lock, flags);
	if (service->cmds[idx].tag != tag || service->cmds[idx].done) {
		dev_err(ep->dcp->dev,
			"AFK[ep:%02x]: unexpected V2 response tag %u on if:%u\n",
			ep->endpoint, tag, service->channel);
		spin_unlock_irqrestore(&service->lock, flags);
		return;
	}

	/*
	 * The reply lives in the RX ring, which is recycled as soon as this
	 * returns, so copy it out before waking the caller.
	 */
	if (service->cmds[idx].rxbuf && service->cmds[idx].rxlen)
		memcpy(service->cmds[idx].rxbuf, data + EPIC_V2_HDR,
		       min_t(size_t, service->cmds[idx].rxlen,
			     data_size - EPIC_V2_HDR));

	service->cmds[idx].done = true;
	service->cmds[idx].retcode = status;
	if (service->cmds[idx].free_on_ack)
		bitmap_release_region(service->cmd_map, idx, 0);
	if (service->cmds[idx].completion)
		complete(service->cmds[idx].completion);
	spin_unlock_irqrestore(&service->lock, flags);

	dev_info(ep->dcp->dev,
		 "J700_AFK_V2_RSP: ep:%02x if:%u tag:%u status:%#x body:%zu\n",
		 ep->endpoint, service->channel, tag, status,
		 data_size - EPIC_V2_HDR);
}

/*
 * H17P/V2 command *from* the firmware (category 1): the DPTX AUX / link /
 * display-hint callbacks.  Dispatch through the same ops->call() the classic
 * path uses (request and reply bodies live at offset 96), then answer with a
 * category-2 response echoing the identifier and tag.  Services without a
 * .call handler (e.g. the DP controllers) are answered kIOReturnUnsupported
 * so the firmware is never left waiting.
 */
static void afk_recv_handle_command_v2(struct apple_dcp_afkep *ep,
				       struct apple_epic_service *service,
				       const u8 *data, size_t data_size)
{
	u32 call_idx, dlen, status = EPIC_V2_STATUS_UNSUPPORTED;
	unsigned long flags;
	u8 *reply;
	int ret;

	if (data_size < EPIC_V2_BODY) {
		dev_err(ep->dcp->dev,
			"AFK[ep:%02x]: V2 command too small: %zx\n",
			ep->endpoint, data_size);
		return;
	}
	if (get_unaligned_le32(data + 44) != EPIC_SERVICE_CALL_MAGIC) {
		dev_err(ep->dcp->dev,
			"AFK[ep:%02x]: V2 command bad magic on if:%u\n",
			ep->endpoint, service->channel);
		return;
	}

	call_idx = get_unaligned_le32(data + 36);
	dlen = get_unaligned_le32(data + 40);
	if (dlen > data_size - EPIC_V2_BODY) {
		dev_err(ep->dcp->dev,
			"AFK[ep:%02x]: V2 command len %u exceeds packet on if:%u\n",
			ep->endpoint, dlen, service->channel);
		return;
	}

	reply = kzalloc(data_size, GFP_KERNEL);
	if (!reply)
		return;
	/* echo transport / common / service-call headers, then overwrite body */
	memcpy(reply, data, data_size);

	if (service->ops && service->ops->call) {
		ret = service->ops->call(service, call_idx, data + EPIC_V2_BODY,
					 dlen, reply + EPIC_V2_BODY, dlen);
		if (!ret)
			status = 0;
	}

	spin_lock_irqsave(&service->lock, flags);
	reply[0] = service->seq++;
	spin_unlock_irqrestore(&service->lock, flags);

	put_unaligned_le16(service->channel, reply + 2);
	put_unaligned_le32(data_size - 8, reply + 4);
	reply[16] = EPIC_V2_TYPE_COMMAND;
	reply[17] = EPIC_V2_CAT_RESPONSE;
	reply[18] = 0;
	/* identifier (reply[8..15]) and tag (reply[25]) are kept from the echo */
	put_unaligned_le32(status, reply + 28);

	dev_info(ep->dcp->dev,
		 "J700_AFK_V2_CB: ep:%02x if:%u tag:%u call:%u dlen:%u -> status:%#x\n",
		 ep->endpoint, service->channel, reply[25], call_idx, dlen,
		 status);

	afk_ring_write_v2(ep, reply, data_size);
	kfree(reply);
}

/* H17P/V2 dispatch for a message on an already-registered service. */
static void afk_recv_handle_v2(struct apple_dcp_afkep *ep,
			       struct apple_epic_service *service,
			       const u8 *data, size_t data_size)
{
	u8 vtype, vcat;

	if (data_size < EPIC_V2_REPORT_HDR) {
		dev_err(ep->dcp->dev, "AFK[ep:%02x]: V2 message too small: %zx\n",
			ep->endpoint, data_size);
		return;
	}
	vtype = data[16];
	vcat = data[17];

	if (service->torndown) {
		dev_warn(ep->dcp->dev,
			 "AFK[ep:%02x]: V2 message on torn down service (if:%u)\n",
			 ep->endpoint, service->channel);
		return;
	}

	switch (vcat) {
	case EPIC_V2_CAT_RESPONSE:
		afk_recv_handle_reply_v2(ep, service, data, data_size);
		return;
	case EPIC_V2_CAT_COMMAND:
		afk_recv_handle_command_v2(ep, service, data, data_size);
		return;
	case EPIC_V2_CAT_REPORT:
		if (vtype == EPIC_V2_TYPE_CLOSE)
			return afk_recv_handle_teardown(ep, service->channel);
		if (vtype == EPIC_V2_TYPE_OPEN)
			return; /* open acknowledgement, nothing to do */
		if (service->ops && service->ops->report)
			service->ops->report(service, vtype,
					     data + EPIC_V2_REPORT_HDR,
					     data_size - EPIC_V2_REPORT_HDR);
		return;
	default:
		dev_err(ep->dcp->dev,
			"AFK[ep:%02x]: V2 unhandled category %u type %#x on if:%u\n",
			ep->endpoint, vcat, vtype, service->channel);
	}
}

static void afk_recv_handle(struct apple_dcp_afkep *ep, u32 channel, u32 type,
			    u8 *data, size_t data_size)
{
	struct apple_epic_service *service;
	struct epic_hdr *ehdr = (struct epic_hdr *)data;
	struct epic_sub_hdr *eshdr =
		(struct epic_sub_hdr *)(data + sizeof(*ehdr));
	struct epic_sub_hdr h17p_eshdr;
	size_t hdr_len = sizeof(*ehdr) + sizeof(*eshdr);
	u16 subtype;
	u8 *payload;
	size_t payload_size;

	if (afk_endpoint_has_v2_rx(ep)) {
		const struct epic_sub_hdr_h17p *c =
			(const struct epic_sub_hdr_h17p *)(data + sizeof(*ehdr));

		hdr_len = sizeof(*ehdr) + sizeof(*c);
		if (data_size < hdr_len)
			goto too_small;

		/*
		 * H17P multiplexes every service of an endpoint onto queue
		 * channel 0 and carries the real channel in the EPIC header
		 * instead: byte 0 is a sequence counter, byte 1 a flags byte
		 * and bytes 2-3 the channel.  ep:20 announces "system" as
		 * channel 1 and "powerlog-service" as channel 3, then sends the
		 * mNits/uAmps/iDAC backlight reports on channel 3.  Taking the
		 * queue channel at face value binds "system" to 0 and makes
		 * every later announce and report look like a duplicate.
		 */
		channel = le16_to_cpup((__le16 *)(data + 2));

		/*
		 * Normalise into the wide sub-header the rest of this file
		 * expects, mapping H17P's announce subtype onto the canonical
		 * one so no downstream check has to know the difference.
		 */
		memset(&h17p_eshdr, 0, sizeof(h17p_eshdr));
		h17p_eshdr.category = c->category;
		h17p_eshdr.type = cpu_to_le16(c->type == EPIC_SUBTYPE_ANNOUNCE_H17P ?
					      EPIC_SUBTYPE_ANNOUNCE : c->type);
		h17p_eshdr.tag = cpu_to_le16(le32_to_cpu(c->tag));
		eshdr = &h17p_eshdr;
	}

	if (data_size < hdr_len)
		goto too_small;

	subtype = le16_to_cpu(eshdr->type);
	payload = data + hdr_len;
	payload_size = data_size - hdr_len;

	trace_afk_recv_handle(ep, channel, type, data_size, ehdr, eshdr);

	service = afk_epic_find_service(ep, channel);

	if (!service) {
		if (type != EPIC_TYPE_NOTIFY && type != EPIC_TYPE_REPLY) {
			dev_err(ep->dcp->dev,
				"AFK[ep:%02x]: expected notify but got 0x%x on channel %d\n",
				ep->endpoint, type, channel);
			return;
		}
		if (eshdr->category != EPIC_CAT_REPORT) {
			if (ep->endpoint == SYSTEM_ENDPOINT &&
			    afk_endpoint_has_v2_rx(ep))
				dev_dbg(ep->dcp->dev,
					"AFK[ep:20]: unmatched H17P service category %#x type %#x on channel %u\n",
					eshdr->category, subtype, channel);
			else
				dev_err(ep->dcp->dev,
					"AFK[ep:%02x]: expected report but got 0x%x on channel %d\n",
					ep->endpoint, eshdr->category, channel);
			return;
		}
		if (subtype == EPIC_SUBTYPE_TEARDOWN) {
			dev_dbg(ep->dcp->dev,
				"AFK[ep:%02x]: teardown without service on channel %d\n",
				ep->endpoint, channel);
			return;
		}
		if (subtype != EPIC_SUBTYPE_ANNOUNCE) {
			/* Unsolicited V2 reports can precede their service publish. */
			if (ep->endpoint == SYSTEM_ENDPOINT &&
			    afk_endpoint_has_v2_rx(ep))
				dev_dbg(ep->dcp->dev,
					"AFK[ep:20]: unmatched H17P report type %#x on channel %u\n",
					subtype, channel);
			else
				dev_err(ep->dcp->dev,
					"AFK[ep:%02x]: expected announce but got 0x%x on channel %d\n",
					ep->endpoint, subtype, channel);
			return;
		}

		return afk_recv_handle_init(ep, channel, payload, payload_size);
	}

	if (!service) {
		dev_err(ep->dcp->dev, "AFK[ep:%02x]: channel %d has no service\n",
			ep->endpoint, channel);
		return;
	}

	/*
	 * On H17P the announce/report decode above is already V2, but the
	 * command/response exchange for a registered service uses the V2
	 * envelope (inline payload, category byte, tag at +25).  Handle it
	 * here; the classic checks below never match a V2 message.
	 */
	if (afk_endpoint_has_v2_rx(ep)) {
		afk_recv_handle_v2(ep, service, data, data_size);
		return;
	}

	if (type == EPIC_TYPE_NOTIFY && eshdr->category == EPIC_CAT_REPORT &&
	    subtype == EPIC_SUBTYPE_TEARDOWN)
		return afk_recv_handle_teardown(ep, channel);

	if (type == EPIC_TYPE_REPLY && eshdr->category == EPIC_CAT_REPLY)
		return afk_recv_handle_reply(ep, channel,
					     le16_to_cpu(eshdr->tag), payload,
					     payload_size);

	if (subtype == EPIC_SUBTYPE_STD_SERVICE)
		return afk_recv_handle_std_service(
			ep, channel, type, ehdr, eshdr, payload, payload_size);

	dev_err(ep->dcp->dev, "AFK[ep:%02x]: channel %d received unhandled message "
		"(type %x subtype %x)\n", ep->endpoint, channel, type, subtype);
	print_hex_dump(KERN_INFO, "AFK: ", DUMP_PREFIX_NONE, 16, 1, payload,
				   payload_size, true);
	return;

too_small:
	dev_err(ep->dcp->dev, "AFK[ep:%02x]: payload too small: %lx\n",
		ep->endpoint, data_size);
}

static bool afk_recv(struct apple_dcp_afkep *ep)
{
	struct afk_qe *hdr;
	u32 rptr, wptr;
	u32 magic, size, channel, type;

	if (!ep->rxbfr.ready) {
		dev_err(ep->dcp->dev, "AFK[ep:%02x]: got RECV but not ready\n",
			ep->endpoint);
		return false;
	}

	rptr = le32_to_cpu(*afk_rb_rptr(&ep->rxbfr));
	wptr = le32_to_cpu(*afk_rb_wptr(&ep->rxbfr));
	trace_afk_recv_rwptr_pre(ep, rptr, wptr);

	if (rptr == wptr)
		return false;

	if (rptr > (ep->rxbfr.bufsz - sizeof(*hdr))) {
		dev_warn(ep->dcp->dev,
			 "AFK[ep:%02x]: rptr out of bounds: 0x%x > 0x%lx\n",
			 ep->endpoint, rptr, ep->rxbfr.bufsz - sizeof(*hdr));
		return false;
	}

	dma_rmb();

	hdr = ep->rxbfr.buf + rptr;
	magic = le32_to_cpu(hdr->magic);
	size = le32_to_cpu(hdr->size);
	trace_afk_recv_qe(ep, rptr, magic, size);

	if (magic != QE_MAGIC && !ep->dumped_hdr) {
		ep->dumped_hdr = true;
		dev_err(ep->dcp->dev,
			"AFK[ep:%02x]: rptr %#x wptr %#x block %#x bufsz %#zx datoff %#tx\n",
			ep->endpoint, rptr, wptr, ep->rxbfr.block,
			ep->rxbfr.bufsz,
			(u8 *)ep->rxbfr.buf - (u8 *)ep->rxbfr.hdr);
		print_hex_dump(KERN_ERR, "AFKrx: ", DUMP_PREFIX_OFFSET, 16, 4,
			       (u8 *)ep->rxbfr.buf, 0x500, false);
	}

	if (magic != QE_MAGIC) {
		dev_warn(ep->dcp->dev, "AFK[ep:%02x]: invalid queue entry magic: 0x%x\n",
			 ep->endpoint, magic);
		return false;
	}

	/*
	 * If there's not enough space for the payload the co-processor inserted
	 * the current dummy queue entry and we have to advance to the next one
	 * which will contain the real data.
	*/
	if (rptr + size + sizeof(*hdr) > ep->rxbfr.bufsz) {
		rptr = 0;
		hdr = ep->rxbfr.buf + rptr;
		magic = le32_to_cpu(hdr->magic);
		size = le32_to_cpu(hdr->size);
		trace_afk_recv_qe(ep, rptr, magic, size);

		if (magic != QE_MAGIC) {
			dev_warn(ep->dcp->dev,
				 "AFK[ep:%02x]: invalid next queue entry magic: 0x%x\n",
				 ep->endpoint, magic);
			return false;
		}

		*afk_rb_rptr(&ep->rxbfr) = cpu_to_le32(rptr);
	}

	if (rptr + size + sizeof(*hdr) > ep->rxbfr.bufsz) {
		dev_warn(ep->dcp->dev,
			 "AFK[ep:%02x]: queue entry out of bounds: 0x%lx > 0x%lx\n",
			 ep->endpoint, rptr + size + sizeof(*hdr), ep->rxbfr.bufsz);
		return false;
	}

	channel = le32_to_cpu(hdr->channel);
	type = le32_to_cpu(hdr->type);

	rptr = ALIGN(rptr + sizeof(*hdr) + size, ep->rxbfr.block);
	if (WARN_ON(rptr > ep->rxbfr.bufsz))
		rptr = 0;
	if (rptr == ep->rxbfr.bufsz)
		rptr = 0;

	dma_mb();

	*afk_rb_rptr(&ep->rxbfr) = cpu_to_le32(rptr);
	trace_afk_recv_rwptr_post(ep, rptr, wptr);

	/*
	 * TODO: this is theoretically unsafe since DCP could overwrite data
	 *       after the read pointer was updated above. Do it anyway since
	 *       it avoids 2 problems in the DCP tracer:
	 *       1. the tracer sees replies before the notifies from dcp
	 *       2. the tracer tries to read buffers after they are unmapped.
	 */
	afk_recv_handle(ep, channel, type, hdr->data, size);

	return true;
}

static void afk_receive_message_worker(struct work_struct *work_)
{
	struct afk_receive_message_work *work;
	u16 type;

	work = container_of(work_, struct afk_receive_message_work, work);

	type = FIELD_GET(RBEP_TYPE, work->message);
	switch (type) {
	case RBEP_INIT_ACK:
		break;

	case RBEP_START_ACK:
		complete_all(&work->ep->started);
		break;

	case RBEP_SHUTDOWN_ACK:
		complete_all(&work->ep->stopped);
		break;

	case RBEP_GETBUF:
		afk_getbuf(work->ep, work->message);
		break;

	case RBEP_INIT_TX:
		afk_init_rxtx(work->ep, work->message, &work->ep->txbfr);
		break;

	case RBEP_INIT_RX:
		afk_init_rxtx(work->ep, work->message, &work->ep->rxbfr);
		break;

	case RBEP_RECV:
		while (afk_recv(work->ep))
			;
		break;

	default:
		dev_err(work->ep->dcp->dev,
			"Received unknown AFK message type: 0x%x\n", type);
	}

	kfree(work);
}

int afk_receive_message(struct apple_dcp_afkep *ep, u64 message)
{
	struct afk_receive_message_work *work;

	// TODO: comment why decoupling from rtkit thread is required here
	work = kzalloc(sizeof(*work), GFP_KERNEL);
	if (!work)
		return -ENOMEM;

	work->ep = ep;
	work->message = message;

	dev_dbg(ep->dcp->dev, "J700_AFK_RX: ep:%02x message=%#llx type=%#llx\n",
		ep->endpoint, message, (u64)FIELD_GET(RBEP_TYPE, message));

	INIT_WORK(&work->work, afk_receive_message_worker);
	queue_work(ep->wq, &work->work);

	return 0;
}

int afk_send_epic(struct apple_dcp_afkep *ep, u32 channel, u16 tag,
		  enum epic_type etype, enum epic_category ecat, u8 stype,
		  const void *payload, size_t payload_len)
{
	u32 rptr, wptr;
	struct afk_qe *hdr, *hdr2;
	struct epic_hdr *ehdr;
	struct epic_sub_hdr *eshdr;
	unsigned long flags;
	size_t total_epic_size, total_size;
	int ret;

	spin_lock_irqsave(&ep->lock, flags);

	dma_rmb();
	rptr = le32_to_cpu(*afk_rb_rptr(&ep->txbfr));
	wptr = le32_to_cpu(*afk_rb_wptr(&ep->txbfr));
	trace_afk_send_rwptr_pre(ep, rptr, wptr);
	total_epic_size = sizeof(*ehdr) + sizeof(*eshdr) + payload_len;
	total_size = sizeof(*hdr) + total_epic_size;

	hdr = hdr2 = NULL;

	/*
	 * We need to figure out how to place the entire headers and payload
	 * into the ring buffer:
	 * - If the write pointer is in front of the read pointer we just need
	 *   enough space inbetween to store everything.
	 * - If the read pointer has already wrapper around the end of the
	 *   buffer we can
	 *    a) either store the entire payload at the writer pointer if
	 *       there's enough space until the end,
	 *    b) or just store the queue entry at the write pointer to indicate
	 *       that we need to wrap to the start and then store the headers
	 *       and the payload at the beginning of the buffer. The queue
	 *       header has to be store twice in this case.
	 * In either case we have to ensure that there's always enough space
	 * so that we don't accidentally overwrite other buffers.
	 */
	if (wptr < rptr) {
		/*
		 * If wptr < rptr we can't wrap around and only have to make
		 * sure that there's enough space for the entire payload.
		 */
		if (wptr + total_size > rptr) {
			ret = -ENOMEM;
			goto out;
		}

		hdr = ep->txbfr.buf + wptr;
		wptr += sizeof(*hdr);
	} else {
		/* We need enough space to place at least a queue entry */
		if (wptr + sizeof(*hdr) > ep->txbfr.bufsz) {
			ret = -ENOMEM;
			goto out;
		}

		/*
		 * If we can place a single queue entry but not the full payload
		 * we need to place one queue entry at the end of the ring
		 * buffer and then another one together with the entire
		 * payload at the beginning.
		 */
		if (wptr + total_size > ep->txbfr.bufsz) {
			/*
			 * Ensure there's space for the  queue entry at the
			 * beginning
			 */
			if (sizeof(*hdr) > rptr) {
				ret = -ENOMEM;
				goto out;
			}

			/*
			 * Place two queue entries to indicate we want to wrap
			 * around to the firmware.
			 */
			hdr = ep->txbfr.buf + wptr;
			hdr2 = ep->txbfr.buf;
			wptr = sizeof(*hdr);

			/* Ensure there's enough space for the entire payload */
			if (wptr + total_epic_size > rptr) {
				ret = -ENOMEM;
				goto out;
			}
		} else {
			/* We have enough space to place the entire payload */
			hdr = ep->txbfr.buf + wptr;
			wptr += sizeof(*hdr);
		}
	}
	/*
	 * At this point we're guaranteed that hdr (and possibly hdr2) point
	 * to a buffer large enough to fit the queue entry and that we have
	 * enough space at wptr to store the payload.
	 */

	hdr->magic = cpu_to_le32(QE_MAGIC);
	hdr->size = cpu_to_le32(total_epic_size);
	hdr->channel = cpu_to_le32(channel);
	hdr->type = cpu_to_le32(etype);
	if (hdr2)
		memcpy(hdr2, hdr, sizeof(*hdr));

	ehdr = ep->txbfr.buf + wptr;
	memset(ehdr, 0, sizeof(*ehdr));
	ehdr->version = 2;
	ehdr->seq = cpu_to_le16(ep->qe_seq++);
	ehdr->timestamp = cpu_to_le64(0);
	wptr += sizeof(*ehdr);

	eshdr = ep->txbfr.buf + wptr;
	memset(eshdr, 0, sizeof(*eshdr));
	eshdr->length = cpu_to_le32(payload_len);
	eshdr->version = 4;
	eshdr->category = ecat;
	eshdr->type = cpu_to_le16(stype);
	eshdr->timestamp = cpu_to_le64(0);
	eshdr->tag = cpu_to_le16(tag);
	if (ecat == EPIC_CAT_REPLY)
		eshdr->inline_len = cpu_to_le32(payload_len - 4);
	else
		eshdr->inline_len = cpu_to_le32(0);
	wptr += sizeof(*eshdr);

	memcpy(ep->txbfr.buf + wptr, payload, payload_len);
	wptr += payload_len;
	wptr = ALIGN(wptr, ep->txbfr.block);
	if (wptr == ep->txbfr.bufsz)
		wptr = 0;
	trace_afk_send_rwptr_post(ep, rptr, wptr);

	*afk_rb_wptr(&ep->txbfr) = cpu_to_le32(wptr);
	afk_send(ep, FIELD_PREP(RBEP_TYPE, RBEP_SEND) |
			     FIELD_PREP(SEND_WPTR, wptr));
	ret = 0;

out:
	spin_unlock_irqrestore(&ep->lock, flags);
	return ret;
}

/*
 * H17P / AFKEP-V2 command.  Unlike the classic path there is no rxbuf/txbuf
 * DMA descriptor: the whole request (struct epic_service_call + data, i.e. the
 * caller's V1 payload) is written inline into the ring at offset 32, and the
 * response body is copied back out of the RX ring by
 * afk_recv_handle_reply_v2().  The service is OPENed first if it has not been;
 * the firmware ignores commands on an unopened service.
 */
static int afk_send_command_v2(struct apple_epic_service *service, u8 type,
			       const void *payload, size_t payload_len,
			       void *output, size_t output_len, u32 *retcode)
{
	struct apple_dcp_afkep *ep = service->ep;
	DECLARE_COMPLETION_ONSTACK(completion);
	unsigned long flags;
	size_t bytes;
	u8 *pkt;
	u8 tag;
	int idx, ret;

	/*
	 * Only the standard group/command service call (used by afk_service_call
	 * and every DP path) is characterised for V2.  The direct-subtype form
	 * (systemep's setProperty, verbose-logging debug only) is not, so reject
	 * it rather than emit a malformed guess.
	 */
	if (type != EPIC_SUBTYPE_STD_SERVICE) {
		dev_warn_once(ep->dcp->dev,
			      "AFK[ep:%02x]: V2 direct command type %#x not implemented\n",
			      ep->endpoint, type);
		return -EINVAL;
	}

	/* Native opens a service before it will accept any command. */
	if (!READ_ONCE(service->opened)) {
		ret = afk_service_open_v2(service);
		if (ret)
			return ret;
		WRITE_ONCE(service->opened, true);
		dev_info(ep->dcp->dev, "J700_AFK_V2_OPEN: ep:%02x if:%u\n",
			 ep->endpoint, service->channel);
	}

	bytes = EPIC_V2_HDR + payload_len;
	pkt = kzalloc(bytes, GFP_KERNEL);
	if (!pkt)
		return -ENOMEM;

	spin_lock_irqsave(&service->lock, flags);
	idx = bitmap_find_free_region(service->cmd_map, MAX_PENDING_CMDS, 0);
	if (idx < 0) {
		spin_unlock_irqrestore(&service->lock, flags);
		kfree(pkt);
		return -ENOSPC;
	}
	tag = idx + 1; /* 1..MAX_PENDING_CMDS; 0 is never a live tag */
	service->cmds[idx].tag = tag;
	service->cmds[idx].rxbuf = output;
	service->cmds[idx].txbuf = NULL;
	service->cmds[idx].rxbuf_dma = 0;
	service->cmds[idx].txbuf_dma = 0;
	service->cmds[idx].rxlen = output_len;
	service->cmds[idx].txlen = 0;
	service->cmds[idx].free_on_ack = false;
	service->cmds[idx].done = false;
	service->cmds[idx].completion = &completion;
	pkt[0] = service->seq++;
	spin_unlock_irqrestore(&service->lock, flags);

	put_unaligned_le16(service->channel, pkt + 2);
	put_unaligned_le32(bytes - 8, pkt + 4);
	pkt[16] = EPIC_V2_TYPE_COMMAND;
	pkt[17] = EPIC_V2_CAT_COMMAND;
	pkt[18] = 0;
	pkt[24] = 0; /* inline, no out-of-band buffers */
	pkt[25] = tag;
	put_unaligned_le32(payload_len, pkt + 28); /* response capacity */
	memcpy(pkt + EPIC_V2_HDR, payload, payload_len);

	dev_info(ep->dcp->dev,
		 "J700_AFK_V2_CMD: ep:%02x if:%u tag:%u cat:1 cap:%zu bytes:%zu\n",
		 ep->endpoint, service->channel, tag, payload_len, bytes);

	ret = afk_ring_write_v2(ep, pkt, bytes);
	kfree(pkt);
	if (ret)
		goto err_free_cmd;

	ret = wait_for_completion_timeout(&completion,
					  msecs_to_jiffies(MSEC_PER_SEC));
	if (ret <= 0) {
		spin_lock_irqsave(&service->lock, flags);
		if (!service->cmds[idx].done) {
			/* let a late response free the slot; don't reuse it now */
			service->cmds[idx].completion = NULL;
			service->cmds[idx].free_on_ack = true;
			spin_unlock_irqrestore(&service->lock, flags);
			return -ETIMEDOUT;
		}
		spin_unlock_irqrestore(&service->lock, flags);
	}

	ret = 0;
	if (retcode)
		*retcode = service->cmds[idx].retcode;
	/* output was filled by afk_recv_handle_reply_v2() before completion */

err_free_cmd:
	spin_lock_irqsave(&service->lock, flags);
	bitmap_release_region(service->cmd_map, idx, 0);
	spin_unlock_irqrestore(&service->lock, flags);
	return ret;
}

int afk_send_command(struct apple_epic_service *service, u8 type,
		     const void *payload, size_t payload_len, void *output,
		     size_t output_len, u32 *retcode)
{
	struct epic_cmd cmd;
	void *rxbuf, *txbuf;
	dma_addr_t rxbuf_dma, txbuf_dma;
	unsigned long flags;
	int ret, idx;
	u16 tag;
	struct apple_dcp_afkep *ep = service->ep;
	DECLARE_COMPLETION_ONSTACK(completion);

	/* H17P speaks AFKEP-V2: inline command/response, no DMA descriptor. */
	if (afk_endpoint_is_v2(ep))
		return afk_send_command_v2(service, type, payload, payload_len,
					   output, output_len, retcode);

	rxbuf = dma_alloc_coherent(ep->dcp->dev, output_len, &rxbuf_dma,
				   GFP_KERNEL);
	if (!rxbuf)
		return -ENOMEM;
	txbuf = dma_alloc_coherent(ep->dcp->dev, payload_len, &txbuf_dma,
				   GFP_KERNEL);
	if (!txbuf) {
		ret = -ENOMEM;
		goto err_free_rxbuf;
	}

	memcpy(txbuf, payload, payload_len);

	memset(&cmd, 0, sizeof(cmd));
	cmd.retcode = cpu_to_le32(0);
	cmd.rxbuf = cpu_to_le64(rxbuf_dma);
	cmd.rxlen = cpu_to_le32(output_len);
	cmd.txbuf = cpu_to_le64(txbuf_dma);
	cmd.txlen = cpu_to_le32(payload_len);

	spin_lock_irqsave(&service->lock, flags);
	idx = bitmap_find_free_region(service->cmd_map, MAX_PENDING_CMDS, 0);
	if (idx < 0) {
		ret = -ENOSPC;
		goto err_unlock;
	}

	tag = (service->cmd_tag & 0xff) << 8;
	tag |= idx & 0xff;
	service->cmd_tag++;

	service->cmds[idx].tag = tag;
	service->cmds[idx].rxbuf = rxbuf;
	service->cmds[idx].txbuf = txbuf;
	service->cmds[idx].rxbuf_dma = rxbuf_dma;
	service->cmds[idx].txbuf_dma = txbuf_dma;
	service->cmds[idx].rxlen = output_len;
	service->cmds[idx].txlen = payload_len;
	service->cmds[idx].free_on_ack = false;
	service->cmds[idx].done = false;
	service->cmds[idx].completion = &completion;
	init_completion(&completion);

	spin_unlock_irqrestore(&service->lock, flags);

	ret = afk_send_epic(service->ep, service->channel, tag,
			    EPIC_TYPE_COMMAND, EPIC_CAT_COMMAND, type, &cmd,
			    sizeof(cmd));
	if (ret)
		goto err_free_cmd;

	ret = wait_for_completion_timeout(&completion,
					  msecs_to_jiffies(MSEC_PER_SEC));

	if (ret <= 0) {
		spin_lock_irqsave(&service->lock, flags);
		/*
		 * Check again while we're inside the lock to make sure
		 * the command wasn't completed just after
		 * wait_for_completion_timeout returned.
		 */
		if (!service->cmds[idx].done) {
			service->cmds[idx].completion = NULL;
			service->cmds[idx].free_on_ack = true;
			spin_unlock_irqrestore(&service->lock, flags);
			return -ETIMEDOUT;
		}
		spin_unlock_irqrestore(&service->lock, flags);
	}

	ret = 0;
	if (retcode)
		*retcode = service->cmds[idx].retcode;
	if (output && output_len)
		memcpy(output, rxbuf, output_len);

err_free_cmd:
	spin_lock_irqsave(&service->lock, flags);
	bitmap_release_region(service->cmd_map, idx, 0);
err_unlock:
	spin_unlock_irqrestore(&service->lock, flags);
	dma_free_coherent(ep->dcp->dev, payload_len, txbuf, txbuf_dma);
err_free_rxbuf:
	dma_free_coherent(ep->dcp->dev, output_len, rxbuf, rxbuf_dma);
	return ret;
}

int afk_service_call(struct apple_epic_service *service, u16 group, u32 command,
		     const void *data, size_t data_len, size_t data_pad,
		     void *output, size_t output_len, size_t output_pad)
{
	struct epic_service_call *call;
	void *bfr;
	size_t bfr_len = max(data_len + data_pad, output_len + output_pad) +
			 sizeof(*call);
	int ret;
	u32 retcode;
	u32 retlen;

	bfr = kzalloc(bfr_len, GFP_KERNEL);
	if (!bfr)
		return -ENOMEM;

	call = bfr;

	memset(call, 0, sizeof(*call));
	call->group = cpu_to_le16(group);
	call->command = cpu_to_le32(command);
	call->data_len = cpu_to_le32(data_len + data_pad);
	call->magic = cpu_to_le32(EPIC_SERVICE_CALL_MAGIC);

	memcpy(bfr + sizeof(*call), data, data_len);

	ret = afk_send_command(service, EPIC_SUBTYPE_STD_SERVICE, bfr, bfr_len,
			       bfr, bfr_len, &retcode);
	if (ret)
		goto out;
	if (retcode) {
		ret = -EINVAL;
		goto out;
	}
	if (le32_to_cpu(call->magic) != EPIC_SERVICE_CALL_MAGIC ||
	    le16_to_cpu(call->group) != group ||
	    le32_to_cpu(call->command) != command) {
		ret = -EINVAL;
		goto out;
	}

	retlen = le32_to_cpu(call->data_len);
	if (output_len < retlen)
		retlen = output_len;
	if (output && output_len) {
		memset(output, 0, output_len);
		memcpy(output, bfr + sizeof(*call), retlen);
	}

out:
	kfree(bfr);
	return ret;
}

#if IS_ENABLED(CONFIG_DRM_APPLE_DEBUG)

#define AFK_DEBUGFS_MAX_REPLY 8192

static ssize_t service_call_write_file(struct file *file, const char __user *user_buf,
				       size_t count, loff_t *ppos)
{
	struct apple_epic_service *srv = file->private_data;
	void *buf;
	int ret;
	struct {
		u32 group;
		u32 command;
	} call_info;

	if (count < sizeof(call_info))
		return -EINVAL;
	if (!srv->debugfs.scratch) {
		srv->debugfs.scratch = \
			devm_kzalloc(srv->ep->dcp->dev, AFK_DEBUGFS_MAX_REPLY, GFP_KERNEL);
		if (!srv->debugfs.scratch)
			return -ENOMEM;
	}

	ret = copy_from_user(&call_info, user_buf, sizeof(call_info));
	if (ret == sizeof(call_info))
		return -EFAULT;
	user_buf += sizeof(call_info);
	count -= sizeof(call_info);

	buf = kmalloc(count, GFP_KERNEL);
	if (!buf)
		return -ENOMEM;
	ret = copy_from_user(buf, user_buf, count);
	if (ret == count) {
		kfree(buf);
		return -EFAULT;
	}

	memset(srv->debugfs.scratch, 0, AFK_DEBUGFS_MAX_REPLY);
	dma_mb();

	ret = afk_service_call(srv, call_info.group, call_info.command, buf, count, 0,
			       srv->debugfs.scratch, AFK_DEBUGFS_MAX_REPLY, 0);
	kfree(buf);

	if (ret < 0)
		return ret;

	return count + sizeof(call_info);
}

static ssize_t service_call_read_file(struct file *file, char __user *user_buf,
				      size_t count, loff_t *ppos)
{
	struct apple_epic_service *srv = file->private_data;

	if (!srv->debugfs.scratch)
		return -EINVAL;

	return simple_read_from_buffer(user_buf, count, ppos,
				       srv->debugfs.scratch, AFK_DEBUGFS_MAX_REPLY);
}

static const struct file_operations service_call_fops = {
	.open = simple_open,
	.write = service_call_write_file,
	.read = service_call_read_file,
};

static ssize_t service_raw_call_write_file(struct file *file, const char __user *user_buf,
					   size_t count, loff_t *ppos)
{
	struct apple_epic_service *srv = file->private_data;
	u32 retcode;
	int ret;

	if (!srv->debugfs.scratch) {
		srv->debugfs.scratch = \
			devm_kzalloc(srv->ep->dcp->dev, AFK_DEBUGFS_MAX_REPLY, GFP_KERNEL);
		if (!srv->debugfs.scratch)
			return -ENOMEM;
	}

	memset(srv->debugfs.scratch, 0, AFK_DEBUGFS_MAX_REPLY);
	ret = copy_from_user(srv->debugfs.scratch, user_buf, count);
	if (ret == count)
		return -EFAULT;

	ret = afk_send_command(srv, EPIC_SUBTYPE_STD_SERVICE, srv->debugfs.scratch, count,
			       srv->debugfs.scratch, AFK_DEBUGFS_MAX_REPLY, &retcode);
	if (ret < 0)
		return ret;
	if (retcode)
		return -EINVAL;

	return count;
}

static ssize_t service_raw_call_read_file(struct file *file, char __user *user_buf,
					  size_t count, loff_t *ppos)
{
	struct apple_epic_service *srv = file->private_data;

	if (!srv->debugfs.scratch)
		return -EINVAL;

	return simple_read_from_buffer(user_buf, count, ppos,
				       srv->debugfs.scratch, AFK_DEBUGFS_MAX_REPLY);
}

static const struct file_operations service_raw_call_fops = {
	.open = simple_open,
	.write = service_raw_call_write_file,
	.read = service_raw_call_read_file,
};

static void afk_populate_service_debugfs(struct apple_epic_service *srv)
{
	if (!srv->ep->debugfs_entry || !srv->ops)
		return;

	if (strcmp(srv->ops->name, "DCPAVAudioInterface") == 0) {
		srv->debugfs.entry = debugfs_create_dir(srv->ops->name,
							srv->ep->debugfs_entry);
		debugfs_create_file("call", 0600, srv->debugfs.entry, srv,
				&service_call_fops);
		debugfs_create_file("raw_call", 0600, srv->debugfs.entry, srv,
				&service_raw_call_fops);
	}
}

static void afk_remove_service_debugfs(struct apple_epic_service *srv)
{
	if (srv->debugfs.entry) {
		debugfs_remove_recursive(srv->debugfs.entry);
		srv->debugfs.entry = NULL;
	}
}

#endif
