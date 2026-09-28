#!/usr/bin/env python3
"""Exercise actual driver entry points with mocked firmware calls."""
from pathlib import Path
import argparse
import hashlib
import os
import json
import re
import subprocess
import tempfile

BASE = Path(__file__).resolve().parents[5]
FIXTURES = Path(__file__).resolve().parent / "fixtures"
parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument("--receipt", type=Path)
args = parser.parse_args()
SOURCE = BASE / 'drivers/gpu/drm/apple/dcp.c'


def function(source, name):
    match = re.search(r'^(?:bool|int|void) ' + re.escape(name) + r'\([^;]+?\)\n\{', source, re.M)
    assert match, name
    start = match.start()
    brace = match.end() - 1
    depth = 1
    end = brace + 1
    while depth:
        depth += (source[end] == '{') - (source[end] == '}')
        end += 1
    return source[start:end]


prefix = r'''
#include <assert.h>
#include <stdbool.h>
#include <stdint.h>
#include <stdio.h>
typedef uint32_t u32;
enum { DCP_FIRMWARE_V_12_3, DCP_FIRMWARE_V_13_5, DCP_FIRMWARE_H17P };
enum { DRM_MODE_CONNECTOR_USB = 20, DRM_MODE_CONNECTOR_eDP = 14 };
struct connector { bool connected; };
struct port { bool enabled, connected; void *service; };
struct apple_dcp {
    int fw_compat, fixed_connector_type;
    bool typec_cable_connected, typec_crtc_off, typec_unplugged;
    void *active_typec_route, *dev, *avep;
    struct connector *connector;
    struct port dptxport[1];
    int typec_reconnect_wq, typec_reconnect_tries;
    bool *hdmi_hpd;
};
struct platform_device { struct apple_dcp *dcp; };
static int clears, hpd, releases, enables, av_disconnects;
static char events[128];
static unsigned event_count;
static void event(char c) { assert(event_count < sizeof(events)); events[event_count++]=c; }
#define READ_ONCE(x) (x)
#define WRITE_ONCE(x,v) ((x)=(v))
#define dev_info(...) ((void)0)
#define dev_warn(...) ((void)0)
#define WARN_ONCE(...) ((void)0)
#define DPTX_RECONNECT_DELAY 1
#define system_freezable_wq ((void *)0)
static struct apple_dcp *platform_get_drvdata(struct platform_device *p) { return p->dcp; }
static void cancel_delayed_work(int *w) { (void)w; }
static void mod_delayed_work(void *q, int *w, int t) { (void)q; (void)w; (void)t; }
static int gpiod_get_value_cansleep(bool *p) { return *p; }
static void disconnected_hpd_event(struct connector *p) { event('D'); if (p) p->connected=false; }
static void av_service_disconnect(struct apple_dcp *d) { (void)d; event('A'); av_disconnects++; }
static void av_service_connect(struct apple_dcp *d) { (void)d; }
static int dptxport_set_hpd(void *s, bool v) { (void)s; (void)v; event('H'); hpd++; return 0; }
static int dcp_dptx_disconnect(struct apple_dcp *d, u32 p) { event('R'); d->dptxport[p].connected=false; releases++; return 0; }
static int dcp_dptx_connect(struct apple_dcp *d, u32 p) { d->dptxport[p].connected=true; return 0; }
static void _dcp_poweroff(struct apple_dcp *d) { (void)d; event('Q'); clears++; }
static void iomfb_poweron_v12_3(struct apple_dcp *d) { (void)d; enables++; }
static void iomfb_poweron_v13_3(struct apple_dcp *d) { (void)d; enables++; }
static void iomfb_poweron_h17p(struct apple_dcp *d) { (void)d; enables++; }
'''

