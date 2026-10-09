#!/usr/bin/env python3
"""Exercise the real T8130 DP state machine without hardware."""
from pathlib import Path
import subprocess
import tempfile


def function(source, name):
    start = source.index("static int " + name + "(")
    opening = source.index("{", start)
    depth, end = 1, opening + 1
    while depth:
        depth += (source[end] == "{") - (source[end] == "}")
        end += 1
    return source[start:end]


def main():
    root = Path(__file__).resolve().parents[5]
    source = (root / "drivers/phy/apple/atc.c").read_text()
    real = "\n".join(function(source, name) for name in (
        "atcphy_t8130_dp_stop", "atcphy_t8130_dpphy_configure"))
    prefix = r"""
#include <assert.h>
#include <stdint.h>
#include <stdbool.h>
#include <stdio.h>
#include <string.h>
typedef uint32_t u32;
#define BIT(n) (1U << (n))
#define EINVAL 22
#define ENOENT 2
#define ETIMEDOUT 110
#define lockdep_assert_held(...) ((void)0)
#define guard(type) __attribute__((unused)) int *guard_lock =
#define dev_err(...) ((void)0)
enum atcphy_dp_link_rate { ATCPHY_DP_LINK_RATE_RBR, ATCPHY_DP_LINK_RATE_HBR,
 ATCPHY_DP_LINK_RATE_HBR2, ATCPHY_DP_LINK_RATE_HBR3 };
enum atcphy_mode { APPLE_ATCPHY_MODE_OFF, APPLE_ATCPHY_MODE_USB2,
 APPLE_ATCPHY_MODE_USB3, APPLE_ATCPHY_MODE_USB3_DP, APPLE_ATCPHY_MODE_DP };
struct phy_configure_opts_dp {
 unsigned link_rate, lanes, voltage[4], pre[4];
 bool ssc, set_rate, set_lanes, set_voltages;
};
struct atcphy_mode_configuration { bool dp_lane[2]; };
struct apple_atcphy {
 enum atcphy_mode mode;
 int dp_link_rate, lock;
 bool dp_main_active, dp_ssc, swap_lanes;
 unsigned dp_pma_mask;
 u32 dp_training[5][16];
 bool dp_training_valid[5];
};
static unsigned events[40][4], n;
static int init_error, stop_error;
static void record(unsigned op, unsigned a, unsigned b, unsigned c) {
 assert(n < 40); events[n][0]=op; events[n][1]=a; events[n][2]=b; events[n++][3]=c;
}
static const struct atcphy_mode_configuration *atcphy_get_mode_config(
 struct apple_atcphy *a, enum atcphy_mode mode) {
 static const struct atcphy_mode_configuration both={{true,true}},
 normal={{false,true}}, reverse={{true,false}};
 return mode == APPLE_ATCPHY_MODE_DP ? &both : a->swap_lanes ? &reverse : &normal;
}
static int atcphy_t8130_dp_main_init(struct apple_atcphy *a, unsigned mask,
 enum atcphy_dp_link_rate rate, bool ssc) {
 record(1,mask,rate,ssc); return init_error;
}
static int atcphy_t8130_dp_main_shutdown(struct apple_atcphy *a, unsigned mask) {
 record(2,mask,0,0); return stop_error;
}
static void atcphy_t8130_dp_set_eq(struct apple_atcphy *a, unsigned lane,
 bool tx, u32 preset) { record(3,lane,tx,preset); }
"""
    suffix = r"""
static struct apple_atcphy device(void) {
 struct apple_atcphy a = {.mode=APPLE_ATCPHY_MODE_DP,.dp_link_rate=-1};
 a.dp_training_valid[0]=a.dp_training_valid[2]=true;
 for (unsigned i=0;i<16;i++) {
  a.dp_training[0][i]=0x1000+i; a.dp_training[2][i]=0x2000+i;
 }
 n=0; init_error=stop_error=0; return a;
}
int main(void) {
 struct apple_atcphy a=device();
 struct phy_configure_opts_dp rate={.set_rate=true,.link_rate=2700,.lanes=4};
 assert(atcphy_t8130_dpphy_configure(&a,&rate)==0);
 assert(n==1 && events[0][0]==1 && events[0][1]==3 && events[0][2]==1);
 assert(a.dp_main_active && a.dp_link_rate==1);
 assert(atcphy_t8130_dpphy_configure(&a,&rate)==0 && n==1);
 rate.ssc=true;
 assert(atcphy_t8130_dpphy_configure(&a,&rate)==0);
 assert(n==3 && events[1][0]==2 && events[2][0]==1 && events[2][3]==1);
 struct phy_configure_opts_dp volts={.set_voltages=true,.lanes=4,
  .voltage={0,1,2,3},.pre={3,2,1,0}};
 n=0;
 assert(atcphy_t8130_dpphy_configure(&a,&volts)==0 && n==4);
 for(unsigned i=0;i<4;i++) {
  assert(events[i][1]==(1^(i/2)) && events[i][2]==(i&1));
  assert(events[i][3]==0x2000+volts.voltage[i]*4+volts.pre[i]);
 }
 a.swap_lanes=true; n=0;
 assert(atcphy_t8130_dpphy_configure(&a,&volts)==0 && n==4);
 for(unsigned i=0;i<4;i++) assert(events[i][1]==i/2);
 volts.voltage[3]=4; n=0;
 assert(atcphy_t8130_dpphy_configure(&a,&volts)==-EINVAL && n==0);
 volts.voltage[3]=3; volts.lanes=5;
 assert(atcphy_t8130_dpphy_configure(&a,&volts)==-EINVAL && n==0);
 volts.lanes=4;
 struct phy_configure_opts_dp stop={.set_rate=true};
 stop.set_voltages=true;
 assert(atcphy_t8130_dpphy_configure(&a,&stop)==-EINVAL && n==0);
 stop.set_voltages=false; stop_error=-ETIMEDOUT;
 assert(atcphy_t8130_dpphy_configure(&a,&stop)==-ETIMEDOUT);
 assert(a.dp_main_active && a.dp_pma_mask==3);
 n=0; rate.link_rate=5400;
 assert(atcphy_t8130_dpphy_configure(&a,&rate)==-ETIMEDOUT);
 assert(n==1 && events[0][0]==2);
 stop_error=0; n=0;
 assert(atcphy_t8130_dpphy_configure(&a,&stop)==0 && n==1);
 assert(!a.dp_main_active && a.dp_pma_mask==0 && a.dp_link_rate==-1);
 n=0; assert(atcphy_t8130_dpphy_configure(&a,&volts)==-EINVAL && n==0);
 assert(atcphy_t8130_dpphy_configure(&a,&stop)==0 && n==0);

 for(unsigned reverse=0;reverse<2;reverse++) {
  a=device(); a.mode=APPLE_ATCPHY_MODE_USB3_DP; a.swap_lanes=reverse;
  rate.link_rate=8100; rate.lanes=2; volts.lanes=2;
  assert(atcphy_t8130_dpphy_configure(&a,&rate)==0);
  assert(events[0][1]==(reverse?1:2)); n=0;
  assert(atcphy_t8130_dpphy_configure(&a,&volts)==0 && n==2);
  assert(events[0][1]==!reverse && events[1][1]==!reverse);
  assert(events[0][3]==0x1003 && events[1][3]==0x1006);
  volts.lanes=4; n=0;
  assert(atcphy_t8130_dpphy_configure(&a,&volts)==-EINVAL && n==0);
  struct phy_configure_opts_dp lanes={.set_lanes=true,.lanes=3};
  assert(atcphy_t8130_dpphy_configure(&a,&lanes)==-EINVAL && n==0);
  lanes.lanes=2; assert(atcphy_t8130_dpphy_configure(&a,&lanes)==0 && n==0);
 }
 a=device(); a.dp_training_valid[0]=false; n=0;
 assert(atcphy_t8130_dpphy_configure(&a,&rate)==-ENOENT && n==0);
 a=device(); a.mode=APPLE_ATCPHY_MODE_USB2;
 assert(atcphy_t8130_dpphy_configure(&a,&rate)==-EINVAL && n==0);
 a=device(); init_error=-ETIMEDOUT;
 assert(atcphy_t8130_dpphy_configure(&a,&rate)==-ETIMEDOUT);
 assert(n==2 && events[0][0]==1 && events[1][0]==2);
 assert(!a.dp_main_active && a.dp_link_rate==-1);
 a=device(); init_error=stop_error=-ETIMEDOUT;
 assert(atcphy_t8130_dpphy_configure(&a,&rate)==-ETIMEDOUT);
 assert(a.dp_main_active && a.dp_pma_mask==3 && a.dp_link_rate==-1);
 puts("T8130 DP state, native training lookup, orientation, cleanup: PASS");
}
"""
    with tempfile.TemporaryDirectory(prefix="t8130-state-") as tmp:
        cfile, binary = Path(tmp)/"test.c", Path(tmp)/"test"
        cfile.write_text(prefix + real + suffix)
        subprocess.run(["clang", "-std=gnu11", "-O1", "-fsanitize=address,undefined",
                        str(cfile), "-o", str(binary)], check=True)
        subprocess.run([str(binary)], check=True)


if __name__ == "__main__":
    main()
