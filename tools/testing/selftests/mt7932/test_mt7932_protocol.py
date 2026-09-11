#!/usr/bin/env python3
"""Compile the actual clean MT7932 wire helpers with ASan/UBSan."""
from pathlib import Path
import subprocess
import tempfile
import sys
import hashlib

# Original container metadata only, not radio instruction analysis. Pin whole
# pairs so an accidental mixed release cannot silently select another fixture.
cohorts = {
    ('64acca8fb14cc270025b630d7b2d5e6b3ea4128d1d35032af39490a0ce6aec9e',
     '4cabf6be010e3cb98e9304d02b74aa613ab5afc964167ba89d33afec88e2de24'): (
        (0, 426768, 0x907c78, 0x20), (426768, 348048, 0x200f060, 0),
        (774816, 15312, 0x404400, 0), (790128, 7120, 0x210a000, 0),
        (797248, 393104, 0xe0200040, 0)),
    ('60e37f53dbc6d757c2a23fb125e53b0b04c0104be5315910885955d0f6b57379',
     'aa9ca3443279aa2878ca8275e69829d429fa8856ca74b84da1f65f7c2da989d4'): (
        (0, 426832, 0x907c60, 0x20), (426832, 348112, 0x200f020, 0),
        (774944, 15312, 0x404400, 0), (790256, 7120, 0x210a000, 0),
        (797376, 393104, 0xe0200050, 0)),
}
expected_regions = next(iter(cohorts.values()))
if len(sys.argv) not in (1, 3):
    raise SystemExit('usage: test_mt7932_protocol.py [STOCK_PATCH STOCK_RAM]')
if len(sys.argv) == 3:
    pair = tuple(hashlib.sha256(Path(path).read_bytes()).hexdigest() for path in sys.argv[1:])
    if pair not in cohorts:
        raise SystemExit('unknown or mixed stock firmware pair; no fixture selected')
    expected_regions = cohorts[pair]

root = Path(__file__).resolve().parents[4]
header = root / 'drivers/net/wireless/mediatek/mt7932/protocol.h'
source = '\n'.join(line for line in header.read_text().splitlines()
                   if not line.startswith('#include'))
source += '\n' + '\n'.join(line for line in header.with_name('firmware.h').read_text().splitlines()
                          if not line.startswith('#include'))
source += '\n' + header.with_name('channels.h').read_text()
source += '\n' + '\n'.join(line for line in header.with_name('scan.h').read_text().splitlines()
                          if not line.startswith('#include'))
source += '\n' + '\n'.join(line for line in header.with_name('association.h').read_text().splitlines()
                          if not line.startswith('#include'))
prefix = r'''
#include <stdint.h>
#include <stddef.h>
#include <stdbool.h>
#include <string.h>
#include <errno.h>
#include <assert.h>
#include <stdio.h>
#include <stdlib.h>
typedef uint8_t u8;
typedef uint16_t u16;
typedef uint32_t u32;
static uint32_t get_unaligned_le32(const void *v) {
 const uint8_t *p=v; return p[0]|((uint32_t)p[1]<<8)|((uint32_t)p[2]<<16)|((uint32_t)p[3]<<24);
}
static uint16_t get_unaligned_le16(const void *v) {
 const uint8_t *p=v; return p[0]|((uint16_t)p[1]<<8);
}
static uint32_t get_unaligned_be32(const void *v) {
 const uint8_t *p=v;return ((uint32_t)p[0]<<24)|((uint32_t)p[1]<<16)|((uint32_t)p[2]<<8)|p[3];
}
static void put_unaligned_le32(uint32_t v,void *out) {
 uint8_t *p=out; for(int i=0;i<4;i++)p[i]=v>>(8*i);
}
static void put_unaligned_le16(uint16_t v,void *out) {
 uint8_t *p=out; for(int i=0;i<2;i++)p[i]=v>>(8*i);
}
'''
prefix += 'static const uint32_t expected_regions[5][4] = {' + ','.join(
    '{' + ','.join(str(field) for field in region) + '}' for region in expected_regions) + '};\n'
