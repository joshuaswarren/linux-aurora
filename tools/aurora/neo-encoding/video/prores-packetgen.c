// SPDX-License-Identifier: MIT
// Generate bounded rotated diagnostic files. This program never accesses DMA.
#include <errno.h>
#include <fcntl.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <unistd.h>

#define REQUIRE(x) do { if (!(x)) { fprintf(stderr,"packet guard failed at line %d\n",__LINE__);return 2; } } while (0)
static uint32_t u32(const uint8_t *p) {return p[0]|(uint32_t)p[1]<<8|(uint32_t)p[2]<<16|(uint32_t)p[3]<<24;}
static uint16_t u16(const uint8_t *p) {return p[0]|(uint16_t)p[1]<<8;}
static void put32(uint8_t *p,uint32_t v) {for(unsigned i=0;i<4;i++)p[i]=v>>(i*8);}
int main(int argc,char **argv) {
    REQUIRE(argc==5);
    char *end=NULL;unsigned frames=strtoul(argv[2],&end,10);REQUIRE(end && !*end && frames>=1 && frames<=60);
    int warm=strcmp(argv[4],"warm")==0;REQUIRE(warm || strcmp(argv[4],"cold")==0);
    FILE *input=fopen(argv[1],"rb");REQUIRE(input);
    REQUIRE(fseek(input,0,SEEK_END)==0);long length=ftell(input);REQUIRE(length>=416 && length<=4*1024*1024);
    rewind(input);uint8_t *packet=malloc(length);REQUIRE(packet && fread(packet,1,length,input)==(size_t)length);fclose(input);
    REQUIRE(u32(packet)==0x31525041 && u32(packet+4)==384 && u32(packet+28)==0);
    unsigned writes=u32(packet+8),relocs=u32(packet+12);REQUIRE(writes<=2048 && relocs<=11);
    size_t n[3]={u32(packet+16),u32(packet+20),u32(packet+24)};
    for(unsigned i=0;i<3;i++)REQUIRE(n[i]<=1024*1024);
    size_t desc_off=32+writes*8,reloc_off=desc_off+384,planes_off=reloc_off+relocs*12;
    REQUIRE(planes_off+n[0]+n[1]+n[2]==(size_t)length);
    const uint8_t *desc=packet+desc_off;unsigned flags=u32(desc),sub=(flags>>2)&3;
    REQUIRE(u16(desc+0x18)==320 && u16(desc+0x1a)==240 && (flags&3)==3 && !(flags&0x40));
    REQUIRE(sub==2 || sub==3);REQUIRE(n[0] && n[1] && (!!n[2])==(!!(flags&0x100)));
    size_t stride[3],offset[3]={0},bpp[3]={2,sub==2?2:4,flags&0x80?2:1};
    for(unsigned plane=0;plane<3;plane++) {
        stride[plane]=u16(desc+0x20+plane*2)*64;
        if(!n[plane])continue;
        unsigned found=0,slot=0x28+plane*0x10;
        for(unsigned i=0;i<relocs;i++) {
            const uint8_t *r=packet+reloc_off+i*12;
            REQUIRE(u32(r)!=slot+8);
            if(u32(r)==slot) {REQUIRE(u32(r+4)==plane+1);offset[plane]=u32(r+8);found++;}
        }
        REQUIRE(found==1 && stride[plane]>=320*bpp[plane]);
        REQUIRE(offset[plane]+239*stride[plane]+320*bpp[plane]<=n[plane]);
    }
    uint8_t *generated=malloc(length);REQUIRE(generated);
    for(unsigned frame=0;frame<frames;frame++) {
        unsigned count=frame==0 && !warm?writes:0;
        size_t new_desc=32+count*8,new_reloc=new_desc+384,new_planes=new_reloc+relocs*12;
        size_t bytes=new_planes+n[0]+n[1]+n[2];
        memcpy(generated,packet,32+count*8);put32(generated+8,count);
        memcpy(generated+new_desc,desc,384+relocs*12);
        memcpy(generated+new_planes,packet+planes_off,n[0]+n[1]+n[2]);
        size_t base=0;
        for(unsigned plane=0;plane<3;plane++) {
            if(n[plane]) for(unsigned y=0;y<240;y++) {
                size_t row=base+offset[plane]+y*stride[plane],row_bytes=320*bpp[plane],shift=(frame*8%320)*bpp[plane];
                memcpy(generated+new_planes+row,packet+planes_off+row+shift,row_bytes-shift);
                memcpy(generated+new_planes+row+row_bytes-shift,packet+planes_off+row,shift);
            }
            base+=n[plane];
        }
        char path[4096];int len=snprintf(path,sizeof(path),"%s/packet-%03u.bin",argv[3],frame);REQUIRE(len>0 && (size_t)len<sizeof(path));
        int fd=open(path,O_WRONLY|O_CREAT|O_EXCL,0600);REQUIRE(fd>=0);
        size_t done=0;while(done<bytes) {ssize_t put=write(fd,generated+done,bytes-done);REQUIRE(put>0);done+=put;}
        REQUIRE(close(fd)==0);
    }
    free(generated);free(packet);printf("PRORES_PACKET_FILES_READY %u\n",frames);return 0;
}
