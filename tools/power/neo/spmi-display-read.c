// SPDX-License-Identifier: GPL-2.0-only
/* Bounded, read-only inventory on J700's unclaimed nub-spmi AP queues.
 * Adapted from pmu-read.c. The --brightness option submits READ commands,
 * never peripheral writes. Register 0x1e00 is a native-driver candidate,
 * not a proven J700 brightness register. First qualify --status separately.
 * Do not use while a kernel driver owns the controller.
 */
#include <errno.h>
#include <fcntl.h>
#include <inttypes.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/file.h>
#include <sys/mman.h>
#include <time.h>
#include <unistd.h>

static uint64_t base = UINT64_C(0x308614000);
static unsigned int slave = 13;
#define RX_EMPTY (1U << 24)
#define TX_EMPTY (1U << 8)

static volatile uint32_t *regs;

static double now(void)
{
	struct timespec t;
	clock_gettime(CLOCK_MONOTONIC, &t);
	return t.tv_sec + t.tv_nsec / 1e9;
}

static uint32_t rd(unsigned int offset)
{
	uint32_t value = regs[offset / 4];
	__asm__ volatile("dmb oshld" ::: "memory");
	return value;
}

static int read_pmu(uint16_t addr, unsigned int count)
{
	uint32_t status = rd(0), cmd, reply, data[2] = { 0 };
	double deadline;
	unsigned int i;

	/* Never drain an unknown transaction or retry an ambiguous timeout. */
	if ((status & (RX_EMPTY | TX_EMPTY)) != (RX_EMPTY | TX_EMPTY)) {
		fprintf(stderr, "controller not empty: %08x\n", status);
		return 1;
	}
	cmd = ((uint32_t)addr << 16) | (1U << 15) | (slave << 8) | 0x38 | (count - 1);
	printf("read addr=%04x count=%u cmd=%08x\n", addr, count, cmd);
	regs[1] = cmd;
	__asm__ volatile("dsb oshst" ::: "memory");
	deadline = now() + .05;
	for (i = 0; i < 1 + (count + 3) / 4; i++) {
		while (rd(0) & RX_EMPTY) {
			if (now() >= deadline) {
				fprintf(stderr, "RX timeout at word %u\n", i);
				return 1;
			}
		}
		if (!i)
			reply = rd(8);
		else
			data[i - 1] = rd(8);
	}
	printf("reply=%08x data=%08x %08x status=%08x\n", reply, data[0], data[1], rd(0));
	if ((reply & 0x7fff) != (cmd & 0x7fff) ||
	    ((reply >> 16) & ((1U << count) - 1)) != (1U << count) - 1)
		return 1;
	if (addr == 0xf802)
		printf("rtc_ticks=%" PRIu64 " rtc_seconds=%.6f\n",
		       ((uint64_t)data[1] << 32 | data[0]) >> 1,
		       (double)(((uint64_t)data[1] << 32 | data[0]) >> 1) / 32768);
	return 0;
}

int main(int argc, char **argv)
{
	char line[512], compatible[256];
	unsigned long long start, end;
	int fd, lock, ret = 0;
	FILE *f;
	size_t len;

	setbuf(stdout, NULL);
	if (argc != 2 || (strcmp(argv[1], "--status") && strcmp(argv[1], "--brightness") &&
			 strcmp(argv[1], "--main-brightness"))) {
		fprintf(stderr, "usage: %s --status|--brightness|--main-brightness\n", argv[0]);
		return 2;
	}
	if (!strcmp(argv[1], "--main-brightness")) {
		base = UINT64_C(0x308714000);
		slave = 14;
	}
	f = fopen("/sys/firmware/devicetree/base/compatible", "rb");
	if (!f)
		return 2;
	len = fread(compatible, 1, sizeof(compatible), f);
	fclose(f);
	if (len < 11 || memcmp(compatible, "apple,j700\0", 11)) {
		fprintf(stderr, "requires J700\n");
		return 2;
	}
	lock = open(slave == 14 ? "/run/neo-pmu-read.lock" : "/run/neo-spmi1-read.lock",
		    O_CREAT | O_RDWR, 0600);
	if (lock < 0 || flock(lock, LOCK_EX | LOCK_NB))
		return 2;
	f = fopen("/proc/iomem", "r");
	if (!f)
		return 2;
	while (fgets(line, sizeof(line), f)) {
		if (sscanf(line, "%llx-%llx", &start, &end) == 2 &&
		    start <= base + 0xff && end >= base) {
			fprintf(stderr, "controller claimed: %s", line);
			return 2;
		}
	}
	fclose(f);
	fd = open("/dev/mem", O_RDWR | O_SYNC);
	if (fd < 0) {
		perror("open /dev/mem");
		return 2;
	}
	regs = mmap(NULL, 0x4000, PROT_READ | PROT_WRITE, MAP_SHARED, fd, base);
	if (regs == MAP_FAILED) {
		perror("mmap");
		return 2;
	}
	printf("about to read SPMI status at base=%" PRIx64 " slave=%u\n", base, slave);
	printf("status=%08x\n", rd(0));
	if (strcmp(argv[1], "--status"))
		ret = read_pmu(0x1e00, 2);
	munmap((void *)regs, 0x4000);
	close(fd);
	close(lock);
	return ret;
}
