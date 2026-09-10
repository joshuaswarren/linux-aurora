/* Bounded, encrypted AVDTP discovery. No audio, pairing or key storage.
 * Build on ARM64 Linux: cc -O2 -Wall -Wextra -Werror \
 *     n1-bt-audio-probe.c -o n1-bt-audio-probe
 * The selected peer address is accepted as input but never printed.
 */
#include <errno.h>
#include <poll.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/socket.h>
#include <unistd.h>
#include <bluetooth/bluetooth.h>
#include <bluetooth/l2cap.h>

static void fail(const char *what)
{
	perror(what);
	exit(1);
}

static void wait_fd(int fd, short events, int timeout)
{
	struct pollfd p = { .fd = fd, .events = events };
	int rc;
	do { rc = poll(&p, 1, timeout); } while (rc < 0 && errno == EINTR);
	if (rc < 0) fail("poll");
	if (!rc) { errno = ETIMEDOUT; fail("AVDTP timeout"); }
	if (!(p.revents & events)) { errno = ECONNRESET; fail("AVDTP socket"); }
}

static size_t exchange(int fd, const uint8_t *request, size_t size,
		       uint8_t *response, size_t capacity)
{
	ssize_t n;
	wait_fd(fd, POLLOUT, 5000);
	n = send(fd, request, size, MSG_NOSIGNAL);
	if (n < 0) fail("AVDTP send");
	if ((size_t)n != size) { errno = EIO; fail("AVDTP short send"); }
	wait_fd(fd, POLLIN, 5000);
	n = recv(fd, response, capacity, MSG_TRUNC);
	if (n < 0) fail("AVDTP receive");
	if (n < 2 || (size_t)n > capacity) { errno = EPROTO; fail("AVDTP length"); }
	/* Single packet, matching transaction, response accept, same signal. */
	if (response[0] != (uint8_t)(request[0] | 2) ||
	    response[1] != request[1]) {
		fprintf(stderr, "AVDTP unexpected response: header=%02x signal=%02x\n",
			response[0], response[1]);
		exit(1);
	}
	return (size_t)n;
}

int main(int argc, char **argv)
{
	struct sockaddr_l2 peer = { .l2_family = AF_BLUETOOTH,
		.l2_psm = htobs(0x19), .l2_bdaddr_type = BDADDR_BREDR };
	struct bt_security sec = { .level = BT_SECURITY_MEDIUM };
	socklen_t len;
	uint8_t response[2048], request[] = { 0, 1 };
	uint8_t cap_request[] = { 0x10, 0x0c, 0 };
	int fd, error = 0;
	size_t n, i;
	unsigned sinks = 0;
	unsigned address[6];
	char trailing;
	if (argc != 2 || sscanf(argv[1], "%2x:%2x:%2x:%2x:%2x:%2x%c",
	    &address[0], &address[1], &address[2], &address[3], &address[4],
	    &address[5], &trailing) != 6) {
		fprintf(stderr, "Usage: n1-bt-audio-probe SELECTED_PEER_ADDRESS\n");
		return 2;
	}
	for (i = 0; i < 6; i++) peer.l2_bdaddr.b[i] = address[5 - i];
	fd = socket(AF_BLUETOOTH, SOCK_SEQPACKET | SOCK_NONBLOCK | SOCK_CLOEXEC,
		    BTPROTO_L2CAP);
	if (fd < 0) fail("L2CAP socket");
	if (setsockopt(fd, SOL_BLUETOOTH, BT_SECURITY, &sec, sizeof(sec)))
		fail("BT_SECURITY");
	if (connect(fd, (struct sockaddr *)&peer, sizeof(peer))) {
		if (errno != EINPROGRESS) fail("AVDTP connect");
		wait_fd(fd, POLLOUT, 20000);
		len = sizeof(error);
		if (getsockopt(fd, SOL_SOCKET, SO_ERROR, &error, &len)) fail("SO_ERROR");
		if (error) { errno = error; fail("AVDTP connect"); }
	}
	len = sizeof(sec);
	if (getsockopt(fd, SOL_BLUETOOTH, BT_SECURITY, &sec, &len))
		fail("BT_SECURITY readback");
	printf("AVDTP connected: security_level=%u key_bytes=%u\n", sec.level, sec.key_size);
	if (sec.level < BT_SECURITY_MEDIUM || sec.key_size < 16) {
		fprintf(stderr, "Expected encrypted connection with 16-byte key\n");
		return 1;
	}
	n = exchange(fd, request, sizeof(request), response, sizeof(response));
	if ((n - 2) % 2) { errno = EPROTO; fail("AVDTP endpoint length"); }
	printf("AVDTP endpoints=%zu\n", (n - 2) / 2);
	for (i = 2; i < n; i += 2) {
		unsigned seid = response[i] >> 2;
		unsigned media = response[i + 1] >> 4;
		unsigned sink = (response[i + 1] >> 3) & 1;
		unsigned in_use = (response[i] >> 1) & 1;
		printf("endpoint seid=%u media=%u sink=%u in_use=%u\n", seid, media, sink, in_use);
		if (!media && sink && !in_use && seid) {
			if (!sinks) cap_request[2] = seid << 2;
			sinks++;
		}
	}
	if (!sinks) { errno = ENOENT; fail("Available audio sink"); }
	n = exchange(fd, cap_request, sizeof(cap_request), response, sizeof(response));
	for (i = 2; i < n;) {
		unsigned category, size;
		if (n - i < 2) { errno = EPROTO; fail("AVDTP capability header"); }
		category = response[i]; size = response[i + 1]; i += 2;
		if (size > n - i) { errno = EPROTO; fail("AVDTP capability length"); }
		printf("capability category=%u bytes=%u", category, size);
		if (category == 7 && size >= 2)
			printf(" media=%u codec=%u", response[i] >> 4, response[i + 1]);
		putchar('\n'); i += size;
	}
	close(fd);
	puts("N1_ENCRYPTED_AVDTP_DISCOVERY_OK");
	return 0;
}
