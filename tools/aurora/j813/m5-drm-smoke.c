// SPDX-License-Identifier: MIT
/* Exercise M5's application ABI without publishing a GPU command.
 * Build against this kernel's DRM headers, e.g.:
 * cc -static -Os -s -Wall -Wextra -Werror -D__user= -Iinclude/uapi/drm \
 *    tools/aurora/j813/m5-drm-smoke.c -o m5-drm-smoke
 */
#include <errno.h>
#include <fcntl.h>
#include <inttypes.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/ioctl.h>
#include <sys/mman.h>
#include <time.h>
#include <unistd.h>
#include <linux/sync_file.h>
#include "asahi_drm.h"

static void require(int condition, const char *what)
{
	if (!condition) {
		fprintf(stderr, "M5_DRM_SMOKE_FAIL %s errno=%d (%s)\n",
			what, errno, strerror(errno));
		exit(1);
	}
}

static void call(int fd, unsigned long request, void *arg, const char *what)
{
	require(ioctl(fd, request, arg) == 0, what);
}

static int64_t deadline(void)
{
	struct timespec now;
	require(clock_gettime(CLOCK_MONOTONIC, &now) == 0, "clock_gettime");
	return (int64_t)now.tv_sec * 1000000000 + now.tv_nsec + 2000000000;
}

static uint32_t new_sync(int fd, uint32_t flags)
{
	struct drm_syncobj_create create = { .flags = flags };
	call(fd, DRM_IOCTL_SYNCOBJ_CREATE, &create, "syncobj_create");
	return create.handle;
}

static void drop_sync(int fd, uint32_t handle)
{
	struct drm_syncobj_destroy destroy = { .handle = handle };
	call(fd, DRM_IOCTL_SYNCOBJ_DESTROY, &destroy, "syncobj_destroy");
}

static void check_error_fence(int fd, uint32_t handle, uint64_t point,
			      int error)
{
	if (point) {
		struct drm_syncobj_timeline_wait wait = {
			.handles = (uintptr_t)&handle, .points = (uintptr_t)&point,
			.timeout_nsec = deadline(), .count_handles = 1,
			.flags = DRM_SYNCOBJ_WAIT_FLAGS_WAIT_ALL |
				 DRM_SYNCOBJ_WAIT_FLAGS_WAIT_FOR_SUBMIT,
		};
		call(fd, DRM_IOCTL_SYNCOBJ_TIMELINE_WAIT, &wait, "timeline_wait");
		uint64_t completed = 0;
		struct drm_syncobj_timeline_array query = {
			.handles = (uintptr_t)&handle, .points = (uintptr_t)&completed,
			.count_handles = 1,
		};
		call(fd, DRM_IOCTL_SYNCOBJ_QUERY, &query, "timeline_query");
		require(completed >= point, "timeline point completed");
	} else {
		struct drm_syncobj_wait wait = {
			.handles = (uintptr_t)&handle, .timeout_nsec = deadline(),
			.count_handles = 1,
			.flags = DRM_SYNCOBJ_WAIT_FLAGS_WAIT_ALL |
				 DRM_SYNCOBJ_WAIT_FLAGS_WAIT_FOR_SUBMIT,
		};
		call(fd, DRM_IOCTL_SYNCOBJ_WAIT, &wait, "binary_wait");
	}
	struct drm_syncobj_handle export = {
		.handle = handle, .fd = -1, .point = point,
		.flags = DRM_SYNCOBJ_HANDLE_TO_FD_FLAGS_EXPORT_SYNC_FILE |
			(point ? DRM_SYNCOBJ_HANDLE_TO_FD_FLAGS_TIMELINE : 0),
	};
	call(fd, DRM_IOCTL_SYNCOBJ_HANDLE_TO_FD, &export, "export_error_fence");
	struct sync_file_info info = {0};
	call(export.fd, SYNC_IOC_FILE_INFO, &info, "sync_file_info");
	printf("M5_DRM_SMOKE_FENCE_STATUS handle=%u point=%" PRIu64
		" wrapper_status=%d leaves=%u\n", handle, point, info.status, info.num_fences);
	/* A timeline exports a dma_fence_chain. Its wrapper reports successful
	 * retirement independently of the contained job's error. Query the
	 * unwrapped job fences before checking the submission result. */
	require(info.num_fences > 0 && info.num_fences < 16, "bounded fence leaf count");
	struct sync_fence_info leaves[16] = {0};
	info.sync_fence_info = (uintptr_t)leaves;
	call(export.fd, SYNC_IOC_FILE_INFO, &info, "sync_file_leaf_info");
	printf("M5_DRM_SMOKE_FENCE_STATUS handle=%u point=%" PRIu64
		" job_status=%d\n", handle, point, info.status);
	require(info.status == -error, "exported fence carries submission error");
	close(export.fd);
}

