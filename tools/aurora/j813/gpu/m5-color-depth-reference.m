// SPDX-License-Identifier: MIT
// Native M5 color/depth oracle for bounded HAL300 capture; not Linux proof.
#import <Foundation/Foundation.h>
#import <Metal/Metal.h>
#include <sys/sysctl.h>
#include <math.h>

int main(int argc, const char **argv)
{
    @autoreleasepool {
        if (argc != 3 || (strcmp(argv[1], "color") && strcmp(argv[1], "depth") &&
                          strcmp(argv[1], "flat") && strcmp(argv[1], "flat-depth"))) return 2;
        const BOOL depth = strstr(argv[1], "depth") != NULL;
        const BOOL flat = !strncmp(argv[1], "flat", 4);
        char model[64] = {0}; size_t modelSize = sizeof(model);
        if (sysctlbyname("hw.model", model, &modelSize, NULL, 0) || strcmp(model, "Mac17,3")) return 3;
        id<MTLDevice> device = MTLCreateSystemDefaultDevice();
        if (![device.name isEqualToString:@"Apple M5"]) return 4;
        NSString *source = @"#include <metal_stdlib>\nusing namespace metal;\n"
            "struct V { float4 p [[position]]; float4 c; float size [[point_size]]; };\n"
            "vertex V vertexColor(uint i [[vertex_id]], device const float4 *v [[buffer(0)]]) {"
            " return {v[2*i],v[2*i+1],1.0f}; }\n"
            "fragment float4 fragmentColor(V v [[stage_in]]) { return v.c; }\n";
        if (flat) source = [source stringByReplacingOccurrencesOfString:@"float4 c;"
                                                            withString:@"float4 c [[flat]];"];
        NSError *error = nil;
        id<MTLLibrary> library = [device newLibraryWithSource:source options:nil error:&error];
        if (!library) { fprintf(stderr,"shader: %s\n",error.description.UTF8String); return 5; }
        MTLRenderPipelineDescriptor *pd = [MTLRenderPipelineDescriptor new];
        pd.vertexFunction = [library newFunctionWithName:@"vertexColor"];
        pd.fragmentFunction = [library newFunctionWithName:@"fragmentColor"];
        pd.colorAttachments[0].pixelFormat = MTLPixelFormatRGBA8Unorm;
        if (depth) pd.depthAttachmentPixelFormat = MTLPixelFormatDepth32Float;
        id<MTLRenderPipelineState> pipeline = [device newRenderPipelineStateWithDescriptor:pd error:&error];
        if (!pipeline) { fprintf(stderr,"pipeline: %s\n",error.description.UTF8String); return 6; }

        // Near RGB triangle followed, in depth mode, by the same far white
        // triangle. Correct LESS/write/clear state leaves the RGB image intact.
        const float vertices[][4] = {
            {-.75f,-.75f,.25f,1},{1,0,0,1},
            { .75f,-.75f,.25f,1},{0,1,0,1},
            {    0, .75f,.25f,1},{0,0,1,1},
            {-.75f,-.75f,.75f,1},{1,1,1,1},
            { .75f,-.75f,.75f,1},{1,1,1,1},
            {    0, .75f,.75f,1},{1,1,1,1},
        };
        id<MTLBuffer> vertex = [device newBufferWithBytes:vertices length:sizeof(vertices)
                                                 options:MTLResourceStorageModeShared];
        const unsigned width=256, height=256, pitch=width*4;
        id<MTLBuffer> pixels = [device newBufferWithLength:pitch*height options:MTLResourceStorageModeShared];
        if (!vertex || !pixels) return 7;
        memset(pixels.contents,0xa5,pixels.length);
        MTLTextureDescriptor *td = [MTLTextureDescriptor texture2DDescriptorWithPixelFormat:
            MTLPixelFormatRGBA8Unorm width:width height:height mipmapped:NO];
        td.storageMode=MTLStorageModeShared; td.usage=MTLTextureUsageRenderTarget;
        id<MTLTexture> color = [pixels newTextureWithDescriptor:td offset:0 bytesPerRow:pitch];
        id<MTLTexture> z = nil;
        id<MTLDepthStencilState> zs = nil;
        if (depth) {
            MTLTextureDescriptor *zd = [MTLTextureDescriptor texture2DDescriptorWithPixelFormat:
                MTLPixelFormatDepth32Float width:width height:height mipmapped:NO];
            zd.storageMode=MTLStorageModePrivate; zd.usage=MTLTextureUsageRenderTarget;
            z=[device newTextureWithDescriptor:zd];
            MTLDepthStencilDescriptor *ds=[MTLDepthStencilDescriptor new];
            ds.depthCompareFunction=MTLCompareFunctionLess; ds.depthWriteEnabled=YES;
            zs=[device newDepthStencilStateWithDescriptor:ds];
            if (!z || !zs) return 8;
        }
        if (!color) return 9;
        id<MTLCommandQueue> queue=[device newCommandQueue];
        queue.label=@"M5 color/depth reference";
        id<MTLCommandBuffer> command=[queue commandBuffer];
        MTLRenderPassDescriptor *pass=[MTLRenderPassDescriptor renderPassDescriptor];
        pass.colorAttachments[0].texture=color;
        pass.colorAttachments[0].loadAction=MTLLoadActionClear;
        pass.colorAttachments[0].storeAction=MTLStoreActionStore;
        pass.colorAttachments[0].clearColor=MTLClearColorMake(.0625,.0625,.0625,1);
        if (depth) {
            pass.depthAttachment.texture=z; pass.depthAttachment.clearDepth=1;
            pass.depthAttachment.loadAction=MTLLoadActionClear;
            pass.depthAttachment.storeAction=MTLStoreActionStore;
        }
        id<MTLRenderCommandEncoder> encoder=[command renderCommandEncoderWithDescriptor:pass];
        [encoder setRenderPipelineState:pipeline];
        [encoder setVertexBuffer:vertex offset:0 atIndex:0];
        if (depth) [encoder setDepthStencilState:zs];
        [encoder drawPrimitives:MTLPrimitiveTypeTriangle vertexStart:0 vertexCount:3];
        if (depth) [encoder drawPrimitives:MTLPrimitiveTypeTriangle vertexStart:3 vertexCount:3];
        [encoder endEncoding];
        fprintf(stderr,"M5_COLOR_DEPTH_READY mode=%s pixels_gpu=%#llx vertex_gpu=%#llx bytes=%lu\n",
                argv[1],pixels.gpuAddress,vertex.gpuAddress,(unsigned long)pixels.length);
        fflush(stderr);
        if (getenv("M5_REFERENCE_WAIT_STDIN") && getchar()!='\n') return 10;
        [command commit]; [command waitUntilCompleted];
        if (command.status!=MTLCommandBufferStatusCompleted) {
            fprintf(stderr,"GPU: %s\n",command.error.description.UTF8String); return 11;
        }
        const uint8_t *data=pixels.contents;
        unsigned checked=0, wrong=0, interior=0;
        for (unsigned y=0;y<height;y++) for (unsigned x=0;x<width;x++) {
            double blue=(224.0-(y+.5))/192.0;
            double red=(1-blue-((x+.5)-128.0)/96.0)*.5;
            double green=1-blue-red;
            BOOL inside=red>.02 && green>.02 && blue>.02;
            BOOL outside=red<-.02 || green<-.02 || blue<-.02;
            if (!inside && !outside) continue;
            int expected[4]={16,16,16,255};
            if (inside) {
                expected[0]=lround(red*255); expected[1]=lround(green*255);
                expected[2]=lround(blue*255); interior++;
                if (flat) { expected[0]=255; expected[1]=0; expected[2]=0; }
            }
            BOOL ok=YES;
            for (unsigned c=0;c<4;c++) ok &= abs(data[y*pitch+x*4+c]-expected[c])<=2;
            checked++; wrong+=!ok;
        }
        NSString *path=[NSString stringWithUTF8String:argv[2]];
        if (![[NSData dataWithBytes:data length:pixels.length] writeToFile:path atomically:YES]) return 12;
        NSDictionary *result=@{@"mode":@(argv[1]),@"width":@(width),@"height":@(height),
            @"checked_pixels":@(checked),@"interior_pixels":@(interior),@"wrong_pixels":@(wrong),
            @"gpu_ms":@((command.GPUEndTime-command.GPUStartTime)*1000),
            @"linux_gpu_proven":@NO,@"dcp_presentation_tested":@NO};
        NSData *json=[NSJSONSerialization dataWithJSONObject:result options:NSJSONWritingPrettyPrinted error:nil];
        fwrite(json.bytes,1,json.length,stdout);putchar('\n');fflush(stdout);
        if (wrong || interior<15000 || checked<55000) return 13;
        if (getenv("M5_REFERENCE_HOLD_AFTER")) {
            NSArray *retained=@[device,library,pipeline,vertex,pixels,color,queue,command,pass,encoder];
            fprintf(stderr,"M5_COLOR_DEPTH_COMPLETED_RETAINED\n");fflush(stderr);
            if (getchar()!='\n' || retained.count!=10) return 14;
        }
        return 0;
    }
}
