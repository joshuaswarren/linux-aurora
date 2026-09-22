// SPDX-License-Identifier: GPL-2.0-only
/* Freestanding AArch64 Linux shared-memory test for a minimal RAM initramfs.
 * Ten pinned processes exchange a 256-byte payload over 100,000 handoffs.
 * Build: clang --target=aarch64-linux-gnu -fuse-ld=lld -nostdlib -static -O2
 *        -ffreestanding -fno-stack-protector -fno-builtin -o smp-coherence FILE
 */
typedef unsigned long u64;
struct shared {
	u64 sequence;
	u64 failed;
	u64 payload[32];
};

static long sys6(long nr, long a, long b, long c, long d, long e, long f)
{
	register long x0 __asm__("x0") = a;
	register long x1 __asm__("x1") = b;
	register long x2 __asm__("x2") = c;
	register long x3 __asm__("x3") = d;
	register long x4 __asm__("x4") = e;
	register long x5 __asm__("x5") = f;
	register long x8 __asm__("x8") = nr;
	__asm__ volatile("svc #0" : "+r"(x0) : "r"(x1), "r"(x2), "r"(x3),
			 "r"(x4), "r"(x5), "r"(x8) : "memory", "cc");
	return x0;
}

static __attribute__((noreturn)) void finish(int status)
{
	sys6(93, status, 0, 0, 0, 0, 0);
	__builtin_unreachable();
}

static u64 pattern(u64 sequence, u64 word)
{
	return sequence * 0x9e3779b97f4a7c15UL ^ word * 0xd1b54a32d192ed03UL;
}

static void fail(struct shared *s, unsigned cpu, unsigned reason)
{
	char message[] = "SMP_COHERENCE_FAIL CPU=0 REASON=0\n";
	message[23] = '0' + cpu;
	message[32] = '0' + reason;
	__atomic_store_n(&s->failed, 1, __ATOMIC_RELEASE);
	sys6(64, 1, (long)message, sizeof(message) - 1, 0, 0, 0);
	finish(reason);
}

static void worker(struct shared *s, unsigned cpu)
{
	u64 mask = 1UL << cpu;
	unsigned actual = 99;

	if (sys6(122, 0, sizeof(mask), (long)&mask, 0, 0, 0) ||
	    sys6(168, (long)&actual, 0, 0, 0, 0, 0) || actual != cpu)
		fail(s, cpu, 1);
	for (u64 round = 0; round < 10000; round++) {
		u64 expected = round * 10 + cpu;
		u64 spins = 0;
		while (__atomic_load_n(&s->sequence, __ATOMIC_ACQUIRE) != expected) {
			if (__atomic_load_n(&s->failed, __ATOMIC_ACQUIRE))
				finish(2);
			if (++spins == 1000000000UL)
				fail(s, cpu, 3);
			__asm__ volatile("nop");
		}
		if (expected)
			for (u64 word = 0; word < 32; word++)
				if (s->payload[word] != pattern(expected, word))
					fail(s, cpu, 4);
		for (u64 word = 0; word < 32; word++)
			s->payload[word] = pattern(expected + 1, word);
		__atomic_store_n(&s->sequence, expected + 1, __ATOMIC_RELEASE);
	}
	finish(0);
}

void _start(void)
{
	long map = sys6(222, 0, 16384, 3, 0x21, -1, 0);
	struct shared *s = (void *)map;
	long children[10];
	int ok = 1;

	if ((u64)map >= (u64)-4095)
		finish(10);
	for (unsigned cpu = 0; cpu < 10; cpu++) {
		long pid = sys6(220, 17, 0, 0, 0, 0, 0);
		if (pid == 0)
			worker(s, cpu);
		if (pid < 0) {
			__atomic_store_n(&s->failed, 1, __ATOMIC_RELEASE);
			finish(11);
		}
		children[cpu] = pid;
	}
	for (unsigned cpu = 0; cpu < 10; cpu++) {
		int status = -1;
		if (sys6(260, children[cpu], (long)&status, 0, 0, 0, 0) != children[cpu] || status)
			ok = 0;
	}
	if (__atomic_load_n(&s->sequence, __ATOMIC_ACQUIRE) != 100000 ||
	    __atomic_load_n(&s->failed, __ATOMIC_ACQUIRE))
		ok = 0;
	if (ok) {
		static const char message[] = "SMP_COHERENCE_PASS CPUS=10 HANDOFFS=100000 BYTES_PER_HANDOFF=256\n";
		sys6(64, 1, (long)message, sizeof(message) - 1, 0, 0, 0);
	}
	finish(ok ? 0 : 12);
}
