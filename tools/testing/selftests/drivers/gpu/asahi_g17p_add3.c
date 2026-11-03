// SPDX-License-Identifier: GPL-2.0-only OR MIT
/*
 * Minimal synchronous G17P add3 proof through the public DRM UAPI.
 *
 * Run only with asahi.g17p_test_stage=2. The shader and CDM stream are the
 * source-built A18 Pro witnesses used by the m1n1 hardware experiment. The
 * kernel supplies the retained QID4 graph and copies the three attachment
 * addresses into its register-0x1a510 resource table before publication.
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
#include <unistd.h>

#ifndef __user
#define __user
#endif
#include <drm/asahi_drm.h>

#define GPU_PAGE_SIZE UINT64_C(0x4000)
#define USC_BASE UINT64_C(0x10000000000)
#define SHADER_VA UINT64_C(0x100000a8000)
#define CDM_VA UINT64_C(0x100000c8000)
#define INPUT_A_VA UINT64_C(0x10000030000)
#define INPUT_B_VA UINT64_C(0x10000038000)
#define OUTPUT_VA UINT64_C(0x10000040000)
#define KERNEL_START UINT64_C(0x7000000000)
#define KERNEL_END UINT64_C(0x7100000000)
#define VECTOR_COUNT 64

static const uint8_t add3_shader[] = {
	0x2c, 0xa0, 0x02, 0x00, 0x12, 0x08, 0x7c, 0x00,
	0x3c, 0x80, 0x02, 0x00, 0x04, 0x00, 0x00, 0x00,
	0x8c, 0xa0, 0x42, 0x00, 0x00, 0x00, 0x0c, 0x00,
	0x9c, 0x80, 0x42, 0x00, 0x04, 0x00, 0x00, 0x00,
	0x67, 0x00, 0x54, 0x2c, 0x02, 0x00, 0x00, 0x00,
	0x59, 0x00, 0x02, 0x40, 0x26, 0x00, 0x67, 0x00,
	0x54, 0x24, 0x02, 0x00, 0x00, 0x00, 0x57, 0x00,
	0x00, 0x40, 0x26, 0x00, 0x67, 0x00, 0x54, 0x30,
	0x18, 0x00, 0x00, 0x00, 0x59, 0x04, 0x00, 0x40,
	0x26, 0x00, 0x77, 0x00, 0x2a, 0x41, 0x00, 0x00,
	0x00, 0x00, 0x77, 0x01, 0xaa, 0x07, 0x00, 0x00,
	0x00, 0x02, 0x04, 0x00, 0xf7, 0x00, 0x2a, 0x00,
	0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x1c, 0x80,
	0x02, 0x00, 0x00, 0x00, 0x00, 0x00, 0x14, 0x81,
	0x11, 0x06, 0x00, 0x00, 0x00, 0x00, 0x0c, 0x80,
	0x02, 0x00, 0x04, 0x00, 0x00, 0x00, 0x9f, 0x11,
	0x54, 0x00, 0x02, 0x00, 0x08, 0xa8, 0x10, 0x05,
	0x1c, 0x80, 0x02, 0x00, 0x04, 0x00, 0x00, 0x00,
	0x0f, 0x12, 0x54, 0x00, 0x4c, 0x00, 0x4b, 0x2c,
	0x09, 0x44, 0x5b, 0x2e, 0x09, 0x04, 0x0b, 0x24,
	0x09, 0x04, 0x1b, 0x26, 0x09, 0x04, 0x2b, 0x28,
	0x09, 0x04, 0x3b, 0x2a, 0x09, 0x04, 0x6b, 0x30,
	0x09, 0x04, 0x7b, 0x32, 0x09, 0x04, 0x03, 0x00,
	0x07, 0x00, 0x02, 0x00, 0x00, 0x00, 0x60, 0x00,
	0x0e, 0x00, 0x00, 0x00,
};

static const uint8_t add3_constant_program[] = {
	0x03, 0x00, 0x07, 0x00, 0x02, 0x00, 0x00, 0x00,
	0x60, 0x00, 0x0e, 0x00, 0x00, 0x00, 0x06, 0x00,
	0x06, 0x00, 0x06, 0x00, 0x06, 0x00, 0x06, 0x00,
	0x06, 0x00, 0x06, 0x00, 0x06, 0x00, 0x06, 0x00,
	0x06, 0x00, 0x06, 0x00, 0x06, 0x00, 0x06, 0x00,
	0x06, 0x00, 0x06, 0x00, 0x06, 0x00, 0x06, 0x00,
	0x06, 0x00, 0x06, 0x00, 0x06, 0x00, 0x06, 0x00,
	0x06, 0x00, 0x06, 0x00, 0x06, 0x00, 0x06, 0x00,
};

static const uint8_t add3_main_program[] = {
	0x1c, 0xa0, 0x10, 0x06, 0x67, 0x10, 0x54, 0x00,
	0x00, 0x01, 0x20, 0x00, 0x51, 0x01, 0x00, 0x40,
	0x46, 0x00, 0x67, 0x00, 0x44, 0x04, 0x01, 0x01,
	0x20, 0x00, 0x51, 0x01, 0x00, 0x40, 0x46, 0x00,
	0x09, 0x05, 0x1c, 0x01, 0x00, 0xc0, 0xe7, 0x00,
	0x54, 0x00, 0x02, 0x01, 0x21, 0x00, 0x11, 0x00,
	0x00, 0x90, 0x11, 0x00, 0x0e, 0x00, 0x00, 0x00,
};

/* Direct grid 64x1x1, threadgroup 32x1x1, then the stream terminator. */
static const uint8_t add3_cdm[] = {
	0x00, 0x00, 0x08, 0x00, 0x00, 0x00, 0x00, 0x01,
	0x00, 0x2a, 0x00, 0x00, 0x01, 0x00, 0x00, 0x40,
	0x40, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00,
	0x01, 0x00, 0x00, 0x00, 0x20, 0x00, 0x00, 0x00,
	0x01, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00,
	0x60, 0x01, 0x00, 0x60, 0x00, 0x00, 0x00, 0x40,
};

