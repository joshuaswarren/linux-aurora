/* SPDX-License-Identifier: GPL-2.0-only */
#include "mt7932.h"

static int mt_peer_reserve(struct mt7932 *m);
static int mt_association_calibration(struct mt7932 *m);
static void mt_peer_release(struct mt7932 *m);
static int mt_retire_connection(struct mt7932 *m);
static bool mt_open_request(const struct cfg80211_connect_params *p);

/* command_mutex held.  The native J700 nicConfigPowerSaveProfile sends legacy
 * CID05, set=1, no response, four bytes: BSS index, profile, two reserved
 * bytes; CAM=0, MAX PS=1, FAST PS=2 (AppleSunriseWLAN
 * _wlanoidSet802dot11PowerSaveProfile; the same layout as MediaTek's published
 * gen3 CMD_ID_POWER_SAVE_MODE).  Station radio policy only: the HIF, DMA and
 * Ethernet path keep running.  A submission is not a firmware acknowledgement.
 */
static int mt_station_ps_submit(struct mt7932 *m, u8 profile)
{
	u8 body[4] = {0, profile, 0, 0};
	int ret;

	lockdep_assert_held(&m->command_mutex);
	ret = mt_request(m, 0x05, true, true, false, body, sizeof(body));
	dev_info(&m->pdev->dev, "STATION_PS_SUBMIT: bss=0 profile=%u result=%d\n",
		 profile, ret);
	return ret;
}

/* Reapply the requested policy after each association, before reporting it
 * to cfg80211: the firmware's own default for a fresh peer is a power-saving
 * profile, which on the J700 bench showed as 50-150 ms mean and 700 ms peak
 * gateway RTTs with a few percent loss; CAM (the default here, and what
 * NetworkManager asks for with powersave=disable) removes that.
 */
int mt_station_ps_restore(struct mt7932 *m)
{
	return mt_station_ps_submit(m, m->ps_profile);
}

/*
 * wpa_supplicant enables connection-quality monitoring so that it only scans
 * when the link actually degrades.  Without it `bgscan simple` falls back to a
 * timed sweep of every channel -- on this machine every ~31 s, which shows up
 * as periodic latency spikes on an otherwise idle link.  The associated AP's
 * beacons already carry an RSSI, so drive both the threshold events and the
 * station signal from those rather than polling the firmware.
 */
void mt_signal_update(struct mt7932 *m, const struct mt7932_rx_frame *frame, int signal)
{
	enum nl80211_cqm_rssi_threshold_event event;
	s32 thold;
	u32 hyst;
	int state;

	/* addr3 of a beacon is the BSSID; mt7932_rx_beacon() checked the length. */
	if (!READ_ONCE(m->connected) || frame->length < 22 || signal <= -128)
		return;
	if (!ether_addr_equal(frame->data + 16, m->connect_bssid))
		return;

	WRITE_ONCE(m->assoc_signal_dbm, signal);
	WRITE_ONCE(m->assoc_signal_valid, true);

	thold = READ_ONCE(m->cqm_rssi_thold);
	if (!thold)
		return;
	hyst = READ_ONCE(m->cqm_rssi_hyst);
	if (signal >= thold + (int)hyst)
		state = 1;
	else if (signal <= thold - (int)hyst)
		state = -1;
	else
		return;				/* inside the hysteresis band */
	if (state == READ_ONCE(m->cqm_rssi_state))
		return;
	WRITE_ONCE(m->cqm_rssi_state, state);
	event = state > 0 ? NL80211_CQM_RSSI_THRESHOLD_EVENT_HIGH :
			    NL80211_CQM_RSSI_THRESHOLD_EVENT_LOW;
	cfg80211_cqm_rssi_notify(m->netdev, event, signal, GFP_ATOMIC);
}

int mt_set_cqm_rssi_config(struct wiphy *wiphy, struct net_device *netdev,
			   s32 rssi_thold, u32 rssi_hyst)
{
	struct mt7932 *m = mt_from_wiphy(wiphy);

	if (netdev != m->netdev || m->wdev.iftype != NL80211_IFTYPE_STATION)
		return -EINVAL;
	WRITE_ONCE(m->cqm_rssi_thold, rssi_thold);
	WRITE_ONCE(m->cqm_rssi_hyst, rssi_hyst);
	WRITE_ONCE(m->cqm_rssi_state, 0);	/* re-arm: report the next crossing */
	return 0;
}

