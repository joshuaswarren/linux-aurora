// SPDX-License-Identifier: GPL-2.0-only
/* Bounded Linux Bluetooth management test; no device names/addresses printed. */
#include <errno.h>
#include <poll.h>
#include <signal.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>
#include <sys/socket.h>
#include <sys/random.h>
#include <time.h>
#include <unistd.h>

struct sockaddr_hci_local {
	sa_family_t family;
	uint16_t device;
	uint16_t channel;
};

static unsigned int reports, unique;
static unsigned char devices[256][7];
static unsigned char raw_reply[255];
static size_t raw_reply_length;
static volatile sig_atomic_t interrupted;

static uint16_t get16(const unsigned char *p)
{
	return p[0] | (uint16_t)p[1] << 8;
}

static uint32_t get32(const unsigned char *p)
{
	return get16(p) | (uint32_t)get16(p + 2) << 16;
}

static void put16(unsigned char *p, uint16_t value)
{
	p[0] = value;
	p[1] = value >> 8;
}

static int64_t now_ms(void)
{
	struct timespec ts;
	clock_gettime(CLOCK_MONOTONIC, &ts);
	return (int64_t)ts.tv_sec * 1000 + ts.tv_nsec / 1000000;
}

static void stop_signal(int signo)
{
	(void)signo;
	interrupted = 1;
}

static int receive_event(int fd, unsigned char *buffer, size_t capacity, int timeout)
{
	struct pollfd pollfd = { .fd = fd, .events = POLLIN };
	int n = poll(&pollfd, 1, timeout);
	unsigned int i;

	if (n <= 0)
		return n;
	n = read(fd, buffer, capacity);
	if (n < 6 || get16(buffer + 4) != n - 6)
		return -1;
	if (get16(buffer) == 0x0012 && get16(buffer + 2) == 0 && n >= 20) {
		reports++;
		for (i = 0; i < unique; i++)
			if (!memcmp(devices[i], buffer + 6, 7))
				break;
		if (i == unique && unique < 256)
			memcpy(devices[unique++], buffer + 6, 7);
	}
	return n;
}

static int command(int fd, uint16_t opcode, const void *payload, uint16_t length,
		   unsigned char *reply, size_t capacity)
{
	unsigned char request[16] = { 0 }, event[8192];
	int64_t deadline = now_ms() + 12000;
	int n, status;

	if (length > sizeof(request) - 6)
		return -1;
	put16(request, opcode);
	put16(request + 2, 0);
	put16(request + 4, length);
	if (length)
		memcpy(request + 6, payload, length);
	if (write(fd, request, length + 6) != length + 6)
		return -1;
	while (now_ms() < deadline && !interrupted) {
		n = receive_event(fd, event, sizeof(event), 250);
		if (n < 0 && errno != EINTR)
			return -1;
		if (n < 9 || get16(event + 2) != 0)
			continue;
		if ((get16(event) == 1 || get16(event) == 2) &&
		    get16(event + 6) == opcode)
			status = event[8];
		else
			continue;
		printf("MGMT opcode=0x%04x status=0x%02x reply_bytes=%d\n",
		       opcode, status, n - 9);
		if (status)
			return -1;
		if (reply && n - 9 > (int)capacity)
			return -1;
		if (reply)
			memcpy(reply, event + 9, n - 9);
		return n - 9;
	}
	fprintf(stderr, "MGMT opcode=0x%04x timed out or interrupted\n", opcode);
	return -1;
}

/* Diagnostic raw HCI scan while the management controller is powered. This
 * deliberately bypasses Linux discovery policy to isolate scan PHY/type.
 */
static int raw_event(int fd, unsigned char *event, size_t capacity, int timeout)
{
	struct pollfd pfd = { .fd = fd, .events = POLLIN };
	int n = poll(&pfd, 1, timeout);

	if (n <= 0)
		return n;
	n = read(fd, event, capacity);
	if (n < 3 || event[0] != 4 || event[2] != n - 3)
		return -1;
	if (n >= 5 && event[1] == 0x3e &&
	    (event[3] == 2 || event[3] == 0x0d))
		reports += event[4];
	return n;
}

static int raw_command(int fd, uint16_t opcode, const void *payload, size_t length)
{
	unsigned char request[260] = { 1 }, event[260];
	int64_t deadline = now_ms() + 4000;
	int n, status;

	if (length > 255)
		return -1;
	raw_reply_length = 0;
	put16(request + 1, opcode);
	request[3] = length;
	if (length)
		memcpy(request + 4, payload, length);
	if (write(fd, request, length + 4) != (ssize_t)length + 4)
		return -1;
	while (now_ms() < deadline && !interrupted) {
		n = raw_event(fd, event, sizeof(event), 250);
		if (n >= 7 && event[1] == 0x0e && get16(event + 4) == opcode) {
			status = event[6];
			raw_reply_length = n - 7;
			memcpy(raw_reply, event + 7, raw_reply_length);
		} else if (n >= 7 && event[1] == 0x0f && get16(event + 5) == opcode)
			status = event[3];
		else
			continue;
		printf("HCI opcode=0x%04x status=0x%02x\n", opcode, status);
		return status ? -1 : 0;
	}
	fprintf(stderr, "HCI opcode=0x%04x timed out or interrupted\n", opcode);
	return -1;
}

