/* SPDX-License-Identifier: BSD-3-Clause-Clear */
#ifndef __MT7932_MCU_LAB_H
#define __MT7932_MCU_LAB_H

#include <linux/types.h>

struct dentry;
struct sk_buff;
struct mt792x_dev;

#define NEO_WIFID_MAGIC		0x3146574eU /* "NWF1" */
#define NEO_WIFID_VERSION	1
#define NEO_WIFID_MAX_REQUEST	4096
#define NEO_WIFID_MAX_RECORD	16384

enum neo_wifid_opcode {
	NEO_WIFID_OP_SUBMIT = 1,
	NEO_WIFID_OP_RESET = 2,
	NEO_WIFID_OP_STATE = 3,
	NEO_WIFID_OP_ARM_RUNTIME_EPOCH = 4,
	NEO_WIFID_OP_RUNTIME_EPOCH = 5,
	NEO_WIFID_OP_CONTINUE = 6,
};

enum neo_wifid_transport {
	NEO_WIFID_TRANSPORT_CE = 0,
	NEO_WIFID_TRANSPORT_EXT_RAW = 1,
	NEO_WIFID_TRANSPORT_UNI = 2,
	NEO_WIFID_TRANSPORT_LEGACY_RAW = 3,
};

enum neo_wifid_request_flag {
	NEO_WIFID_F_QUERY = BIT(0),
	NEO_WIFID_F_WIRE_ACK = BIT(1),
	NEO_WIFID_F_WAIT_RESPONSE = BIT(2),
	NEO_WIFID_F_DRAIN_TX = BIT(3),
};

#define NEO_WIFID_REQUEST_FLAGS \
	(NEO_WIFID_F_QUERY | NEO_WIFID_F_WIRE_ACK | \
	 NEO_WIFID_F_WAIT_RESPONSE | NEO_WIFID_F_DRAIN_TX)

enum neo_wifid_record_type {
	NEO_WIFID_RECORD_SUBMIT = 1,
	NEO_WIFID_RECORD_RESPONSE = 2,
	NEO_WIFID_RECORD_EVENT = 3,
	NEO_WIFID_RECORD_STATE = 4,
	NEO_WIFID_RECORD_DROP = 5,
};

enum neo_wifid_phase {
	NEO_WIFID_PHASE_CHECKPOINT = 1,
	NEO_WIFID_PHASE_EPOCH_ARMED = 2,
	NEO_WIFID_PHASE_EPOCH_COMMAND = 3,
	NEO_WIFID_PHASE_EPOCH_DONE = 4,
	NEO_WIFID_PHASE_CONTINUED = 5,
	NEO_WIFID_PHASE_ERROR = 6,
};

struct neo_wifid_request {
	__le32 magic;
	__le16 version;
	__le16 opcode;
	__le32 flags;
	__le32 cookie;
	__le32 transport;
	__le32 cid;
	__le32 ext_cid;
	__le32 timeout_ms;
	__le32 payload_len;
	__le32 reserved;
} __packed;

struct neo_wifid_record {
	__le32 magic;
	__le16 version;
	__le16 type;
	__le32 flags;
	__le32 cookie;
	__le32 status;
	__le32 command;
	__le32 sequence;
	__le32 payload_len;
	__le32 reserved0;
	__le32 reserved1;
	__le64 monotonic_ns;
} __packed;

struct neo_wifid_state {
	__le32 phase;
	__le32 flags;
	__le64 phy_state;
	__le32 command_count;
	__le32 pending_responses;
	__le32 queued_records;
	__le32 dropped_events;
	__le32 tx17_cidx;
	__le32 tx17_didx;
	__le32 tx17_head;
	__le32 tx17_tail;
	__le32 tx17_queued;
} __packed;

int mt7932_mcu_runtime_epoch(struct mt792x_dev *dev);
#if IS_ENABLED(CONFIG_MT7921_J700_MCU_LAB)
bool mt7932_mcu_lab_conformance_active(void);
int mt7932_mcu_lab_checkpoint(struct mt792x_dev *dev);
void mt7932_mcu_lab_destroy(struct mt792x_dev *dev);
bool mt7932_mcu_lab_rx(struct mt792x_dev *dev, struct sk_buff *skb);
#else
static inline bool mt7932_mcu_lab_conformance_active(void)
{
	return false;
}

static inline int mt7932_mcu_lab_checkpoint(struct mt792x_dev *dev)
{
	return 0;
}

static inline void mt7932_mcu_lab_destroy(struct mt792x_dev *dev)
{
}

static inline bool mt7932_mcu_lab_rx(struct mt792x_dev *dev,
				     struct sk_buff *skb)
{
	return false;
}
#endif

#endif /* __MT7932_MCU_LAB_H */
