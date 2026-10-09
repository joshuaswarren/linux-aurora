// SPDX-License-Identifier: GPL-2.0-only
/* Fixed, read-only J700 CPU energy accumulator snapshot. See cpu-energy.py.
 * Build: aarch64-linux-gnu-gcc -O2 -Wall -Wextra -Werror -static
 *        cpu-energy-read.c -o cpu-energy-read
 */
#define _GNU_SOURCE
#include <fcntl.h>
#include <inttypes.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mman.h>
#include <time.h>
#include <unistd.h>

static const uint64_t bases[] = {
	0x210059000, 0x210159000, 0x210259000, 0x210359000,
	0x211059000, 0x211159000, 0x210e48000, 0x211e48000,
};

int main(void)
{
	char compatible[128];
	FILE *f = fopen("/sys/firmware/devicetree/base/compatible", "rb");
	int fd;
	long page = sysconf(_SC_PAGESIZE);

	if (!f || fread(compatible, 1, 11, f) != 11 ||
	    memcmp(compatible, "apple,j700\0", 11) || page != 16384) {
		fprintf(stderr, "requires J700 with 16K pages\n");
		return 2;
	}
	fclose(f);
	fd = open("/dev/mem", O_RDONLY | O_SYNC);
	if (fd < 0) {
		perror("/dev/mem");
		return 2;
	}
	puts("[");
	for (unsigned int i = 0; i < sizeof(bases) / sizeof(bases[0]); i++) {
		uint64_t base = bases[i] & ~((uint64_t)page - 1);
		void *p = mmap(NULL, page, PROT_READ, MAP_SHARED, fd, base);

		if (p == MAP_FAILED) {
			perror("mmap");
			return 2;
		}
		for (unsigned int j = 0; j < 2; j++) {
			uint64_t addr = bases[i] + 8 * j, value;
			struct timespec ts;

			fprintf(stderr, "read64 0x%" PRIx64 "\n", addr);
			fflush(stderr);
			value = *(volatile uint64_t *)((char *)p + addr - base);
			__asm__ volatile("dsb sy" ::: "memory");
			clock_gettime(CLOCK_MONOTONIC, &ts);
			printf("%s{\"address\":\"0x%" PRIx64 "\",\"value\":%" PRIu64
			       ",\"time_ns\":%" PRIu64 "}", i || j ? ",\n" : "",
			       addr, value, (uint64_t)ts.tv_sec * 1000000000 + ts.tv_nsec);
			fflush(stdout);
		}
		munmap(p, page);
	}
	puts("\n]");
	close(fd);
	return 0;
}