int mt_get_station(struct wiphy *wiphy, struct wireless_dev *wdev,
		   const u8 *mac, struct station_info *sinfo)
{
	struct mt7932 *m = mt_from_wiphy(wiphy);

	if (wdev != &m->wdev || !READ_ONCE(m->connected))
		return -ENOENT;
	if (mac && !ether_addr_equal(mac, m->connect_bssid))
		return -ENOENT;
	if (READ_ONCE(m->assoc_signal_valid)) {
		sinfo->signal = READ_ONCE(m->assoc_signal_dbm);
		sinfo->filled |= BIT_ULL(NL80211_STA_INFO_SIGNAL);
	}
	return 0;
}

int mt_set_power_mgmt(struct wiphy *wiphy, struct net_device *netdev,
		      bool enabled, int timeout)
{
	struct mt7932 *m = mt_from_wiphy(wiphy);
	u8 profile = enabled ? (timeout == -1 ? 2 : 1) : 0;
	int ret = 0;

	if (netdev != m->netdev || m->wdev.iftype != NL80211_IFTYPE_STATION)
		return -EINVAL;
	mutex_lock(&m->command_mutex);
	if (m->connecting || m->disconnecting || m->scan_request ||
	    m->retired_scan_seq || m->link_failed) {
		ret = -EBUSY;
		goto out;
	}
	if (m->connected) {
		if (!mt_rf_allowed(m)) {
			ret = -EBUSY;
			goto out;
		}
		ret = mt_station_ps_submit(m, profile);
	}
	/* cfg80211 can set policy before association. Cache it without sending
	 * a command to an absent BSS; connect_work applies it after association.
	 */
	if (!ret)
		m->ps_profile = profile;
out:
	mutex_unlock(&m->command_mutex);
	return ret;
}

/* One owned station, firmware WPA2-PSK handshake; no host key fabrication. */
static int mt_peer_reserve(struct mt7932 *m)
{
	u8 *ids = (u8 *)&m->peer_ids;
	unsigned long allocation = m->peer_alloc;
	unsigned int i, id;

	for (i = 0; i < 6; i++) {
		id = find_first_zero_bit(&allocation, i < 2 ? 15 : 14);
		if (id >= (i < 2 ? 15 : 14))
			return -ENOSPC;
		__set_bit(id, &allocation);
		ids[i] = id;
	}
	m->peer_alloc = allocation;
	return 0;
}

