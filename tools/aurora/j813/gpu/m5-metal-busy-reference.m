// SPDX-License-Identifier: MIT
// Bounded native compute activity for observing M5 pipe state under m1n1.
// Successful output is macOS evidence only.
#import <Foundation/Foundation.h>
#import <Metal/Metal.h>
#include <sys/sysctl.h>
#include <unistd.h>

static uint32_t step(uint32_t x)
{
    x ^= x << 13;
    x ^= x >> 17;
    return x ^ (x << 5);
}

int main(void)
{
    @autoreleasepool {
        char model[64] = {0};
        size_t size = sizeof(model);
        if (sysctlbyname("hw.model", model, &size, NULL, 0) || strcmp(model, "Mac17,3")) return 2;
        unsigned iterations = 65536;
        if (getenv("M5_BUSY_ITERATIONS")) {
            char *end = NULL;
            unsigned long requested = strtoul(getenv("M5_BUSY_ITERATIONS"), &end, 10);
            if (!end || *end || !requested || requested > 1048576) return 3;
            iterations = (unsigned)requested;
        }
        id<MTLDevice> device = MTLCreateSystemDefaultDevice();
        if (!device || ![device.name isEqualToString:@"Apple M5"]) return 4;
        NSString *source = @"#include <metal_stdlib>\nusing namespace metal;\n"
            "kernel void busy(device uint *out [[buffer(0)]], constant uint &n [[buffer(1)]],"
            " uint i [[thread_position_in_grid]]) { uint x = (i & 15u) + 1u;"
            " for (uint j = 0; j < n; ++j) { x ^= x << 13; x ^= x >> 17; x ^= x << 5; }"
            " out[i] = x; }\n";
        NSError *error = nil;
        id<MTLLibrary> library = [device newLibraryWithSource:source options:nil error:&error];
        id<MTLComputePipelineState> pipeline = library ? [device newComputePipelineStateWithFunction:
            [library newFunctionWithName:@"busy"] error:&error] : nil;
        if (!pipeline) {
            fprintf(stderr, "Native busy shader compilation failed: %s\n", error.description.UTF8String);
            return 5;
        }
        id<MTLCommandQueue> queue = [device newCommandQueue];
        if (!queue) return 6;
        NSMutableArray *commands = [NSMutableArray array], *buffers = [NSMutableArray array];
        for (unsigned round = 0; round < 2; ++round) {
            id<MTLBuffer> output = [device newBufferWithLength:65536 * sizeof(uint32_t)
                options:MTLResourceStorageModeShared];
            id<MTLCommandBuffer> command = [queue commandBuffer];
            if (!output || !command) return 7;
            memset(output.contents, 0xef, output.length);
            id<MTLComputeCommandEncoder> encoder = [command computeCommandEncoder];
            [encoder setComputePipelineState:pipeline];
            [encoder setBuffer:output offset:0 atIndex:0];
            [encoder setBytes:&iterations length:sizeof(iterations) atIndex:1];
            [encoder dispatchThreads:MTLSizeMake(65536, 1, 1)
                threadsPerThreadgroup:MTLSizeMake(MIN(128, pipeline.maxTotalThreadsPerThreadgroup), 1, 1)];
            [encoder endEncoding];
            [commands addObject:command];
            [buffers addObject:output];
        }
        [commands[0] commit];
        usleep(2000);
        [commands[1] commit];
        uint32_t expected[16];
        for (unsigned i = 0; i < 16; ++i) {
            uint32_t x = i + 1;
            for (unsigned j = 0; j < iterations; ++j) x = step(x);
            expected[i] = x;
        }
        NSMutableArray *timings = [NSMutableArray array];
        for (unsigned round = 0; round < 2; ++round) {
            id<MTLCommandBuffer> command = commands[round];
            [command waitUntilCompleted];
            if (command.status != MTLCommandBufferStatusCompleted) {
                fprintf(stderr, "Native busy command failed: %s\n", command.error.description.UTF8String);
                return 8;
            }
            id<MTLBuffer> output = buffers[round];
            for (unsigned i = 0; i < 65536; ++i)
                if (((uint32_t *)output.contents)[i] != expected[i & 15]) return 9;
            [timings addObject:@((command.GPUEndTime - command.GPUStartTime) * 1000.0)];
        }
        NSDictionary *result = @{@"platform": @"macOS reference", @"gpu": device.name,
            @"queues": @1, @"commands": @2, @"iterations": @(iterations),
            @"compute_values_verified": @131072, @"command_gpu_ms": timings,
            @"linux_gpu_proven": @NO};
        NSData *data = [NSJSONSerialization dataWithJSONObject:result options:NSJSONWritingPrettyPrinted error:nil];
        fwrite(data.bytes, 1, data.length, stdout);
        putchar('\n');
        return 0;
    }
}
