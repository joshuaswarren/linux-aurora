// SPDX-License-Identifier: MIT
// Controlled, hardware-required H.264/HEVC workload for J700 AVE2 tracing.
#import <Foundation/Foundation.h>
#import <VideoToolbox/VideoToolbox.h>
#include <pthread.h>

enum { WIDTH = 320, HEIGHT = 240, FPS = 30 };
typedef struct {
    FILE *file;
    pthread_mutex_t lock;
    unsigned frames;
    OSStatus error;
    bool parameters;
    CMVideoCodecType codec;
} Capture;

static OSStatus parameterSet(Capture *cap, CMFormatDescriptionRef format, size_t index,
                             const uint8_t **parameter, size_t *size, size_t *count,
                             int *lengthBytes) {
    if (cap->codec == kCMVideoCodecType_HEVC)
        return CMVideoFormatDescriptionGetHEVCParameterSetAtIndex(format, index,
            parameter, size, count, lengthBytes);
    return CMVideoFormatDescriptionGetH264ParameterSetAtIndex(format, index,
        parameter, size, count, lengthBytes);
}

static void nal(FILE *file, const uint8_t *bytes, size_t length) {
    const uint8_t prefix[] = {0, 0, 0, 1};
    fwrite(prefix, 1, sizeof(prefix), file);
    fwrite(bytes, 1, length, file);
}

static void callback(void *ref, void *source, OSStatus status,
                     VTEncodeInfoFlags flags, CMSampleBufferRef sample) {
    (void)source;
    Capture *cap = ref;
    pthread_mutex_lock(&cap->lock);
    if (status || !sample || (flags & kVTEncodeInfo_FrameDropped)) {
        cap->error = status ?: -1;
        goto done;
    }
    CMFormatDescriptionRef format = CMSampleBufferGetFormatDescription(sample);
    int lengthBytes = 0;
    const uint8_t *parameter = NULL;
    size_t size = 0, count = 0;
    status = parameterSet(cap, format, 0, &parameter, &size, &count, &lengthBytes);
    if (status || lengthBytes != 4) { cap->error = status ?: -2; goto done; }
    if (!cap->parameters) {
        for (size_t i = 0; i < count; i++) {
            status = parameterSet(cap, format, i, &parameter, &size, NULL, NULL);
            if (status) { cap->error = status; goto done; }
            nal(cap->file, parameter, size);
        }
        cap->parameters = true;
    }
    CMBlockBufferRef block = CMSampleBufferGetDataBuffer(sample);
    size_t length = CMBlockBufferGetDataLength(block);
    uint8_t *data = malloc(length);
    if (!data) { cap->error = -3; goto done; }
    status = CMBlockBufferCopyDataBytes(block, 0, length, data);
    if (status) { free(data); cap->error = status; goto done; }
    size_t offset = 0;
    while (offset + 4 <= length) {
        uint32_t n = ((uint32_t)data[offset] << 24) | ((uint32_t)data[offset+1] << 16)
                   | ((uint32_t)data[offset+2] << 8) | data[offset+3];
        offset += 4;
        if (!n || n > length - offset) { cap->error = -4; break; }
        nal(cap->file, data + offset, n);
        offset += n;
    }
    if (offset != length) cap->error = -5;
    if (ferror(cap->file)) cap->error = -6;
    if (!cap->error) cap->frames++;
    free(data);
done:
    pthread_mutex_unlock(&cap->lock);
}

static void check(OSStatus status, const char *operation) {
    if (status) { fprintf(stderr, "%s: OSStatus %d\n", operation, (int)status); exit(1); }
}

