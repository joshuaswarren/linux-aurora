/* Bounded ARP/DHCP discovery experiment. No address or lease is configured. */
#define _GNU_SOURCE
#include <arpa/inet.h>
#include <errno.h>
#include <linux/if_ether.h>
#include <linux/if_packet.h>
#include <net/if.h>
#include <poll.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>
#include <sys/ioctl.h>
#include <sys/random.h>
#include <sys/socket.h>
#include <time.h>
#include <unistd.h>

static uint64_t milliseconds(void)
{
    struct timespec value;
    clock_gettime(CLOCK_MONOTONIC, &value);
    return (uint64_t)value.tv_sec * 1000 + value.tv_nsec / 1000000;
}

static void put16(unsigned char *p, unsigned value)
{
    p[0] = value >> 8;
    p[1] = value;
}

static unsigned get16(const unsigned char *p)
{
    return (unsigned)p[0] * 256 + p[1];
}

static int unicast_address(const char *text, unsigned char mac[ETH_ALEN])
{
    unsigned values[ETH_ALEN];
    int used = 0;
    if (strlen(text) != 17 ||
        sscanf(text, "%2x:%2x:%2x:%2x:%2x:%2x%n", &values[0], &values[1],
               &values[2], &values[3], &values[4], &values[5], &used) != 6 ||
        used != 17) return -1;
    for (unsigned i = 0; i < ETH_ALEN; i++) mac[i] = values[i];
    return (mac[0] & 1) || !memcmp(mac, "\0\0\0\0\0\0", ETH_ALEN) ? -1 : 0;
}

static int firmware_station_mac(unsigned char mac[ETH_ALEN])
{
    unsigned char header[80] = {0};
    char line[160];
    unsigned valid, size, tag, length, offset, value, bitmap, link;
    int result;
    FILE *file = fopen("/sys/kernel/debug/apple_centauri/0000:01:00.0/station_stats", "r");
    if (!file) return -1;
    if (!fgets(line, sizeof(line), file) ||
        sscanf(line, "rc=%d valid=%u size=%u tag=%x length=%u",
               &result, &valid, &size, &tag, &length) != 5 ||
        result || valid != 1 || size != 780 || tag != 0xf928 || length != 752) {
        fclose(file);
        errno = EPROTO;
        return -1;
    }
    while (fgets(line, sizeof(line), file)) {
        if (sscanf(line, "%x %u", &offset, &value) != 2 || offset % 4 ||
            offset >= sizeof(header)) continue;
        for (unsigned i = 0; i < 4; i++) header[offset + i] = value >> (8 * i);
    }
    if (ferror(file)) { fclose(file); errno = EIO; return -1; }
    fclose(file);
    /* Measured common header: isStaMLD, link bitmap, three 24-byte records.
     * Peer at +5 and local at +11 match the host statistics printer. The
     * counter portion differs between firmware versions and is not decoded.
     */
    bitmap = header[4];
    if (header[0] || header[1] || header[2] || header[3] ||
        header[5] || header[6] || header[7] ||
        (bitmap != 1 && bitmap != 2 && bitmap != 4)) {
        errno = EPROTO;
        return -1;
    }
    link = bitmap == 1 ? 0 : bitmap == 2 ? 1 : 2;
    memcpy(mac, header + 8 + link * 24 + 11, ETH_ALEN);
    if ((mac[0] & 1) || !memcmp(mac, "\0\0\0\0\0\0", ETH_ALEN)) {
        errno = EPROTO;
        return -1;
    }
    return 0;
}

static unsigned checksum(const unsigned char *data, size_t length, uint32_t sum)
{
    while (length >= 2) { sum += get16(data); data += 2; length -= 2; }
    if (length) sum += (unsigned)data[0] << 8;
    while (sum >> 16) sum = (sum & 0xffff) + (sum >> 16);
    return (~sum) & 0xffff;
}

