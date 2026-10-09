// SPDX-License-Identifier: MIT
// Native M5 graphics oracle. A macOS result is never Linux rendering proof.
#import <Foundation/Foundation.h>
#import <Metal/Metal.h>
#include <sys/sysctl.h>

int main(int argc, const char **argv)
{
    @autoreleasepool {
        if (argc != 2) return 2;
        char model[64] = {0}; size_t size = sizeof(model);
        if (sysctlbyname("hw.model", model, &size, NULL, 0) || strcmp(model, "Mac17,3")) return 3;
        id<MTLDevice> device = MTLCreateSystemDefaultDevice();
        if (![device.name isEqualToString:@"Apple M5"]) return 4;
        NSError *error = nil;
        NSString *source = @"#include <metal_stdlib>\nusing namespace metal;\n"
            "vertex float4 triangle(uint i [[vertex_id]]) {"
            " const float2 p[3]={float2(-0.75,-0.75),float2(0.75,-0.75),float2(0,0.75)};"
            " return float4(p[i],0,1); }\n"
            "fragment float4 color() { return float4(0.25,0.5,0.75,1); }\n";
        id<MTLLibrary> library = [device newLibraryWithSource:source options:nil error:&error];
        if (!library) { fprintf(stderr, "shader: %s\n", error.description.UTF8String); return 5; }
        MTLRenderPipelineDescriptor *pipelineDescriptor = [MTLRenderPipelineDescriptor new];
        pipelineDescriptor.vertexFunction = [library newFunctionWithName:@"triangle"];
        pipelineDescriptor.fragmentFunction = [library newFunctionWithName:@"color"];
        pipelineDescriptor.colorAttachments[0].pixelFormat = MTLPixelFormatRGBA8Unorm;
        id<MTLRenderPipelineState> pipeline = [device newRenderPipelineStateWithDescriptor:pipelineDescriptor error:&error];
        if (!pipeline) { fprintf(stderr, "pipeline: %s\n", error.description.UTF8String); return 6; }
        const unsigned width = 256, height = 256, pitch = width * 4;
        id<MTLBuffer> pixels = [device newBufferWithLength:pitch * height options:MTLResourceStorageModeShared];
        memset(pixels.contents, 0xa5, pixels.length);
        MTLTextureDescriptor *td = [MTLTextureDescriptor texture2DDescriptorWithPixelFormat:
            MTLPixelFormatRGBA8Unorm width:width height:height mipmapped:NO];
        td.storageMode = MTLStorageModeShared;
        td.usage = MTLTextureUsageRenderTarget;
        id<MTLTexture> texture = [pixels newTextureWithDescriptor:td offset:0 bytesPerRow:pitch];
        if (!texture) return 7;
        id<MTLCommandQueue> queue = [device newCommandQueue];
        queue.label = @"M5 triangle reference";
        id<MTLCommandBuffer> command = [queue commandBuffer];
        MTLRenderPassDescriptor *pass = [MTLRenderPassDescriptor renderPassDescriptor];
        pass.colorAttachments[0].texture = texture;
        pass.colorAttachments[0].loadAction = MTLLoadActionClear;
        pass.colorAttachments[0].storeAction = MTLStoreActionStore;
        pass.colorAttachments[0].clearColor = MTLClearColorMake(0.0625,0.0625,0.0625,1);
        id<MTLRenderCommandEncoder> encoder = [command renderCommandEncoderWithDescriptor:pass];
        [encoder setRenderPipelineState:pipeline];
        [encoder drawPrimitives:MTLPrimitiveTypeTriangle vertexStart:0 vertexCount:3];
        [encoder endEncoding];
        fprintf(stderr, "M5_TRIANGLE_READY pixels_gpu=0x%llx bytes=%lu\n",
            pixels.gpuAddress, (unsigned long)pixels.length);
        fflush(stderr);
        // Establish SSH and compile the workload before arming expensive
        // WindowServer-filtered capture points in the single-CPU guest.
        if (getenv("M5_TRIANGLE_WAIT_STDIN") && getchar() != '\n') return 12;
        [command commit];
        [command waitUntilCompleted];
        if (command.status != MTLCommandBufferStatusCompleted) {
            fprintf(stderr, "GPU: %s\n", command.error.description.UTF8String); return 8;
        }
        // Check every pixel well away from the three rasterization edges.
        // Edge coverage is checked separately without imposing a top-left rule.
        uint8_t *bytes = pixels.contents;
        unsigned verified = 0, foreground = 0, background = 0, unexpected = 0;
        const int fg[4] = {64,128,191,255}, bg[4] = {16,16,16,255};
        for (unsigned y=0; y<height; y++) for (unsigned x=0; x<width; x++) {
            uint8_t *p = bytes + y*pitch + x*4;
            BOOL isFg=YES, isBg=YES;
            for (unsigned c=0;c<4;c++) { isFg &= abs(p[c]-fg[c])<=1; isBg &= abs(p[c]-bg[c])<=1; }
            foreground += isFg; background += isBg; unexpected += !isFg && !isBg;
            double px=x+0.5, py=y+0.5;
            double left=128-(py-32)/2, right=128+(py-32)/2;
            BOOL inside=py>34 && py<222 && px>left+2 && px<right-2;
            BOOL outside=py<30 || py>226 || px<left-2 || px>right+2;
            if ((inside && !isFg) || (outside && !isBg)) return 9;
            verified += inside || outside;
        }
        if (unexpected || foreground < 18000 || foreground > 19000) return 10;
        NSString *path = [NSString stringWithUTF8String:argv[1]];
        if (![[NSData dataWithBytes:bytes length:pixels.length] writeToFile:path atomically:YES]) return 11;
        NSDictionary *result = @{@"platform":@"macOS reference", @"gpu":device.name,
            @"width":@(width), @"height":@(height), @"format":@"RGBA8",
            @"pixels_checked_away_from_edges":@(verified), @"foreground_pixels":@(foreground),
            @"background_pixels":@(background), @"unexpected_pixels":@(unexpected),
            @"gpu_ms":@((command.GPUEndTime-command.GPUStartTime)*1000),
            @"linux_gpu_proven":@NO, @"dcp_presentation_tested":@NO};
        NSData *json=[NSJSONSerialization dataWithJSONObject:result options:NSJSONWritingPrettyPrinted error:nil];
        fwrite(json.bytes,1,json.length,stdout); putchar('\n');
        if (getenv("M5_TRIANGLE_HOLD_AFTER")) {
            NSArray *retained=@[device,library,pipeline,pixels,texture,queue,command,pass,encoder];
            fprintf(stderr,"M5_TRIANGLE_COMPLETED_RETAINED\n"); fflush(stderr); fflush(stdout);
            if (getchar()!='\n') return 13;
            if (retained.count!=9) return 14;
        }
        return 0;
    }
}
