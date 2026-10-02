// SPDX-License-Identifier: GPL-2.0-only OR MIT
/*
 * sysfs/debugfs formatting shared by ane.ko and ane_t6021.ko so both
 * drivers expose identical files: same mode (0444), same fields, same
 * parser-friendly ASCII lines. The sysfs and debugfs plumbing stays
 * in each driver; both link this file.
 */

#include <linux/atomic.h>
#include <linux/seq_file.h>
#include <linux/sysfs.h>

#include "ane_stats.h"

/*
 * sysfs ane_stats: ASCII "key value" lines, integers only, mode 0444.
 * busy_ns is the cumulative engine-busy time in nanoseconds, jobs the
 * cumulative completed submissions; neither resets while the device
 * is bound.
 */
ssize_t ane_stats_emit(const struct ane_stats_counters *ctrs, char *buf)
{
	return sysfs_emit(buf, "busy_ns %llu\njobs %llu\n",
			  (unsigned long long)atomic64_read(&ctrs->busy_ns),
			  (unsigned long long)atomic64_read(&ctrs->jobs));
}

/*
 * debugfs ane_timeline: the ring of recent submissions, one line per
 * committed slot:
 *   seq submit_ns start_ns end_ns tasks rc tmst
 * The field is a raw TM tick on ane.ko (unit unknown) and 0 on
 * ane_t6021 (no host TM path). A slot being written holds an odd
 * sequence and is skipped: the reader never blocks the producer and
 * never prints a torn line.
 */
int ane_stats_timeline_show(struct seq_file *m, void *data)
{
	struct ane_stats_ring *ring = data;
	u64 span = (u64)(ring->mask + 1) * 2;
	u64 head = atomic64_read(&ring->head);
	u64 start_seq = head > span ? (head - span) & ~1ull : 0;
	u64 s;

	seq_puts(m, "# ane_timeline: seq submit_ns start_ns end_ns tasks rc tmst\n");
	seq_puts(m, "# tmst: raw TM tick on ane.ko, 0 = unavailable on ane_t6021\n");
	for (s = head & ~1ull; s > start_seq; s -= 2) {
		struct ane_stats_ring_entry *e = &ring->slots[s & ring->mask];

		if (atomic64_read_acquire(&e->seq) != s)
			continue;
		seq_printf(m, "%llu %llu %llu %llu %u %u %llu\n",
			   (unsigned long long)s,
			   (unsigned long long)atomic64_read(&e->submit_ns),
			   (unsigned long long)atomic64_read(&e->start_ns),
			   (unsigned long long)atomic64_read(&e->end_ns),
			   (u32)atomic_read(&e->tasks),
			   (u32)atomic_read(&e->rc),
			   (unsigned long long)atomic64_read(&e->tmst));
	}
	return 0;
}
