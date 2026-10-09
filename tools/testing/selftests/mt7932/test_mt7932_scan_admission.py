#!/usr/bin/env python3
"""Execute the driver's scan admission: never promise a nonexistent completion."""
from pathlib import Path
import subprocess
import tempfile
from mt7932_test_lib import local_linkage

root = Path(__file__).resolve().parents[4]
source = local_linkage((root / 'drivers/net/wireless/mediatek/mt7932/frontend.c').read_text())
admit = source[source.index('static int mt_scan_admit('):source.index('static int mt_scan(struct')]
code = r'''
#include <stdbool.h>
#include <errno.h>
#include <assert.h>
struct mt7932 {
 bool link_failed, retired_scan_seq, rf_ready, reg_pending, policy_failed;
 bool bss_active, connecting, connected, disconnecting, scan_request;
};
static bool mt_rf_allowed(struct mt7932 *m) {
 return m->rf_ready && !m->reg_pending && !m->policy_failed;
}
''' + admit + r'''
int main(void) {
 struct mt7932 m = {.rf_ready=true, .bss_active=true};
 assert(mt_scan_admit(&m)==0);
 m.connected=true; assert(mt_scan_admit(&m)==0);
 m.connected=false; m.connecting=true; assert(mt_scan_admit(&m)==-EAGAIN);
 m.connecting=false; m.disconnecting=true; assert(mt_scan_admit(&m)==-EAGAIN);
 m.disconnecting=false; m.scan_request=true; assert(mt_scan_admit(&m)==-EBUSY);
 m.scan_request=false; m.retired_scan_seq=true; assert(mt_scan_admit(&m)==-EIO);
 m.retired_scan_seq=false; m.link_failed=true; assert(mt_scan_admit(&m)==-EIO);
 m.link_failed=false; m.reg_pending=true; assert(mt_scan_admit(&m)==-EAGAIN);
 m.reg_pending=false; m.bss_active=false; assert(mt_scan_admit(&m)==-EAGAIN);
 m.bss_active=true; assert(mt_scan_admit(&m)==0);
 /* Exhaustive admission states: EBUSY always has a real scan owner. */
 for (unsigned bits=0; bits<1024; bits++) {
  m=(struct mt7932){.link_failed=bits&1, .retired_scan_seq=bits&2,
   .rf_ready=bits&4, .reg_pending=bits&8, .policy_failed=bits&16,
   .bss_active=bits&32, .connecting=bits&64, .connected=bits&128,
   .disconnecting=bits&256, .scan_request=bits&512};
  if (mt_scan_admit(&m)==-EBUSY) assert(m.scan_request);
 }
}
'''
with tempfile.TemporaryDirectory(prefix='mt7932-scan-admission-') as tmp:
    c, exe = Path(tmp) / 'test.c', Path(tmp) / 'test'
    c.write_text(code)
    subprocess.run(['cc', '-Wall', '-Wextra', '-Werror', '-fsanitize=address,undefined',
                    str(c), '-o', str(exe)], check=True)
    subprocess.run([str(exe)], check=True)
print('Actual scan admission: connected/transition/fault states never promise a nonexistent completion PASS')
