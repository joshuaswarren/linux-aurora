// SPDX-License-Identifier: GPL-2.0-only
/*
 * Bounded PMGR power-state word access through /dev/mem for J700 domain
 * gating trials. Every access is an aligned 32-bit volatile load/store; no
 * byte, 64-bit or vector accesses touch the MMIO window. Each write is
 * preceded by a durable (fdatasync) checkpoint line in the log file so a
 * fatal access fault is attributable after external recovery.
 *
 *   pmgr-ps dump BASE LEN                 print nonzero words
 *   pmgr-ps read BASE OFF
 *   pmgr-ps write BASE OFF VALUE LOG      raw 32-bit write, readback
 *   pmgr-ps set BASE OFF TARGET LOG       Linux-style target change:
 *        keep the word, clear DEV_DISABLE/RESET/AUTO_ENABLE/FLAGS/BUSY/TARGET,
 *        set TARGET, write, poll ACTUAL==TARGET (100 ms)
 *
 * Build: aarch64-linux-gnu-gcc -O2 -Wall -Wextra -Werror -static pmgr-ps.c -o pmgr-ps
 */
#include <errno.h>
#include <limits.h>
#include <fcntl.h>
#include <inttypes.h>
#include <stdarg.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mman.h>
#include <time.h>
#include <unistd.h>

#define PS_RESET        (1u << 31)
#define PS_AUTO_ENABLE  (1u << 28)
#define PS_AUTO_MASK    (0xfu << 24)
#define PS_MIN_MASK     (0xfu << 16)
#define PS_PS_RESET     (1u << 12)
#define PS_BUSY         (1u << 11)
#define PS_DEV_DISABLE  (1u << 10)
#define PS_FLAGS        (3u << 8)
#define PS_ACTUAL(v)    (((v) >> 4) & 0xf)
#define PS_TARGET_MASK  0xfu

static volatile uint32_t *map(uint64_t base, size_t len)
{
	int fd = open("/dev/mem", O_RDWR | O_SYNC);
	void *p;

	if (fd < 0) {
		perror("/dev/mem");
		exit(2);
	}
	p = mmap(NULL, len, PROT_READ | PROT_WRITE, MAP_SHARED, fd, (off_t)base);
	if (p == MAP_FAILED) {
		perror("mmap");
		exit(2);
	}
	close(fd);
	return (volatile uint32_t *)p;
}

static uint32_t rd(volatile uint32_t *w, uint64_t off)
{
	uint32_t v = w[off / 4];

	__asm__ volatile("dsb sy" ::: "memory");
	return v;
}

static void wr(volatile uint32_t *w, uint64_t off, uint32_t v)
{
	__asm__ volatile("dsb sy" ::: "memory");
	w[off / 4] = v;
	__asm__ volatile("dsb sy" ::: "memory");
}

static void checkpoint(int log, const char *fmt, ...)
{
	char buf[256];
	va_list ap;
	int n;

	va_start(ap, fmt);
	n = vsnprintf(buf, sizeof(buf), fmt, ap);
	va_end(ap);
	if (n < 0)
		return;
	if (n > (int)sizeof(buf) - 1)
		n = sizeof(buf) - 1;
	if (write(log, buf, n) != n || fdatasync(log)) {
		perror("checkpoint");
		exit(2);
	}
	fputs(buf, stdout);
	fflush(stdout);
}

static uint64_t now_ns(void)
{
	struct timespec ts;

	clock_gettime(CLOCK_BOOTTIME, &ts);
	return (uint64_t)ts.tv_sec * 1000000000ull + ts.tv_nsec;
}

static int poll_actual(volatile uint32_t *w, uint64_t off, uint32_t target, uint32_t *last)
{
	uint64_t t0 = now_ns();

	for (;;) {
		uint32_t v = rd(w, off);

		*last = v;
		if (PS_ACTUAL(v) == target && !(v & PS_BUSY))
			return 0;
		if (now_ns() - t0 > 100ull * 1000000ull)
			return -ETIMEDOUT;
	}
}

/* Reject partial parses BEFORE opening /dev/mem: "f" must not become zero. */
static uint64_t number(const char *s, uint64_t maximum)
{
	char *end;
	unsigned long long value;

	errno = 0;
	value = strtoull(s, &end, 0);
	if (errno || !*s || *s == '-' || end == s || *end || value > maximum) {
		fprintf(stderr, "invalid numeric argument: %s\n", s);
		exit(2);
	}
	return value;
}

int main(int argc, char **argv)
{
	uint64_t base, off, len;
	uint32_t argument = 0;
	volatile uint32_t *w;

	if (argc < 4)
		goto usage;
	if (strcmp(argv[1], "dump") && strcmp(argv[1], "read") &&
	    strcmp(argv[1], "write") && strcmp(argv[1], "set"))
		goto usage;
	if ((!strcmp(argv[1], "write") || !strcmp(argv[1], "set"))) {
		if (argc != 6)
			goto usage;
		argument = number(argv[4], !strcmp(argv[1], "set") ? 15 : UINT32_MAX);
	} else if (argc != 4) {
		goto usage;
	}
	base = number(argv[2], UINT64_MAX);
	if (base & 4095)
		goto usage;
	if (!strcmp(argv[1], "dump")) {
		len = number(argv[3], 0x20000);
		if (!len || (len & 3))
			goto usage;
		w = map(base, len);
		for (off = 0; off < len; off += 4) {
			uint32_t v = rd(w, off);

			if (v)
				printf("%05" PRIx64 ": %08x\n", off, v);
		}
		return 0;
	}
	off = number(argv[3], 0x1fffc);
	if (off & 3)
		goto usage;
	len = ((off / 4096) + 1) * 4096;
	w = map(base, len);
	if (!strcmp(argv[1], "read")) {
		printf("%08x\n", rd(w, off));
		return 0;
	}
	if (argc < 6)
		goto usage;
	int log = open(argv[5], O_WRONLY | O_CREAT | O_APPEND | O_DSYNC, 0644);
	if (log < 0) {
		perror(argv[5]);
		return 2;
	}
	uint32_t before = rd(w, off);
	uint32_t value, after;
	if (!strcmp(argv[1], "write")) {
		value = argument;
		checkpoint(log, "about-to-write base=%#" PRIx64 " off=%#" PRIx64 " before=%#010x value=%#010x\n",
			   base, off, before, value);
		wr(w, off, value);
		after = rd(w, off);
		checkpoint(log, "wrote base=%#" PRIx64 " off=%#" PRIx64 " readback=%#010x\n", base, off, after);
		return 0;
	}
	if (!strcmp(argv[1], "set")) {
		uint32_t target = argument;
		int ret;

		value = before & ~(PS_DEV_DISABLE | PS_PS_RESET | PS_AUTO_ENABLE | PS_FLAGS | PS_BUSY | PS_TARGET_MASK);
		value |= target;
		checkpoint(log, "about-to-set base=%#" PRIx64 " off=%#" PRIx64 " before=%#010x value=%#010x target=%x\n",
			   base, off, before, value, target);
		wr(w, off, value);
		ret = poll_actual(w, off, target, &after);
		checkpoint(log, "set-done base=%#" PRIx64 " off=%#" PRIx64 " after=%#010x actual=%x result=%d\n",
			   base, off, after, PS_ACTUAL(after), ret);
		return ret ? 1 : 0;
	}
usage:
	fprintf(stderr, "usage: pmgr-ps dump BASE LEN | read BASE OFF | write BASE OFF VALUE LOG | set BASE OFF TARGET LOG\n");
	return 2;
}
