#!/usr/bin/env python3
"""Compile the actual OPEN request gate against contradictory crypto inputs."""
from pathlib import Path
import subprocess
import tempfile
from mt7932_test_lib import local_linkage

root = Path(__file__).resolve().parents[4]
source = local_linkage((root / 'drivers/net/wireless/mediatek/mt7932/link.c').read_text())
start = source.index('static bool mt_open_request(')
gate = source[start:source.index('\n}', start) + 2]
prefix = r'''
#include <stdbool.h>
#include <stdint.h>
#include <assert.h>
#include <stddef.h>
#define ETH_P_PAE 0x888e
#define cpu_to_be16(v) ((uint16_t)(((v)<<8)|((v)>>8)))
enum { NL80211_MFP_NO, NL80211_MFP_REQUIRED };
enum { NL80211_AUTHTYPE_OPEN_SYSTEM, NL80211_AUTHTYPE_SHARED_KEY, NL80211_AUTHTYPE_AUTOMATIC };
struct cfg80211_connect_params {
 bool privacy, want_1x;
 int mfp, auth_type;
 size_t ie_len, key_len, fils_erp_username_len, fils_erp_realm_len, fils_erp_rrk_len;
 const void *key, *fils_erp_username, *fils_erp_realm, *fils_erp_rrk;
 struct {
  unsigned wpa_versions, cipher_group, n_ciphers_pairwise, n_akm_suites, sae_pwd_len;
  const void *psk, *sae_pwd;
  bool control_port, control_port_over_nl80211;
  uint16_t control_port_ethertype;
 } crypto;
};
'''
tests = r'''
int main(void) {
 struct cfg80211_connect_params p={.crypto.control_port_ethertype=cpu_to_be16(ETH_P_PAE)}, q;
 assert(mt_open_request(&p));
 p.auth_type=NL80211_AUTHTYPE_AUTOMATIC;assert(mt_open_request(&p));
 p.crypto.control_port=true;assert(mt_open_request(&p));
#define BAD(field, value) do {q=p;q.field=(value);assert(!mt_open_request(&q));} while(0)
 BAD(privacy,true);BAD(crypto.wpa_versions,2);BAD(crypto.n_ciphers_pairwise,1);
 BAD(crypto.cipher_group,0x000fac04);BAD(crypto.n_akm_suites,1);BAD(crypto.psk,&p);
 BAD(key,&p);BAD(key_len,1);BAD(crypto.sae_pwd,&p);BAD(crypto.sae_pwd_len,1);
 BAD(mfp,NL80211_MFP_REQUIRED);BAD(ie_len,1);BAD(want_1x,true);
 BAD(fils_erp_username,&p);BAD(fils_erp_username_len,1);
 BAD(fils_erp_realm,&p);BAD(fils_erp_realm_len,1);BAD(fils_erp_rrk,&p);BAD(fils_erp_rrk_len,1);
 BAD(crypto.control_port_over_nl80211,true);BAD(crypto.control_port_ethertype,0);
 BAD(auth_type,NL80211_AUTHTYPE_SHARED_KEY);
}
'''
with tempfile.TemporaryDirectory(prefix='mt7932-open-') as tmp:
    c, binary = Path(tmp) / 'test.c', Path(tmp) / 'test'
    c.write_text(prefix + gate + tests)
    subprocess.run(['cc', '-Wall', '-Wextra', '-Werror', '-fsanitize=address,undefined',
                    '-g', str(c), '-o', str(binary)], check=True)
    subprocess.run([str(binary)], check=True)
print('MT7932 actual OPEN gate: ordinary OPEN accepted; contradictory security rejected')