static void put_le16(uint8_t *where, uint16_t value)
{
	where[0] = value;
	where[1] = value >> 8;
}

static void put_le32(uint8_t *where, uint32_t value)
{
	where[0] = value;
	where[1] = value >> 8;
	where[2] = value >> 16;
	where[3] = value >> 24;
}

static void fill_helper_table(uint8_t *image, size_t base)
{
	static const uint8_t opcodes[] = { 0x60, 0x50, 0x40, 0x30, 0x20, 0x10 };
	static const uint8_t terminal[] = {
		0xf7, 0x03, 0xaa, 0x00, 0x8f, 0x02, 0x54, 0x01,
		0x06, 0x00, 0x06, 0x00, 0x06, 0x00, 0x06, 0x00,
	};

	for (size_t i = 0; i < sizeof(opcodes); ++i) {
		uint8_t *record = image + base + i * 0x10;

		memset(record, 0, 0x10);
		record[0] = 0x0f;
		record[1] = 0x00;
		record[2] = 0x54;
		record[3] = opcodes[i];
		put_le16(record + 10, 0x0006);
		put_le16(record + 12, 0x0006);
		put_le16(record + 14, 0x0006);
	}
	memcpy(image + base + 0x60, terminal, sizeof(terminal));
	memcpy(image + base + 0x70, terminal, sizeof(terminal));
}

static void build_add3_code_image(void *mapping)
{
	uint8_t *image = mapping;

	memset(image, 0, GPU_PAGE_SIZE);
	put_le32(image, 0x340);
	for (size_t offset = 0x40; offset < 0x340; offset += 2)
		put_le16(image + offset, 0x0006);
	fill_helper_table(image, 0x100);
	fill_helper_table(image, 0x200);
	put_le32(image + 0x340, 0xc0);
	memcpy(image + 0x380, add3_constant_program,
	       sizeof(add3_constant_program));
	memcpy(image + 0x3c0, add3_main_program, sizeof(add3_main_program));
}

static uint64_t code_image_fingerprint(const uint8_t *image)
{
	uint64_t hash = UINT64_C(0xcbf29ce484222325);

	for (size_t i = 0; i < GPU_PAGE_SIZE; ++i) {
		hash ^= image[i];
		hash *= UINT64_C(0x100000001b3);
	}
	return hash;
}

struct mapped_bo {
	uint32_t handle;
	uint64_t size;
	uint64_t va;
	void *map;
};

static void fail(const char *operation)
{
	fprintf(stderr, "asahi_g17p_add3: %s: %s\n", operation,
		strerror(errno));
	exit(EXIT_FAILURE);
}

static void do_ioctl(int fd, unsigned long request, void *arg,
		     const char *operation)
{
	if (ioctl(fd, request, arg) < 0)
		fail(operation);
}