/* A one-byte command reaches Queue::submit but cannot contain a header.
 * This must fail before any M5 hardware submission and still publish BOTH
 * output fences. Reusing the timeline also checks advancement after errors.
 */
static void reject_with_fences(int fd, uint32_t queue, uint32_t input,
			       uint32_t binary, uint32_t timeline, uint64_t point)
{
	struct drm_asahi_sync syncs[] = {
		{ .sync_type = DRM_ASAHI_SYNC_SYNCOBJ, .handle = input },
		{ .sync_type = DRM_ASAHI_SYNC_SYNCOBJ, .handle = binary },
		{ .sync_type = DRM_ASAHI_SYNC_TIMELINE_SYNCOBJ,
		  .handle = timeline, .timeline_value = point },
	};
	const unsigned char malformed = 0xff;
	struct drm_asahi_submit submit = {
		.syncs = (uintptr_t)syncs, .cmdbuf = (uintptr_t)&malformed,
		.queue_id = queue, .in_sync_count = 1, .out_sync_count = 2,
		.cmdbuf_size = sizeof(malformed),
	};
	errno = 0;
	int ret = ioctl(fd, DRM_IOCTL_ASAHI_SUBMIT, &submit);
	require(ret == -1 && errno == EINVAL, "malformed submission rejected");
	check_error_fence(fd, binary, 0, EINVAL);
	check_error_fence(fd, timeline, point, EINVAL);
	printf("M5_DRM_SMOKE_FENCES queue=%u point=%" PRIu64 " status=-EINVAL\n",
		queue, point);
}

static void check_buffers(int fd, uint32_t vm_id)
{
	const uint64_t addresses[] = {
		UINT64_C(0x4000000000),  /* Mesa M5 data window */
		UINT64_C(0x10100000000), /* Mesa M5 shader window */
		UINT64_C(0x100000000),   /* Mesa robustness zero-page address */
	};
	struct drm_asahi_gem_create bo = {
		.size = 0x4000, .flags = DRM_ASAHI_GEM_VM_PRIVATE, .vm_id = vm_id,
	};
	call(fd, DRM_IOCTL_ASAHI_GEM_CREATE, &bo, "gem_create");
	struct drm_asahi_gem_mmap_offset offset = { .handle = bo.handle };
	call(fd, DRM_IOCTL_ASAHI_GEM_MMAP_OFFSET, &offset, "gem_mmap_offset");
	uint32_t *cpu = mmap(NULL, bo.size, PROT_READ | PROT_WRITE, MAP_SHARED,
			     fd, offset.offset);
	require(cpu != MAP_FAILED, "gem_mmap");
	for (unsigned i = 0; i < bo.size / sizeof(*cpu); ++i)
		cpu[i] = i ^ 0x81420000;
	struct drm_asahi_gem_bind_op op = {
		.flags = DRM_ASAHI_BIND_READ | DRM_ASAHI_BIND_WRITE,
		.handle = bo.handle, .range = bo.size,
	};
	struct drm_asahi_vm_bind bind = {
		.vm_id = vm_id, .num_binds = 1, .stride = sizeof(op),
		.userptr = (uintptr_t)&op,
	};
	for (unsigned i = 0; i < sizeof(addresses) / sizeof(*addresses); ++i) {
		op.addr = addresses[i];
		call(fd, DRM_IOCTL_ASAHI_VM_BIND, &bind, "gem_bind application window");
	}
	op.addr = UINT64_C(0x1000100000);
	errno = 0;
	int ret = ioctl(fd, DRM_IOCTL_ASAHI_VM_BIND, &bind);
	require(ret == -1 && errno == EINVAL, "driver reservation rejects user binding");
	for (unsigned i = 0; i < bo.size / sizeof(*cpu); ++i)
		require(cpu[i] == (i ^ 0x81420000), "GEM CPU contents preserved");
	op.flags = DRM_ASAHI_BIND_UNBIND;
	op.handle = 0;
	for (unsigned i = 0; i < sizeof(addresses) / sizeof(*addresses); ++i) {
		op.addr = addresses[i];
		call(fd, DRM_IOCTL_ASAHI_VM_BIND, &bind, "gem_unbind");
	}
	require(munmap(cpu, bo.size) == 0, "gem_munmap");
	struct drm_gem_close close_bo = { .handle = bo.handle };
	call(fd, DRM_IOCTL_GEM_CLOSE, &close_bo, "gem_close");
	printf("M5_DRM_SMOKE_BUFFERS vm=%u mappings=3 reserved_bind_rejected=1 cpu_words=4096\n", vm_id);
}