/* response_lock held by the sole RX event consumer. Never wait here. */
void mt_link_event(struct mt7932 *m, const struct mt7932_event *event)
{
	const u8 *body;
	size_t length;
	u32 selector;

	if (event->length < 48 || !m->netdev)
		return;
	selector = get_unaligned_le32(event->packet + 40);
	body = event->packet + 48;
	length = event->length - 48;
	dev_info(&m->pdev->dev, "LINK_EVENT: selector=%02x length=%zu\n", selector, length);
	if (selector == 0x44) {
		if (length < 5)
			return;
		dev_info(&m->pdev->dev, "DISCONNECT_EVENT: reason=%u deferred=%u reason-index=%u\n",
			body[0], body[3], body[4]);
		m->disconnect_event_received = true;
		if (!body[3])
			m->disconnect_seen = true;
		if (m->disconnecting && !body[3])
			complete(&m->disconnect_done);
		if (m->connected) {
			m->connected = false;
			m->disconnecting = true;
			m->disconnect_local = false;
			m->disconnect_reason = WLAN_REASON_UNSPECIFIED;
			netif_carrier_off(m->netdev);
			netif_stop_queue(m->netdev);
			schedule_work(&m->disconnect_work);
		}
		if (m->connecting) {
			m->connect_error = -ECONNRESET;
			complete(&m->assoc_start);
			complete(&m->assoc_done);
		}
		return;
	}
	if (!m->connecting)
		return;
	switch (selector) {
	case 0x4d:
		if (length >= 20)
			dev_info(&m->pdev->dev, "ASSOC_START: band=%u width=%u channel=%u extension=%u\n",
				 get_unaligned_le32(body + 12), body[16], body[17], body[19]);
		if (length < 20 || get_unaligned_le32(body + 12) != mt7932_channel_band(m->connect_channel) ||
		    body[16] || body[17] != m->connect_channel || body[19])
			m->connect_error = -EPROTO;
		complete(&m->assoc_start);
		break;
	case 0x40:
		if (length >= 52)
			dev_info(&m->pdev->dev, "ASSOC_REPORT: status=%u band=%u channel=%u bss=%u bmc=%u station=%u peer=%u cipher=%08x\n",
				 get_unaligned_le16(body), body[11], body[12], body[16], body[17], body[20], body[21],
				 get_unaligned_le32(body + 48));
		/* OPEN reports retain cipher as numeric metadata only; it neither
		 * installs a key nor changes the immutable requested security mode.
		 */
		if (!mt7932_assoc_resources(body, length, &m->peer_ids, m->connect_channel, m->connect_open)) {
			m->connect_error = -EPROTO;
			complete(&m->assoc_start);
			complete(&m->assoc_done);
			break;
		}
		m->peer_wtbl = body[21];
		m->peer_valid = true;
		/* HOST decoder confirms the RSN suite namespace, not request enum10. */
		m->event_cipher = get_unaligned_le32(body + 48);
		dev_info(&m->pdev->dev, "ASSOCIATED: peer=%u station=%u cipher-raw=%u\n",
			 m->peer_wtbl, body[20], m->event_cipher);
		break;
	case 0x42:
		if (length >= 2)
			dev_info(&m->pdev->dev, "CONNECT_REPORT: status=%u peer-owned=%u\n", get_unaligned_le16(body), m->peer_valid);
		if (length < 2 || get_unaligned_le16(body) || !m->peer_valid)
			m->connect_error = -ECONNREFUSED;
		complete(&m->assoc_done);
		break;
	}
}

static int mt_association_calibration(struct mt7932 *m)
{
	const struct firmware *oca;
	struct mt7932_cal_piece pieces[2];
	int ret;

	ret = request_firmware_direct(&oca, "mediatek/mt7932/oca2.bin", &m->pdev->dev);
	if (ret)
		return ret;
	ret = mt7932_cal_association(oca->data, oca->size, m->connect_channel, pieces);
	if (ret == 2)
		ret = mt_cal_procedure(m, pieces, 2, 1, 0);
	release_firmware(oca);
	return ret;
}

/* command_mutex and response_lock held; all old DMA/readers already retired. */
static void mt_peer_release(struct mt7932 *m)
{
	const u8 *ids = (const u8 *)&m->peer_ids;
	unsigned int i;

	for (i = 0; i < sizeof(m->peer_ids); i++)
		__clear_bit(ids[i], &m->peer_alloc);
	memset(&m->peer_ids, 0, sizeof(m->peer_ids));
	memset(m->duplicates, 0, sizeof(m->duplicates));
	memzero_explicit(m->connect_pmk, sizeof(m->connect_pmk));
	m->peer_valid = false;
	m->peer_wtbl = 0;
	m->event_cipher = 0;
}

/* No lifecycle/command lock held on entry. RX/IRQ and TX-free processing stay
 * alive throughout. EE44 and queue removal are NOT a DMA fence.
 */