static size_t dhcp_discover(unsigned char *frame, const unsigned char *mac, uint32_t xid)
{
    unsigned char *ip = frame + ETH_HLEN, *udp = ip + 20, *bootp = udp + 8;
    const unsigned udp_length = 308; /* BOOTP/DHCP padded to 300 bytes. */
    uint32_t network_xid = htonl(xid);
    memset(frame, 0, ETH_HLEN + 20 + udp_length);
    memset(frame, 0xff, ETH_ALEN);
    memcpy(frame + ETH_ALEN, mac, ETH_ALEN);
    put16(frame + 12, ETH_P_IP);
    ip[0] = 0x45;
    put16(ip + 2, 20 + udp_length);
    put16(ip + 4, 0x4e31);
    ip[8] = 64; ip[9] = 17;
    memset(ip + 16, 0xff, 4);
    put16(ip + 10, checksum(ip, 20, 0));
    put16(udp, 68); put16(udp + 2, 67); put16(udp + 4, udp_length);
    bootp[0] = 1; bootp[1] = 1; bootp[2] = ETH_ALEN;
    memcpy(bootp + 4, &network_xid, 4);
    put16(bootp + 10, 0x8000); /* Broadcast offers; no IP address is held. */
    memcpy(bootp + 28, mac, ETH_ALEN);
    memcpy(bootp + 236, "\x63\x82\x53\x63", 4);
    bootp[240] = 53; bootp[241] = 1; bootp[242] = 1;
    bootp[243] = 61; bootp[244] = 7; bootp[245] = 1;
    memcpy(bootp + 246, mac, ETH_ALEN);
    bootp[252] = 55; bootp[253] = 3;
    bootp[254] = 1; bootp[255] = 3; bootp[256] = 6; bootp[257] = 255;
    unsigned value = checksum(udp, udp_length, 0xffff + 0xffff + 17 + udp_length);
    put16(udp + 6, value ? value : 0xffff);
    return ETH_HLEN + 20 + udp_length;
}

static unsigned dhcp_response(const unsigned char *frame, size_t size,
                              const unsigned char *mac, uint32_t xid)
{
    if (size < ETH_HLEN + 20 || get16(frame + 12) != ETH_P_IP) return 0;
    const unsigned char *ip = frame + ETH_HLEN;
    unsigned ihl = (ip[0] & 15) * 4, total = get16(ip + 2);
    if (ip[0] >> 4 != 4 || ihl < 20 || ip[9] != 17 ||
        size < ETH_HLEN + total || total < ihl + 8 + 240 ||
        (get16(ip + 6) & 0x3fff) || checksum(ip, ihl, 0)) return 0;
    const unsigned char *udp = ip + ihl, *bootp = udp + 8;
    unsigned length = get16(udp + 4);
    uint32_t network_xid = htonl(xid);
    if (get16(udp) != 67 || get16(udp + 2) != 68 || length < 248 ||
        length > total - ihl || bootp[0] != 2 || bootp[1] != 1 || bootp[2] != 6 ||
        memcmp(bootp + 4, &network_xid, 4) || memcmp(bootp + 28, mac, ETH_ALEN) ||
        memcmp(bootp + 236, "\x63\x82\x53\x63", 4)) return 0;
    if (get16(udp + 6) && checksum(udp, length, get16(ip + 12) + get16(ip + 14) +
                                  get16(ip + 16) + get16(ip + 18) + 17 + length)) return 0;
    for (unsigned off = 240; off < length - 8;) {
        unsigned tag = bootp[off++];
        if (tag == 255) break;
        if (!tag) continue;
        if (off >= length - 8) break;
        unsigned n = bootp[off++];
        if (n > length - 8 - off) break;
        if (tag == 53 && n == 1 && bootp[off] == 2 &&
            memcmp(bootp + 16, "\0\0\0\0", 4)) return 2;
        off += n;
    }
    return 1;
}

