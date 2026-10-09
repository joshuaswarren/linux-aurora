// Inspect an owned L010 CoreVideo allocation. No encoder or hardware submission.
#import <Foundation/Foundation.h>
#import <CoreVideo/CoreVideo.h>
#import <IOSurface/IOSurface.h>
#include <sys/sysctl.h>

int main(int argc, const char **argv) {
    @autoreleasepool {
        if (argc != 2) return 2;
        NSString *output = @(argv[1]);
        if ([[NSFileManager defaultManager] fileExistsAtPath:output]) return 2;
        if (![[NSFileManager defaultManager] createDirectoryAtPath:output
            withIntermediateDirectories:YES attributes:nil error:NULL]) return 2;
        CVPixelBufferRef pixel = NULL;
        NSDictionary *attributes = @{(__bridge NSString *)kCVPixelBufferIOSurfacePropertiesKey:@{},
            (__bridge NSString *)kCVPixelBufferBytesPerRowAlignmentKey:@64};
        CVReturn status = CVPixelBufferCreate(NULL,320,240,kCVPixelFormatType_OneComponent10,
            (__bridge CFDictionaryRef)attributes,&pixel);
        if (status || !pixel) return 1;
        IOSurfaceRef surface = CVPixelBufferGetIOSurface(pixel);
        if (!surface || CVPixelBufferIsPlanar(pixel)) return 1;
        if (CVPixelBufferLockBaseAddress(pixel,0)) return 1;
        size_t stride = CVPixelBufferGetBytesPerRow(pixel);
        size_t dataSize = CVPixelBufferGetDataSize(pixel);
        size_t allocation = IOSurfaceGetAllocSize(surface);
        uint8_t *base = CVPixelBufferGetBaseAddress(pixel);
        uint8_t *surfaceBase = IOSurfaceGetBaseAddress(surface);
        ptrdiff_t offset = base-surfaceBase;
        if (stride != 640 || offset < 0 || allocation > 2*1024*1024 ||
            (size_t)offset+dataSize > allocation || dataSize < stride*240) return 1;
        memset(surfaceBase,0,allocation);
        for (unsigned y=0;y<240;y++) {
            uint16_t *row = (uint16_t *)(base+y*stride);
            for (unsigned x=0;x<320;x++) row[x] = ((x+y)&1023)<<6;
        }
        CVReturn extended = CVPixelBufferFillExtendedPixels(pixel);
        NSData *bytes = [NSData dataWithBytes:base length:dataSize];
        [bytes writeToFile:[output stringByAppendingPathComponent:@"l010-allocation.bin"] atomically:YES];
        char model[128]={0}; size_t length=sizeof(model);
        sysctlbyname("hw.model",model,&length,NULL,0);
        NSDictionary *result=@{@"passed":@YES,@"pixel_format":@"L010",@"width":@320,@"height":@240,
            @"stride":@(stride),@"data_bytes":@(dataSize),@"surface_alloc_bytes":@(allocation),
            @"pixel_offset_in_surface":@(offset),@"bytes_after_visible_pixels":@(dataSize-stride*240),
            @"extended_pixels_status":@(extended),@"model":@(model),
            @"system_version":NSProcessInfo.processInfo.operatingSystemVersionString,
            @"hardware_encoding_claim":@NO,@"scope":@"Host allocation layout only; no encoder invoked"};
        NSData *json=[NSJSONSerialization dataWithJSONObject:result options:NSJSONWritingPrettyPrinted error:NULL];
        [json writeToFile:[output stringByAppendingPathComponent:@"result.json"] atomically:YES];
        puts([[NSString alloc] initWithData:json encoding:NSUTF8StringEncoding].UTF8String);
        CFDictionaryRef description=CVPixelFormatDescriptionCreateWithPixelFormatType(NULL,kCVPixelFormatType_OneComponent10);
        if (description) {
            [(__bridge NSDictionary *)description writeToFile:[output stringByAppendingPathComponent:@"pixel-format.plist"] atomically:YES];
            CFRelease(description);
        }
        CVPixelBufferUnlockBaseAddress(pixel,0);
        CVPixelBufferRelease(pixel);
        return 0;
    }
}
