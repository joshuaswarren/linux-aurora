// SPDX-License-Identifier: MIT
// Target-only hardware encoder enumeration and bounded codec experiments.
#import <Foundation/Foundation.h>
#import <VideoToolbox/VideoToolbox.h>
#import <AVFoundation/AVFoundation.h>
#include <sys/sysctl.h>
#include <pthread.h>
#include <unistd.h>

enum { WIDTH=320, HEIGHT=240, FPS=30 };
#include "coverage-pixels.h"
static AVAssetWriter *movieWriter;
static AVAssetWriterInput *movieInput;
typedef struct {
    CFStringRef directory;
    pthread_mutex_t lock;
    unsigned frames;
    OSStatus error;
} Capture;

static NSString *fourcc(uint32_t value) {
    char text[5] = {(char)(value >> 24), (char)(value >> 16), (char)(value >> 8), (char)value, 0};
    return [NSString stringWithCString:text encoding:NSISOLatin1StringEncoding];
}

static void callback(void *ref, void *source, OSStatus status,
                     VTEncodeInfoFlags flags, CMSampleBufferRef sample) {
    (void)source;
    Capture *cap=ref;
    pthread_mutex_lock(&cap->lock);
    if (status || !sample || (flags & kVTEncodeInfo_FrameDropped)) {
        cap->error=status ?: -1;
    } else {
        CMBlockBufferRef block=CMSampleBufferGetDataBuffer(sample);
        size_t count=block ? CMBlockBufferGetDataLength(block) : 0;
        if (!count) {cap->error=-3;pthread_mutex_unlock(&cap->lock);return;}
        NSMutableData *data=[NSMutableData dataWithLength:count];
        status=CMBlockBufferCopyDataBytes(block,0,count,data.mutableBytes);
        NSString *path=[(__bridge NSString *)cap->directory stringByAppendingPathComponent:
                        [NSString stringWithFormat:@"frame-%03u.sample",cap->frames]];
        if (!status && [data writeToFile:path options:NSDataWritingWithoutOverwriting error:NULL]) cap->frames++;
        else cap->error=status ?: -2;
        if (!movieWriter) {
            NSURL *url=[NSURL fileURLWithPath:[(__bridge NSString *)cap->directory stringByAppendingPathComponent:@"video.mov"]];
            movieWriter=[[AVAssetWriter alloc] initWithURL:url fileType:AVFileTypeQuickTimeMovie error:NULL];
            movieInput=[[AVAssetWriterInput alloc] initWithMediaType:AVMediaTypeVideo outputSettings:nil sourceFormatHint:CMSampleBufferGetFormatDescription(sample)];
            if (![movieWriter canAddInput:movieInput]) cap->error=-5;
            else { [movieWriter addInput:movieInput]; if (![movieWriter startWriting]) cap->error=-6;
                else [movieWriter startSessionAtSourceTime:kCMTimeZero]; }
        }
        if (!cap->error) {
            for (unsigned n=0;!movieInput.readyForMoreMediaData && n<1000;n++) usleep(1000);
            if (!movieInput.readyForMoreMediaData || ![movieInput appendSampleBuffer:sample]) cap->error=-7;
        }
    }
    pthread_mutex_unlock(&cap->lock);
}

