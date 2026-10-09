# MT7932 private throughput experiment

This driver is a private J700 research overlay. Keep this series and its
measurements private until the radio protocol has a clean-room publication
review. The host counters below describe only Linux-side behavior.

The baseline data path drains every RX ring from a threaded IRQ (up to its
ring size per visit), copies translated Ethernet frames into new skbs, and
passes them to `netif_rx()`. TX copies each skb into a coherent slot and uses
`xmit_more` to batch doorbells. These paths do not use streaming DMA mappings,
so per-frame `dma_sync_*()` is not a current cost. The driver has no dedicated
NAPI poller or programmable device interrupt coalescing. A budgeted NAPI
conversion requires separate interrupt and firmware-event validation.

Boot-only parameters use the `mt7932_fullmac.` prefix. With no parameters,
the data path retains the previous 512-entry TX0/RX2/RX7 rings, a 16-entry
RX refill boundary, a 64-entry TX doorbell boundary, and `netif_rx()` delivery:

| Parameter | Default | Experiment |
| --- | ---: | --- |
| `perf_stats` | 0 | Expose Linux-only timing and saturation counters in debugfs. |
| `gro_rx` | 0 | Route translated Ethernet through kernel GRO cells. |
| `tx_data_ring_entries` | 512 | 256 or 512 TX0 descriptors. |
| `rx_data_ring_entries` | 512 | 256, 512, or 1024 RX2/RX7 descriptors. |
| `tx_batch_limit` | 64 | 1..64 TX doorbell boundary; requires `batch_tx=1`. |
| `rx_batch_limit` | 16 | 1..32 RX refill boundary; requires `batch_rx=1`. |

The two batch-limit parameters are writable through sysfs, but change them
only between traffic runs. The ring sizes and GRO setting require a reboot.
Invalid ring sizes fall back to 512. Out-of-range TX batch limits are
rejected; RX batch limits are clamped.
Keep the 20 MHz boot entry available when exercising `max_bw_mhz=80`.

On the j700i 40 MHz association, raising `tx_batch_limit` from 16 to 64
increased measured uplink from 114 to 141 Mbit/s (+24%) without a measured
downlink change. The 16-entry setting remains available as a runtime
comparison through `/sys/module/mt7932_fullmac/parameters/tx_batch_limit`.

When `perf_stats=1`, read
`/sys/kernel/debug/<PCI device name>/mt7932_datapath` as root. Values are
cumulative since boot and are sampled without a firmware query or MMIO. Use
before/after deltas. `irq_total_ns` and `irq_max_ns` cover the threaded
service; `rx_copy_ns` and `tx_copy_ns` cover the skb copy operations only.
`rx_drain_full` counts visits that consumed a complete ring. GRO frames count
submissions, not the number of successful GRO merges. The snapshot is not
atomic across CPU contexts. Enable profiling consistently in both arms of an
A/B comparison; its timestamp calls have a cost.

Do not infer a PHY gain from host parameter values alone. Confirm negotiated
width, MCS/NSS, and AP-side retries. At a fixed 20 MHz width, vary one host
knob per boot/run. Compare the 20 and 80 MHz baselines first to estimate the
width contribution; treat any further gain from GRO or batching as a separate
host data-path result. 80 MHz at equal MCS/NSS has about 3.5 times the 20 MHz
PHY capacity, but observed TCP throughput also depends on rate and airtime.
