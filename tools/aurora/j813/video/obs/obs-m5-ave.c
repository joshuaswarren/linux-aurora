/* SPDX-License-Identifier: GPL-2.0-or-later */
/* Experimental OBS adapter for the qualified M5 320x240 NV12 AVE profile. */
#include <obs-module.h>
#include <obs-frontend-api.h>
#include <util/config-file.h>
#include <util/bmem.h>
#include <graphics/vec2.h>
#include <errno.h>
#include <stdatomic.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/socket.h>
#include <sys/un.h>
#include <unistd.h>

OBS_DECLARE_MODULE()

struct ave_encoder {
    int fd;
    uint8_t *headers, *packet, *input;
    size_t header_bytes;
    unsigned frames;
};
static atomic_uint total_frames;
static bool autotest;
static obs_source_t *bars[8];
static float test_elapsed;
static unsigned last_phase;
static bool recording_requested, stop_requested;

static bool transfer(int fd, void *buffer, size_t bytes, bool writing)
{
    uint8_t *p = buffer;
    while (bytes) {
        ssize_t n = writing ? send(fd, p, bytes, MSG_NOSIGNAL) : recv(fd, p, bytes, 0);
        if (n < 0 && errno == EINTR)
            continue;
        if (n <= 0)
            return false;
        p += n;
        bytes -= (size_t)n;
    }
    return true;
}

static const char *ave_name(void *unused)
{
    (void)unused;
    return "Apple M5 AVE H.264 (experimental, 320x240 / 1 fps)";
}

static void ave_destroy(void *data)
{
    struct ave_encoder *e = data;
    if (!e)
        return;
    if (e->fd >= 0)
        close(e->fd);
    blog(LOG_INFO, "[m5-ave] encoder closed after %u hardware frames", e->frames);
    bfree(e->headers);
    bfree(e->packet);
    bfree(e->input);
    bfree(e);
}

static void *ave_create(obs_data_t *settings, obs_encoder_t *encoder)
{
    (void)settings;
    const struct video_output_info *video = video_output_get_info(obs_encoder_video(encoder));
    if (obs_encoder_get_width(encoder) != 320 || obs_encoder_get_height(encoder) != 240 ||
        video->fps_num != video->fps_den) {
        blog(LOG_ERROR, "[m5-ave] requires exactly 320x240 at 1 fps");
        return NULL;
    }
    struct ave_encoder *e = bzalloc(sizeof(*e));
    e->fd = socket(AF_UNIX, SOCK_STREAM | SOCK_CLOEXEC, 0);
    struct timeval timeout = {.tv_sec = 15};
    if (e->fd < 0)
        goto failed;
    setsockopt(e->fd, SOL_SOCKET, SO_RCVTIMEO, &timeout, sizeof(timeout));
    setsockopt(e->fd, SOL_SOCKET, SO_SNDTIMEO, &timeout, sizeof(timeout));
    struct sockaddr_un address = {.sun_family = AF_UNIX};
    strcpy(address.sun_path, "/run/j813-ave-obs.sock");
    if (connect(e->fd, (struct sockaddr *)&address, sizeof(address)))
        goto failed;
    /* Both peers are native little-endian AArch64; never use this ABI remotely. */
    uint32_t hello[] = {0x4d455641, 1, 320, 240, 1};
    uint32_t response[4];
    if (!transfer(e->fd, hello, sizeof(hello), true) ||
        !transfer(e->fd, response, sizeof(response), false) ||
        response[0] != hello[0] || response[1] != 1 || response[2] < 10 || response[2] > 4096)
        goto failed;
    e->header_bytes = response[2];
    e->headers = bmalloc(e->header_bytes);
    e->input = bmalloc(115200);
    if (!transfer(e->fd, e->headers, e->header_bytes, false))
        goto failed;
    atomic_store(&total_frames, 0);
    blog(LOG_INFO, "[m5-ave] native AVE broker connected; hardware warmup and parameter sets verified");
    return e;
failed:
    blog(LOG_ERROR, "[m5-ave] hardware broker unavailable; no software fallback");
    ave_destroy(e);
    return NULL;
}

