/* SPDX-License-Identifier: MIT */
/* Read only the verified J813 MTP SRAM aperture; never controller registers. */
#include <fcntl.h>
#include <stdint.h>
#include <stdio.h>
#include <sys/mman.h>
#include <unistd.h>

int main(void)
{
    const size_t size = 0x100000;
    int fd = open("/dev/mem", O_RDONLY | O_SYNC);
    if (fd < 0) {
        perror("open");
        return 1;
    }
    volatile const uint32_t *src = mmap(NULL, size, PROT_READ, MAP_SHARED,
                                      fd, (off_t)0x394c00000ULL);
    close(fd);
    if (src == MAP_FAILED) {
        perror("mmap");
        return 1;
    }
    uint32_t page[1024];
    for (size_t offset = 0; offset < size / sizeof(*src); offset += 1024) {
        for (size_t i = 0; i < 1024; i++)
            page[i] = src[offset + i];
        if (fwrite(page, sizeof(page), 1, stdout) != 1) {
            perror("write");
            munmap((void *)src, size);
            return 1;
        }
    }
    munmap((void *)src, size);
    return fflush(stdout) == 0 ? 0 : 1;
}
