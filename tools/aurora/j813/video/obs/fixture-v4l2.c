/* SPDX-License-Identifier: GPL-2.0-or-later */
/* Bounded synthetic scene only. Encoding uses OBS's stock FFmpeg output. */
#include <obs-module.h>
#include <obs-frontend-api.h>
#include <graphics/vec2.h>
#include <util/bmem.h>
#include <stdatomic.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

OBS_DECLARE_MODULE()
static obs_source_t *bars[8];
static obs_scene_t *test_scene;
static float elapsed;
static unsigned last_phase;
static bool requested, stopping, active;
static atomic_bool recording;
static const uint32_t colors[8] = {
    0xffebebeb, 0xff20dddd, 0xffdddd20, 0xff20dd20,
    0xffdd20dd, 0xff2020dd, 0xffdd2020, 0xff202020
};

static void update_colors(void *param)
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

static void start(void *unused)
{
    (void)unused;
    obs_frontend_recording_start();
}

static void stop(void *unused)
{
    (void)unused;
    obs_frontend_recording_stop();
}

static void tick(void *unused, float seconds)
{
    (void)unused;
    elapsed += seconds;
    unsigned phase = (unsigned)(elapsed / 2);
    if (phase != last_phase) {
        last_phase = phase;
        obs_queue_task(OBS_TASK_UI, update_colors, (void *)(uintptr_t)phase, false);
    }
    if (!requested && elapsed >= 3) {
        requested = true;
        obs_queue_task(OBS_TASK_UI, start, NULL, false);
    }
    if (!stopping && elapsed >= 16 && atomic_load(&recording)) {
        stopping = true;
        obs_queue_task(OBS_TASK_UI, stop, NULL, false);
    }
}

static void release_scene(void)
{
    obs_remove_tick_callback(tick, NULL);
    for (unsigned i = 0; i < 8; i++) {
        obs_source_release(bars[i]);
        bars[i] = NULL;
    }
    if (test_scene) {
        obs_source_remove(obs_scene_get_source(test_scene));
        obs_scene_release(test_scene);
        test_scene = NULL;
    }
}

static void event(enum obs_frontend_event event, void *unused)
{
    (void)unused;
    if (event == OBS_FRONTEND_EVENT_FINISHED_LOADING) {
        obs_scene_t *scene = obs_scene_create("M5 V4L2 hardware encoding test");
        for (unsigned i = 0; i < 8; i++) {
            char name[64];
            snprintf(name, sizeof(name), "V4L2 test bar %u", i);
            obs_data_t *settings = obs_data_create();
            obs_data_set_int(settings, "width", 40);
            obs_data_set_int(settings, "height", 240);
            obs_data_set_int(settings, "color", colors[i]);
            bars[i] = obs_source_create("color_source_v3", name, settings, NULL);
            obs_data_release(settings);
            if (!bars[i]) {
                release_scene();
                obs_scene_release(scene);
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
        test_scene = scene;
        obs_add_tick_callback(tick, NULL);
        blog(LOG_INFO, "[m5-v4l2-test] synthetic scene ready; no custom encoder registered");
    } else if (event == OBS_FRONTEND_EVENT_RECORDING_STARTED) {
        atomic_store(&recording, true);
        obs_output_t *output = obs_frontend_get_recording_output();
        blog(LOG_INFO, "[m5-v4l2-test] recording started; output=%s", obs_output_get_id(output));
        obs_output_release(output);
    } else if (event == OBS_FRONTEND_EVENT_RECORDING_STOPPED) {
        char *path = obs_frontend_get_last_recording();
        atomic_store(&recording, false);
        release_scene();
        /* Retire dynamically created canvas sources before module shutdown.
         * At 1 fps their destruction can otherwise race core teardown. */
        obs_wait_for_destroy_queue();
        blog(LOG_INFO, "[m5-v4l2-test] recording stopped; output=%s", path ? path : "unavailable");
        bfree(path);
    } else if (event == OBS_FRONTEND_EVENT_SCENE_COLLECTION_CLEANUP || event == OBS_FRONTEND_EVENT_EXIT) {
        release_scene();
        if (event == OBS_FRONTEND_EVENT_EXIT)
            active = false;
    }
}

bool obs_module_load(void)
{
    active = getenv("M5_V4L2_AUTOTEST") && !strcmp(getenv("M5_V4L2_AUTOTEST"), "1");
    if (active)
        obs_frontend_add_event_callback(event, NULL);
    return true;
}

void obs_module_unload(void)
{
    if (active)
        obs_frontend_remove_event_callback(event, NULL);
}
