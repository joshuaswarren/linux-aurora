// Data-layout checks only: no encoder or target access.
#import <Foundation/Foundation.h>
#import <CoreVideo/CoreVideo.h>
#include <assert.h>
#include "coverage-pixels.h"
int main(int argc,const char **argv) {
    @autoreleasepool {
        assert(argc==2);
        for (NSString *name in @[@"nv12",@"2vuy",@"p010",@"p210",@"y416",@"bgra",@"fdep",@"fdis"]) {
            CVPixelBufferRef pixel=NULL;
            NSDictionary *attrs=@{(__bridge NSString *)kCVPixelBufferIOSurfacePropertiesKey:@{}};
            assert(CVPixelBufferCreate(NULL,320,240,coverageFormat(name),(__bridge CFDictionaryRef)attrs,&pixel)==0);
            NSData *first=coveragePixels(pixel,name,0,NO),*next=coveragePixels(pixel,name,1,NO);
            assert(first && next && first.length==next.length && ![first isEqualToData:next]);
            if ([name isEqual:@"2vuy"]) assert([first isEqualToData:[NSData dataWithContentsOfFile:@(argv[1])]]);
            if ([name isEqual:@"p010"] || [name isEqual:@"p210"] || [name isEqual:@"y416"]) {
                const uint16_t *values=first.bytes;unsigned seen=0;
                BOOL ayuv=[name isEqual:@"y416"];
                for (unsigned i=0;i<320*240;i++) seen|=1<<((values[ayuv?4*i+1:i]>>(ayuv?4:6))&3);
                assert(seen==15); // All two low significant bits survive the generator.
            }
            printf("PIXEL_LAYOUT_PASS %s %s bytes=%lu\n",name.UTF8String,coverageFFmpegFormat(name).UTF8String,(unsigned long)first.length);
            CVPixelBufferRelease(pixel);
        }
    }
}
