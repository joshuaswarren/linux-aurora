"""Preload only the reviewed generic SPTM bootstrap for the M5 qualification."""
import os
from pathlib import Path
import time
from tools.sptm.seed import seed_sptm_bootstrap

if int(hv.u.adt['/chosen'].chip_id) != 0x8142:
    raise RuntimeError('M5 bootstrap requires T8142')
if not getattr(hv, 'j813_kernelcache_verified', False):
    raise RuntimeError('M5 bootstrap requires the verified kernel profile')
if 'cpus=1' not in hv.tba.cmdline.split():
    raise RuntimeError('M5 bootstrap requires the single-CPU qualification')
if os.environ.get('M5_CLPC_SAMPLER_SHADOW') == '1':
    hv.m5_clpc_sampler_shadow = True
    print('M5_CLPC_SAMPLER_SHADOW_ENABLED: performance sampling unqualified')
if os.environ.get('M5_MACOS_VOLUME_INVENTORY'):
    hv.run_script(str(Path(__file__).resolve().parent / 'select-macos-root.py'))
if os.environ.get('M5_NATIVE_POLICY_CAPTURE'):
    hv.run_script(str(Path(__file__).resolve().parent / 'native-macos-policy.py'))
if os.environ.get('M5_ISOLATE_EXTERNAL_DISPLAY') == '1':
    # Guest27: native dcpext0 startup wrote locked DISPEXT0_CPU at
    # 0x380700428. External-display qualification is separate from N1 TX.
    for index in range(2):
        for stem in ('dispext{0}', 'dcpext{0}-expert', 'dcpext{0}',
                     'dart-dcpext{0}', 'dart-dispext{0}', 'dart-dispext{0}-isoc',
                     'admac-dispext{0}', 'admac-dispext{0}-ced-ssw'):
            path = '/arm-io/' + stem.format(index)
            try:
                del hv.adt[path]
                print('M5_EXTERNAL_DISPLAY_NODE_REMOVED', path)
            except KeyError:
                pass
if os.environ.get('M5_ISOLATE_INTERNAL_DISPLAY') == '1':
    if os.environ.get('M5_ISOLATE_EXTERNAL_DISPLAY') != '1':
        raise RuntimeError('Headless M5 qualification also requires external-display isolation')
    # Guest30: dart-disp0 panicked on an inherited scanout mapping before
    # root mount. Keep the guest out of the internal display/DCP graph for
    # the headless N1 experiment. Shared AOP/audio ADMAC nodes are retained.
    for name in ('display-crossbar0', 'disp0', 'dcp0-expert', 'dcp',
                 'dart-dcp', 'dart-disp0', 'dcp-sac-controller',
                 'dart-disp0-isoc', 'admac-disp0', 'admac-disp0-ced-ssw'):
        path = '/arm-io/' + name
        try:
            del hv.adt[path]
            print('M5_INTERNAL_DISPLAY_NODE_REMOVED', path)
        except KeyError:
            pass
if os.environ.get('M5_ISOLATE_CAMERA') == '1':
    # Substring "isp" would also match disp/dispext. Remove only the camera
    # family from the guest copy, before the guest ADT snapshot and SPTM seed.
    for name in ('isp', 'dart-isp', 'isp-exclave-s-proxy', 'isp-exclave-proxy'):
        path = '/arm-io/' + name
        try:
            del hv.adt[path]
            print('M5_CAMERA_NODE_REMOVED', path)
        except KeyError:
            pass

# Save a protected pre-seed topology for the M5 adapter and trace decoder.
# Do not print the ADT; it includes identifying firmware properties.
capture_dir = Path(__file__).resolve().parent / 'logs/adt-snapshots'
capture_dir.mkdir(mode=0o700, parents=True, exist_ok=True)
capture = capture_dir / f'm5-{time.time_ns()}.adt'
fd = os.open(capture, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
with os.fdopen(fd, 'wb') as output:
    output.write(hv.adt.build())
print('M5_ADT_SNAPSHOT_SAVED_PRIVATE')

seed_sptm_bootstrap(hv)
# Expose architectural allocation-tag access after reserving the inherited
# tag-storage arena. Keep XNU's tag-check fault policy under guest control.
# Without HCR.ATA, allocation-tag instructions cannot provide their native
# semantics even when PAPT entries use tagged-normal MAIR index4.
if ((hv.u.mrs('ID_AA64PFR1_EL1') >> 8) & 0xF) < 2:
    raise RuntimeError('M5 MTE qualification requires architectural MTE2')
hv.p.hv_write_hcr(hv.u.mrs('HCR_EL2') | (1 << 56))
hv.u.msr('SCTLR_EL12', hv.u.mrs('SCTLR_EL12') | (3 << 42))
if not (hv.u.mrs('HCR_EL2') & (1 << 56)):
    raise RuntimeError('M5 HCR allocation-tag access did not remain enabled')
print('M5_MTE_ALLOCATION_TAG_ACCESS_ENABLED')
if os.environ.get('M5_PMGR_TRACE') == '1':
    hv.run_script(str(Path(__file__).resolve().parent / 'trace-pmgr.py'))
if os.environ.get('M5_PMP_TRACE') == '1':
    hv.run_script(str(Path(__file__).resolve().parent / 'trace-pmp-m5.py'))
if os.environ.get('M5_N1_TRACE') == '1':
    hv.run_script(str(Path(__file__).resolve().parent / 'n1-trace.py'))
if os.environ.get('M5_SEP_TRACE') == '1':
    hv.run_script(str(Path(__file__).resolve().parent / 'trace-sep-m5.py'))
hv.p.hv_set_time_stealing(False, True)
print('M5_SPTM_BOOTSTRAP_SEEDED: guest entry remains unqualified')
