// SPDX-License-Identifier: GPL-2.0-only
/* J700/X3200 LCD-enable qualification through the existing SMC GPIO driver.
 * Firmware configuration: gP16, on=1, off=0, lcd-off-on-ms=260.
 * Keep line ownership throughout a bounded cycle and restore on interruption.
 */
#include <errno.h>
#include <fcntl.h>
#include <linux/gpio.h>
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/ioctl.h>
#include <time.h>
#include <unistd.h>

static volatile sig_atomic_t stopped;
static void stop(int sig) { (void)sig; stopped = 1; }

static int value(int fd, int set, unsigned int val)
{
	struct gpio_v2_line_values values = { .mask = 1, .bits = val };
	if (ioctl(fd, set ? GPIO_V2_LINE_SET_VALUES_IOCTL : GPIO_V2_LINE_GET_VALUES_IOCTL,
		  &values) < 0)
		return -1;
	return values.bits & 1;
}

int main(int argc, char **argv)
{
	struct gpiochip_info chip = { 0 };
	struct gpio_v2_line_info info = { .offset = 22 };
	struct gpio_v2_line_request req = { .offsets = { 22 }, .num_lines = 1 };
	struct gpio_v2_line_config config = { .flags = GPIO_V2_LINE_FLAG_OUTPUT };
	struct sigaction sa = { .sa_handler = stop };
	struct timespec delay = { .tv_sec = 1 };
	char compatible[32], *end;
	unsigned long seconds = 0;
	int chipfd, fd, before, off, ret = 1;
	FILE *f;

	setbuf(stdout, NULL);
	if (argc != 2 && argc != 3)
		goto usage;
	if (argc == 3 && !strcmp(argv[1], "--cycle")) {
		errno = 0;
		seconds = strtoul(argv[2], &end, 10);
		if (errno || *end || seconds < 1 || seconds > 600)
			goto usage;
	} else if (argc != 2 || strcmp(argv[1], "--read")) {
		goto usage;
	}
	f = fopen("/sys/firmware/devicetree/base/compatible", "rb");
	if (!f)
		return 2;
	if (fread(compatible, 1, sizeof(compatible), f) < 11 ||
	    memcmp(compatible, "apple,j700\0", 11)) {
		fclose(f);
		return 2;
	}
	fclose(f);
	chipfd = open("/dev/gpiochip0", O_RDONLY | O_CLOEXEC);
	if (chipfd < 0 || ioctl(chipfd, GPIO_GET_CHIPINFO_IOCTL, &chip) < 0)
		return 2;
	if (strcmp(chip.label, "macsmc-pmu-gpio") || chip.lines != 64 ||
	    ioctl(chipfd, GPIO_V2_GET_LINEINFO_IOCTL, &info) < 0 ||
	    (info.flags & GPIO_V2_LINE_FLAG_USED)) {
		fprintf(stderr, "wrong chip, unreadable line, or line already owned\n");
		close(chipfd);
		return 2;
	}
	printf("gP16 line=22 flags=%#llx consumer=%s\n", info.flags, info.consumer);
	strcpy(req.consumer, "neo-panel-qualification");
	/* No input/output flag: preserve the inherited direction for the read. */
	if (ioctl(chipfd, GPIO_V2_GET_LINE_IOCTL, &req) < 0) {
		perror("request line without mode change");
		close(chipfd);
		return 2;
	}
	close(chipfd);
	fd = req.fd;
	before = value(fd, 0, 0);
	printf("inherited=%d\n", before);
	if (before < 0 || !seconds)
		goto out;
	if (!(info.flags & GPIO_V2_LINE_FLAG_OUTPUT) || before != 1) {
		fprintf(stderr, "require inherited output-high before LCD cycle\n");
		goto out;
	}
	sigemptyset(&sa.sa_mask);
	sigaction(SIGINT, &sa, NULL);
	sigaction(SIGTERM, &sa, NULL);
	config.num_attrs = 1;
	config.attrs[0].attr.id = GPIO_V2_LINE_ATTR_ID_OUTPUT_VALUES;
	config.attrs[0].attr.values = before;
	config.attrs[0].mask = 1;
	if (ioctl(fd, GPIO_V2_LINE_SET_CONFIG_IOCTL, &config) < 0) {
		perror("preserve output-high config");
		goto out;
	}
	printf("setting LCD off for %lu seconds\n", seconds);
	if (value(fd, 1, 0) < 0) {
		perror("set LCD off");
		goto restore;
	}
	off = value(fd, 0, 0);
	printf("off readback=%d\n", off);
	if (off != 0) {
		fprintf(stderr, "LCD-enable did not read back low\n");
		goto restore;
	}
	for (unsigned long i = 0; i < seconds && !stopped; i++)
		nanosleep(&delay, NULL);
	ret = 0;
restore:
	/* Respect the firmware's 260 ms minimum even after immediate SIGTERM. */
	delay.tv_sec = 0;
	delay.tv_nsec = 300000000;
	while (nanosleep(&delay, &delay) && errno == EINTR) {}
	if (value(fd, 1, before) < 0 || value(fd, 0, 0) != before) {
		perror("restore LCD enable");
		ret = 1;
	} else {
		printf("restored=%d\n", before);
	}
out:
	if (!seconds && before >= 0)
		ret = 0;
	close(fd);
	return ret;
usage:
	fprintf(stderr, "usage: %s --read | --cycle SECONDS(1..600)\n", argv[0]);
	return 2;
}