static int mt_retire_connection(struct mt7932 *m)
{
	u8 body[24] = {}, queue[32];
	unsigned long flags, deadline;
	unsigned int i;
	bool drained, event_received;
	int ret = 0;

	netif_carrier_off(m->netdev);
	netif_tx_disable(m->netdev);
	/* ndo_start_xmit rechecks the closed admission gate under data_lock. */
	spin_lock_irqsave(&m->data_lock, flags);
	mt_data_kick_locked(m);
	spin_unlock_irqrestore(&m->data_lock, flags);
	flush_work(&m->cal_work);
	mutex_lock(&m->command_mutex);
	spin_lock_irqsave(&m->response_lock, flags);
	drained = m->disconnect_seen;
	event_received = m->disconnect_event_received;
	if (!drained)
		reinit_completion(&m->disconnect_done);
	spin_unlock_irqrestore(&m->response_lock, flags);
	if (!drained) {
		put_unaligned_le64(8, body); /* version, not an IEEE reason */
		put_unaligned_le16(1, body + 8);
		body[10] = 8;
		memcpy(body + 13, m->connect_bssid, 6);
		put_unaligned_le16(30, body + 20);
		if (!event_received)
			ret = mt_request_ext(m, 0xea, 0x42, true, true, false, body, sizeof(body));
		if (!ret && !wait_for_completion_timeout(&m->disconnect_done, msecs_to_jiffies(10000)))
			ret = -ETIMEDOUT;
	}
	if (!ret) {
		mt7932_peer_queue(queue, m->peer_ids.station[0], false);
		ret = mt_request(m, 0x6b, true, true, false, queue, sizeof(queue));
	}
	if (!ret) {
		memset(queue, 0, sizeof(queue));
		put_unaligned_le16(15, queue); /* owned BSS0 queue bitmap */
		queue[2] = 1;
		queue[14] = 1;
		ret = mt_request(m, 0x6b, true, true, false, queue, sizeof(queue));
	}
	deadline = jiffies + msecs_to_jiffies(5000);
	while (!ret) {
		spin_lock_irqsave(&m->data_lock, flags);
		mt_data_clean_locked(m);
		drained = !m->tx[0].queued;
		for (i = 0; i < ARRAY_SIZE(m->data_live); i++)
			drained &= !m->data_live[i] && !m->data_token[i];
		spin_unlock_irqrestore(&m->data_lock, flags);
		mt_tx_clean(&m->tx[17]);
		if (drained && !m->tx[17].queued)
			break;
		if (time_after_eq(jiffies, deadline)) {
			ret = -ETIMEDOUT;
			break;
		}
		usleep_range(1000, 2000);
	}
	mutex_unlock(&m->command_mutex);
	/* Join already queued event consumers, without excluding future IRQs or
	 * claiming this is a firmware generation fence. Single-flight firmware
	 * lifecycle ordering remains necessary for asynchronous EE events.
	 */
	synchronize_irq(m->pdev->irq);
	flush_work(&m->cal_work);
	mutex_lock(&m->command_mutex);
	spin_lock_irqsave(&m->response_lock, flags);
	if (!ret && (m->cal_state.active || m->cal_state.error || m->cal_request_count || m->waiting))
		ret = -EPROTO;
	if (!ret)
		mt_peer_release(m);
	else
		m->link_failed = true; /* uncertain ownership: recovery required */
	spin_unlock_irqrestore(&m->response_lock, flags);
	mutex_unlock(&m->command_mutex);
	dev_info(&m->pdev->dev, "CONNECTION_RETIRED: status=%d%s\n", ret,
		 ret ? "; recovery required, DMA ownership retained" : "; old TX drained, six IDs released");
	return ret;
}

/* Hints affect preference, not admission. Only real cfg80211 scan entries
 * supply channel/BSSID and the reference passed to connect_done().
 */
struct cfg80211_bss *mt_connect_find_bss(struct mt7932 *m)
{
	const u8 *bssid = is_valid_ether_addr(m->connect_bssid_req) ? m->connect_bssid_req : NULL;
	struct cfg80211_bss *bss;
	enum ieee80211_privacy privacy = m->connect_open ?
		IEEE80211_PRIVACY_OFF : IEEE80211_PRIVACY_ON;

	bss = cfg80211_get_bss(m->wiphy, m->connect_channel_req ?: m->connect_channel_hint,
			       bssid ?: (is_valid_ether_addr(m->connect_bssid_hint) ?
					 m->connect_bssid_hint : NULL),
			       m->connect_ssid, m->connect_ssid_length,
			       IEEE80211_BSS_TYPE_ESS, privacy);
	if (!bss && (m->connect_channel_hint || is_valid_ether_addr(m->connect_bssid_hint)))
		bss = cfg80211_get_bss(m->wiphy, m->connect_channel_req, bssid,
				       m->connect_ssid, m->connect_ssid_length,
				       IEEE80211_BSS_TYPE_ESS, privacy);
	return bss;
}

