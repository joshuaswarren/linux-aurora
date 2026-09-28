/* SPDX-License-Identifier: GPL-2.0-only */
#ifndef MT7932_H
#define MT7932_H


#include <linux/completion.h>
#include <linux/delay.h>
#include <linux/debugfs.h>
#include <linux/dma-mapping.h>
#include <linux/firmware.h>
#include <linux/interrupt.h>
#include <linux/irq.h>
#include <linux/iommu.h>
#include <linux/iopoll.h>
#include <linux/module.h>
#include <linux/of.h>
#include <linux/of_net.h>
#include <linux/etherdevice.h>
#include <linux/ethtool.h>
#include <linux/if_vlan.h>
#include <linux/pci.h>
#include <linux/random.h>
#include <linux/ktime.h>
#include <linux/workqueue.h>
#include <linux/mutex.h>
#include <net/cfg80211.h>
#include <net/gro_cells.h>
#include "firmware.h"
#include "calibration.h"
#include "packet.h"
#include "scan.h"
#include "association.h"
#include "bandwidth.h"
#include "regulatory.h"


#define W 0xd4000
#define H 0xd6000
#define C 0xd7000
#define IRQ_MASK 0xe2c8000d
#define DONE BIT(31)
#define STRIDE 8192
#define RX_STRIDE ALIGN(MT7932_RX_CAPACITY, 64)

struct mt7932_desc {
	__le32 address, control, second, info;
};

struct mt7932_ring {
	struct mt7932_desc *desc;
	dma_addr_t dma, buffers_dma;
	u8 *buffers;
	u32 reg;
	u16 count, head, tail, queued;
	u16 slots, used;
};

struct mt7932 {
	struct pci_dev *pdev;
	void __iomem *bar;
	struct mt7932_ring tx[19], rx[9];
	struct completion response;
	struct completion cal_response;
	struct mt7932_cal_completion cal_state;
	u8 cal_requests[32][16];
	u8 cal_request_seq[32];
	unsigned int cal_request_head, cal_request_count;
	spinlock_t response_lock;
	u8 seq, waiting;
	u8 mac[ETH_ALEN];
	u8 reply[MT7932_RX_CAPACITY];
	size_t reply_length;
	u64 interrupts, packets;
	bool running;
	bool irq_vectors_allocated, irq_requested;
	bool initialized, suspend_running;
	bool removing;
	bool ram_config;
	bool runtime_epoch;
	bool runtime_cal_valid;
	u8 runtime_cal, board_type, smart_version, preload_version;
	u8 module_byte;
	u8 six_ghz;
	bool six_ghz_valid;
	bool stof_supported;
	u8 phy_cap[12];
	u8 antenna_mask;
	struct wiphy *wiphy;
	struct ieee80211_supported_band band2;
	struct ieee80211_channel channels[MT7932_CHANNELS_2G];
	struct ieee80211_channel channels5[MT7932_CHANNELS_5G];
	struct ieee80211_supported_band band5;
	struct ieee80211_sband_iftype_data band5_iftype;
	unsigned int bw_limit;
	bool bw_fallback, bw_policy_pending, fw_asserted;
	bool bw_cal_active, bw_cal_rejected;
	u8 assoc_width, assoc_sco, assoc_center;
	u8 scan_band, scan_batch;
	struct ieee80211_rate rates[12];
	struct work_struct startup_work;
	struct mutex command_mutex;
	bool startup_started, rf_ready, reg_pending, policy_failed;
	bool interface_registered, interface_up;
	u32 reg_generation, reg_attempted;
	struct mt7932_reg_snapshot reg_desired;
	struct net_device *netdev;
	struct wireless_dev wdev;
	struct cfg80211_scan_request *scan_request;
	struct work_struct scan_finish_work;
	struct work_struct cal_work;
	struct delayed_work scan_timeout_work;
	u8 scan_seq, retired_scan_seq, scan_home_channel;
	bool scan_finished, scan_aborted, bss_active;
	u64 beacons;
	unsigned long peer_alloc;
	struct mt7932_peer_ids peer_ids;
	struct cfg80211_bss *connect_bss;

	/*
	 * RSSI of the associated AP, taken from its own beacons, and the
	 * connection-quality thresholds wpa_supplicant asks us to watch.  Written
	 * from the RX path, read from cfg80211 ops, so keep them access-once.
	 */
	int assoc_signal_dbm;
	bool assoc_signal_valid;
	s32 cqm_rssi_thold;
	u32 cqm_rssi_hyst;
	int cqm_rssi_state;
	struct work_struct connect_work, disconnect_work;
	struct completion assoc_update, assoc_done, disconnect_done;
	bool assoc_pending; /* response_lock; freezes the calibration parameters */
	struct completion discovery_done;
	struct completion scan_done;
	struct ieee80211_channel *connect_channel_req, *connect_channel_hint;
	u8 connect_bssid_req[ETH_ALEN], connect_bssid_hint[ETH_ALEN];
	bool discovering, discovery_finished, connect_cancelled;
	u8 connect_ssid[32], connect_pmk[32], connect_bssid[6];
	u8 connect_ssid_length, connect_channel, peer_wtbl;
	bool connecting, connected, peer_valid, link_failed, connect_open;
	u8 ps_profile; /* requested associated-station policy (CAM=0, MAX=1, FAST=2); command_mutex */
	bool disconnecting, disconnect_seen, disconnect_event_received, disconnect_local;
	u16 disconnect_reason;
	int connect_error;
	u32 event_cipher;
	spinlock_t data_lock;
	u8 *data_headers, *data_payloads;
	dma_addr_t data_headers_dma, data_payloads_dma;
	bool data_live[512], data_token[512], data_retired[512];
	struct mt7932_duplicate duplicates[17];
	u64 tx_released, rx_ethernet;
	u64 tx_doorbells, tx_busy, tx_free_events, rx_descriptors, rx_doorbells;
	u32 tx_kick_max, tx_pending_count;
	u64 rx_parse_errors, rx_amsdu, rx_filtered, rx_rate_missing;
	/* Optional host-side profiling; the IRQ thread owns RX samples. */
	struct dentry *perf_dir;
	u64 perf_irq_ns, perf_irq_max_ns, perf_rx_copy_ns, perf_tx_copy_ns;
	u64 perf_rx_frames, perf_rx_bytes, perf_tx_frames, perf_tx_bytes;
	u64 perf_rx_drain_full, perf_rx_drain_max, perf_tx_queue_stops;
	u64 perf_tx_queued_max, perf_rx_gro_frames;
	struct gro_cells gro_cells;
	bool gro_cells_ready;
	/* response_lock protects the associated unicast RX rate snapshot. */
	struct rate_info rx_rate;
	unsigned long rx_rate_updated;
	u32 last_rxv;
	bool rx_rate_valid;
	bool data_ready;
	bool tx_pending; /* data_lock: descriptors awaiting a doorbell */
	bool bss_absent;
	u8 bss_quota;
	u32 irq_mask;
	void *ipc, *aux;
	dma_addr_t ipc_dma, aux_dma;
};

