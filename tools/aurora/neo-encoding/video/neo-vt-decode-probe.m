#import <Foundation/Foundation.h>
#import <AVFoundation/AVFoundation.h>
#import <VideoToolbox/VideoToolbox.h>
#import <CoreVideo/CoreVideo.h>
#include <sys/sysctl.h>
#include <math.h>

// Independent decoder for the Apple auxiliary formats that FFmpeg does not
// expose through their deph/dish sample entries. Encoding selection is separate.
int main(int argc, const char **argv) {
    @autoreleasepool {
        if (argc!=4 && argc!=5) return 2;
        BOOL allowHost=argc==5 && strcmp(argv[4],"--allow-host-decoder")==0;
        if (argc==5 && !allowHost) return 2;
        char model[128]={0};size_t size=sizeof(model);
        if (sysctlbyname("hw.model",model,&size,NULL,0) || (!allowHost && strcmp(model,"Mac17,5"))) return 3;
        NSString *system=NSProcessInfo.processInfo.operatingSystemVersionString;
        if (!allowHost && ![system containsString:@"25G76"]) return 3;
        NSString *formatName=@(argv[3]);OSType format=0;size_t bytesPerPixel=4;
        if ([formatName isEqual:@"bgra"]) format=kCVPixelFormatType_32BGRA;
        else if ([formatName isEqual:@"y416"]) {format=kCVPixelFormatType_4444AYpCbCr16;bytesPerPixel=8;}
        else if ([formatName isEqual:@"fdep"]) format=kCVPixelFormatType_DepthFloat32;
        else if ([formatName isEqual:@"fdis"]) format=kCVPixelFormatType_DisparityFloat32;
        else return 2;
        NSString *directory=@(argv[2]);NSFileManager *fm=NSFileManager.defaultManager;
        if ([fm fileExistsAtPath:directory] || ![fm createDirectoryAtPath:directory withIntermediateDirectories:YES attributes:nil error:NULL]) return 2;
        NSError *error=nil;
#pragma clang diagnostic push
#pragma clang diagnostic ignored "-Wdeprecated-declarations"
        AVURLAsset *asset=[AVURLAsset URLAssetWithURL:[NSURL fileURLWithPath:@(argv[1])] options:nil];
        NSArray<AVAssetTrack *> *tracks=[asset tracksWithMediaType:AVMediaTypeVideo];
        AVAssetReader *reader=[[AVAssetReader alloc] initWithAsset:asset error:&error];
#pragma clang diagnostic pop
        AVAssetReaderTrackOutput *output=nil;
        BOOL started=NO,layoutValid=YES;unsigned frames=0;
        NSMutableData *decoded=[NSMutableData data];NSMutableArray *timestamps=[NSMutableArray array];
        @try {
            if (reader && tracks.count==1) {
                output=[[AVAssetReaderTrackOutput alloc] initWithTrack:tracks[0]
                    outputSettings:@{(id)kCVPixelBufferPixelFormatTypeKey:@(format)}];
                output.alwaysCopiesSampleData=NO;
                if ([reader canAddOutput:output]) {[reader addOutput:output];started=[reader startReading];}
            }
            while (started && frames<120) {
                CMSampleBufferRef sample=[output copyNextSampleBuffer];if (!sample) break;
                CVPixelBufferRef pixel=CMSampleBufferGetImageBuffer(sample);
                if (!pixel || CVPixelBufferGetPixelFormatType(pixel)!=format ||
                    CVPixelBufferGetWidth(pixel)!=320 || CVPixelBufferGetHeight(pixel)!=240 ||
                    CVPixelBufferIsPlanar(pixel)) {layoutValid=NO;CFRelease(sample);break;}
                CVReturn lock=CVPixelBufferLockBaseAddress(pixel,kCVPixelBufferLock_ReadOnly);
                size_t stride=CVPixelBufferGetBytesPerRow(pixel);
                if (lock || stride<320*bytesPerPixel) {layoutValid=NO;if(!lock)CVPixelBufferUnlockBaseAddress(pixel,kCVPixelBufferLock_ReadOnly);CFRelease(sample);break;}
                const uint8_t *base=CVPixelBufferGetBaseAddress(pixel);
                for (unsigned y=0;y<240;y++) [decoded appendBytes:base+y*stride length:320*bytesPerPixel];
                [timestamps addObject:@(CMTimeGetSeconds(CMSampleBufferGetPresentationTimeStamp(sample)))];
                CVPixelBufferUnlockBaseAddress(pixel,kCVPixelBufferLock_ReadOnly);CFRelease(sample);frames++;
            }
        } @catch (NSException *exception) {
            error=[NSError errorWithDomain:@"NeoDecode" code:-1 userInfo:@{NSLocalizedDescriptionKey:exception.reason?:exception.name}];
            layoutValid=NO;
        }
        BOOL passed=started && layoutValid && frames>0 && reader.status==AVAssetReaderStatusCompleted;
        NSMutableDictionary *result=[@{@"passed":@(passed),@"model":@(model),@"system_version":system,
            @"format":formatName,@"width":@320,@"height":@240,@"frames":@(frames),
            @"bytes":@(decoded.length),@"reader_status":@(reader.status),@"timestamps":timestamps,
            @"decoder":@"AVAssetReader independent decode",@"hardware_encoding_claim":@NO} mutableCopy];
        result[@"allow_host_decoder"]=@(allowHost);
        NSError *failure=error?:reader.error;if(failure)result[@"error"]=failure.localizedDescription;
        [decoded writeToFile:[directory stringByAppendingPathComponent:@"decoded.raw"] options:NSDataWritingWithoutOverwriting error:NULL];
        NSData *json=[NSJSONSerialization dataWithJSONObject:result options:NSJSONWritingPrettyPrinted error:NULL];
        [json writeToFile:[directory stringByAppendingPathComponent:@"result.json"] options:NSDataWritingWithoutOverwriting error:NULL];
        return passed?0:1;
    }
}