tests = r'''
int main(int argc,char **argv) {
 u8 rts[12],rts_wire[76];
 const u8 rts_expected[]={1,0,0,0,0x2b,9,0,0,2,0,0,0};
 mt7932_rts_config(rts);assert(!memcmp(rts,rts_expected,12));
 assert(mt7932_envelope(rts_wire,76,0xed,0x3e,17,true,true,true,rts,12)==76);
 assert(get_unaligned_le16(rts_wire+32)==44 && rts_wire[36]==0xed && rts_wire[37]==0xa0);
 assert(rts_wire[38]==1 && rts_wire[39]==17 && rts_wire[41]==0x3e && !rts_wire[42] && rts_wire[43]==1);
 assert(!memcmp(rts_wire+64,rts_expected,12));
 u8 join[856],pmk[32],bssid[6]={2,0,0,0,0,1},queue[32],assoc[52]={0};
 memset(pmk,0x5a,32);struct mt7932_peer_ids ids={{1,2},{3,4,5,6}};
 assert(mt7932_connect_body(join,sizeof(join),(const u8*)"TEST",4,pmk,bssid,6,50,2,&ids,false)==856);
 assert(join[1]==4 && get_unaligned_le32(join+4)==16 && get_unaligned_le16(join+0x12)==1);
 assert(get_unaligned_le32(join+0x18)==4 && !memcmp(join+0x1c,"TEST",4));
 assert(get_unaligned_le32(join+0x40)==32 && get_unaligned_le32(join+0x44)==6);
 assert(!get_unaligned_le16(join+0x4a));
 assert(!memcmp(join+0x4c,pmk,32));
 for(unsigned i=0x1c8;i<0x210;i++)assert(!join[i]);
 /* Synthetic keys only: the owned command copy must survive wiping the
  * transient builder buffer. This does not qualify firmware PMK ownership.
  */
 u8 connect_wire[920], transient_join[856];
 memcpy(transient_join,join,856);
 assert(mt7932_envelope(connect_wire,920,0xea,0x40,149,true,true,false,transient_join,856)==920);
 memset(transient_join,0,856);
 assert(connect_wire[36]==0xea && connect_wire[38]==1 && connect_wire[39]==149);
 assert(connect_wire[41]==0x40 && !connect_wire[43]);
 assert(get_unaligned_le32(connect_wire+64+0x40)==32);
 assert(!memcmp(connect_wire+64+0x4c,pmk,32));
 assert(!memcmp(connect_wire+64,join,856));
 assert(get_unaligned_le32(join+0x220)==5);
 assert(join[0x1c0]==2);
 assert(mt7932_connect_body(join,856,(const u8*)"TEST",4,pmk,bssid,6,20,64,&ids,false)<0);
 assert(get_unaligned_le16(join+0x228)==20 && !memcmp(join+0x32c,bssid,6));
 assert(join[0x338]==1 && join[0x339]==6 && join[0x354]==1);
 for(int i=0x6c;i<0x198;i++)assert(!join[i]);
 /* The coherent original type6 binary result, including an embedded NUL. */
 u8 binary_join[856],binary_pmk[32];
 for(unsigned i=0;i<32;i++)binary_pmk[i]=i;
 memcpy(binary_join,join,856);
 assert(mt7932_connect_binary_pmk(binary_join,856,binary_pmk,31)<0);
 assert(mt7932_connect_binary_pmk(binary_join,856,binary_pmk,33)<0);
 assert(mt7932_connect_binary_pmk(binary_join,855,binary_pmk,32)<0);
 assert(!memcmp(binary_join,join,856));
 assert(!mt7932_connect_binary_pmk(binary_join,856,binary_pmk,32));
 assert(get_unaligned_le32(binary_join+0x40)==32&&get_unaligned_le32(binary_join+0x44)==6);
 assert(!get_unaligned_le32(binary_join+0x48)&&!memcmp(binary_join+0x4c,binary_pmk,32));
 for(unsigned i=0x1c8;i<0x210;i++)assert(!binary_join[i]);
 for(unsigned i=0;i<856;i++)if(!((i>=0x40&&i<0xd0)||(i>=0x1c8&&i<0x210)))
  assert(binary_join[i]==join[i]);
 assert(mt7932_envelope(connect_wire,920,0xea,0x40,150,true,true,false,binary_join,856)==920);
 memset(binary_join,0,856);
 assert(get_unaligned_le32(connect_wire+64+0x40)==32&&!memcmp(connect_wire+64+0x4c,binary_pmk,32));
 assert(mt7932_connect_body(join,855,(const u8*)"TEST",4,pmk,bssid,6,1,0,&ids,false)<0);
 ids.wtbl[3]=0;assert(mt7932_connect_body(join,856,(const u8*)"TEST",4,pmk,bssid,6,1,0,&ids,false)<0);ids.wtbl[3]=6;
 assert(mt7932_connect_body(join,856,(const u8*)"TEST",4,pmk,bssid,14,1,0,&ids,false)<0);
 u8 open_join[856];memset(open_join,0xa5,856);
 u8 join5[856];
 assert(mt7932_connect_body(join5,856,(const u8*)"TEST",4,pmk,bssid,36,20,2,&ids,false)==856);
 assert(join5[0x338]==2 && join5[0x339]==36 && !join5[0x1c4]);
 assert(get_unaligned_le32(join5+0x220)==5);
 assert(mt7932_connect_body(join5,856,(const u8*)"TEST",4,pmk,bssid,52,20,2,&ids,false)<0);
 assert(mt7932_connect_body(open_join,856,(const u8*)"TEST",4,pmk,bssid,6,20,2,&ids,true)<0);
 assert(mt7932_connect_body(open_join,856,(const u8*)"TEST",4,NULL,bssid,6,20,2,&ids,false)<0);
 assert(mt7932_connect_body(open_join,856,(const u8*)"TEST",4,NULL,bssid,6,20,2,&ids,true)==856);
 assert(!open_join[1]&&!get_unaligned_le32(open_join+4)&&get_unaligned_le32(open_join+8)==1);
 assert(!get_unaligned_le16(open_join+0x12)&&!get_unaligned_le16(open_join+0x14));
 assert(get_unaligned_le32(open_join+0x20c)==1&&!get_unaligned_le32(open_join+0x220));
 for(unsigned i=0x40;i<0x198;i++)assert(!open_join[i]);
 for(unsigned i=0x1c8;i<0x20c;i++)assert(!open_join[i]);
 for(unsigned i=0;i<856;i++)if(!(i==1||(i>=4&&i<12)||(i>=0x12&&i<0x16)||
   (i>=0x40&&i<0xd0)||(i>=0x1c8&&i<0x210)||(i>=0x220&&i<0x224)))assert(open_join[i]==join[i]);
 mt7932_peer_queue(queue,0,true);assert(queue[2]==5 && queue[14]==1 && get_unaligned_le16(queue+16)==15);
 assoc[11]=1;assoc[12]=6;assoc[17]=3;assoc[20]=1;assoc[21]=4;
 put_unaligned_le32(0x000fac04,assoc+48);
 assert(mt7932_assoc_resources(assoc,52,&ids,6,false));
 assoc[11]=2;assoc[12]=36;
 assert(mt7932_assoc_resources(assoc,52,&ids,36,false));
 assert(!mt7932_assoc_resources(assoc,52,&ids,6,false));
 assoc[11]=1;assert(!mt7932_assoc_resources(assoc,52,&ids,36,false));assoc[12]=6;
 put_unaligned_le32(10,assoc+48);assert(!mt7932_assoc_resources(assoc,52,&ids,6,false));
 /* OPEN's cipher is metadata only; status/resource guards remain strict. */
 assert(mt7932_assoc_resources(assoc,52,&ids,6,true));
 assoc[0]=1;assert(!mt7932_assoc_resources(assoc,52,&ids,6,true));assoc[0]=0;
 assoc[21]=7;assert(!mt7932_assoc_resources(assoc,52,&ids,6,true));assoc[21]=4;
 assert(!mt7932_assoc_resources(assoc,51,&ids,6,true));
 put_unaligned_le32(0x000fac04,assoc+48);
 for(size_t cut=0;cut<52;cut++)assert(!mt7932_assoc_resources(assoc,cut,&ids,6,false));
 assoc[21]=7;assert(!mt7932_assoc_resources(assoc,52,&ids,6,false));
 u8 scan[0x4d4],channels[13];for(int i=0;i<13;i++)channels[i]=i+1;
 assert(mt7932_scan_body(scan,sizeof(scan),127,channels,13)==0x4d4);
 unsigned char before_home[0x4d4];memcpy(before_home,scan,sizeof(scan));
 assert(!mt7932_scan_home(scan,36));assert(scan[0x4cc]==36&&scan[0x4cd]==2);
 assert(!memcmp(scan,before_home,0x4cc)&&!memcmp(scan+0x4ce,before_home+0x4ce,sizeof(scan)-0x4ce));
 assert(!mt7932_scan_home(scan,4));assert(scan[0x4cc]==4&&scan[0x4cd]==1);
 assert(mt7932_scan_home(scan,14)==-EINVAL);assert(mt7932_scan_home(scan,52)==-EINVAL);
 assert(!mt7932_scan_home(scan,0));assert(!memcmp(scan,before_home,sizeof(scan)));
 assert(scan[0]==127 && scan[1]==0 && scan[2]==1 && scan[3]==1 && scan[6]==0x12 && scan[7]==1);
 assert(get_unaligned_le16(scan+0x9a)==20 && !get_unaligned_le16(scan+0x9c));
 assert(scan[0x9e]==4 && scan[0x9f]==13);
 for(int i=0;i<13;i++)assert(scan[0xa0+2*i]==1 && scan[0xa1+2*i]==i+1);
 assert(get_unaligned_le16(scan+0xe0)==21 && scan[0xe2]==0x7f);
 assert(get_unaligned_le32(scan+0x4b8)==0x1800 && get_unaligned_le32(scan+0x4bc)==45);
 assert(get_unaligned_le16(scan+0x4c2)==110 && get_unaligned_le16(scan+0x4ca)==110);
 assert(mt7932_scan_body(scan,sizeof(scan)-1,1,channels,13)<0);
 assert(mt7932_scan_body(scan,sizeof(scan),0,channels,13)<0);
 assert(mt7932_scan_body(scan,sizeof(scan),128,channels,13)<0);
 assert(mt7932_scan_body(scan,sizeof(scan),1,channels,0)<0);
 channels[12]=14;assert(mt7932_scan_body(scan,sizeof(scan),1,channels,13)<0);
 channels[12]=1;assert(mt7932_scan_body(scan,sizeof(scan),1,channels,13)<0);
 channels[12]=13;
 u8 channels5[]={36,40,44,48};
 assert(mt7932_scan_body(scan,sizeof(scan),1,channels5,4)>0);
 for(unsigned i=0;i<4;i++)assert(scan[0xa0+2*i]==2 && scan[0xa1+2*i]==channels5[i]);
 channels5[3]=6;assert(mt7932_scan_body(scan,sizeof(scan),1,channels5,4)<0);
 channels5[3]=52;assert(mt7932_scan_body(scan,sizeof(scan),1,channels5,4)<0);
 assert(mt7932_scan_body(scan,sizeof(scan),1,channels,13)>0);
 assert(!mt7932_scan_options(scan,NULL,0,NULL,0) && get_unaligned_le16(scan+0xe0)==21);
 assert(!mt7932_scan_options(scan,(const u8*)"TEST",4,NULL,0));
 assert(scan[3]==8 && scan[4]==1 && get_unaligned_le32(scan+8)==4 && !memcmp(scan+12,"TEST",4));
 assert(!get_unaligned_le16(scan+0xe0));
 u8 ies[602]={0};for(unsigned i=0;i<600;i+=2)ies[i]=221;
 assert(!mt7932_scan_options(scan,NULL,0,ies,600));
 assert(get_unaligned_le16(scan+0xe0)==600 && !memcmp(scan+0xe2,ies,600));
 assert(mt7932_scan_options(scan,NULL,0,ies,601)<0);
 assert(mt7932_scan_options(scan,NULL,0,ies,599)<0);
 ies[1]=255;assert(mt7932_scan_options(scan,NULL,0,ies,4)<0);
 assert(mt7932_scan_options(scan,(const u8*)"TEST",33,NULL,0)<0);
 /* Passive mode is explicit, cannot become directed via options, and
  * retains the native dwell/tail and validated caller IE carriage.
  */
 const u8 passive_ies[]={0x7f,1,0};
 assert(mt7932_scan_body(scan,sizeof(scan),1,channels,13)>0);
 mt7932_scan_passive(scan);
 assert(!scan[2]&&!scan[3]&&!scan[4]&&!scan[5]&&scan[6]==0x12);
 assert(get_unaligned_le16(scan+0x9a)==40&&get_unaligned_le16(scan+0x33c)==40);
 assert(get_unaligned_le32(scan+0x4b8)==0x1800&&get_unaligned_le32(scan+0x4bc)==45);
 assert(get_unaligned_le16(scan+0x4c2)==110&&get_unaligned_le16(scan+0x4ca)==110);
 assert(mt7932_scan_options(scan,(const u8*)"TEST",4,NULL,0)==-EINVAL);
 assert(!mt7932_scan_options(scan,NULL,0,passive_ies,sizeof(passive_ies)));
 assert(get_unaligned_le16(scan+0xe0)==sizeof(passive_ies));
 assert(!memcmp(scan+0xe2,passive_ies,sizeof(passive_ies)));
 for(unsigned i=8;i<152;i++)assert(!scan[i]);
 for(unsigned i=0x458;i<0x464;i++)assert(!scan[i]);
 assert(!scan[2]&&!scan[3]&&!scan[4]&&!scan[5]);
 u8 out[8192],payload[12]={0},packet[40]={0};
 struct mt7932_event e;
 put_unaligned_le32(0x70010204,payload+4);
 assert(mt7932_envelope(out,sizeof(out),3,0,255,false,false,true,payload,12)==76);
 assert(get_unaligned_le32(out)==0x0100004c);
 assert(get_unaligned_le32(out+4)==0x10000);
 assert(get_unaligned_le16(out+32)==44);
 assert(out[36]==3 && out[37]==0xa0 && !out[38] && out[39]==255 && !out[43]);
 assert(!memcmp(out+64,payload,12));
 for(int i=8;i<32;i++)assert(!out[i]);
 for(int i=44;i<64;i++)assert(!out[i]);
 assert(mt7932_envelope(out,63,3,0,1,false,false,true,NULL,0)==-EINVAL);
 assert(mt7932_envelope(out,sizeof(out),3,0,0,false,false,true,NULL,0)==-EINVAL);
 assert(mt7932_envelope(out,sizeof(out),3,0,1,false,false,true,payload,SIZE_MAX)==-EINVAL);
 assert(mt7932_envelope(out,sizeof(out),0,0,1,false,false,false,NULL,0)==64);
 assert(get_unaligned_le32(out)==0x01800040 && out[38]==1 && !out[43]);
 assert(mt7932_envelope(out,sizeof(out),0x8a,0,2,true,false,true,NULL,0)==64);
 assert(!out[38] && out[43]==1);
 put_unaligned_le32((7U<<27)|40,packet);
 put_unaligned_le16(16,packet+24);
 packet[28]=2;packet[29]=255;
 assert(!mt7932_event_parse(packet,40,&e));
 assert(e.length==40 && e.eid==2 && mt7932_event_matches(&e,255));
 assert(!mt7932_event_matches(&e,1) && !mt7932_event_matches(&e,0));
 packet[30]=4;
 assert(!mt7932_event_parse(packet,40,&e) && !mt7932_event_matches(&e,255));
 for(size_t i=0;i<40;i++)assert(mt7932_event_parse(packet,i,&e)<0);
 put_unaligned_le16(17,packet+24);
 assert(mt7932_event_parse(packet,40,&e)==-EMSGSIZE);
 put_unaligned_le16(7,packet+24);
 assert(mt7932_event_parse(packet,40,&e)==-EMSGSIZE);
 put_unaligned_le16(16,packet+24);
 put_unaligned_le32((7U<<27)|(1U<<16)|40,packet);
 assert(mt7932_event_parse(packet,40,&e)==-ENOMSG);
 put_unaligned_le32((7U<<27)|33,packet);
 put_unaligned_le16(9,packet+24);
 assert(!mt7932_event_parse(packet,33,&e)); /* compact status, no 36-byte assumption */
 u8 patch[164]={0}; struct mt7932_region region;
 patch[47]=1;patch[99]=2;patch[103]=160;patch[107]=4;
 patch[109]=0x90;patch[115]=4;memset(patch+116,255,4);
 assert(mt7932_patch_region(patch,sizeof(patch),0,&region)==1);
 assert(region.offset==160 && region.length==4 && region.target==0x900000 && region.mode==0x80000000);
 for(size_t i=0;i<164;i++)assert(mt7932_patch_region(patch,i,0,&region)<0);
 assert(mt7932_patch_region(patch,sizeof(patch),1,&region)<0);
 patch[116]=3;assert(mt7932_patch_region(patch,sizeof(patch),0,&region)==-EOPNOTSUPP);
 u8 ram[128]={0};ram[94]=1;put_unaligned_le32(0x907c78,ram+68);
 put_unaligned_le32(4,ram+72);ram[76]=0x20;
 assert(mt7932_ram_region(ram,sizeof(ram),0,&region)==1);
 assert(region.offset==0 && region.target==0x907c78 && region.features==0x20 && region.length==4);
 put_unaligned_le32(53,ram+72);assert(mt7932_ram_region(ram,sizeof(ram),0,&region)<0);
 u8 caps[24]={0};put_unaligned_le16(1,caps);put_unaligned_le32(8,caps+4);
 put_unaligned_le32(12,caps+8);caps[16]=2;
 assert(mt7932_capabilities(caps,sizeof(caps))==1);
 for(size_t i=0;i<24;i++)assert(mt7932_capabilities(caps,i)<0);
 caps[16]=0;assert(mt7932_capabilities(caps,sizeof(caps))==-EINVAL);
 caps[16]=2;put_unaligned_le32(999,caps+4);assert(mt7932_capabilities(caps,sizeof(caps))==1);
 put_unaligned_le16(2,caps);assert(mt7932_capabilities(caps,sizeof(caps))<0);
 /* Hardware W7932_2 capability tag 0x20 has a four-byte opaque value. */
 u8 opaque[16]={1};put_unaligned_le32(0x20,opaque+4);
 put_unaligned_le32(4,opaque+8);opaque[12]=1;
 assert(mt7932_capabilities(opaque,sizeof(opaque))==1);
 for(size_t i=0;i<16;i++)assert(mt7932_capabilities(opaque,i)<0);
 put_unaligned_le32(3,opaque+8);
 assert(mt7932_capabilities(opaque,15)==-EMSGSIZE);
 u8 basic[24],mlme[4];
 const u8 expected_basic[24]={0,0,0,3,0,0,0,0,0,0,0,0,1,0,2,3,0,0x20,0,0,2,0,0,0};
 const u8 expected_mlme[4]={7,4,2,0};
 mt7932_basic_config(basic);mt7932_mlme_config(mlme);
 assert(!memcmp(basic,expected_basic,24) && !memcmp(mlme,expected_mlme,4));
 assert(mt7932_envelope(out,sizeof(out),0xea,0x4e,19,true,true,false,mlme,4)==68);
 assert(out[36]==0xea && out[38]==1 && out[39]==19 && out[41]==0x4e && !out[43]);
 assert(!memcmp(out+64,mlme,4));
 if(argc==3) {
  for(int file=0;file<2;file++) {
   FILE *f=fopen(argv[file+1],"rb");assert(f);
   assert(!fseek(f,0,SEEK_END));long bytes=ftell(f);assert(bytes>0 && bytes<4000000);
   rewind(f);u8 *data=malloc(bytes);assert(data);assert(fread(data,1,bytes,f)==(size_t)bytes);fclose(f);
   int n=file ? mt7932_ram_region(data,bytes,0,&region):mt7932_patch_region(data,bytes,0,&region);
   assert(n==(file ? 5:1));
   for(int i=0;i<n;i++) {
    assert((file ? mt7932_ram_region(data,bytes,i,&region):mt7932_patch_region(data,bytes,i,&region))==n);
    if(!file)assert(region.offset==160 && region.length==33408 && region.target==0x900000);
    if(file) {
     assert(region.offset==expected_regions[i][0] && region.length==expected_regions[i][1]);
     assert(region.target==expected_regions[i][2] && region.features==expected_regions[i][3]);
    }
   }
   free(data);
  }
 }
}
'''
with tempfile.TemporaryDirectory(prefix='mt7932-protocol-') as tmp:
    c, binary = Path(tmp) / 'test.c', Path(tmp) / 'test'
    c.write_text(prefix + source + tests)
    subprocess.run(['cc', '-Wall', '-Wextra', '-Werror', '-fsanitize=address,undefined',
                    '-g', str(c), '-o', str(binary)], check=True)
    subprocess.run([str(binary), *sys.argv[1:]], check=True)
print('MT7932: real C envelope, bounds, compact event and response matching tests passed')