void mt_connect_work(struct work_struct *work)
{
	struct mt7932 *m = container_of(work, struct mt7932, connect_work);
	struct cfg80211_connect_resp_params response = {};
	u8 *body, mac[8] = {}, queue[32];
	unsigned long flags;
	bool submitted = false, peer_owned = false;
	const char *phase = "prepare";
	unsigned long started = jiffies;
	int ret;

	mutex_lock(&m->command_mutex);
	body = kzalloc(856, GFP_KERNEL);
	ret = body ? 0 : -ENOMEM;
	if (ret)
		goto report;
	if (!mt_rf_allowed(m) || READ_ONCE(m->connect_error)) {
		ret = -ECANCELED;
		goto free;
	}
	if (!m->connect_bss) {
		dev_info(&m->pdev->dev, "CONNECT_DISCOVERY: refreshing missing scan candidate\n");
		phase = "discovery";
		ret = mt_connect_discover(m);
		if (ret)
			goto free;
	}
	if (m->connect_bss->channel->flags & (IEEE80211_CHAN_DISABLED | IEEE80211_CHAN_NO_IR |
					    IEEE80211_CHAN_NO_20MHZ)) {
		ret = -EINVAL;
		goto free;
	}
	spin_lock_irqsave(&m->response_lock, flags);
	ret = m->connect_error ?: mt_peer_reserve(m);
	if (!ret) {
		peer_owned = true;
		m->connect_channel = m->connect_bss->channel->hw_value;
		memcpy(m->connect_bssid, m->connect_bss->bssid, ETH_ALEN);
	}
	spin_unlock_irqrestore(&m->response_lock, flags);
	if (ret)
		goto free;
	/* Initial station policy: framework M=0 branch and no optional flags.
	 * This yields flag2 independently of credentials; no scan-result count
	 * belongs in this field. The recovered framework uses listen interval20.
	 * Other platform flag policies remain unsupported, not guessed mappings.
	 */
	phase = "connect-submit";
	ret = mt7932_connect_body(body, 856, m->connect_ssid, m->connect_ssid_length,
		m->connect_open ? NULL : m->connect_pmk, m->connect_bssid,
		m->connect_channel, 20, 2, &m->peer_ids, m->connect_open);
	if (ret < 0)
		goto free;
	memcpy(mac + 2, m->mac, ETH_ALEN);
	submitted = true;
	ret = mt_request(m, 0xb8, true, true, false, mac, sizeof(mac));
	ret = ret ?: READ_ONCE(m->connect_error);
	if (!ret)
		ret = mt_request(m, 9, true, true, false, NULL, 0);
	ret = ret ?: READ_ONCE(m->connect_error);
	if (!ret)
		ret = mt_request_ext(m, 0xea, 0x40, true, true, false, body, 856);
free:
	kfree_sensitive(body);
	memzero_explicit(m->connect_pmk, sizeof(m->connect_pmk));
	if (ret)
		goto report;
	dev_info(&m->pdev->dev, "CONNECT_SUBMITTED: BSSID=%pM channel=%u\n", m->connect_bssid, m->connect_channel);
	phase = "assoc-start";
	/* Genuine D7 requests must remain serviceable during asynchronous waits. */
	mutex_unlock(&m->command_mutex);
	ret = wait_for_completion_timeout(&m->assoc_start, msecs_to_jiffies(5000)) ? 0 : -ETIMEDOUT;
	mutex_lock(&m->command_mutex);
	if (ret) {
		ret = -ETIMEDOUT;
		goto report;
	}
	ret = READ_ONCE(m->connect_error);
	if (ret)
		goto report;
	phase = "association-calibration";
	ret = mt_association_calibration(m);
	ret = ret ?: READ_ONCE(m->connect_error);
	if (ret)
		goto report;
	dev_info(&m->pdev->dev, "ASSOCIATION_CALIBRATED: channel=%u\n", m->connect_channel);
	phase = "peer-queue";
	mt7932_peer_queue(queue, m->peer_ids.station[0], true);
	ret = mt_request(m, 0x6b, true, true, false, queue, sizeof(queue));
	if (ret)
		goto report;
	mutex_unlock(&m->command_mutex);
	phase = "connect-complete";
	if (!wait_for_completion_timeout(&m->assoc_done, msecs_to_jiffies(10000)))
		ret = -ETIMEDOUT;
	else
		ret = READ_ONCE(m->connect_error);
	mutex_lock(&m->command_mutex);
report:
	ret = ret ?: READ_ONCE(m->connect_error);
	if (!ret) {
		phase = "power-save-policy";
		ret = mt_station_ps_restore(m);
	}
	if (ret) {
		dev_err(&m->pdev->dev,
			"CONNECT_PHASE_FAILED: phase=%s status=%d elapsed-ms=%u start=%u done=%u peer=%u\n",
			phase, ret, jiffies_to_msecs(jiffies - started),
			completion_done(&m->assoc_start), completion_done(&m->assoc_done),
			READ_ONCE(m->peer_valid));
		mt_transport_snapshot(m);
	}
	memzero_explicit(m->connect_pmk, sizeof(m->connect_pmk));
	if (ret) {
		spin_lock_irqsave(&m->response_lock, flags);
		m->disconnecting = true;
		spin_unlock_irqrestore(&m->response_lock, flags);
		mutex_unlock(&m->command_mutex);
		if (submitted) {
			mt_retire_connection(m);
		} else if (peer_owned) {
			mutex_lock(&m->command_mutex);
			spin_lock_irqsave(&m->response_lock, flags);
			mt_peer_release(m);
			spin_unlock_irqrestore(&m->response_lock, flags);
			mutex_unlock(&m->command_mutex);
		}
		mutex_lock(&m->command_mutex);
	}
	/* Serialize the final outcome with cancellation. In particular, a late
	 * firmware success must not publish carrier/authorization after .deauth.
	 * cfg80211 queues these notifications; GFP_ATOMIC keeps this IRQ-safe.
	 */
	spin_lock_irqsave(&m->response_lock, flags);
	if (!ret && m->connect_error) {
		ret = m->connect_error;
		spin_unlock_irqrestore(&m->response_lock, flags);
		goto report;
	}
	response.status = ret ? WLAN_STATUS_UNSPECIFIED_FAILURE : WLAN_STATUS_SUCCESS;
	response.links[0].bss = m->connect_bss;
	response.links[0].bssid = m->connect_bssid;
	m->connect_bss = NULL; /* cfg80211 consumes this reference. */
	m->connecting = false;
	m->connected = !ret;
	m->disconnecting = false;
	if (m->connect_cancelled) {
		if (response.links[0].bss)
			cfg80211_put_bss(m->wiphy, response.links[0].bss);
		cfg80211_disconnected(m->netdev, m->disconnect_reason, NULL, 0, true, GFP_ATOMIC);
	} else {
		cfg80211_connect_done(m->netdev, &response, GFP_ATOMIC);
	}
	if (!ret) {
		if (READ_ONCE(m->data_ready)) {
			netif_carrier_on(m->netdev);
			netif_wake_queue(m->netdev);
		}
		if (m->connect_open) {
			dev_info(&m->pdev->dev, "OPEN_CONNECTED: owned peer, firmware completion; Ethernet=%u\n", m->data_ready);
		} else {
			cfg80211_port_authorized(m->netdev, m->connect_bssid, NULL, 0, GFP_ATOMIC);
			dev_info(&m->pdev->dev, "WPA2_AUTHORIZED: firmware handshake complete; Ethernet=%u\n", m->data_ready);
		}
	} else {
		dev_err(&m->pdev->dev, "CONNECT_FAILED: %d; recovery-required=%u\n", ret, m->link_failed);
	}
	spin_unlock_irqrestore(&m->response_lock, flags);
	mutex_unlock(&m->command_mutex);
}