static bool ave_encode(void *data, struct encoder_frame *frame,
                       struct encoder_packet *packet, bool *received)
{
    struct ave_encoder *e = data;
    *received = false;
    if (!frame)
        return true;
    if (!frame->data[0] || !frame->data[1] || frame->linesize[0] < 320 || frame->linesize[1] < 320)
        return false;
    for (unsigned y = 0; y < 240; y++)
        memcpy(e->input + y * 320, frame->data[0] + y * frame->linesize[0], 320);
    for (unsigned y = 0; y < 120; y++)
        memcpy(e->input + 76800 + y * 320, frame->data[1] + y * frame->linesize[1], 320);
    uint8_t request[12];
    uint32_t length = 115200;
    memcpy(request, &length, 4);
    memcpy(request + 4, &frame->pts, 8);
    uint32_t response[2];
    if (!transfer(e->fd, request, sizeof(request), true) ||
        !transfer(e->fd, e->input, length, true) ||
        !transfer(e->fd, response, sizeof(response), false) ||
        response[0] != 0 || response[1] < 5 || response[1] > 0x3c000)
        return false;
    bfree(e->packet);
    e->packet = bmalloc(response[1]);
    if (!transfer(e->fd, e->packet, response[1], false))
        return false;
    packet->data = e->packet;
    packet->size = response[1];
    packet->type = OBS_ENCODER_VIDEO;
    packet->keyframe = true;
    packet->pts = frame->pts;
    packet->dts = frame->pts;
    packet->timebase_num = 1;
    packet->timebase_den = 1;
    *received = true;
    e->frames++;
    atomic_fetch_add(&total_frames, 1);
    blog(LOG_INFO, "[m5-ave] hardware frame %u pts=%lld bytes=%u", e->frames,
         (long long)frame->pts, response[1]);
    return true;
}

static bool ave_extra(void *data, uint8_t **extra, size_t *bytes)
{
    struct ave_encoder *e = data;
    *extra = e->headers;
    *bytes = e->header_bytes;
    return true;
}

static void ave_format(void *data, struct video_scale_info *info)
{
    (void)data;
    info->format = VIDEO_FORMAT_NV12;
    info->colorspace = VIDEO_CS_709;
    info->range = VIDEO_RANGE_PARTIAL;
}

static struct obs_encoder_info encoder_info = {
    .id = "m5_ave_h264",
    .type = OBS_ENCODER_VIDEO,
    .codec = "h264",
    .get_name = ave_name,
    .create = ave_create,
    .destroy = ave_destroy,
    .encode = ave_encode,
    .get_extra_data = ave_extra,
    .get_video_info = ave_format,
};

/* Optional fixture: a synthetic OBS scene and bounded recording, no desktop,
 * microphone or streaming capture. Normal plugin loads do not activate it. */
static const uint32_t colors[8] = {
    0xffebebeb, 0xff20dddd, 0xffdddd20, 0xff20dd20,
    0xffdd20dd, 0xff2020dd, 0xffdd2020, 0xff202020
};

static void update_test_colors(void *param)
{
    unsigned phase = (unsigned)(uintptr_t)param;
    for (unsigned i = 0; i < 8; i++) {
        if (!bars[i])
            continue;
        obs_data_t *settings = obs_data_create();
        obs_data_set_int(settings, "color", colors[(i + phase) % 8]);
        obs_source_update(bars[i], settings);
        obs_data_release(settings);
    }
}

static void start_test(void *unused)
{
    (void)unused;
    blog(LOG_INFO, "[m5-ave-test] starting bounded OBS hardware recording");
    obs_frontend_recording_start();
}

static void stop_test(void *unused)
{
    (void)unused;
    blog(LOG_INFO, "[m5-ave-test] stopping recording at %u hardware frames", atomic_load(&total_frames));
    obs_frontend_recording_stop();
}

