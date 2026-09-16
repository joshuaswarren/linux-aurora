// SPDX-License-Identifier: GPL-2.0-only
/* Read-only J700 core power-state sampling, pinned to a selected CPU.
 * The observer perturbs that CPU/cluster. Fractions are sampled states, not
 * hardware residency counters. No PMGR or interrupt-controller writes.
 */
#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <sched.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>
#include <sys/mman.h>
#include <time.h>
#include <unistd.h>

int main(int argc, char **argv)
{
	static const unsigned offsets[] = { 0, 8, 16, 24, 48, 56 };
	static const char *names[] = { "E0", "E1", "E2", "E3", "P0", "P1" };
	unsigned states[6][16] = {}, both_p_off = 0;
	unsigned samples = 2000;
	int busy = 0, observer = 1;
	uint32_t last[6] = {}, seed = 0x814064;
	char compatible[64] = {};
	struct timespec start, end, delay;
	cpu_set_t affinity;
	FILE *f;
	int fd;
	void *pmgr, *aic;

	for (int i = 1; i < argc; i++) {
		if (!strcmp(argv[i], "--busy")) {
			busy = 1;
		} else if (!strcmp(argv[i], "--cpu") && i + 1 < argc &&
			   strlen(argv[i + 1]) == 1 &&
			   argv[i + 1][0] >= '0' && argv[i + 1][0] <= '5') {
			observer = argv[++i][0] - '0';
		} else {
			fprintf(stderr, "usage: %s [--busy] [--cpu 0..5]\n", argv[0]);
			return 2;
		}
	}
	if (busy)
		samples = 200000;
	f = fopen("/sys/firmware/devicetree/base/compatible", "rb");
	if (!f || fread(compatible, 1, sizeof(compatible), f) < 11 ||
	    memcmp(compatible, "apple,j700\0", 11))
		return 2;
	fclose(f);
	CPU_ZERO(&affinity);
	CPU_SET(observer, &affinity);
	if (sched_setaffinity(0, sizeof(affinity), &affinity)) {
		perror("sched_setaffinity");
		return 2;
	}
	fd = open("/dev/mem", O_RDONLY | O_SYNC | O_CLOEXEC);
	if (fd < 0)
		return 2;
	pmgr = mmap(NULL, 16384, PROT_READ, MAP_SHARED, fd, 0x300700000ULL);
	aic = mmap(NULL, 16384, PROT_READ, MAP_SHARED, fd, 0x301000000ULL);
	if (pmgr == MAP_FAILED || aic == MAP_FAILED)
		return 2;
	setbuf(stdout, NULL);
	/* These two AIC registers are read by the active kernel driver. */
	printf("{\"aic_version\":%u,\"aic_config\":%u,\"observer_cpu\":%d,",
	       *(volatile uint32_t *)aic, *(volatile uint32_t *)(aic + 0x14),
	       observer);
	clock_gettime(CLOCK_MONOTONIC, &start);
	for (unsigned i = 0; i < samples; i++) {
		for (unsigned c = 0; c < 6; c++) {
			last[c] = *(volatile uint32_t *)(pmgr + offsets[c]);
			states[c][(last[c] >> 4) & 15]++;
		}
		if (!(last[4] & 0xf0) && !(last[5] & 0xf0))
			both_p_off++;
		if (busy)
			continue;
		/* Deterministic jitter avoids synchronizing to a 100 Hz timer. */
		seed = seed * 1664525 + 1013904223;
		delay = (struct timespec){ .tv_nsec = 7000000 + seed % 6000000 };
		while (nanosleep(&delay, &delay) && errno == EINTR) {}
	}
	clock_gettime(CLOCK_MONOTONIC, &end);
	printf("\"seconds\":%.6f,\"samples\":%u,\"busy\":%d,\"both_p_off\":%u,\"cores\":[",
	       end.tv_sec - start.tv_sec + (end.tv_nsec - start.tv_nsec) / 1e9,
	       samples, busy, both_p_off);
	for (unsigned c = 0; c < 6; c++) {
		printf("%s{\"name\":\"%s\",\"last_word\":%u,\"actual_state_counts\":[",
		       c ? "," : "", names[c], last[c]);
		for (unsigned s = 0; s < 16; s++)
			printf("%s%u", s ? "," : "", states[c][s]);
		printf("]}");
	}
	puts("]}");
	munmap(aic, 16384);
	munmap(pmgr, 16384);
	close(fd);
	return 0;
}