static void check_vm_status(int fd, uint32_t vm_id)
{
	struct drm_asahi_vm_status status = { .vm_id = vm_id };
	struct drm_asahi_get_params get = {
		.param_group = DRM_ASAHI_PARAM_GROUP_VM_STATUS,
		.pointer = (uintptr_t)&status, .size = sizeof(status),
	};
	call(fd, DRM_IOCTL_ASAHI_GET_PARAMS, &get, "get_vm_status");
	require(status.error == 0, "validation errors do not poison the VM");
}

int main(int argc, char **argv)
{
	setvbuf(stdout, NULL, _IONBF, 0);
	require(argc == 2, "usage: m5-drm-smoke /dev/dri/renderD128");
	int fd = open(argv[1], O_RDWR | O_CLOEXEC);
	require(fd >= 0, "open render node");
	struct drm_asahi_params_global params = {0};
	struct drm_asahi_get_params get = {
		.pointer = (uintptr_t)&params, .size = sizeof(params),
	};
	call(fd, DRM_IOCTL_ASAHI_GET_PARAMS, &get, "get_params");
	require(params.chip_id == 0x8142 && params.gpu_generation == 17,
		"M5 T8142 identity");
	unsigned cores = 0;
	for (unsigned i = 0; i < DRM_ASAHI_MAX_CLUSTERS; ++i)
		cores += __builtin_popcountll(params.core_masks[i]);
	require(cores == 10 && params.num_dies == 1, "M5 ten active cores");
	require(params.max_commands_per_submission == 1, "first-job command limit");
	printf("M5_DRM_SMOKE_ID chip=%x generation=%u variant=%u hal=%u usc=%u cores=%u\n",
		params.chip_id, params.gpu_generation, params.gpu_variant,
		params.gpu_hal_generation, params.usc_generation, cores);

	for (unsigned iteration = 0; iteration < 3; ++iteration) {
		struct drm_asahi_vm_create vm = {
			.kernel_start = UINT64_C(0x1000000000),
			.kernel_end = UINT64_C(0x1800000000),
		};
		call(fd, DRM_IOCTL_ASAHI_VM_CREATE, &vm, "vm_create");
		check_buffers(fd, vm.vm_id);
		struct drm_asahi_queue_create queue = {
			.vm_id = vm.vm_id, .priority = DRM_ASAHI_PRIORITY_MEDIUM,
			.usc_exec_base = UINT64_C(0x10100000000),
		};
		call(fd, DRM_IOCTL_ASAHI_QUEUE_CREATE, &queue, "queue_create");
		uint32_t input = new_sync(fd, DRM_SYNCOBJ_CREATE_SIGNALED);
		uint32_t binary = new_sync(fd, 0), timeline = new_sync(fd, 0);
		reject_with_fences(fd, queue.queue_id, input, binary, timeline, 1);
		reject_with_fences(fd, queue.queue_id, input, binary, timeline, 2);
		/* Propagating an already errored input must also complete outputs. */
		reject_with_fences(fd, queue.queue_id, binary, binary, timeline, 3);
		check_vm_status(fd, vm.vm_id);
		drop_sync(fd, input);
		drop_sync(fd, binary);
		drop_sync(fd, timeline);
		struct drm_asahi_queue_destroy qd = { .queue_id = queue.queue_id };
		call(fd, DRM_IOCTL_ASAHI_QUEUE_DESTROY, &qd, "queue_destroy");
		struct drm_asahi_vm_destroy vd = { .vm_id = vm.vm_id };
		call(fd, DRM_IOCTL_ASAHI_VM_DESTROY, &vd, "vm_destroy");
		printf("M5_DRM_SMOKE_LIFECYCLE iteration=%u complete\n", iteration + 1);
	}
	close(fd);
	puts("M5_DRM_SMOKE_VERIFIED metadata=1 lifecycles=3 mappings=9 rejected_submits=9 error_fences=18 gpu_jobs=0");
	return 0;
}