/* Narrow ordinary OPEN policy. Arbitrary association IEs are not forwarded
 * by this fixture, so reject nonempty requests instead of discarding them.
 * The standard supplicant's Ethernet EAPOL control-port declaration is allowed;
 * nl80211 control-port transport and key/802.1X offloads are not implemented.
 */
static bool mt_open_request(const struct cfg80211_connect_params *p)
{
	return !p->privacy && !p->crypto.wpa_versions &&
		!p->crypto.n_ciphers_pairwise && !p->crypto.cipher_group &&
		!p->crypto.n_akm_suites && !p->crypto.psk &&
		!p->key && !p->key_len && !p->crypto.sae_pwd && !p->crypto.sae_pwd_len &&
		p->mfp == NL80211_MFP_NO && !p->ie_len && !p->want_1x &&
		!p->fils_erp_username && !p->fils_erp_username_len &&
		!p->fils_erp_realm && !p->fils_erp_realm_len &&
		!p->fils_erp_rrk && !p->fils_erp_rrk_len &&
		!p->crypto.control_port_over_nl80211 &&
		p->crypto.control_port_ethertype == cpu_to_be16(ETH_P_PAE) &&
		(p->auth_type == NL80211_AUTHTYPE_OPEN_SYSTEM ||
		 p->auth_type == NL80211_AUTHTYPE_AUTOMATIC);
}

