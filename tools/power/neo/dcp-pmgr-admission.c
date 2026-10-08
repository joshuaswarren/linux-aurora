// SPDX-License-Identifier: GPL-2.0-only
/* J700-only awake PMGR access diagnostic. Never requests a lower PS target.
 * Run under a transient service: SSH output alone may be lost on a bus fault.
 * Each checkpoint is durable before the next MMIO operation. Recovery may
 * require DebugUSB. Do not run concurrently with display power experiments.
 */
#include <errno.h>
#include <fcntl.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mman.h>
#include <sys/utsname.h>
#include <unistd.h>

static int kmsg = -1;

static uint32_t rd(void *base, unsigned offset)
{
	uint32_t value = *(volatile uint32_t *)((char *)base + offset);
	__asm__ volatile("dmb oshld" ::: "memory");
	return value;
}

static void wr(void *base, unsigned offset, uint32_t value)
{
	*(volatile uint32_t *)((char *)base + offset) = value;
	__asm__ volatile("dsb oshst" ::: "memory");
}

static void checkpoint(const char *message)
{
	puts(message);
	if (fsync(STDOUT_FILENO)) {
		perror("fsync");
		exit(2);
	}
	if (dprintf(kmsg, "<6>NEO_PMGR_ADMISSION: %s\n", message) < 0)
		exit(2);
	usleep(500000);
}

int main(int argc, char **argv)
{
	const unsigned offsets[] = { 0x280, 0x318, 0x10000 };
	struct utsname uts;
	char compatible[11], boot_id[64] = { 0 };
	int fd, log;
	uint32_t value;
	void *pmgr, *asc;
	int manual, matched, readback_first;

	if (argc != 3 || (strcmp(argv[1], "--same-value") &&
			  strcmp(argv[1], "--same-value-readback") &&
			  strcmp(argv[1], "--manual-active"))) {
		fprintf(stderr, "usage: %s --same-value|--same-value-readback|--manual-active NEW_LOG\n", argv[0]);
		return 2;
	}
	manual = !strcmp(argv[1], "--manual-active");
	readback_first = !strcmp(argv[1], "--same-value-readback");
	kmsg = open("/dev/kmsg", O_WRONLY);
	if (kmsg < 0)
		return 2;
	log = open(argv[2], O_WRONLY | O_CREAT | O_EXCL | O_DSYNC, 0600);
	if (log < 0 || dup2(log, STDOUT_FILENO) < 0) {
		perror("open log");
		return 2;
	}
	close(log);
	setvbuf(stdout, NULL, _IONBF, 0);
	fd = open("/proc/sys/kernel/random/boot_id", O_RDONLY);
	if (fd < 0 || read(fd, boot_id, sizeof(boot_id) - 1) < 1)
		return 2;
	close(fd);
	if (uname(&uts))
		return 2;
	printf("mode=%s kernel=%s %s boot=%s", argv[1], uts.release, uts.version, boot_id);
	sync();
	checkpoint("admission-started");
	fd = open("/sys/firmware/devicetree/base/compatible", O_RDONLY);
	if (fd < 0 || read(fd, compatible, sizeof(compatible)) != sizeof(compatible) ||
	    memcmp(compatible, "apple,j700\0", sizeof(compatible)))
		return 2;
	close(fd);
	fd = open("/dev/mem", O_RDWR | O_SYNC);
	if (fd < 0)
		return 2;
	pmgr = mmap(NULL, 0x14000, PROT_READ | PROT_WRITE, MAP_SHARED, fd, 0x300700000ULL);
	asc = mmap(NULL, 0x4000, PROT_READ, MAP_SHARED, fd, 0x412e00000ULL);
	if (pmgr == MAP_FAILED || asc == MAP_FAILED)
		return 2;
	for (unsigned i = 0; i < sizeof(offsets) / sizeof(offsets[0]); i++) {
		value = rd(pmgr, offsets[i]);
		printf("PMGR[%x]=%08x\n", offsets[i], value);
		if ((value & 0xff) != 0xff || (value & 0x80001c00))
			return 2;
	}
	uint32_t control = rd(asc, 0x44), status = rd(asc, 0x48);
	printf("CPU control=%08x status=%08x\n", control, status);
	if (control != 0x10 || status != 0x6d || rd(pmgr, 0x10000) != 0x1f0000ff)
		return 2;
	value = manual ? 0x0f0000ff : 0x1f0000ff;
	printf("requested=%08x\n", value);
	checkpoint("before PMGR write");
	if (readback_first) {
		/* ApplePMGR emits a plain STR, then a status read. This isolated
		 * same-value probe distinguishes that ordering from the explicit
		 * post-write DSB in wr(). It never changes ownership or PS target.
		 * It is not a general recommendation to omit MMIO barriers.
		 */
		*(volatile uint32_t *)((char *)pmgr + 0x10000) = value;
		value = rd(pmgr, 0x10000);
		printf("immediate-readback=%08x\n", value);
		checkpoint("PMGR store/read returned; before explicit write barrier");
		__asm__ volatile("dsb oshst" ::: "memory");
		checkpoint("PMGR write barrier returned after readback");
	} else {
		wr(pmgr, 0x10000, value);
		checkpoint("PMGR write/barrier returned; before readback");
		value = rd(pmgr, 0x10000);
	}
	printf("readback=%08x\n", value);
	matched = value == (manual ? 0x0f0000ff : 0x1f0000ff);
	if (manual) {
		/* Restore AUTO even if the readback does not match the request. */
		checkpoint("before AUTO restore");
		wr(pmgr, 0x10000, 0x1f0000ff);
		checkpoint("AUTO restore write/barrier returned; before readback");
		value = rd(pmgr, 0x10000);
		printf("restored=%08x\n", value);
	}
	matched = matched && value == 0x1f0000ff;
	checkpoint(matched ? "admission-complete" : "readback-mismatch");
	return matched ? 0 : 3;
}