static int raw_scan(const char *mode, int native_address, int native_host_awake,
		    int public_address)
{
	struct sockaddr_hci_local address = {
		.family = AF_BLUETOOTH, .device = 0, .channel = 0,
	};
	struct {
		uint32_t type_mask, event_mask[2];
		uint16_t opcode;
	} filter = { 1U << 4, { 0xffffffff, 0xffffffff }, 0 };
	unsigned char legacy[7] = { 0, 0x60, 0, 0x30, 0, 1, 0 };
	unsigned char extended[8] = { 1, 0, 1, 0, 0x60, 0, 0x30, 0 };
	unsigned char enable[6] = { 1, 0, 0, 0, 0, 0 }, event[260];
	unsigned char temporary_public[6], native_public[6];
	int ext = !strncmp(mode, "extended-", 9);
	int active = strstr(mode, "-active") != NULL;
	int fd, ret = 1, i;
	int64_t deadline;

	fd = socket(AF_BLUETOOTH, SOCK_RAW | SOCK_CLOEXEC, 1);
	if (fd < 0 || bind(fd, (struct sockaddr *)&address, sizeof(address)) ||
	    setsockopt(fd, 0, 2, &filter, sizeof(filter))) {
		perror("Bluetooth raw HCI socket");
		if (fd >= 0)
			close(fd);
		return 1;
	}
	legacy[0] = active;
	legacy[5] = public_address ? 0 : 1;
	extended[0] = public_address ? 0 : 1;
	extended[3] = active;
	if (!strcmp(mode, "native-features")) {
		/* Native ACI read-only capability command, four little-endian bytes.
		 * macOS selects HRB Create Connection on Proxima when bit 9 is set.
		 */
		if (raw_command(fd, 0xffa3, NULL, 0) || raw_reply_length != 4)
			goto close;
		printf("NATIVE_FEATURES ota=0x%08x hrb_create=%u\n",
		       get32(raw_reply), !!(get32(raw_reply) & (1U << 9)));
		ret = 0;
		goto close;
	}
	if (native_address) {
		/* Host 26A5425a bluetoothd _ACI_WriteBdAddr sends six bytes via
		 * 0xfc06 during initialization. This opt-in RAM experiment verifies
		 * the target command by standard Read BD_ADDR before any scan.
		 */
		if (getrandom(temporary_public, sizeof(temporary_public), 0) != sizeof(temporary_public))
			goto close;
		temporary_public[5] = (temporary_public[5] & 0xfc) | 0x02;
		/* Hardware confirms the vendor command uses display/network order,
		 * opposite to standard HCI Read BD_ADDR's least-significant byte first.
		 */
		for (i = 0; i < 6; i++)
			native_public[i] = temporary_public[5 - i];
		if (raw_command(fd, 0xfc06, native_public, sizeof(native_public)) ||
		    raw_command(fd, 0x1009, NULL, 0))
			goto close;
		if (raw_reply_length != sizeof(temporary_public) ||
		    memcmp(raw_reply, temporary_public, sizeof(temporary_public))) {
			fputs("Native address readback mismatch; scan not started\n", stderr);
			goto close;
		}
		puts("NATIVE_ADDRESS_READBACK_VERIFIED temporary=1 factory_identity=0");
	}
	if (native_host_awake) {
		/* Native HostAwakeVSC passes true to the controller method, which
		 * inverts it before _ACI_WriteLocalHostState: wire bytes are 00 00.
		 */
		const unsigned char awake[2] = { 0, 0 };

		if (raw_command(fd, 0xfda0, awake, sizeof(awake)))
			goto close;
		puts("NATIVE_HOST_AWAKE_ACCEPTED");
	}
	if (!strcmp(mode, "native-host-awake")) {
		ret = 0;
		goto close;
	}
	printf("RAW_SCAN mode=%s phy=1M interval=60ms window=30ms duration=8s\n", mode);
	if (raw_command(fd, ext ? 0x2041 : 0x200b,
			ext ? extended : legacy, ext ? sizeof(extended) : sizeof(legacy)))
		goto close;
	if (raw_command(fd, ext ? 0x2042 : 0x200c, enable, ext ? 6 : 2))
		goto close;
	deadline = now_ms() + 8000;
	while (now_ms() < deadline && !interrupted)
		raw_event(fd, event, sizeof(event), 250);
	interrupted = 0;
	enable[0] = 0;
	if (raw_command(fd, ext ? 0x2042 : 0x200c, enable, ext ? 6 : 2))
		goto close;
	printf("RAW_SCAN_COMPLETE reports=%u\n", reports);
	ret = reports ? 0 : 2;
close:
	close(fd);
	return ret;
}