/* wpa_supplicant 2.11 lists every AKM the profile allows on a PSK+SAE AP
 * (PSK and PSK-SHA256 together) so a fullmac driver can choose; the firmware
 * handshake here is WPA2-PSK, so accept the list when PSK is in it and nothing
 * outside the PSK family is.
 */
static bool mt_psk_akm_list(const struct cfg80211_connect_params *p)
{
	bool psk = false;
	int i;

	if (!p->crypto.n_akm_suites)
		return false;
	for (i = 0; i < p->crypto.n_akm_suites; i++) {
		if (p->crypto.akm_suites[i] == WLAN_AKM_SUITE_PSK)
			psk = true;
		else if (p->crypto.akm_suites[i] != WLAN_AKM_SUITE_PSK_SHA256)
			return false;
	}
	return psk;
}

int mt_connect(struct wiphy *wiphy, struct net_device *netdev,
		      struct cfg80211_connect_params *params)
{
	struct mt7932 *m = mt_from_wiphy(wiphy);
	unsigned long flags;
	bool open = mt_open_request(params);
	int ret = 0;

	if (!mt_rf_allowed(m) || !m->bss_active || netdev != m->netdev)
		return -EAGAIN;
	dev_info(&m->pdev->dev, "CONNECT_REQUEST: WPA=%u pairwise-count=%d group=%08x AKM-count=%d PMK-present=%u MFP=%u IE-bytes=%zu\n",
		 params->crypto.wpa_versions, params->crypto.n_ciphers_pairwise, params->crypto.cipher_group,
		 params->crypto.n_akm_suites, !!params->crypto.psk, params->mfp, params->ie_len);
	if (!params->ssid_len || params->ssid_len > 32)
		return -EINVAL;
	if (!open && (params->crypto.wpa_versions != NL80211_WPA_VERSION_2 ||
	    params->crypto.n_ciphers_pairwise != 1 || params->crypto.ciphers_pairwise[0] != WLAN_CIPHER_SUITE_CCMP ||
	    params->crypto.cipher_group != WLAN_CIPHER_SUITE_CCMP ||
	    !mt_psk_akm_list(params) ||
	    !params->crypto.psk || params->mfp == NL80211_MFP_REQUIRED ||
	    !params->ssid_len || params->ssid_len > 32))
		return -EOPNOTSUPP;
	if (params->bssid && !is_valid_ether_addr(params->bssid))
		return -EINVAL;
	mutex_lock(&m->command_mutex);
	spin_lock_irqsave(&m->response_lock, flags);
	if (!mt_rf_allowed(m) || m->link_failed || m->connecting || m->connected || m->disconnecting || m->scan_request || m->retired_scan_seq) {
		ret = m->link_failed ? -EIO : -EBUSY;
		goto unlock;
	}
	reinit_completion(&m->assoc_start);
	m->disconnect_seen = false;
	m->disconnect_event_received = false;
	reinit_completion(&m->assoc_done);
	reinit_completion(&m->disconnect_done);
	m->connect_channel_req = params->channel;
	m->connect_channel_hint = params->channel_hint;
	eth_zero_addr(m->connect_bssid_req);
	eth_zero_addr(m->connect_bssid_hint);
	if (params->bssid)
		ether_addr_copy(m->connect_bssid_req, params->bssid);
	if (params->bssid_hint)
		ether_addr_copy(m->connect_bssid_hint, params->bssid_hint);
	ether_addr_copy(m->connect_bssid, m->connect_bssid_req);
	memcpy(m->connect_ssid, params->ssid, params->ssid_len);
	m->connect_ssid_length = params->ssid_len;
	m->connect_open = open;
	memzero_explicit(m->connect_pmk, sizeof(m->connect_pmk));
	if (!open)
		memcpy(m->connect_pmk, params->crypto.psk, 32);
	m->connect_error = 0;
	m->connect_cancelled = false;
	m->peer_valid = false;
	m->connecting = true;
unlock:
	spin_unlock_irqrestore(&m->response_lock, flags);
	if (!ret) {
		m->connect_bss = mt_connect_find_bss(m);
		schedule_work(&m->connect_work);
	}
	mutex_unlock(&m->command_mutex);
	return ret;
}

