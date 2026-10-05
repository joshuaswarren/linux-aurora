// SPDX-License-Identifier: GPL-2.0-only
#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <pthread.h>
#include <stdatomic.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/auxv.h>
#include <sys/mman.h>
#include <sys/prctl.h>
#include <sys/stat.h>
#include <time.h>
#include <unistd.h>

static atomic_int stop;
static char *anon;
static size_t anon_len;
static const char *directory;
static unsigned long stress_loops;
static double now(void)
{
 struct timespec t;
 clock_gettime(CLOCK_MONOTONIC, &t);
 return t.tv_sec + t.tv_nsec / 1e9;
}
static void *pressure(void *unused)
{
 char name[512];
 (void)unused;
 prctl(PR_SET_NAME, "bcar-pressure");
 snprintf(name, sizeof(name), "%s/churn", directory);
 int fd = open(name, O_CREAT | O_RDWR | O_TRUNC, 0600);
 char *buf = malloc(1 << 20);
 if (fd < 0 || !buf) { perror("pressure setup"); atomic_store(&stop, 1); return NULL; }
 memset(buf, 0x57, 1 << 20);
 while (!atomic_load(&stop)) {
  for (int i = 0; i < 96 && !atomic_load(&stop); i++) {
   if (pwrite(fd, buf, 1 << 20, (off_t)i << 20) != 1 << 20) {
    fprintf(stderr, "PRESSURE write errno=%d %s\n", errno, strerror(errno));
    atomic_store(&stop, 1); break;
   }
  }
  for (size_t i = 0; i < anon_len; i += 16384) anon[i]++;
  stress_loops++;
 }
 fsync(fd);
 close(fd); free(buf);
 return NULL;
}
int main(int argc, char **argv)
{
 if (argc != 5) { fprintf(stderr, "usage: %s directory seconds anon_MiB gate\n", argv[0]); return 2; }
 directory = argv[1];
 unsigned seconds = atoi(argv[2]);
 anon_len = (size_t)atoi(argv[3]) << 20;
 prctl(PR_SET_NAME, "bcar-write");
 setvbuf(stdout, NULL, _IONBF, 0);
 printf("START pid=%d AT_PAGESZ=%lu duration=%u anon=%zu\n", getpid(), getauxval(AT_PAGESZ), seconds, anon_len);
 anon = mmap(NULL, anon_len, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
 if (anon == MAP_FAILED) { perror("mmap"); return 2; }
 for (size_t i = 0; i < anon_len; i += 4096) anon[i] = 1;
 FILE *files[16];
 char name[512], record[4093];
 memset(record, 'r', sizeof(record)); record[sizeof(record)-1] = '\n';
 for (unsigned i = 0; i < 16; i++) {
  snprintf(name, sizeof(name), "%s/append-%02u", directory, i);
  files[i] = fopen(name, "a");
  if (!files[i]) { perror("fopen"); return 2; }
 }
 snprintf(name, sizeof(name), "%s/inodes", directory);
 FILE *info = fopen(name, "w");
 if (!info) { perror("inodes"); return 2; }
 for (unsigned i = 0; i < 16; i++) {
  struct stat st;
  if (fstat(fileno(files[i]), &st)) { perror("fstat"); return 2; }
  fprintf(info, "%llu\n", (unsigned long long)st.st_ino);
  printf("FILE slot=%u ino=%llu\n", i, (unsigned long long)st.st_ino);
 }
 fclose(info);
 while (access(argv[4], F_OK)) usleep(10000);
 pthread_t worker;
 int err = pthread_create(&worker, NULL, pressure, NULL);
 if (err) { fprintf(stderr, "pthread_create: %s\n", strerror(err)); return 2; }
 unsigned long records = 0, bytes = 0, evictions = 0;
 int eio = 0, other = 0;
 double deadline = now() + seconds;
 while (now() < deadline && !atomic_load(&stop)) {
  FILE *f = files[records % 16];
  size_t len = records % 3 == 0 ? 17 : records % 3 == 1 ? 511 : sizeof(record);
  errno = 0;
  size_t n = fwrite(record, 1, len, f);
  int rc = fflush(f), saved = errno;
  if (n != len || rc) {
   fprintf(stderr, "FAIL clock=%.6f file=%lu record=%lu offset=%ld len=%zu wrote=%zu fflush=%d errno=%d %s\n", now(), records % 16, records, ftell(f), len, n, rc, saved, strerror(saved));
   if (saved == EIO) eio++; else other++;
   break;
  }
  bytes += len; records++;
  if (records % 64 == 0) {
   for (unsigned i = 0; i < 16; i++) {
    if (fdatasync(fileno(files[i]))) { perror("fdatasync"); other++; break; }
    int r = posix_fadvise(fileno(files[i]), 0, 0, POSIX_FADV_DONTNEED);
    if (r) { fprintf(stderr, "fadvise errno=%d\n", r); other++; break; }
    evictions++;
   }
   if (other) break;
  }
 }
 atomic_store(&stop, 1); pthread_join(worker, NULL);
 for (unsigned i = 0; i < 16; i++) if (fclose(files[i])) { perror("fclose"); other++; }
 munmap(anon, anon_len);
 unsigned long verified = 0;
 if (!eio && !other) {
  for (unsigned i = 0; i < 16; i++) {
   snprintf(name, sizeof(name), "%s/append-%02u", directory, i);
   FILE *f = fopen(name, "r");
   if (!f) { perror("verify fopen"); other++; break; }
   char got[sizeof(record)];
   for (unsigned long r = i; r < records; r += 16) {
    size_t len = r % 3 == 0 ? 17 : r % 3 == 1 ? 511 : sizeof(record);
    if (fread(got, 1, len, f) != len || memcmp(got, record, len)) {
     fprintf(stderr, "VERIFY failed file=%u record=%lu errno=%d\n", i, r, errno);
     other++; break;
    }
    verified += len;
   }
   if (fgetc(f) != EOF || ferror(f)) { fprintf(stderr, "VERIFY tail file=%u\n", i); other++; }
   fclose(f);
  }
 }
 printf("VERIFY bytes=%lu expected=%lu\n", verified, bytes);
 printf("RESULT records=%lu bytes=%lu evictions=%lu pressure_loops=%lu eio=%d other=%d\n", records, bytes, evictions, stress_loops, eio, other);
 return eio || other ? 1 : 0;
}
