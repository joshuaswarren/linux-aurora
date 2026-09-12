// SPDX-License-Identifier: MIT
/* Submit the private native M5 triangle through application-owned DRM BOs.
 * This is a DRM submission test, not a Mesa/Vulkan application test. The
 * original code stays private; only decoded VDM PPP references are relocated.
 * With just a bundle argument, inspect/relocate in CPU memory without DRM.
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
#include "asahi_drm.h"
#include "agx_vdm_decode_core.h"
#include "agx_vdm_ppp_state_core.h"

#define PAGE 0x4000
#define SOURCE_BASE UINT64_C(0x1000000000)
#define APP_BASE UINT64_C(0x4000000000)
#define RELOCATE (APP_BASE - SOURCE_BASE)
#define USC_BASE UINT64_C(0x10000000000)
#define PIXELS (USC_BASE + 0x58000)
#define VDM (SOURCE_BASE + 0x18000)
#define MAX_PAGES 384

struct page {
	uint64_t address;
	uint32_t flags, handle;
	unsigned char *data, *cpu;
	int selected;
};
static struct page pages[MAX_PAGES];
static unsigned count;
static uint64_t relocation = RELOCATE;
static int all_reference_pages;
static const char *generated_fragment;

static void require(int ok, const char *what)
{
	if (!ok) {
		fprintf(stderr, "M5_DRM_RENDER_FAIL %s errno=%d (%s)\n",
			what, errno, strerror(errno));
		exit(1);
	}
}
static void call(int fd, unsigned long request, void *arg, const char *what)
{
	require(ioctl(fd, request, arg) == 0, what);
}
static uint32_t u32(const void *p) { uint32_t v; memcpy(&v, p, 4); return v; }
static uint64_t u64(const void *p) { uint64_t v; memcpy(&v, p, 8); return v; }
static uint64_t fnv(const unsigned char *p, size_t size)
{
	uint64_t h = UINT64_C(0xcbf29ce484222325);
	for (size_t i = 0; i < size; ++i)
		h = (h ^ p[i]) * UINT64_C(0x100000001b3);
	return h;
}
static struct page *page_at(uint64_t address)
{
	for (unsigned i = 0; i < count; ++i)
		if (pages[i].address == (address & ~(uint64_t)(PAGE - 1)))
			return &pages[i];
	return NULL;
}
static unsigned char *source_range(uint64_t address, size_t size)
{
	struct page *p = page_at(address);
	require(p && size && size <= PAGE - (address & (PAGE - 1)),
		"source reference stays in one captured page");
	p->selected = 1;
	return p->data + (address & (PAGE - 1));
}
static void load_bundle(const char *path)
{
	FILE *f = fopen(path, "rb");
	require(f != NULL, "open private bundle");
	require(fseek(f, 0, SEEK_END) == 0, "bundle seek");
	long size = ftell(f);
	require(size == 5477664 && fseek(f, 0, SEEK_SET) == 0, "native bundle size");
	unsigned char *bytes = malloc(size);
	require(bytes && fread(bytes, 1, size, f) == (size_t)size, "read native bundle");
	fclose(f);
	require(!memcmp(bytes, "M5GFXR01", 8) && u32(bytes + 8) == 1 &&
		u32(bytes + 12) == 64 && u32(bytes + 16) == PAGE &&
		u32(bytes + 20) == 334 && u32(bytes + 24) == 256 &&
		u32(bytes + 28) == 256 && u32(bytes + 32) == 1024 &&
		u32(bytes + 36) == 3 && u64(bytes + 40) == PIXELS &&
		u64(bytes + 48) == UINT64_C(11884608949817576648) &&
		fnv(bytes + 64, size - 64) == u64(bytes + 48) && u64(bytes + 56) == 0,
		"exact M5 native program profile");
	count = u32(bytes + 20);
	for (unsigned i = 0; i < count; ++i) {
		unsigned char *record = bytes + 64 + i * (16 + PAGE);
		uint64_t a = u64(record);
		require(!(a & (PAGE - 1)) &&
			((a >= SOURCE_BASE && a < SOURCE_BASE + 0x400000) ||
			 (a >= USC_BASE && a < USC_BASE + 0x400000)), "bundle VA window");
		require((u32(record + 8) == 1 || u32(record + 8) == 3) &&
			u32(record + 12) == PAGE && (i == 0 || a > pages[i - 1].address),
			"ordered unique source pages and permissions");
		pages[i] = (struct page) {
			.address = a, .flags = u32(record + 8), .data = record + 16,
			.selected = all_reference_pages || a >= USC_BASE,
		};
	}
}

static void relocate_vdm(void)
{
	unsigned char *stream = source_range(VDM, PAGE);
	size_t offset = 0;
	unsigned ppp = 0, states = 0, draws = 0;
	for (;;) {
		struct agx_vdm_decoded_record record;
		require(offset < 256 && agx_vdm_decode_one(AGX_CHIP_G17G,
			stream + offset, PAGE - offset, &record), "decode native VDM record");
		printf("M5_DRM_VDM offset=%zu kind=%u bytes=%u", offset, record.kind, record.length);
		switch (record.kind) {
		case AGX_VDM_RECORD_PPP_STATE_UPDATE: {
			uint64_t compact = record.data.ppp_state_update.pointer;
			unsigned words = record.data.ppp_state_update.size_words;
			require(compact < 0x400000 && words, "native compact PPP range");
			unsigned char *payload = source_range(SOURCE_BASE + compact, words * 4);
			struct agx_vdm_ppp_state_core packed;
			require(agx_vdm_ppp_state_core_pack(AGX_CHIP_G17G,
                compact + relocation, words, &packed), "encode relocated PPP reference");
			memcpy(stream + offset, packed.bytes, packed.length);
			printf(" ppp=%" PRIx64 " relocated=%" PRIx64 " words=%u header=%x",
                compact, compact + relocation, words, u32(payload));
			++ppp;
			break;
		}
		case AGX_VDM_RECORD_STATE_UPDATE:
			printf(" vertex_program=%" PRIx64, record.data.state_update.pipeline);
			require(record.data.state_update.pipeline == USC_BASE + 0x160000,
				"native vertex program");
			source_range(record.data.state_update.pipeline, 84);
			++states;
			break;
		case AGX_VDM_RECORD_INDEX_LIST_NONINDEXED:
			require(record.data.index_list_nonindexed.count == 3 &&
				record.data.index_list_nonindexed.instances == 1 &&
				record.data.index_list_nonindexed.start == 0, "native direct triangle");
			++draws;
			break;
		case AGX_VDM_RECORD_STREAM_TERMINATE:
			puts("");
			require(ppp == 14 && states == 1 && draws == 1, "complete native VDM grammar");
			printf("M5_DRM_RELOCATION_VERIFIED ppp=%u states=%u draws=%u bytes=%zu\n",
				ppp, states, draws, offset + record.length);
			return;
		default:
			require(0, "unsupported native VDM record");
		}
		puts("");
		offset += record.length;
	}
}

static void map_pages(int fd, uint32_t vm)
{
	unsigned selected = 0, low = 0;
	for (unsigned i = 0; i < count; ++i) {
		struct page *p = &pages[i];
		if (!p->selected) continue;
		struct drm_asahi_gem_create bo = {
			.size = PAGE, .flags = DRM_ASAHI_GEM_VM_PRIVATE, .vm_id = vm,
		};
		call(fd, DRM_IOCTL_ASAHI_GEM_CREATE, &bo, "gem_create");
		p->handle = bo.handle;
		struct drm_asahi_gem_mmap_offset mmap_offset = { .handle = bo.handle };
		call(fd, DRM_IOCTL_ASAHI_GEM_MMAP_OFFSET, &mmap_offset, "gem_mmap_offset");
		p->cpu = mmap(NULL, PAGE, PROT_READ | PROT_WRITE, MAP_SHARED, fd, mmap_offset.offset);
		require(p->cpu != MAP_FAILED, "gem_mmap");
		memcpy(p->cpu, p->data, PAGE);
		uint64_t address = p->address;
        if (address < USC_BASE) { address += relocation; ++low; }
		struct drm_asahi_gem_bind_op op = {
			.flags = DRM_ASAHI_BIND_READ | (p->flags == 3 ? DRM_ASAHI_BIND_WRITE : 0),
			.handle = bo.handle, .range = PAGE, .addr = address,
		};
		struct drm_asahi_vm_bind bind = {
			.vm_id = vm, .num_binds = 1, .stride = sizeof(op), .userptr = (uintptr_t)&op,
		};
		call(fd, DRM_IOCTL_ASAHI_VM_BIND, &bind, "bind application-owned page");
		++selected;
	}
	printf("M5_DRM_RENDER_MAPPINGS total=%u relocated_data=%u\n", selected, low);
}

static void check_pixels(const char *output)
{
	unsigned foreground = 0, background = 0, unexpected = 0, wrong = 0;
	FILE *f = fopen(output, "wb");
	require(f != NULL, "open RGBA output");
	for (unsigned index = 0; index < 256 * 256; ++index) {
		uint64_t address = PIXELS + index * 4;
		struct page *p = page_at(address);
		require(p && p->cpu, "output page retained");
		unsigned char *rgba = p->cpu + (address & (PAGE - 1));
		require(fwrite(rgba, 1, 4, f) == 4, "save GPU output");
		int fg = abs(rgba[0] - (generated_fragment ? 191 : 64)) <= 1 &&
            abs(rgba[1] - (generated_fragment ? 64 : 128)) <= 1 &&
			abs(rgba[2] - (generated_fragment ? 128 : 191)) <= 1 && rgba[3] >= 254;
		int bg = abs(rgba[0] - 16) <= 1 && abs(rgba[1] - 16) <= 1 &&
			abs(rgba[2] - 16) <= 1 && rgba[3] >= 254;
		foreground += fg; background += bg; unexpected += !fg && !bg;
		int x = (index % 256) * 2 + 1, y = (index / 256) * 2 + 1;
		int inside = y > 68 && y < 444 && 2*x+y > 584 && 2*x-y < 440;
		int outside = y < 60 || y > 452 || 2*x+y < 568 || 2*x-y > 456;
		wrong += (inside && !fg) || (outside && !bg);
	}
	require(fclose(f) == 0, "close output");
	printf("M5_DRM_RENDER_PIXELS foreground=%u background=%u unexpected=%u wrong=%u\n",
		foreground, background, unexpected, wrong);
	require(foreground == 18432 && background == 47104 && !unexpected && !wrong,
		"native triangle geometry and colours");
}

int main(int argc, char **argv)
{
	setvbuf(stdout, NULL, _IONBF, 0);
    while (argc > 2 && argv[argc - 1][0] == '-') {
        if (!strcmp(argv[argc - 1], "--native-va"))
            relocation = 0;
        else if (!strcmp(argv[argc - 1], "--all-reference-pages"))
            all_reference_pages = 1;
        else if (!strncmp(argv[argc - 1], "--generated-fragment=", 21))
            generated_fragment = argv[argc - 1] + 21;
        else
            require(0, "unknown render diagnostic option");
        --argc;
    }
    require(argc == 2 || argc == 4,
        "usage: m5-drm-render bundle [render-node output.rgba] [--native-va] [--all-reference-pages] [--generated-fragment=FILE]");
    /* Indirect resources have not all been decoded. Keep their captured
     * addresses intact instead of guessing which data words need relocation.
     * The exact bundle profile still restricts every mapping to its two
     * four-megabyte user windows, with captured RO/RW permissions.
     */
    require(!all_reference_pages || !relocation,
        "all reference pages require native virtual addresses");
    require(!generated_fragment || (all_reference_pages && !relocation),
        "generated fragment requires the qualified complete native-VA resource layout");
    printf("M5_DRM_RENDER_LAYOUT data=%" PRIx64 " relocation=%" PRIx64 "\n",
        SOURCE_BASE + relocation, relocation);
	load_bundle(argv[1]);
	if (generated_fragment) {
        unsigned char shader[60];
        FILE *f = fopen(generated_fragment, "rb");
        require(f != NULL, "open Mesa-generated fragment");
        require(fread(shader, 1, sizeof(shader), f) == sizeof(shader) &&
                fgetc(f) == EOF && !ferror(f), "Mesa fragment must occupy the qualified 60-byte slot");
        require(fclose(f) == 0, "close Mesa-generated fragment");
        unsigned char *slot = source_range(USC_BASE + 0x3c0, sizeof(shader));
        require(memcmp(slot, shader, sizeof(shader)) != 0,
                "Mesa colour variant must differ from the captured shader");
        memcpy(slot, shader, sizeof(shader));
        puts("M5_DRM_MESA_FRAGMENT_INSTALLED bytes=60 expected_rgb=191,64,128");
    }
	relocate_vdm();
	unsigned selected_pages = 0;
	for (unsigned i = 0; i < count; ++i)
		selected_pages += pages[i].selected != 0;
	printf("M5_DRM_RENDER_REFERENCE_PAGES selected=%u captured=%u all=%d\n",
		selected_pages, count, all_reference_pages);
	if (argc == 2) { puts("M5_DRM_RENDER_INSPECT_ONLY_NO_DEVICE_OPENED"); return 0; }
	int fd = open(argv[2], O_RDWR | O_CLOEXEC);
	require(fd >= 0, "open render node");
	struct drm_asahi_params_global params = {0};
	struct drm_asahi_get_params get = { .pointer = (uintptr_t)&params, .size = sizeof(params) };
	call(fd, DRM_IOCTL_ASAHI_GET_PARAMS, &get, "get_params");
	require(params.chip_id == 0x8142 && params.gpu_hal_generation == 300, "M5 identity");
    struct drm_asahi_vm_create vm = {
        .kernel_start = SOURCE_BASE + (relocation ? 0 : 0x400000),
        .kernel_end = UINT64_C(0x1800000000),
    };
	call(fd, DRM_IOCTL_ASAHI_VM_CREATE, &vm, "vm_create");
	map_pages(fd, vm.vm_id);
	struct drm_asahi_queue_create queue = {
		.vm_id = vm.vm_id, .priority = DRM_ASAHI_PRIORITY_MEDIUM, .usc_exec_base = USC_BASE,
	};
	call(fd, DRM_IOCTL_ASAHI_QUEUE_CREATE, &queue, "queue_create");
	struct drm_syncobj_create fence = {0};
	call(fd, DRM_IOCTL_SYNCOBJ_CREATE, &fence, "syncobj_create");
	struct {
		struct drm_asahi_cmd_header header;
		struct drm_asahi_cmd_render render;
	} command = {
		.header = { .cmd_type = DRM_ASAHI_CMD_RENDER, .size = sizeof(struct drm_asahi_cmd_render),
			.vdm_barrier = UINT16_MAX, .cdm_barrier = UINT16_MAX },
		.render = {
			.flags = DRM_ASAHI_RENDER_PROCESS_EMPTY_TILES | DRM_ASAHI_RENDER_RSRC_SPEC_HI,
            .vdm_ctrl_stream_base = VDM + relocation,
			.isp_scissor_base = USC_BASE + 0x180000, .isp_dbias_base = USC_BASE + 0x2d8000,
			.ppp_multisamplectl = 0x88, .ppp_ctrl = 0x202,
			.width_px = 256, .height_px = 256, .layers = 1,
			.utile_width_px = 32, .utile_height_px = 32, .samples = 1, .sample_size_B = 0,
			.isp_merge_upper_x = 0x3bddb3d9, .isp_merge_upper_y = 0x3bddb3d9,
			.bg = { .usc = 0x170200, .rsrc_spec = 0x40 }, .eot = { .usc = 0x170400 },
			.partial_bg = { .usc = 0x170000, .rsrc_spec = 0x40 }, .partial_eot = { .usc = 0x170400 },
			.isp_bgobjdepth = 0x3f800000, .bg_rsrc_spec_hi = 0xffff8000,
			.bg_partial_rsrc_spec_hi = 0xffff8000,
		},
	};
	_Static_assert(sizeof(command) == sizeof(command.header) + sizeof(command.render), "packed UAPI record");
	struct drm_asahi_sync sync = { .sync_type = DRM_ASAHI_SYNC_SYNCOBJ, .handle = fence.handle };
	struct drm_asahi_submit submit = {
		.syncs = (uintptr_t)&sync, .cmdbuf = (uintptr_t)&command, .queue_id = queue.queue_id,
		.out_sync_count = 1, .cmdbuf_size = sizeof(command),
	};
	printf("M5_DRM_RENDER_SUBMIT vm=%u queue=%u bytes=%zu\n", vm.vm_id, queue.queue_id, sizeof(command));
	call(fd, DRM_IOCTL_ASAHI_SUBMIT, &submit, "submit application render");
	struct timespec now;
	require(clock_gettime(CLOCK_MONOTONIC, &now) == 0, "monotonic clock");
	struct drm_syncobj_wait wait = {
		.handles = (uintptr_t)&fence.handle, .count_handles = 1,
		.timeout_nsec = (int64_t)now.tv_sec * 1000000000 + now.tv_nsec + 2000000000,
		.flags = DRM_SYNCOBJ_WAIT_FLAGS_WAIT_ALL,
	};
	call(fd, DRM_IOCTL_SYNCOBJ_WAIT, &wait, "wait output fence");
	check_pixels(argv[3]);
    printf("M5_DRM_RENDER_VERIFIED native_program=1 relocated_application_data=%u mesa_vulkan=0\n",
        relocation != 0);
	close(fd);
	return 0;
}
