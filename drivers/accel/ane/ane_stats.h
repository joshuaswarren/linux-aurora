/* SPDX-License-Identifier: GPL-2.0-only OR MIT */
/*
 * Producer-side ANE statistics shared by ane.ko (T8103/T6000) and
 * ane_t6021.ko (T602x/T8112): cumulative busy-time and job counters
 * plus a ring of recent submissions. The sysfs ane_stats file and the
 * debugfs ane_timeline file (formatting in ane_stats_show.c) exist
 * only when the module's `stats` parameter (default 1) is set; the
 * hot path then costs one predictable branch and atomics only -- no
 * allocation, no locking, no formatting.
 *
 * busy_ns counts nanoseconds the engine was busy since bind. For an
 * engine that serializes submissions (ane.ko, behind engine_lock) it
 * is the sum of the submit-to-completion windows; ane_t6021 can run
 * submissions in parallel, so it folds windows with the union rule
 * (see ane_stats_complete()).
 */

#ifndef _ANE_STATS_H_
#define _ANE_STATS_H_

#include <linux/atomic.h>
#include <linux/ktime.h>
#include <linux/minmax.h>
#include <linux/seq_file.h>
#include <linux/string.h>
#include <linux/types.h>

#define ANE_STATS_RING_ORDER_DEFAULT 8	/* 256 slots, power of two */

/*
 * Per-device counters. busy_ns is the cumulative union of busy
 * intervals in nanoseconds since the device was bound, jobs the
 * cumulative completed submissions. last_busy_end is the union
 * helper: the latest busy-window end folded into busy_ns so far; a
 * concurrent submission only adds the part of its window past it.
 */
struct ane_stats_counters {
	atomic64_t	busy_ns;
	atomic64_t	jobs;
	atomic64_t	last_busy_end;
};

/*
 * Ring slot with a seqlock-style sequence: odd while the writer is
 * filling the slot, even once committed, so a reader never prints a
 * torn line and never blocks the producer. start/end 0 means the
 * slot was never used. tmst carries the raw TM tick on ane.ko (unit
 * unknown) and 0 on ane_t6021 (no host TM path).
 */
struct ane_stats_ring_entry {
	atomic64_t	seq;
	atomic64_t	submit_ns;
	atomic64_t	start_ns;
	atomic64_t	end_ns;
	atomic_t	tasks;
	atomic_t	rc;
	atomic64_t	tmst;
};

/*
 * Ring of the last n submissions (n a power of two, preallocated at
 * probe). head counts reserved slots; slots points at the zeroed
 * entry array.
 */
struct ane_stats_ring {
	atomic64_t	head;
	u32		n;
	u32		mask;
	struct ane_stats_ring_entry *slots;
};

static inline void ane_stats_counters_init(struct ane_stats_counters *ctrs,
					   struct ane_stats_ring *ring,
					   u32 n_shift)
{
	memset(ctrs, 0, sizeof(*ctrs));
	memset(ring, 0, sizeof(*ring));
	ring->n = 1u << n_shift;
	ring->mask = ring->n - 1;
}

/*
 * Hot-path submission start: fills the slot at the current head,
 * marks it committed and returns its index for ane_stats_complete().
 * The caller's existing serialization (engine_lock on ane.ko, one
 * command buffer on ane_t6021) makes the head reservation safe.
 */
static inline u32 ane_stats_begin(struct ane_stats_ring *ring, u64 submit_ns,
				  u32 tasks)
{
	u64 head = atomic64_read(&ring->head);
	struct ane_stats_ring_entry *e = &ring->slots[head & ring->mask];

	smp_wmb(); /* Order slot state before the writer sequence opens. */
	atomic64_set_release(&e->seq, (head + 2) | 1);	/* odd: filling */
	atomic64_set_release(&e->submit_ns, submit_ns);
	atomic64_set_release(&e->start_ns, submit_ns);
	atomic64_set_release(&e->end_ns, submit_ns);
	atomic_set(&e->tasks, tasks);
	atomic_set(&e->rc, 0xffffffffU);	/* not completed yet */
	atomic64_set_release(&e->tmst, 0);
	atomic64_set_release(&e->seq, head + 4);	/* even: committed */
	atomic64_add(1, &ring->head);

	return head & ring->mask;
}

/*
 * Hot-path submission completion. The submission window [start, end]
 * folds into busy_ns with the union rule
 *	add = max(0, end - max(start, last_busy_end))
 * under a cmpxchg on last_busy_end, so overlapping windows count
 * once; on a single-producer engine start >= last_busy_end always
 * and the contribution is end - start. Then end_ns, the call status
 * and the TM tick are committed into the slot and jobs increments.
 */
static inline void ane_stats_complete(struct ane_stats_counters *ctrs,
				      struct ane_stats_ring *ring, u32 idx,
				      u64 end_ns, u32 rc, u64 tmst)
{
	struct ane_stats_ring_entry *e = &ring->slots[idx];
	u64 start = atomic64_read(&e->start_ns);
	u64 prev = atomic64_read(&ctrs->last_busy_end);
	u64 add_ns;

	for (;;) {
		u64 lo = max(start, prev);
		u64 next = max(end_ns, prev);

		add_ns = end_ns > lo ? end_ns - lo : 0;
		if (atomic64_try_cmpxchg(&ctrs->last_busy_end,
					 (s64 *)&prev, (s64)next))
			break;
	}
	if (add_ns)
		atomic64_add(add_ns, &ctrs->busy_ns);
	atomic64_add(1, &ctrs->jobs);

	smp_wmb(); /* Publish completion data before updating the sequence. */
	atomic64_set_release(&e->end_ns, end_ns);
	atomic_set(&e->rc, rc);
	atomic64_set_release(&e->tmst, tmst);
	smp_wmb(); /* Publish the completed slot before the final sequence. */
	atomic64_set_release(&e->seq, atomic64_read(&ring->head) * 2);
}

ssize_t ane_stats_emit(const struct ane_stats_counters *ctrs, char *buf);
int ane_stats_timeline_show(struct seq_file *m, void *data);

#endif /* _ANE_STATS_H_ */