unsigned int mt_bw_requested(void);
void mt_bw_caps(struct mt7932 *m);
u32 mt_bw_channel_flags(struct mt7932 *m, struct ieee80211_channel *channel);
int mt_bw_assoc_start(struct mt7932 *m, const u8 *body, size_t length);
void mt_bw_latch(struct mt7932 *m, const char *reason);
void mt_bw_narrow_host(struct mt7932 *m);
int mt_bw_apply_fallback(struct mt7932 *m);
bool mt_rf_allowed(struct mt7932 *m);
u32 mt_read(struct mt7932 *m, u32 reg);
void mt_write(struct mt7932 *m, u32 reg, u32 value);
void mt_rmw(struct mt7932 *m, u32 reg, u32 mask, u32 value);
int mt_poll(struct mt7932 *m, u32 reg, u32 mask, u32 expected, u32 timeout);
void *mt_alloc(struct mt7932 *m, size_t size, dma_addr_t *dma);
void mt_tx_clean(struct mt7932_ring *q);
int mt_request_ext(struct mt7932 *m, u8 cid, u8 ext, bool runtime, bool set, bool wait,
		      const void *payload, size_t length);
int mt_request(struct mt7932 *m, u8 cid, bool runtime, bool set, bool wait,
		      const void *payload, size_t length);
void mt_transport_snapshot(struct mt7932 *m);
int mt_recovery_gate(struct mt7932 *m);
int mt_cal_procedure(struct mt7932 *m, struct mt7932_cal_piece *pieces,
				    unsigned int count, unsigned int logical, u8 context_version);
int mt_calibration_gate(struct mt7932 *m);
void mt_data_kick_locked(struct mt7932 *m);
void mt_data_clean_locked(struct mt7932 *m);
void mt_data_clean(struct mt7932 *m);
void mt_bss_presence(struct mt7932 *m, const struct mt7932_event *event);
void mt_data_complete(struct mt7932 *m, const u8 *packet, size_t length);
netdev_tx_t mt_net_xmit(struct sk_buff *skb, struct net_device *netdev);
void mt_data_receive(struct mt7932 *m, const struct mt7932_rx_frame *frame);
int mt_data_prepare(struct mt7932 *m);
struct mt7932 *mt_from_wiphy(struct wiphy *wiphy);
void mt_scan_event(struct mt7932 *m, const struct mt7932_event *event);
void mt_abort_scan(struct wiphy *wiphy, struct wireless_dev *wdev);
int mt_scan_quiesce(struct mt7932 *m);
int mt_connect_discover(struct mt7932 *m);
int mt_scan(struct wiphy *wiphy, struct cfg80211_scan_request *request);
int mt_register_interface(struct mt7932 *m);
int mt_enable_scan(struct mt7932 *m);
void mt_packet_receive(struct mt7932 *m, const u8 *packet, size_t length);
void mt_link_event(struct mt7932 *m, const struct mt7932_event *event);
struct cfg80211_bss *mt_connect_find_bss(struct mt7932 *m);
void mt_connect_work(struct work_struct *work);
int mt_connect(struct wiphy *wiphy, struct net_device *netdev,
		      struct cfg80211_connect_params *params);
void mt_disconnect_work(struct work_struct *work);
int mt_disconnect(struct wiphy *wiphy, struct net_device *netdev, u16 reason);
int mt_deauth(struct wiphy *wiphy, struct net_device *netdev,
		     struct cfg80211_deauth_request *request);
int mt_change_interface(struct wiphy *wiphy, struct net_device *netdev,
			       enum nl80211_iftype type, struct vif_params *params);
int mt_register_regulatory_gate(struct mt7932 *m);
int mt_station_ps_restore(struct mt7932 *m);
int mt_set_power_mgmt(struct wiphy *wiphy, struct net_device *netdev,
		      bool enabled, int timeout);

int mt_get_station(struct wiphy *wiphy, struct wireless_dev *wdev,
		   const u8 *mac, struct station_info *sinfo);
int mt_set_cqm_rssi_config(struct wiphy *wiphy, struct net_device *netdev,
			   s32 rssi_thold, u32 rssi_hyst);
void mt_signal_update(struct mt7932 *m, const struct mt7932_rx_frame *frame, int signal);

extern const struct ethtool_ops mt_ethtool_ops;
bool mt_perf_enabled(void);
bool mt_gro_enabled(void);
void mt_perf_init(struct mt7932 *m);

bool mt_transport_polled(void);

#endif