tests = r'''
static struct apple_dcp fresh(void) {
    return (struct apple_dcp) { .fw_compat=DCP_FIRMWARE_H17P,
        .active_typec_route=(void *)1, .typec_cable_connected=true,
        .dptxport={{.enabled=true,.connected=true}}, .avep=(void *)1 };
}
int main(void) {
    struct apple_dcp d=fresh(); struct platform_device p={&d};
    dcp_dptx_disconnect_oob(&p,0);
    assert(d.typec_unplugged && !d.typec_cable_connected);
    assert(events[0]=='Q' && events[1]=='D' && events[2]=='A' && events[3]=='H' && events[4]=='R');
    assert(hpd==1 && releases==1 && av_disconnects==1);
    dcp_poweroff(&p);
    assert(clears==1 && hpd==1 && releases==1 && av_disconnects==1);
    /* The route may be gone before userspace processes the unplug. */
    d.active_typec_route=NULL;
    dcp_poweroff(&p); assert(clears==1 && hpd==1);
    /* A quick replug must not erase the old CRTC's shutdown owner. */
    d.active_typec_route=(void *)1;
    dcp_dptx_connect_oob(&p,0);
    assert(d.typec_cable_connected && d.typec_unplugged);
    dcp_poweroff(&p); assert(clears==1 && hpd==1);
    dcp_poweron(&p);
    assert(!d.typec_unplugged && enables==1);
    /* Connected DPMS and subsequent CRTC enables retain their old path. */
    dcp_poweroff(&p); assert(clears==2 && hpd==2 && releases==2);
    dcp_poweron(&p); assert(enables==2);
    dcp_dptx_disconnect_oob(&p,0); dcp_poweroff(&p);
    assert(clears==3 && hpd==3 && releases==3);
    /* Scope the workaround to H17P; retain both legacy firmware paths. */
    for (int fw=DCP_FIRMWARE_V_12_3;fw<=DCP_FIRMWARE_V_13_5;fw++) {
        d=fresh(); d.fw_compat=fw; int old=clears;
        dcp_dptx_disconnect_oob(&p,0); dcp_poweroff(&p);
        assert(clears==old+1);
    }
    /* An internal CRTC has no unplug marker. */
    d=fresh(); d.active_typec_route=NULL;
    d.fixed_connector_type=DRM_MODE_CONNECTOR_eDP; int old=clears;
    dcp_poweroff(&p); assert(clears==old+1);
    puts("PASS: poweroff precedes DRM disconnect and HPD removal; delayed disable, quick replug, repeated cycle, legacy and internal paths");
}
'''

names = ['dcp_is_typec_output', 'dcp_dptx_connect_oob', 'dcp_dptx_disconnect_oob',
         'dcp_poweron', 'dcp_poweroff']
source = SOURCE.read_text()
with tempfile.TemporaryDirectory() as directory:
    root = Path(directory)
    for label, text in [('candidate', source), ('baseline', (FIXTURES / 'before-dcp.c').read_text())]:
        c = root / (label + '.c')
        c.write_text(prefix + '\n'.join(function(text, name) for name in names) + tests)
        exe = root / label
        subprocess.run([os.environ.get('CC', 'clang'), '-std=gnu11', '-Wall', '-Wextra',
                        '-Werror', '-fsanitize=address,undefined', str(c), '-o', str(exe)], check=True)
        result = subprocess.run([str(exe)], text=True, capture_output=True)
        if label == 'candidate':
            assert result.returncode == 0, result.stderr
            print(result.stdout.strip())
        else:
            assert result.returncode != 0 and 'events[0]' in result.stderr, result.stderr
            print('PASS: DP4 baseline lacks the pre-HPD quiesce ordering')
receipt = {
    'driver_sha256': hashlib.sha256(source.encode()).hexdigest(),
    'actual_entry_points': names, 'sanitizers': ['address', 'undefined'],
    'candidate_passed': True, 'baseline_regression_reproduced': True,
    'hardware_verified': False,
}
if args.receipt:
    args.receipt.write_text(json.dumps(receipt, indent=2) + '\n')
