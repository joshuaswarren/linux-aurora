/* Check GBM -> DCP GEM handles without rendering or changing scanout.
 * Build: cc m5-gbm-dcp-smoke.c $(pkg-config --cflags --libs gbm libdrm) -o m5-gbm-dcp-smoke
 * Run with the same Mesa environment as Xorg and its appledrm card path.
 */
#include <errno.h>
#include <fcntl.h>
#include <gbm.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>
#include <unistd.h>
#include <xf86drm.h>
#include <xf86drmMode.h>
#include <drm_fourcc.h>

static int add_fb(int fd, struct gbm_bo *bo, uint32_t handle, const char *kind)
{
    uint32_t handles[4] = { handle }, pitches[4] = { gbm_bo_get_stride(bo) };
    uint32_t offsets[4] = { gbm_bo_get_offset(bo, 0) }, fb = 0;
    uint64_t modifiers[4] = { gbm_bo_get_modifier(bo) };
    int rc = drmModeAddFB2WithModifiers(fd, gbm_bo_get_width(bo),
        gbm_bo_get_height(bo), gbm_bo_get_format(bo), handles, pitches,
        offsets, modifiers, &fb,
        modifiers[0] == DRM_FORMAT_MOD_INVALID ? 0 : DRM_MODE_FB_MODIFIERS);
    int saved = errno;
    printf("M5_GBM_%s handle=%u pitch=%u modifier=%#llx addfb_rc=%d errno=%d fb=%u\n",
        kind, handle, pitches[0], (unsigned long long)modifiers[0], rc,
        rc ? saved : 0, fb);
    if (!rc)
        drmModeRmFB(fd, fb);
    return rc;
}

int main(int argc, char **argv)
{
    if (argc != 2) {
        fprintf(stderr, "Usage: %s /dev/dri/<appledrm-card>\n", argv[0]);
        return 2;
    }
    int fd = open(argv[1], O_RDWR | O_CLOEXEC);
    if (fd < 0) { perror("open display"); return 1; }
    drmVersionPtr version = drmGetVersion(fd);
    if (!version || strcmp(version->name, "apple")) {
        fprintf(stderr, "Expected apple DRM display device, found %s\n",
            version ? version->name : "unknown");
        if (version) drmFreeVersion(version);
        close(fd);
        return 1;
    }
    drmFreeVersion(version);
    struct gbm_device *gbm = gbm_create_device(fd);
    if (!gbm) { perror("gbm_create_device"); close(fd); return 1; }
    printf("M5_GBM backend=%s fd=%d\n", gbm_device_get_backend_name(gbm),
        gbm_device_get_fd(gbm));
    uint64_t linear = DRM_FORMAT_MOD_LINEAR;
    struct gbm_bo *bo = gbm_bo_create_with_modifiers2(gbm, 256, 256,
        GBM_FORMAT_XRGB8888, &linear, 1,
        GBM_BO_USE_SCANOUT | GBM_BO_USE_RENDERING);
    if (!bo) {
        perror("gbm_bo_create"); gbm_device_destroy(gbm); close(fd); return 1;
    }
    uint32_t direct = gbm_bo_get_handle(bo).u32;
    int direct_rc = add_fb(fd, bo, direct, "DIRECT");
    int dma_fd = gbm_bo_get_fd(bo);
    uint32_t imported = 0;
    int import_rc = dma_fd < 0 ? -1 : drmPrimeFDToHandle(fd, dma_fd, &imported);
    printf("M5_GBM_PRIME rc=%d errno=%d handle=%u\n", import_rc,
        import_rc ? errno : 0, imported);
    int imported_fb_rc = import_rc ? -1 : add_fb(fd, bo, imported, "IMPORTED");
    /* Importing the same object on the same DRM file returns the existing
     * handle. Do not close that handle behind the GBM owner's back. */
    if (!import_rc && imported != direct)
        drmCloseBufferHandle(fd, imported);
    if (dma_fd >= 0) close(dma_fd);
    gbm_bo_destroy(bo);
    gbm_device_destroy(gbm);
    close(fd);
    return direct_rc || import_rc || imported_fb_rc ? 1 : 0;
}