static void test_tick(void *unused, float seconds)
{
    (void)unused;
    test_elapsed += seconds;
    unsigned phase = (unsigned)(test_elapsed / 2);
    if (phase != last_phase) {
        last_phase = phase;
        obs_queue_task(OBS_TASK_UI, update_test_colors, (void *)(uintptr_t)phase, false);
    }
    if (!recording_requested && test_elapsed >= 3) {
        recording_requested = true;
        obs_queue_task(OBS_TASK_UI, start_test, NULL, false);
    }
    if (!stop_requested && (atomic_load(&total_frames) >= 8 || test_elapsed > 25)) {
        stop_requested = true;
        obs_queue_task(OBS_TASK_UI, stop_test, NULL, false);
    }
}

static void frontend_event(enum obs_frontend_event event, void *unused)
{
    (void)unused;
    if (event == OBS_FRONTEND_EVENT_FINISHED_LOADING) {
        obs_scene_t *scene = obs_scene_create("M5 AVE hardware encoding test");
        for (unsigned i = 0; i < 8; i++) {
            char name[64];
            snprintf(name, sizeof(name), "AVE test bar %u", i);
            obs_data_t *settings = obs_data_create();
            obs_data_set_int(settings, "width", 40);
            obs_data_set_int(settings, "height", 240);
            obs_data_set_int(settings, "color", colors[i]);
            bars[i] = obs_source_create("color_source_v3", name, settings, NULL);
            obs_data_release(settings);
            if (!bars[i]) {
                blog(LOG_ERROR, "[m5-ave-test] color source unavailable");
                return;
            }
            obs_sceneitem_t *item = obs_scene_add(scene, bars[i]);
            struct vec2 position;
            vec2_set(&position, (float)(i * 40), 0);
            obs_sceneitem_set_pos(item, &position);
        }
        for (unsigned i = 1; i <= 5; i++)
            obs_set_output_source(i, NULL);
        obs_frontend_set_current_scene(obs_scene_get_source(scene));
        obs_scene_release(scene);
        obs_add_tick_callback(test_tick, NULL);
        blog(LOG_INFO, "[m5-ave-test] synthetic test scene ready");
    } else if (event == OBS_FRONTEND_EVENT_RECORDING_STOPPED) {
        char *path = obs_frontend_get_last_recording();
        blog(LOG_INFO, "[m5-ave-test] recording stopped; frames=%u output=%s",
             atomic_load(&total_frames), path ? path : "unavailable");
        bfree(path);
        obs_remove_tick_callback(test_tick, NULL);
        /* The fixture has finished; retain only the scene's own references. */
        for (unsigned i = 0; i < 8; i++) {
            obs_source_release(bars[i]);
            bars[i] = NULL;
        }
    } else if (event == OBS_FRONTEND_EVENT_SCENE_COLLECTION_CLEANUP ||
               event == OBS_FRONTEND_EVENT_EXIT) {
        obs_remove_tick_callback(test_tick, NULL);
        for (unsigned i = 0; i < 8; i++) {
            obs_source_release(bars[i]);
            bars[i] = NULL;
        }
        /* The frontend clears its callback list during application exit. */
        if (event == OBS_FRONTEND_EVENT_EXIT)
            autotest = false;
    }
}

bool obs_module_load(void)
{
    obs_register_encoder(&encoder_info);
    autotest = getenv("M5_AVE_AUTOTEST") && strcmp(getenv("M5_AVE_AUTOTEST"), "1") == 0;
    if (autotest)
        obs_frontend_add_event_callback(frontend_event, NULL);
    blog(LOG_INFO, "[m5-ave] experimental native hardware encoder registered; autotest=%d", autotest);
    return true;
}

void obs_module_unload(void)
{
    if (autotest)
        obs_frontend_remove_event_callback(frontend_event, NULL);
}