int main(int argc, const char **argv) {
    @autoreleasepool {
        if (argc != 5 && argc != 6) {
            fprintf(stderr, "Usage: neo-vt-encode INPUT.nv12 OUTPUT.annexb RESULT.json FRAMES [h264|hevc]\n");
            return 2;
        }
        unsigned frameCount = (unsigned)strtoul(argv[4], NULL, 10);
        if (!frameCount || frameCount > 600) return 2;
        NSString *codecName = argc == 6 ? @(argv[5]) : @"h264";
        if (![codecName isEqual:@"h264"] && ![codecName isEqual:@"hevc"]) return 2;
        CMVideoCodecType codec = [codecName isEqual:@"hevc"] ? kCMVideoCodecType_HEVC : kCMVideoCodecType_H264;
        NSData *input = [NSData dataWithContentsOfFile:@(argv[1])];
        size_t frameBytes = WIDTH * HEIGHT * 3 / 2;
        if (!input || input.length < frameBytes * frameCount) {
            fprintf(stderr, "Input is smaller than requested NV12 frame count\n"); return 2;
        }
        FILE *output = fopen(argv[2], "wbx");
        if (!output) { perror("create output"); return 2; }
        Capture cap = {.file=output, .lock=PTHREAD_MUTEX_INITIALIZER, .codec=codec};
        NSDictionary *encoder = @{(__bridge NSString *)kVTVideoEncoderSpecification_RequireHardwareAcceleratedVideoEncoder:@YES};
        NSDictionary *pixels = @{(__bridge NSString *)kCVPixelBufferPixelFormatTypeKey:@(kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange),
                                 (__bridge NSString *)kCVPixelBufferWidthKey:@(WIDTH),
                                 (__bridge NSString *)kCVPixelBufferHeightKey:@(HEIGHT),
                                 (__bridge NSString *)kCVPixelBufferIOSurfacePropertiesKey:@{}};
        VTCompressionSessionRef session = NULL;
        check(VTCompressionSessionCreate(NULL, WIDTH, HEIGHT, codec,
            (__bridge CFDictionaryRef)encoder, (__bridge CFDictionaryRef)pixels, NULL,
            callback, &cap, &session), "VTCompressionSessionCreate hardware required");
        NSDictionary *properties = @{
            (__bridge NSString *)kVTCompressionPropertyKey_RealTime:@NO,
            (__bridge NSString *)kVTCompressionPropertyKey_AllowFrameReordering:@NO,
            (__bridge NSString *)kVTCompressionPropertyKey_AverageBitRate:@1000000,
            (__bridge NSString *)kVTCompressionPropertyKey_ExpectedFrameRate:@(FPS),
            (__bridge NSString *)kVTCompressionPropertyKey_MaxKeyFrameInterval:@60,
            (__bridge NSString *)kVTCompressionPropertyKey_ProfileLevel:(__bridge NSString *)(
                codec == kCMVideoCodecType_HEVC ? kVTProfileLevel_HEVC_Main_AutoLevel : kVTProfileLevel_H264_Main_AutoLevel)};
        check(VTSessionSetProperties(session, (__bridge CFDictionaryRef)properties), "set encoder properties");
        check(VTCompressionSessionPrepareToEncodeFrames(session), "prepare encoder");
        CFTypeRef hardware = NULL;
        check(VTSessionCopyProperty(session, kVTCompressionPropertyKey_UsingHardwareAcceleratedVideoEncoder,
                                   NULL, &hardware), "query hardware encoder");
        bool usingHardware = hardware && CFEqual(hardware, kCFBooleanTrue);
        if (hardware) CFRelease(hardware);
        if (!usingHardware) { fprintf(stderr, "Encoder is not hardware accelerated\n"); return 1; }
        fprintf(stderr, "NEO_VT_HARDWARE_READY width=%d height=%d frames=%u\n", WIDTH, HEIGHT, frameCount);
        uint64_t start = clock_gettime_nsec_np(CLOCK_MONOTONIC_RAW);
        for (unsigned frame = 0; frame < frameCount; frame++) {
            CVPixelBufferRef pixel = NULL;
            check(CVPixelBufferPoolCreatePixelBuffer(NULL, VTCompressionSessionGetPixelBufferPool(session), &pixel), "allocate NV12 pixel buffer");
            check(CVPixelBufferLockBaseAddress(pixel, 0), "lock pixel buffer");
            const uint8_t *source = (const uint8_t *)input.bytes + frame * frameBytes;
            for (size_t plane = 0; plane < 2; plane++) {
                uint8_t *destination = CVPixelBufferGetBaseAddressOfPlane(pixel, plane);
                size_t rows = plane ? HEIGHT / 2 : HEIGHT;
                size_t stride = CVPixelBufferGetBytesPerRowOfPlane(pixel, plane);
                const uint8_t *origin = source + (plane ? WIDTH * HEIGHT : 0);
                for (size_t row = 0; row < rows; row++) memcpy(destination + row * stride, origin + row * WIDTH, WIDTH);
            }
            check(CVPixelBufferUnlockBaseAddress(pixel, 0), "unlock pixel buffer");
            check(VTCompressionSessionEncodeFrame(session, pixel, CMTimeMake(frame, FPS),
                CMTimeMake(1, FPS), NULL, NULL, NULL), "encode frame");
            CVPixelBufferRelease(pixel);
        }
        check(VTCompressionSessionCompleteFrames(session, kCMTimeInvalid), "complete frames");
        double seconds = (clock_gettime_nsec_np(CLOCK_MONOTONIC_RAW) - start) / 1e9;
        VTCompressionSessionInvalidate(session);
        CFRelease(session);
        int ioStatus = fclose(output);
        pthread_mutex_destroy(&cap.lock);
        NSDictionary *result = @{ @"hardware_required":@YES, @"using_hardware_encoder":@(usingHardware),
            @"width":@(WIDTH), @"height":@(HEIGHT), @"fps":@(FPS), @"input_pixel_format":@"nv12-video-range",
            @"codec":codecName, @"bitrate":@1000000, @"requested_frames":@(frameCount),
            @"encoded_frames":@(cap.frames), @"encode_seconds":@(seconds), @"callback_status":@(cap.error),
            @"system_version":[NSProcessInfo processInfo].operatingSystemVersionString };
        NSData *json = [NSJSONSerialization dataWithJSONObject:result options:NSJSONWritingPrettyPrinted error:NULL];
        if (![json writeToFile:@(argv[3]) options:NSDataWritingWithoutOverwriting error:NULL]) return 1;
        bool passed = !cap.error && !ioStatus && cap.frames == frameCount;
        fprintf(stderr, "NEO_VT_ENCODE_%s frames=%u seconds=%.6f\n", passed ? "PASS" : "FAIL", cap.frames, seconds);
        return passed ? 0 : 1;
    }
}