int main(int argc, const char **argv) {
    alarm(120); // Bounded, including synchronous KIS reference snapshots.
    @autoreleasepool {
        char model[64]={0}; size_t modelSize=sizeof(model);
        if (sysctlbyname("hw.model",model,&modelSize,NULL,0) || strcmp(model,"Mac17,3")) {
            fprintf(stderr,"Refusing to identify another Mac's encoder as M5 hardware\n"); return 2;
        }
        if (argc < 3) { fprintf(stderr,"Usage: probe list OUTPUT.json | probe encode FOURCC OUTPUT_DIR FRAMES [PIXEL_FORMAT] [HARDWARE_ENCODER_ID] [CONFIG.json]\n");return 2; }
        if (!strcmp(argv[1],"list")) {
            CFArrayRef list=NULL;
            OSStatus status=VTCopyVideoEncoderList(NULL,&list);
            NSMutableArray *encoders=[NSMutableArray array];
            for (NSDictionary *entry in (__bridge NSArray *)list) {
                id codec=entry[(__bridge NSString *)kVTVideoEncoderList_CodecType];
                NSMutableDictionary *row=[NSMutableDictionary dictionaryWithDictionary:entry];
                if (codec) row[@"fourcc"]=fourcc([codec unsignedIntValue]);
                [encoders addObject:row];
            }
            NSDictionary *report=@{@"model":@(model),@"system_version":[NSProcessInfo processInfo].operatingSystemVersionString,
                @"status":@(status),@"encoders":encoders};
            NSData *data=[NSJSONSerialization dataWithJSONObject:report options:NSJSONWritingPrettyPrinted error:NULL];
            bool saved=[data writeToFile:@(argv[2]) options:NSDataWritingWithoutOverwriting error:NULL];
            if (list) CFRelease(list);
            printf("M5_CODEC_LIST status=%d encoders=%lu\n",(int)status,(unsigned long)encoders.count);
            return status || !saved;
        }
        if (strcmp(argv[1],"encode") || argc < 5 || argc > 8 || strlen(argv[2]) != 4) return 2;
        const unsigned char *tag=(const unsigned char *)argv[2];
        CMVideoCodecType codec=((uint32_t)tag[0]<<24)|((uint32_t)tag[1]<<16)|((uint32_t)tag[2]<<8)|tag[3];
        unsigned requested=argc>=5 ? (unsigned)strtoul(argv[4],NULL,10) : 3;
        if (!requested || requested>60) return 2;
        NSString *pixelName=argc>=6?@(argv[5]):@"nv12";
        OSType format=coverageFormat(pixelName);
        if (!format) return 2;
        NSDictionary *config=@{};
        if (argc==8) {
            NSData *data=[NSData dataWithContentsOfFile:@(argv[7])];
            config=data?[NSJSONSerialization JSONObjectWithData:data options:0 error:NULL]:nil;
            if (![config isKindOfClass:NSDictionary.class]) return 2;
            NSSet *allowed=[NSSet setWithArray:@[@"profile",@"output_bit_depth",@"preserve_alpha",@"pattern",@"multiview",@"capture_gate"]];
            for (NSString *key in config) if (![allowed containsObject:key]) return 2;
        }
        if (([pixelName isEqual:@"fdep"] && codec!='deph') || ([pixelName isEqual:@"fdis"] && codec!='dish')) return 2;
        if (config[@"output_bit_depth"] && ![@[@8,@10,@12] containsObject:config[@"output_bit_depth"]]) return 2;
        if (config[@"profile"] && ![config[@"profile"] isKindOfClass:NSString.class]) return 2;
        if (config[@"pattern"] && ![@[@"bars",@"gradient"] containsObject:config[@"pattern"]]) return 2;
        if ((config[@"preserve_alpha"] && ![@[@YES,@NO] containsObject:config[@"preserve_alpha"]]) ||
            (config[@"multiview"] && ![@[@YES,@NO] containsObject:config[@"multiview"]])) return 2;
        BOOL multiview=[config[@"multiview"] boolValue];
        if (config[@"capture_gate"] && ![@[@YES,@NO] containsObject:config[@"capture_gate"]]) return 2;
        BOOL captureGate=[config[@"capture_gate"] boolValue];
        if (captureGate && (requested!=2 || ![@[@"apcn",@"apch",@"apcs",@"apco",@"ap4h",@"ap4x"] containsObject:fourcc(codec)])) return 2;
        if (multiview && (codec!='hvc1' || ![pixelName isEqual:@"nv12"])) return 2;
        NSString *encoderID=argc>=7 ? @(argv[6]) : nil;
        bool selectedHardware=false;
        if (encoderID) {
            CFArrayRef list=NULL;
            OSStatus status=VTCopyVideoEncoderList(NULL,&list);
            if (!status) for (NSDictionary *entry in (__bridge NSArray *)list) {
                if ([entry[(__bridge NSString *)kVTVideoEncoderList_EncoderID] isEqual:encoderID] &&
                    [entry[(__bridge NSString *)kVTVideoEncoderList_CodecType] unsignedIntValue]==codec &&
                    [entry[(__bridge NSString *)kVTVideoEncoderList_IsHardwareAccelerated] isEqual:@YES]) selectedHardware=true;
            }
            if (list) CFRelease(list);
            if (!selectedHardware) {fprintf(stderr,"Encoder ID is not advertised as hardware for this codec\n");return 2;}
        }
        NSString *directory=@(argv[3]);
        if ([[NSFileManager defaultManager] fileExistsAtPath:directory] ||
            ![[NSFileManager defaultManager] createDirectoryAtPath:directory withIntermediateDirectories:YES attributes:nil error:NULL]) return 2;
        Capture cap={.directory=(__bridge CFStringRef)directory,.lock=PTHREAD_MUTEX_INITIALIZER};
        NSMutableDictionary *spec=[@{(__bridge NSString *)kVTVideoEncoderSpecification_RequireHardwareAcceleratedVideoEncoder:@YES} mutableCopy];
        if (encoderID) spec[(__bridge NSString *)kVTVideoEncoderSpecification_EncoderID]=encoderID;
        NSDictionary *pixels=@{(__bridge NSString *)kCVPixelBufferPixelFormatTypeKey:@(format),
            (__bridge NSString *)kCVPixelBufferWidthKey:@(WIDTH),(__bridge NSString *)kCVPixelBufferHeightKey:@(HEIGHT),
            (__bridge NSString *)kCVPixelBufferIOSurfacePropertiesKey:@{}};
        VTCompressionSessionRef session=NULL;
        OSStatus create=VTCompressionSessionCreate(NULL,WIDTH,HEIGHT,codec,(__bridge CFDictionaryRef)spec,
            (__bridge CFDictionaryRef)pixels,NULL,callback,&cap,&session);
        OSStatus prepare=create, query=create, encode=create, complete=create;
        bool hardware=false;
        NSMutableArray *propertyResults=[NSMutableArray array];
        if (!create) {
            NSMutableDictionary *properties=[@{(__bridge NSString *)kVTCompressionPropertyKey_RealTime:@NO,
                                      (__bridge NSString *)kVTCompressionPropertyKey_ExpectedFrameRate:@(FPS)} mutableCopy];
            if (config[@"profile"]) properties[(__bridge NSString *)kVTCompressionPropertyKey_ProfileLevel]=config[@"profile"];
            if (config[@"output_bit_depth"]) properties[(__bridge NSString *)kVTCompressionPropertyKey_OutputBitDepth]=config[@"output_bit_depth"];
            if (config[@"preserve_alpha"]) {
                // The pinned Neo muxa encoder advertises EncodesAlpha, while
                // PreserveAlphaChannel explicitly returned -12900. Select the
                // advertised control and verify its value before submitting.
                NSString *alphaKey=codec=='muxa'?@"EncodesAlpha":(__bridge NSString *)kVTCompressionPropertyKey_PreserveAlphaChannel;
                properties[alphaKey]=config[@"preserve_alpha"];
                properties[(__bridge NSString *)kVTCompressionPropertyKey_AlphaChannelMode]=(__bridge NSString *)kVTAlphaChannelMode_StraightAlpha;
            }
            if (multiview) {
                properties[(__bridge NSString *)kVTCompressionPropertyKey_MVHEVCVideoLayerIDs]=@[@0,@1];
                properties[(__bridge NSString *)kVTCompressionPropertyKey_MVHEVCViewIDs]=@[@0,@1];
                properties[(__bridge NSString *)kVTCompressionPropertyKey_MVHEVCLeftAndRightViewIDs]=@[@0,@1];
            }
            CFDictionaryRef supported=NULL;
            if (!VTSessionCopySupportedPropertyDictionary(session,&supported) && supported) {
                NSData *plist=[NSPropertyListSerialization dataWithPropertyList:(__bridge NSDictionary *)supported format:NSPropertyListXMLFormat_v1_0 options:0 error:NULL];
                [plist writeToFile:[directory stringByAppendingPathComponent:@"supported-properties.plist"] options:NSDataWritingWithoutOverwriting error:NULL];
                CFRelease(supported);
            }
            OSStatus requiredStatus=0;
            for (NSString *key in properties) {
                OSStatus s=VTSessionSetProperty(session,(__bridge CFStringRef)key,(__bridge CFTypeRef)properties[key]);
                [propertyResults addObject:@{@"property":key,@"requested":properties[key],@"status":@(s)}];
                if (s && ![key isEqual:(__bridge NSString *)kVTCompressionPropertyKey_RealTime] &&
                    ![key isEqual:(__bridge NSString *)kVTCompressionPropertyKey_ExpectedFrameRate]) requiredStatus=s;
            }
            prepare=requiredStatus?:VTCompressionSessionPrepareToEncodeFrames(session);
            if (!prepare && codec=='muxa' && config[@"preserve_alpha"]) {
                CFTypeRef alpha=NULL;
                OSStatus alphaStatus=VTSessionCopyProperty(session,CFSTR("EncodesAlpha"),NULL,&alpha);
                BOOL alphaMatches=!alphaStatus && alpha && CFEqual(alpha,(__bridge CFTypeRef)config[@"preserve_alpha"]);
                [propertyResults addObject:@{@"property":@"EncodesAlpha readback",@"status":@(alphaStatus),@"matches_requested":@(alphaMatches)}];
                if (alpha) CFRelease(alpha);
                if (!alphaMatches) prepare=alphaStatus?:-5;
            }
            CFTypeRef value=NULL;
            query=VTSessionCopyProperty(session,kVTCompressionPropertyKey_UsingHardwareAcceleratedVideoEncoder,NULL,&value);
            hardware=!query && value && CFEqual(value,kCFBooleanTrue);
            if (value) CFRelease(value);
        }
        NSMutableData *reference=[NSMutableData data];
        NSMutableData *rightReference=[NSMutableData data];
        // JPEG does not implement the status query. An explicitly selected ID
        // advertised as hardware, combined with RequireHardware, is a separate
        // selection guarantee. Preserve the missing property in the report.
        bool hardwareSelectionAccepted=hardware || (selectedHardware && query==kVTPropertyNotSupportedErr);
        if (!create && !prepare && hardwareSelectionAccepted) {
            encode=0;
            for (unsigned frame=0;frame<requested && !encode;frame++) {
                CVPixelBufferRef pixel=NULL;
                encode=CVPixelBufferPoolCreatePixelBuffer(NULL,VTCompressionSessionGetPixelBufferPool(session),&pixel);
                if (encode) break;
                if (CVPixelBufferGetWidth(pixel)!=WIDTH || CVPixelBufferGetHeight(pixel)!=HEIGHT ||
                    CVPixelBufferGetPixelFormatType(pixel)!=format) {
                    CVPixelBufferRelease(pixel);encode=-4;break;
                }
                NSData *input=coveragePixels(pixel,pixelName,captureGate?0:frame,[config[@"pattern"] isEqual:@"gradient"]);
                if (input) {
                    [reference appendData:input];
                    if (!multiview) encode=VTCompressionSessionEncodeFrame(session,pixel,CMTimeMake(frame,FPS),CMTimeMake(1,FPS),NULL,NULL,NULL);
                    else {
                        CVPixelBufferRef right=NULL;CMTagCollectionRef tags[2]={NULL,NULL};CMTaggedBufferGroupRef group=NULL;
                        encode=CVPixelBufferPoolCreatePixelBuffer(NULL,VTCompressionSessionGetPixelBufferPool(session),&right);
                        NSData *rightInput=!encode?coveragePixels(right,pixelName,frame+11,YES):nil;
                        if (!encode && !rightInput) encode=-10;
                        if (rightInput) [rightReference appendData:rightInput];
                        for (unsigned eye=0;!encode && eye<2;eye++) {
                            CMTag values[]={CMTagMakeWithSInt64Value(kCMTagCategory_VideoLayerID,eye),
                                CMTagMakeWithFlagsValue(kCMTagCategory_StereoView,eye?kCMStereoView_RightEye:kCMStereoView_LeftEye)};
                            encode=CMTagCollectionCreate(NULL,values,2,&tags[eye]);
                        }
                        if (!encode) encode=CMTaggedBufferGroupCreate(NULL,(__bridge CFArrayRef)@[(__bridge id)tags[0],(__bridge id)tags[1]],
                            (__bridge CFArrayRef)@[(__bridge id)pixel,(__bridge id)right],&group);
                        if (!encode) encode=VTCompressionSessionEncodeMultiImageFrame(session,group,CMTimeMake(frame,FPS),CMTimeMake(1,FPS),NULL,NULL,NULL);
                        if (group) CFRelease(group);for (unsigned eye=0;eye<2;eye++) if (tags[eye]) CFRelease(tags[eye]);
                        if (right) CFRelease(right);
                    }
                } else encode=-8;
                CVPixelBufferRelease(pixel);
                if (!encode && captureGate && frame==0) {
                    encode=VTCompressionSessionCompleteFrames(session,kCMTimeInvalid);
                    if (!encode) {
                        NSString *ready=[directory stringByAppendingPathComponent:@"capture-ready"];
                        NSString *resume=[directory stringByAppendingPathComponent:@"capture-continue"];
                        if (![@"first frame drained\n" writeToFile:ready atomically:YES encoding:NSUTF8StringEncoding error:NULL]) encode=-11;
                        unsigned wait;
                        for (wait=0;!encode && wait<900 && ![[NSFileManager defaultManager] fileExistsAtPath:resume];wait++) usleep(100000);
                        if (wait==900) encode=-12;
                    }
                }
            }
            complete=VTCompressionSessionCompleteFrames(session,kCMTimeInvalid);
        }
        if (session) {VTCompressionSessionInvalidate(session);CFRelease(session);}
        if (movieWriter) {
            [movieInput markAsFinished];
            dispatch_semaphore_t done=dispatch_semaphore_create(0);
            [movieWriter finishWritingWithCompletionHandler:^{dispatch_semaphore_signal(done);}];
            if (dispatch_semaphore_wait(done,dispatch_time(DISPATCH_TIME_NOW,10*NSEC_PER_SEC)) || movieWriter.status!=AVAssetWriterStatusCompleted) cap.error=cap.error?:-9;
        }
        pthread_mutex_destroy(&cap.lock);
        bool passed=!create&&!prepare&&hardwareSelectionAccepted&&!encode&&!complete&&!cap.error&&cap.frames==requested;
        NSDictionary *report=@{@"model":@(model),@"system_version":[NSProcessInfo processInfo].operatingSystemVersionString,
            @"codec":fourcc(codec),@"width":@(WIDTH),@"height":@(HEIGHT),@"fps":@(FPS),
            @"input_pixel_format":coverageFFmpegFormat(pixelName),@"input_cv_format":fourcc(format),@"configuration":config,@"hardware_required":@YES,
            @"using_hardware_encoder":query ? (id)[NSNull null] : @(hardware),
            @"encoder_id":encoderID ?: (id)[NSNull null],@"selected_encoder_advertised_hardware":@(selectedHardware),
            @"hardware_evidence":hardware ? @"session_property" : (hardwareSelectionAccepted ? @"explicit_hardware_id_and_require_hardware" : @"unconfirmed"),
            @"create_status":@(create),@"prepare_status":@(prepare),@"hardware_query_status":@(query),
            @"encode_status":@(encode),@"complete_status":@(complete),@"callback_status":@(cap.error),
            @"requested_frames":@(requested),@"encoded_frames":@(cap.frames),@"passed":@(passed),@"property_results":propertyResults};
        NSData *json=[NSJSONSerialization dataWithJSONObject:report options:NSJSONWritingPrettyPrinted error:NULL];
        bool saved=[json writeToFile:[directory stringByAppendingPathComponent:@"result.json"] options:NSDataWritingWithoutOverwriting error:NULL];
        if (reference.length) saved=saved&&[reference writeToFile:[directory stringByAppendingPathComponent:@"input.raw"] options:NSDataWritingWithoutOverwriting error:NULL];
        if (rightReference.length) saved=saved&&[rightReference writeToFile:[directory stringByAppendingPathComponent:@"input-right.raw"] options:NSDataWritingWithoutOverwriting error:NULL];
        printf("M5_CODEC_PROBE codec=%s create=%d prepare=%d hardware=%d frames=%u/%u passed=%d\n",argv[2],(int)create,(int)prepare,hardware,cap.frames,requested,passed);
        return !saved ? 2 : (passed ? 0 : 1);
    }
}