static struct mapped_bo create_and_bind(int fd, uint32_t vm_id, uint64_t va,
					uint64_t size)
{
	struct drm_asahi_gem_create create = {
		.size = size,
		.flags = DRM_ASAHI_GEM_WRITEBACK,
	};
	struct drm_asahi_gem_mmap_offset offset = {0};
	struct drm_asahi_gem_bind_op op = {
		.flags = DRM_ASAHI_BIND_READ | DRM_ASAHI_BIND_WRITE,
		.range = size,
		.addr = va,
	};
	struct drm_asahi_vm_bind bind = {
		.vm_id = vm_id,
		.num_binds = 1,
		.stride = sizeof(op),
		.userptr = (uintptr_t)&op,
	};
	struct mapped_bo bo = {
		.size = size,
		.va = va,
	};

	do_ioctl(fd, DRM_IOCTL_ASAHI_GEM_CREATE, &create, "GEM_CREATE");
	bo.handle = create.handle;
	offset.handle = bo.handle;
	do_ioctl(fd, DRM_IOCTL_ASAHI_GEM_MMAP_OFFSET, &offset,
		 "GEM_MMAP_OFFSET");
	bo.map = mmap(NULL, size, PROT_READ | PROT_WRITE, MAP_SHARED, fd,
		      offset.offset);
	if (bo.map == MAP_FAILED)
		fail("mmap");
	op.handle = bo.handle;
	do_ioctl(fd, DRM_IOCTL_ASAHI_VM_BIND, &bind, "VM_BIND");
	return bo;
}

struct add3_cmdbuf {
	struct drm_asahi_cmd_header attachment_header;
	struct drm_asahi_attachment attachments[3];
	struct drm_asahi_cmd_header compute_header;
	struct drm_asahi_cmd_compute compute;
};

_Static_assert(sizeof(add3_shader) == 196, "add3 shader size changed");
_Static_assert(sizeof(add3_cdm) == 48, "add3 CDM size changed");
_Static_assert(sizeof(add3_constant_program) == 64,
	       "add3 constant program size changed");
_Static_assert(sizeof(add3_main_program) == 56,
	       "add3 main program size changed");
_Static_assert(sizeof(struct drm_asahi_cmd_compute) == 64,
	       "compute UAPI size changed");

