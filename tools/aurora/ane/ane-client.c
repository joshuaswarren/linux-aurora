// SPDX-License-Identifier: GPL-2.0-only
/* Userspace client and on-device qualification for the private ANE runtime.
 * Build on Linux with the same apple-ane-user.h as the kernel module.
 */
#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <math.h>
#include <stdbool.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/ioctl.h>
#include <sys/mman.h>
#include <sys/wait.h>
#include <unistd.h>
#include "apple-ane-user.h"

typedef uint8_t u8;
typedef uint32_t u32;
static inline uint16_t get_unaligned_le16(const void *p)
{
	uint16_t v;
	memcpy(&v, p, 2);
	return v;
}
#include "apple-ane-tiny.h"

_Static_assert(sizeof(struct aurora_ane_run) == 64, "RUN layout");
_Static_assert(sizeof(struct aurora_ane_info) == 88, "INFO layout");
static unsigned sequence, native_values, cpu_values;

static void require(bool condition, const char *what)
{
	if (!condition) {
		fprintf(stderr, "ANE_CLIENT_FAIL %s errno=%d (%s)\n", what, errno, strerror(errno));
		exit(1);
	}
}

static void expect_errno(int result, int expected, const char *what)
{
	require(result == -1 && errno == expected, what);
}

static size_t read_file(const char *path, void *buffer, size_t capacity, bool exact)
{
	FILE *file = fopen(path, "rb");
	require(file != NULL, path);
	size_t size = fread(buffer, 1, capacity, file);
	require(!ferror(file) && fgetc(file) == EOF && (!exact || size == capacity), "file size");
	require(fclose(file) == 0, "close input");
	return size;
}

static void path_join(char *out, size_t size, const char *root, const char *name)
{
	int result = snprintf(out, size, "%s/%s", root, name);
	require(result >= 0 && (size_t)result < size, "path length");
}

static struct aurora_ane_info info(int fd)
{
	struct aurora_ane_info result;
	require(ioctl(fd, AURORA_ANE_INFO, &result) == 0, "INFO");
	require(result.abi == AURORA_ANE_ABI && result.model == AURORA_ANE_MODEL_TINY260K &&
		result.input_bytes == 4096 && result.output_bytes == 32768 && result.max_jobs == 256,
		"runtime capabilities");
	require(!result.reserved[0] && !result.reserved[1] && !result.reserved[2], "INFO reserved");
	return result;
}

static struct aurora_ane_run request(const void *input, void *output)
{
	return (struct aurora_ane_run) {
		.abi = AURORA_ANE_ABI, .input_bytes = 4096, .output_bytes = 32768,
		.input = (uintptr_t)input, .output = (uintptr_t)output, .cookie = 0x123456789abcULL,
	};
}

static void run(int fd, const void *input, void *output)
{
	struct aurora_ane_run job = request(input, output);
	memset(output, 0xa5, 32768);
	require(ioctl(fd, AURORA_ANE_RUN, &job) == 0, "RUN");
	sequence++;
	require(job.sequence == sequence && job.elapsed_ns && job.cookie == 0x123456789abcULL,
		"RUN completion metadata");
	struct aurora_ane_info state = info(fd);
	require(state.completed_jobs == sequence && state.events == sequence * 3ULL &&
		state.state == AURORA_ANE_STATE_LOADED && !state.last_error, "completion counters");
}

static void invalid_requests(int fd, u8 *input, u8 *output)
{
	struct aurora_ane_run good = request(input, output), bad;
	for (unsigned i = 0; i < 12; i++) {
		bad = good;
		switch (i) {
		case 0: bad.abi = 0; break;
		case 1: bad.flags = 1; break;
		case 2: bad.input_bytes--; break;
		case 3: bad.output_bytes++; break;
		case 4: bad.reserved[0] = 1; break;
		case 5: bad.reserved[1] = 1; break;
		case 6: bad.sequence = 1; break;
		case 7: bad.elapsed_ns = 1; break;
		case 8: bad.input = 0; break;
		case 9: bad.output = 0; break;
		case 10: bad.input = 1; break;
		case 11: bad.output = UINT64_MAX; break;
		}
		expect_errno(ioctl(fd, AURORA_ANE_RUN, &bad), i < 10 ? EINVAL : EFAULT, "invalid RUN");
	}
	expect_errno(ioctl(fd, AURORA_ANE_RUN, (void *)1), EFAULT, "invalid request pointer");
	u8 saved[2]; memcpy(saved, input, 2); input[0] = 0; input[1] = 0x7c;
	expect_errno(ioctl(fd, AURORA_ANE_RUN, &good), EDOM, "nonfinite input");
	memcpy(input, saved, 2);
	struct aurora_ane_info state = info(fd);
	require(state.completed_jobs == 0 && state.events == 0, "rejected inputs must not execute");
	puts("ANE_INVALID_REQUESTS_PASS jobs=0 events=0");
}