void mt_disconnect_work(struct work_struct *work)
{
	struct mt7932 *m = container_of(work, struct mt7932, disconnect_work);
	u16 reason = READ_ONCE(m->disconnect_reason);
	bool local = READ_ONCE(m->disconnect_local);
	unsigned long flags;
	int ret = mt_scan_quiesce(m);

	if (!ret)
		ret = mt_retire_connection(m);
	else
		WRITE_ONCE(m->link_failed, true);

	/* Clear admission and queue its terminal event before a new connect can
	 * start; both paths are serialized by response_lock.
	 */
	spin_lock_irqsave(&m->response_lock, flags);
	m->disconnecting = false;
	cfg80211_disconnected(m->netdev, reason, NULL, 0, local, GFP_ATOMIC);
	spin_unlock_irqrestore(&m->response_lock, flags);
	dev_info(&m->pdev->dev, "DISCONNECTED: teardown=%d local=%u\n", ret, local);
}

int mt_disconnect(struct wiphy *wiphy, struct net_device *netdev, u16 reason)
{
	struct mt7932 *m = mt_from_wiphy(wiphy);
	unsigned long flags;

	if (netdev != m->netdev)
		return -ENODEV;
	spin_lock_irqsave(&m->response_lock, flags);
	if (m->connecting) {
		m->connect_error = -ECANCELED;
		m->connect_cancelled = true;
		m->disconnect_reason = reason;
		complete(&m->assoc_start);
		complete(&m->assoc_done);
		complete(&m->discovery_done);
	} else if (m->connected) {
		m->connected = false;
		m->disconnecting = true;
		m->disconnect_seen = false;
		m->disconnect_event_received = false;
		m->disconnect_local = true;
		m->disconnect_reason = reason;
		netif_carrier_off(m->netdev);
		netif_stop_queue(m->netdev);
		reinit_completion(&m->disconnect_done);
		schedule_work(&m->disconnect_work);
	}
	spin_unlock_irqrestore(&m->response_lock, flags);
	/* iwd may issue its next connect as soon as this command is ACKed.
	 * Both workers finish through queued cfg80211 notifications, never by
	 * taking the wiphy mutex held by this callback. Drain them without any
	 * command/response lock so the ACK cannot release a still-owned epoch.
	 */
	flush_work(&m->connect_work);
	flush_work(&m->disconnect_work);
	return 0;
}

/* iwd uses DEAUTHENTICATE to cancel an accepted fullmac connect before its
 * association event. Do not advertise softmac auth/assoc or fabricate an RX
 * deauth frame; retire that same firmware owner via the disconnect lifecycle.
 */
int mt_deauth(struct wiphy *wiphy, struct net_device *netdev,
		     struct cfg80211_deauth_request *request)
{
	struct mt7932 *m = mt_from_wiphy(wiphy);
	unsigned long flags;
	bool matches;

	if (request->ie_len || request->local_state_change)
		return -EOPNOTSUPP;
	spin_lock_irqsave(&m->response_lock, flags);
	matches = netdev == m->netdev && (m->connecting || m->connected) &&
		ether_addr_equal(request->bssid, m->connect_bssid);
	spin_unlock_irqrestore(&m->response_lock, flags);
	if (!matches)
		return -ENOTCONN;
	return mt_disconnect(wiphy, netdev, request->reason_code);
}

int mt_change_interface(struct wiphy *wiphy, struct net_device *netdev,
			       enum nl80211_iftype type, struct vif_params *params)
{
	return type == NL80211_IFTYPE_STATION ? 0 : -EOPNOTSUPP;
}