int main(int argc, char **argv)
{
	if (argc == 2 && !strcmp(argv[1], "--selftest-code-image")) {
		uint8_t *image = calloc(1, GPU_PAGE_SIZE);

		if (!image)
			fail("allocate code-image selftest buffer");
		build_add3_code_image(image);
		uint64_t fingerprint = code_image_fingerprint(image);

		if (fingerprint != UINT64_C(0x72b014fffa72f6a9)) {
			fprintf(stderr,
				"asahi_g17p_add3: code image fingerprint %#" PRIx64
				" expected 0x72b014fffa72f6a9\n", fingerprint);
			free(image);
			return EXIT_FAILURE;
		}
		free(image);
		puts("asahi_g17p_add3: code image selftest PASS");
		return EXIT_SUCCESS;
	}
	const char *node = argc > 1 ? argv[1] : "/dev/dri/renderD128";
	struct drm_asahi_params_global params = {0};
	struct drm_asahi_get_params get = {
		.pointer = (uintptr_t)&params,
		.size = sizeof(params),
	};
	struct drm_asahi_vm_create vm = {
		.kernel_start = KERNEL_START,
		.kernel_end = KERNEL_END,
	};
	struct drm_asahi_queue_create queue = {
		.priority = DRM_ASAHI_PRIORITY_MEDIUM,
		.usc_exec_base = USC_BASE,
	};
	struct mapped_bo code, shader, cdm, input_a, input_b, output;
	struct mapped_bo usc_resource;
	struct add3_cmdbuf commands = {0};
	struct drm_asahi_submit submit = {0};
	float *a, *b, *result;
	int fd;

	fd = open(node, O_RDWR | O_CLOEXEC);
	if (fd < 0)
		fail("open render node");
	do_ioctl(fd, DRM_IOCTL_ASAHI_GET_PARAMS, &get, "GET_PARAMS");
	if (params.chip_id != 0x8140 || params.gpu_generation != 17 ||
	    params.gpu_variant != 'P' || params.usc_generation != 3 ||
	    params.gpu_hal_generation != DRM_ASAHI_GPU_HAL_200) {
		fprintf(stderr,
			"asahi_g17p_add3: expected T8140/G17P/USC3/HAL200, got "
			"%#x/G%u/%c/USC%u/HAL%u\n",
			params.chip_id, params.gpu_generation,
			(char)params.gpu_variant, params.usc_generation,
			params.gpu_hal_generation);
		return EXIT_FAILURE;
	}

	puts("asahi_g17p_add3: creating VM and QID4 user queue");
	do_ioctl(fd, DRM_IOCTL_ASAHI_VM_CREATE, &vm, "VM_CREATE (use g17p_test_stage=2)");
	queue.vm_id = vm.vm_id;
	do_ioctl(fd, DRM_IOCTL_ASAHI_QUEUE_CREATE, &queue, "QUEUE_CREATE");

	code = create_and_bind(fd, vm.vm_id, USC_BASE, GPU_PAGE_SIZE);
	/*
	 * add3_shader[6:8] == 0x007c selects a USC resource at
	 * usc_exec_base + 0x7c * 0x2000 == USC_BASE + 0xF8000. Under m1n1 that
	 * offset was covered by the RESOURCE object; the driver moved that
	 * allocation to the dynamic low range, so nothing backs it any more and
	 * the GPU dereferences an unmapped address on the first kick.
	 */
	usc_resource = create_and_bind(fd, vm.vm_id, USC_BASE + 0xF8000,
				       0xC000);
	shader = create_and_bind(fd, vm.vm_id, SHADER_VA, GPU_PAGE_SIZE);
	cdm = create_and_bind(fd, vm.vm_id, CDM_VA, GPU_PAGE_SIZE);
	input_a = create_and_bind(fd, vm.vm_id, INPUT_A_VA, GPU_PAGE_SIZE);
	input_b = create_and_bind(fd, vm.vm_id, INPUT_B_VA, GPU_PAGE_SIZE);
	output = create_and_bind(fd, vm.vm_id, OUTPUT_VA, GPU_PAGE_SIZE);
	{
		/*
		 * m1n1 backed this offset with the RESOURCE object, which
		 * carried the operand addresses at +0x14a0. Reproduce that
		 * layout so the USC resource fetch finds real pointers.
		 */
		uint64_t *res = (uint64_t *)((uint8_t *)usc_resource.map + 0x14a0);
		res[0] = INPUT_A_VA;
		res[1] = INPUT_B_VA;
		res[2] = OUTPUT_VA;
	}
	build_add3_code_image(code.map);
	memcpy(shader.map, add3_shader, sizeof(add3_shader));
	memcpy(cdm.map, add3_cdm, sizeof(add3_cdm));
	a = input_a.map;
	b = input_b.map;
	result = output.map;
	for (unsigned int i = 0; i < VECTOR_COUNT; ++i) {
		a[i] = 1000.0f + i;
		b[i] = 0.5f;
	}
	memset(result, 0, VECTOR_COUNT * sizeof(*result));
	__sync_synchronize();

	commands.attachment_header.cmd_type = DRM_ASAHI_SET_COMPUTE_ATTACHMENTS;
	commands.attachment_header.size = sizeof(commands.attachments);
	commands.attachment_header.vdm_barrier = DRM_ASAHI_BARRIER_NONE;
	commands.attachment_header.cdm_barrier = DRM_ASAHI_BARRIER_NONE;
	commands.attachments[0].pointer = INPUT_A_VA;
	commands.attachments[0].size = VECTOR_COUNT * sizeof(*a);
	commands.attachments[1].pointer = INPUT_B_VA;
	commands.attachments[1].size = VECTOR_COUNT * sizeof(*b);
	commands.attachments[2].pointer = OUTPUT_VA;
	commands.attachments[2].size = VECTOR_COUNT * sizeof(*result);
	commands.compute_header.cmd_type = DRM_ASAHI_CMD_COMPUTE;
	commands.compute_header.size = sizeof(commands.compute);
	commands.compute_header.vdm_barrier = DRM_ASAHI_BARRIER_NONE;
	commands.compute_header.cdm_barrier = DRM_ASAHI_BARRIER_NONE;
	commands.compute.flags = DRM_ASAHI_COMPUTE_G17P_ADD3_PROOF;
	commands.compute.cdm_ctrl_stream_base = CDM_VA;
	commands.compute.cdm_ctrl_stream_end = CDM_VA + sizeof(add3_cdm);

	submit.cmdbuf = (uintptr_t)&commands;
	submit.queue_id = queue.queue_id;
	submit.cmdbuf_size = sizeof(commands);
	puts("asahi_g17p_add3: submitting one synchronous 64-float add3");
	do_ioctl(fd, DRM_IOCTL_ASAHI_SUBMIT, &submit, "SUBMIT");
	__sync_synchronize();
	for (unsigned int i = 0; i < VECTOR_COUNT; ++i) {
		float expected = 1000.5f + i;

		if (result[i] != expected) {
			fprintf(stderr,
				"asahi_g17p_add3: output[%u]=%.9g expected %.9g\n",
				i, result[i], expected);
			return EXIT_FAILURE;
		}
	}

	puts("asahi_g17p_add3: PASS — 64/64 exact add3 results");
	close(fd);
	return EXIT_SUCCESS;
}