static void copyback_errors(int fd, u8 *input, u8 *output, const u8 *expected)
{
	void *readonly_output = mmap(NULL, 32768, PROT_READ, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
	require(readonly_output != MAP_FAILED, "map read-only output");
	struct aurora_ane_run job = request(input, readonly_output);
	expect_errno(ioctl(fd, AURORA_ANE_RUN, &job), EFAULT, "output copyback fault");
	sequence++;
	struct aurora_ane_info state = info(fd);
	require(state.completed_jobs == sequence && state.events == sequence * 3ULL &&
		state.state == AURORA_ANE_STATE_LOADED, "copyback retains completion");
	require(munmap(readonly_output, 32768) == 0, "unmap read-only output");
	size_t page = (size_t)sysconf(_SC_PAGESIZE);
	struct aurora_ane_run *readonly_request = mmap(NULL, page, PROT_READ | PROT_WRITE,
						    MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
	require(readonly_request != MAP_FAILED, "map request");
	*readonly_request = request(input, output);
	require(mprotect(readonly_request, page, PROT_READ) == 0, "protect request");
	expect_errno(ioctl(fd, AURORA_ANE_RUN, readonly_request), EFAULT, "request copyback fault");
	sequence++;
	require(!memcmp(output, expected, 32768), "output survives request copyback fault");
	native_values += 16384;
	state = info(fd);
	require(state.completed_jobs == sequence && state.events == sequence * 3ULL &&
		state.state == AURORA_ANE_STATE_LOADED, "second copyback retains completion");
	require(munmap(readonly_request, page) == 0, "unmap request");
	puts("ANE_COPYBACK_ERRORS_PASS completed=2 events=6 session_usable=1");
}

static void decode_fixture(int fd, const char *root, const char *name, const u8 *embeddings)
{
	char folder[1024], path[1200], filename[100];
	u32 tokens[33], expected_tokens[8], next;
	u8 input[4096], output[32768], expected_input[4096];
	float cpu[16384];
	path_join(folder, sizeof(folder), root, name);
	path_join(path, sizeof(path), folder, "prompt-tokens.bin");
	size_t bytes = read_file(path, tokens, sizeof(tokens), false);
	require(bytes && bytes % 4 == 0 && bytes / 4 + 8 <= 33, "prompt length");
	unsigned count = bytes / 4;
	path_join(path, sizeof(path), folder, "next-tokens.bin");
	read_file(path, expected_tokens, sizeof(expected_tokens), true);
	for (unsigned step = 0; step < 8; step++) {
		require(apple_ane_tiny_input(embeddings, 65536, tokens, count, input, 4096) == 0, "embedding lookup");
		snprintf(filename, sizeof(filename), "%u.bin", step);
		path_join(path, sizeof(path), folder, filename);
		read_file(path, expected_input, sizeof(expected_input), true);
		require(!memcmp(input, expected_input, 4096), "autoregressive input matches CPU prefix");
		run(fd, input, output);
		snprintf(filename, sizeof(filename), "%u-cpu-logits.bin", step);
		path_join(path, sizeof(path), folder, filename);
		read_file(path, cpu, sizeof(cpu), true);
		float max_error = 0;
		for (unsigned i = 0; i < 16384; i++) {
			_Float16 half;
			memcpy(&half, output + i * 2, 2);
			float value = (float)half;
			require(isfinite(value) && isfinite(cpu[i]), "finite logits");
			float error = fabsf(value - cpu[i]);
			if (error > max_error) max_error = error;
		}
		require(apple_ane_tiny_argmax(output, 32768, count - 1, &next) == 0, "greedy selection");
		printf("ANE_USER_DECODE prompt=%s step=%u context=%u token=%u expected=%u max_cpu_error=%.9g\n",
			name, step, count, next, expected_tokens[step], max_error);
		require(max_error < 0.3f && next == expected_tokens[step], "CPU numerical reference");
		cpu_values += 16384;
		tokens[count++] = next;
	}
	printf("ANE_PROMPT_PASS name=%s generated=8 tokens=", name);
	for (unsigned i = count - 8; i < count; i++) printf("%s%u", i == count - 8 ? "" : ",", tokens[i]);
	puts("");
}

static void qualify(int fd, const char *device, const char *root)
{
	u8 inputs[3][4096], expected[3][32768], output[32768], embeddings[65536];
	char path[1200], name[100];
	for (unsigned i = 0; i < 3; i++) {
		snprintf(name, sizeof(name), "input-%u.bin", i); path_join(path, sizeof(path), root, name);
		read_file(path, inputs[i], sizeof(inputs[i]), true);
		snprintf(name, sizeof(name), "output-%u.bin", i); path_join(path, sizeof(path), root, name);
		read_file(path, expected[i], sizeof(expected[i]), true);
	}
	path_join(path, sizeof(path), root, "embeddings.bin");
	read_file(path, embeddings, sizeof(embeddings), true);
	struct aurora_ane_info state = info(fd);
	require(!state.completed_jobs && !state.events && state.state == AURORA_ANE_STATE_READY, "fresh runtime");
	int other = open(device, O_RDWR | O_CLOEXEC);
	expect_errno(other, EBUSY, "exclusive owner");
	expect_errno(ioctl(fd, _IO('J', 0xee)), ENOTTY, "unknown ioctl");
	expect_errno(ioctl(fd, AURORA_ANE_INFO, (void *)1), EFAULT, "bad INFO pointer");
	expect_errno(lseek(fd, 0, SEEK_SET), ESPIPE, "nonseekable device");
	expect_errno(ioctl(fd, AURORA_ANE_LOAD, 1UL), EINVAL, "LOAD takes no arguments");
	struct aurora_ane_run job = request(inputs[0], output);
	expect_errno(ioctl(fd, AURORA_ANE_RUN, &job), ESHUTDOWN, "RUN before LOAD");
	pid_t child = fork(); require(child >= 0, "fork permission check");
	if (!child) {
		require(setuid(65534) == 0, "drop privilege");
		expect_errno(ioctl(fd, AURORA_ANE_INFO, &state), EPERM, "inherited fd capability check");
		expect_errno(open(device, O_RDWR), EACCES, "device permissions");
		_exit(0);
	}
	int status; require(waitpid(child, &status, 0) == child && WIFEXITED(status) && !WEXITSTATUS(status), "permission child");
	puts("ANE_OWNER_AND_PERMISSION_PASS");
	require(ioctl(fd, AURORA_ANE_LOAD, 0UL) == 0, "LOAD");
	expect_errno(ioctl(fd, AURORA_ANE_LOAD, 0UL), EBUSY, "duplicate LOAD");
	invalid_requests(fd, inputs[0], output);
	copyback_errors(fd, inputs[0], output, expected[0]);
	for (unsigned i = 0; i < 237; i++) {
		run(fd, inputs[i % 3], output);
		require(!memcmp(output, expected[i % 3], sizeof(output)), "exact native output");
		native_values += 16384;
		if ((i + 1) % 32 == 0 || i == 236)
			printf("ANE_NATIVE_REPEAT_PASS repeated=%u completed=%u native_values=%u\n", i + 1, sequence, native_values);
	}
	decode_fixture(fd, root, "one-day", embeddings);
	decode_fixture(fd, root, "little-girl", embeddings);
	run(fd, inputs[0], output);
	require(!memcmp(output, expected[0], sizeof(output)), "last native output");
	native_values += 16384;
	job = request(inputs[0], output);
	expect_errno(ioctl(fd, AURORA_ANE_RUN, &job), EOVERFLOW, "session job limit");
	state = info(fd);
	require(sequence == 256 && state.completed_jobs == 256 && state.events == 768, "limit consumes no work");
	require(ioctl(fd, AURORA_ANE_SHUTDOWN, 0UL) == 0, "SHUTDOWN");
	require(ioctl(fd, AURORA_ANE_SHUTDOWN, 0UL) == 0, "idempotent SHUTDOWN");
	state = info(fd);
	require(state.state == AURORA_ANE_STATE_CLOSED && state.flags == AURORA_ANE_RESET_REQUIRED &&
		!state.last_error && state.completed_jobs == 256 && state.events == 768, "closed state");
	expect_errno(ioctl(fd, AURORA_ANE_RUN, &job), ESHUTDOWN, "RUN after shutdown");
	printf("ANE_RUNTIME_QUALIFICATION_PASS jobs=%u events=768 native_values=%u cpu_values=%u decoded_tokens=16\n",
		sequence, native_values, cpu_values);
}

int main(int argc, char **argv)
{
	setvbuf(stdout, NULL, _IOLBF, 0);
	if (!((argc == 4 && !strcmp(argv[2], "--qualify")) ||
	      (argc == 5 && !strcmp(argv[2], "--run")))) {
		fprintf(stderr, "Usage: %s DEVICE --qualify FIXTURES | DEVICE --run INPUT OUTPUT\n", argv[0]);
		return 2;
	}
	int fd = open(argv[1], O_RDWR | O_CLOEXEC);
	require(fd >= 0, "open device");
	if (!strcmp(argv[2], "--qualify")) {
		qualify(fd, argv[1], argv[3]);
	} else {
		u8 input[4096], output[32768];
		read_file(argv[3], input, sizeof(input), true);
		info(fd);
		require(ioctl(fd, AURORA_ANE_LOAD, 0UL) == 0, "LOAD");
		run(fd, input, output);
		FILE *file = fopen(argv[4], "wb"); require(file != NULL, "open output");
		require(fwrite(output, 1, sizeof(output), file) == sizeof(output) && fclose(file) == 0, "write output");
		/* Normal application exit tests the driver's final-fd-close path. */
	}
	require(close(fd) == 0, "close runtime");
	fd = open(argv[1], O_RDWR | O_CLOEXEC); require(fd >= 0, "reopen for final INFO");
	struct aurora_ane_info state = info(fd);
	require(state.state == AURORA_ANE_STATE_CLOSED && state.completed_jobs == sequence &&
		state.events == sequence * 3ULL && !state.last_error, "final close state");
	printf("ANE_FINAL_CLOSE_PASS jobs=%u events=%llu state=closed reset_required=%u\n",
		sequence, (unsigned long long)state.events, state.flags);
	require(close(fd) == 0, "close info fd");
	return 0;
}
