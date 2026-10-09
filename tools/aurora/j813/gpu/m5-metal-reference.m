// SPDX-License-Identifier: MIT
// A small macOS-only GPU workload for M5 startup traces. This is reference
// evidence, never proof of Linux GPU support or DCP presentation.
#import <Foundation/Foundation.h>
#import <Metal/Metal.h>
#include <sys/sysctl.h>

static BOOL finish(id<MTLCommandBuffer> command, NSMutableArray *timings)
{
    [command commit];
    [command waitUntilCompleted];
    if (command.status != MTLCommandBufferStatusCompleted) {
        fprintf(stderr, "GPU command failed: %s\n", command.error.description.UTF8String);
        return NO;
    }
    [timings addObject:@((command.GPUEndTime - command.GPUStartTime) * 1000.0)];
    return YES;
}

int main(void)
{
    @autoreleasepool {
        char model[64] = {0};
        size_t length = sizeof(model);
        if (sysctlbyname("hw.model", model, &length, NULL, 0) || strcmp(model, "Mac17,3")) {
            fprintf(stderr, "This reference workload requires M5 J813\n");
            return 2;
        }
        id<MTLDevice> device = MTLCreateSystemDefaultDevice();
        if (!device || ![device.name isEqualToString:@"Apple M5"]) {
            fprintf(stderr, "M5 Metal device is unavailable\n");
            return 3;
        }
        // The opt-in replay experiment compiles +6 but expects the owned
        // Linux program's +5 result after replacing GPU program pages.
        // Defaults preserve the original three-queue reference workload.
        unsigned shaderAdd = 5, expectedAdd = 5;
        const char *shaderOption = getenv("M5_REFERENCE_SHADER_ADD");
        const char *expectedOption = getenv("M5_REFERENCE_EXPECT_ADD");
        if (shaderOption) {
            if (strlen(shaderOption) != 1 || (shaderOption[0] != '5' && shaderOption[0] != '6')) return 12;
            shaderAdd = (unsigned)(shaderOption[0] - '0');
            expectedAdd = shaderAdd;
        }
        if (expectedOption) {
            if (strlen(expectedOption) != 1 || (expectedOption[0] != '5' && expectedOption[0] != '6')) return 12;
            expectedAdd = (unsigned)(expectedOption[0] - '0');
        }
        BOOL triangle = getenv("M5_REFERENCE_TRIANGLE") != NULL;
        BOOL computeOnly = triangle || getenv("M5_REFERENCE_COMPUTE_ONLY") != NULL;
        unsigned rounds = computeOnly ? 1 : 3;
        NSString *source = [NSString stringWithFormat:@"#include <metal_stdlib>\nusing namespace metal;\n"
            "kernel void calculate(device const uint *in [[buffer(0)]], "
            "device uint *out [[buffer(1)]], uint i [[thread_position_in_grid]]) {"
            " out[i] = in[i] * 3u + %uu; }\n"
            "vertex float4 triangle(uint i [[vertex_id]]) {"
            " const float2 p[3] = {float2(-1,-1), float2(3,-1), float2(-1,3)};"
            " return float4(p[i],0,1); }\n"
            "fragment float4 color() { return float4(0.25,0.5,0.75,1.0); }\n", shaderAdd];
        if (triangle) {
            NSString *oldBody = [NSString stringWithFormat:@"out[i] = in[i] * 3u + %uu;", shaderAdd];
            source = [source stringByReplacingOccurrencesOfString:oldBody withString:
                @"uint p=in[i]/7u; int x=int(p&31u), y=int(p>>5);"
                 " bool inside=y>=4 && y<28 && abs(2*x-31)<=y-4;"
                 " out[i]=inside ? (0xffff0000u | (uint(y*8)<<8) | uint(x*8)) : 0xff181818u;"];
        }
        NSError *error = nil;
        id<MTLLibrary> library = [device newLibraryWithSource:source options:nil error:&error];
        if (!library) {
            fprintf(stderr, "Metal shader compilation failed: %s\n", error.description.UTF8String);
            return 4;
        }
        id<MTLComputePipelineState> compute = [device newComputePipelineStateWithFunction:
            [library newFunctionWithName:@"calculate"] error:&error];
        MTLRenderPipelineDescriptor *descriptor = [MTLRenderPipelineDescriptor new];
        descriptor.vertexFunction = [library newFunctionWithName:@"triangle"];
        descriptor.fragmentFunction = [library newFunctionWithName:@"color"];
        descriptor.colorAttachments[0].pixelFormat = MTLPixelFormatRGBA8Unorm;
        id<MTLRenderPipelineState> render = [device newRenderPipelineStateWithDescriptor:descriptor error:&error];
        if (!compute || !render) {
            fprintf(stderr, "Metal pipeline creation failed\n");
            return 5;
        }
        NSMutableArray *timings = [NSMutableArray array];
        for (unsigned round = 0; round < rounds; round++) {
            @autoreleasepool {
                id<MTLCommandQueue> queue = [device newCommandQueue];
                id<MTLBuffer> input = [device newBufferWithLength:4096 options:MTLResourceStorageModeShared];
                id<MTLBuffer> output = [device newBufferWithLength:4096 options:MTLResourceStorageModeShared];
                if (!queue || !input || !output) return 6;
                if (getenv("M5_TRACE_BUFFERS")) {
                    // Private reference metadata for correlating a captured
                    // native GPU payload with this test's actual resources.
                    fprintf(stderr, "M5_TRACE_BUFFERS round=%u input=%#llx output=%#llx bytes=4096\n",
                            round, (unsigned long long)input.gpuAddress,
                            (unsigned long long)output.gpuAddress);
                    fflush(stderr);
                }
                for (unsigned i = 0; i < 1024; i++) {
                    ((uint32_t *)input.contents)[i] = i * 7u + round;
                    ((uint32_t *)output.contents)[i] = 0xdeadbeef;
                }
                id<MTLCommandBuffer> command = [queue commandBuffer];
                id<MTLComputeCommandEncoder> encoder = [command computeCommandEncoder];
                [encoder setComputePipelineState:compute];
                [encoder setBuffer:input offset:0 atIndex:0];
                [encoder setBuffer:output offset:0 atIndex:1];
                [encoder dispatchThreads:MTLSizeMake(1024, 1, 1)
                    threadsPerThreadgroup:MTLSizeMake(MIN(64, compute.maxTotalThreadsPerThreadgroup), 1, 1)];
                [encoder endEncoding];
                if (getenv("M5_REFERENCE_WAIT_STDIN")) {
                    fprintf(stderr,"M5_REFERENCE_READY\n"); fflush(stderr);
                    if (getchar() != '\n') return 13;
                }
                if (!finish(command, timings)) return 7;
                for (unsigned i = 0; i < 1024; i++) {
                    int x=i%32, y=i/32;
                    BOOL covered = y>=4 && y<28 && abs(2*x-31)<=y-4;
                    uint32_t expected = triangle ? (covered ? 0xffff0000u | ((uint32_t)y*8<<8) | (uint32_t)x*8 : 0xff181818u)
                                                : (i*7u+round)*3u+expectedAdd;
                    if (((uint32_t *)output.contents)[i] != expected) return 8;
                    if (((uint32_t *)input.contents)[i] != i*7u+round) return 14;
                }
                if (triangle) {
                    NSData *image = [NSData dataWithBytes:output.contents length:4096];
                    if (![image writeToFile:@"/tmp/m5-compute-triangle-reference.rgba" atomically:YES]) return 15;
                }
                if (computeOnly) continue;

                MTLTextureDescriptor *textureDescriptor = [MTLTextureDescriptor
                    texture2DDescriptorWithPixelFormat:MTLPixelFormatRGBA8Unorm width:64 height:64 mipmapped:NO];
                textureDescriptor.storageMode = MTLStorageModeShared;
                textureDescriptor.usage = MTLTextureUsageRenderTarget;
                id<MTLTexture> texture = [device newTextureWithDescriptor:textureDescriptor];
                if (!texture) return 9;
                MTLRenderPassDescriptor *pass = [MTLRenderPassDescriptor renderPassDescriptor];
                pass.colorAttachments[0].texture = texture;
                pass.colorAttachments[0].loadAction = MTLLoadActionClear;
                pass.colorAttachments[0].storeAction = MTLStoreActionStore;
                pass.colorAttachments[0].clearColor = MTLClearColorMake(0, 0, 0, 0);
                command = [queue commandBuffer];
                id<MTLRenderCommandEncoder> draw = [command renderCommandEncoderWithDescriptor:pass];
                [draw setRenderPipelineState:render];
                [draw drawPrimitives:MTLPrimitiveTypeTriangle vertexStart:0 vertexCount:3];
                [draw endEncoding];
                if (!finish(command, timings)) return 10;
                uint8_t pixels[64 * 64 * 4];
                [texture getBytes:pixels bytesPerRow:256 fromRegion:MTLRegionMake2D(0, 0, 64, 64) mipmapLevel:0];
                const int expected[] = {64, 128, 191, 255};
                for (unsigned i = 0; i < sizeof(pixels); i++)
                    if (abs((int)pixels[i] - expected[i % 4]) > 1) return 11;
            }
        }
        NSDictionary *result = @{@"platform": @"macOS reference", @"gpu": device.name,
            @"queues": @(rounds), @"compute_values_verified": @(1024 * rounds),
            @"shader_add": @(shaderAdd), @"expected_add": @(expectedAdd),
            @"compute_triangle": @(triangle),
            @"render_pixels_verified": @(computeOnly ? 0 : 4096 * rounds), @"command_gpu_ms": timings,
            @"linux_gpu_proven": @NO, @"dcp_presentation_tested": @NO};
        NSData *data = [NSJSONSerialization dataWithJSONObject:result options:NSJSONWritingPrettyPrinted error:nil];
        fwrite(data.bytes, 1, data.length, stdout);
        putchar('\n');
        return 0;
    }
}