int main(int argc, char **argv)
{
    struct in_addr target;
    struct ifreq request = { .ifr_name = "wlan0" };
    struct sockaddr_ll peer = { .sll_family = AF_PACKET,
        .sll_protocol = htons(ETH_P_ARP), .sll_halen = ETH_ALEN };
    unsigned char mac[ETH_ALEN], frame[512] = {0}, received[2048];
    unsigned char *ethernet, *arp;
    unsigned prefix = 0, llc = 0, dhcp = 0, unicast = 0;
    unsigned sent = 0, replies = 0, matching = 0;
    uint32_t xid = 0;
    uint64_t start, next;
    size_t length;
    int fd;

    if ((argc != 3 && argc != 4) || inet_pton(AF_INET, argv[1], &target) != 1)
        return 2;
    if (!strcmp(argv[2], "prefix2")) prefix = 2;
    else if (!strcmp(argv[2], "llc")) llc = 8;
    else if (!strcmp(argv[2], "dhcp")) dhcp = 1;
    else if (!strcmp(argv[2], "unicast")) unicast = 1;
    else if (strcmp(argv[2], "ethernet")) return 2;
    if (unicast) {
        if (argc != 4 || unicast_address(argv[3], peer.sll_addr)) return 2;
    } else if (argc == 4 && strcmp(argv[3], "firmware")) return 2;
    if (dhcp) peer.sll_protocol = htons(ETH_P_IP);
    fd = socket(AF_PACKET, SOCK_RAW | SOCK_NONBLOCK, htons(ETH_P_ALL));
    if (fd < 0) { printf("N1_PACKET_PROBE error=%d\n", errno); return 2; }
    if (ioctl(fd, SIOCGIFINDEX, &request) < 0) goto failed;
    peer.sll_ifindex = request.ifr_ifindex;
    if (ioctl(fd, SIOCGIFHWADDR, &request) < 0) goto failed;
    memcpy(mac, request.ifr_hwaddr.sa_data, ETH_ALEN);
    if (argc == 4 && !unicast && firmware_station_mac(mac)) goto failed;
    {
        struct sockaddr_ll receiver = peer;
        receiver.sll_protocol = htons(ETH_P_ALL);
        if (bind(fd, (struct sockaddr *)&receiver, sizeof(receiver)) < 0)
            goto failed;
    }
    if (!unicast) memset(peer.sll_addr, 0xff, ETH_ALEN);
    /* FF padding keeps the diagnostic driver's broadcast-only filter valid.
     * The experiment asks whether firmware skips this prefix; it does not
     * claim this is a valid Ethernet frame at the Linux packet boundary.
     */
    memset(frame, 0xff, prefix + ETH_ALEN);
    ethernet = frame + prefix;
    memcpy(ethernet, peer.sll_addr, ETH_ALEN);
    memcpy(ethernet + ETH_ALEN, mac, ETH_ALEN);
    put16(ethernet + 12, llc ? 28 + llc : ETH_P_ARP);
    if (llc) {
        const unsigned char snap[8] = {0xaa, 0xaa, 3, 0, 0, 0, 8, 6};
        memcpy(ethernet + ETH_HLEN, snap, sizeof(snap));
    }
    arp = ethernet + ETH_HLEN + llc;
    put16(arp, 1);
    put16(arp + 2, ETH_P_IP);
    arp[4] = ETH_ALEN;
    arp[5] = 4;
    put16(arp + 6, 1);
    memcpy(arp + 8, mac, ETH_ALEN);
    if (unicast) memcpy(arp + 18, peer.sll_addr, ETH_ALEN);
    /* Sender IP zero: no address claim. Unicast tests a known peer directly. */
    memcpy(arp + 24, &target, 4);
    length = prefix + ETH_HLEN + llc + 28;
    if (dhcp) {
        if (getrandom(&xid, sizeof(xid), 0) != sizeof(xid)) goto failed;
        length = dhcp_discover(frame, mac, xid);
    }
    start = next = milliseconds();
    while (milliseconds() - start < 5500) {
        struct pollfd pollfd = { .fd = fd, .events = POLLIN };
        struct sockaddr_ll from;
        socklen_t from_length = sizeof(from);
        ssize_t count;
        if (sent < 3 && milliseconds() >= next) {
            if (sendto(fd, frame, length, 0, (struct sockaddr *)&peer,
                       sizeof(peer)) != (ssize_t)length) goto failed;
            sent++;
            next = start + sent * 1000;
        }
        if (poll(&pollfd, 1, 100) <= 0) continue;
        count = recvfrom(fd, received, sizeof(received), 0,
                         (struct sockaddr *)&from, &from_length);
        if (count < ETH_HLEN + 28 || from.sll_pkttype == PACKET_OUTGOING)
            continue;
        if (dhcp) {
            unsigned response = dhcp_response(received, count, mac, xid);
            replies += response != 0;
            matching += response == 2;
            continue;
        }
        unsigned offset = ETH_HLEN;
        unsigned type = get16(received + 12);
        if (type <= 1500 && count >= ETH_HLEN + 8 + 28 &&
            !memcmp(received + ETH_HLEN, "\xaa\xaa\x03\x00\x00\x00", 6)) {
            type = get16(received + ETH_HLEN + 6);
            offset += 8;
        }
        arp = received + offset;
        if (type != ETH_P_ARP || get16(arp) != 1 ||
            get16(arp + 2) != ETH_P_IP || arp[4] != ETH_ALEN || arp[5] != 4 ||
            get16(arp + 6) != 2 || memcmp(arp + 14, &target, 4)) continue;
        replies++;
        matching += !memcmp(arp + 18, mac, ETH_ALEN);
    }
    if (dhcp)
        printf("N1_DHCP_PROBE source=%s sent=%u replies=%u offers=%u\n",
               argc == 4 ? "firmware" : "netdev", sent, replies, matching);
    else
        printf("N1_PACKET_PROBE mode=%s source=%s sent=%u replies=%u target_matches_local=%u\n",
               argv[2], argc == 4 && !unicast ? "firmware" : "netdev", sent, replies, matching);
    close(fd);
    return matching ? 0 : 1;
failed:
    printf("N1_PACKET_PROBE error=%d\n", errno);
    close(fd);
    return 2;
}