int main(int argc, char **argv)
{
	struct sockaddr_hci_local address = {
		.family = AF_BLUETOOTH, .device = 0xffff, .channel = 3,
	};
	unsigned char reply[512], event[8192], enabled = 1, disabled = 0, discovery = 6;
	unsigned char static_address[6];
	int fd, length, ret = 1, i, native_address = 0, native_host_awake = 0;
	int public_address;
	int64_t deadline;

	if (argc == 2 && (!strcmp(argv[1], "native-host-awake") ||
			  !strcmp(argv[1], "native-features"))) {
		setvbuf(stdout, NULL, _IONBF, 0);
		return raw_scan(argv[1], 0, !strcmp(argv[1], "native-host-awake"), 0);
	}
	if (argc > 4 || (argc >= 2 && strcmp(argv[1], "legacy-passive") &&
	    strcmp(argv[1], "legacy-active") && strcmp(argv[1], "extended-passive") &&
	    strcmp(argv[1], "extended-active"))) {
		fprintf(stderr, "usage: %s [native-features|native-host-awake|legacy-passive|legacy-active|extended-passive|extended-active [--native-address] [--native-host-awake]]\n", argv[0]);
		return 1;
	}
	for (i = 2; i < argc; i++) {
		if (!strcmp(argv[i], "--native-address") && !native_address)
			native_address = 1;
		else if (!strcmp(argv[i], "--native-host-awake") && !native_host_awake)
			native_host_awake = 1;
		else {
			fputs("Unknown or duplicate diagnostic option\n", stderr);
			return 1;
		}
	}
	setvbuf(stdout, NULL, _IONBF, 0);
	signal(SIGINT, stop_signal);
	signal(SIGTERM, stop_signal);
	fd = socket(AF_BLUETOOTH, SOCK_RAW | SOCK_CLOEXEC, 1);
	if (fd < 0 || bind(fd, (struct sockaddr *)&address, sizeof(address))) {
		perror("Bluetooth management socket");
		return 1;
	}
	length = command(fd, 4, NULL, 0, reply, sizeof(reply));
	if (length < 17)
		goto close;
	public_address = !!(reply[0] | reply[1] | reply[2] | reply[3] | reply[4] | reply[5]);
	printf("CONTROLLER version=%u manufacturer=%u address_nonzero=%u supported=0x%08x current=0x%08x\n",
	       reply[6], get16(reply + 7),
	       public_address,
	       get32(reply + 9), get32(reply + 13));
	if (!(get32(reply + 13) & (1U << 9)) &&
	    command(fd, 0x0d, &enabled, 1, NULL, 0) < 0)
		goto close;
	if (!(get32(reply + 13) & 1) && (argc >= 2 || !public_address) &&
	    (get32(reply + 13) & (1U << 7)) &&
	    command(fd, 0x2a, &disabled, 1, NULL, 0) < 0)
		goto close;
	if (!public_address &&
	    !(get32(reply + 13) & 1)) {
		/* No factory public address is provisioned yet. Qualify BLE using
		 * the standard volatile static-random address mechanism instead.
		 */
		if (getrandom(static_address, sizeof(static_address), 0) != sizeof(static_address))
			goto close;
		static_address[5] |= 0xc0;
		if (command(fd, 0x2b, static_address, sizeof(static_address), NULL, 0) < 0)
			goto close;
		puts("Temporary static-random BLE address configured; factory identity unresolved");
	}
	if (command(fd, 5, &enabled, 1, NULL, 0) < 0)
		goto close;
	if (argc >= 2) {
		ret = raw_scan(argv[1], native_address, native_host_awake, public_address);
		goto close;
	}
	if (command(fd, 0x23, &discovery, 1, NULL, 0) < 0)
		goto close;
	deadline = now_ms() + 8000;
	while (now_ms() < deadline && !interrupted)
		receive_event(fd, event, sizeof(event), 250);
	/* Always stop discovery, including when the bounded test is interrupted. */
	interrupted = 0;
	if (command(fd, 0x24, &discovery, 1, NULL, 0) < 0)
		goto close;
	printf("DISCOVERY_COMPLETE reports=%u unique_devices=%u\n", reports, unique);
	ret = reports ? 0 : 2;
close:
	close(fd);
	return ret;
}
