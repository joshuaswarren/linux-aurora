// SPDX-License-Identifier: MIT
// Target-only inventory and explicit hardware audio encoding. No SW fallback.
#import <Foundation/Foundation.h>
#import <AudioToolbox/AudioToolbox.h>
#include <sys/sysctl.h>
#include <unistd.h>
#include <math.h>

// AudioFormat.h declares these manufacturer values only for iOS targets.
// Preserve the same wire identifiers when inspecting macOS class descriptions.
enum { NeoHardwareManufacturer='aphw', NeoSoftwareManufacturer='appl' };

static NSString *tag(UInt32 v) {
    unsigned char s[]={v>>24,v>>16,v>>8,v,0};
    return [[NSString alloc] initWithBytes:s length:4 encoding:NSISOLatin1StringEncoding];
}
static NSDictionary *asbd(AudioStreamBasicDescription a) {
    return @{@"format":tag(a.mFormatID),@"sample_rate":@(a.mSampleRate),
        @"channels":@(a.mChannelsPerFrame),@"flags":@(a.mFormatFlags),
        @"frames_per_packet":@(a.mFramesPerPacket),@"bytes_per_packet":@(a.mBytesPerPacket),
        @"bytes_per_frame":@(a.mBytesPerFrame),@"bits_per_channel":@(a.mBitsPerChannel)};
}
static BOOL save(id object, NSString *path) {
    NSData *data=[NSJSONSerialization dataWithJSONObject:object options:NSJSONWritingPrettyPrinted error:NULL];
    return data && [data writeToFile:path options:NSDataWritingWithoutOverwriting error:NULL];
}
typedef struct { const uint8_t *bytes; UInt32 frames, position, channels; } PCM;
static OSStatus input(AudioConverterRef converter, UInt32 *count, AudioBufferList *buffers,
                      AudioStreamPacketDescription **packets, void *opaque) {
    (void)converter;
    PCM *p=opaque; UInt32 n=MIN(*count,p->frames-p->position);
    *count=n; buffers->mNumberBuffers=1;
    buffers->mBuffers[0]=(AudioBuffer){p->channels,n*p->channels*2,(void *)(p->bytes+p->position*p->channels*2)};
    p->position+=n;
    if (packets) *packets=NULL;
    return noErr;
}
int main(int argc,const char **argv) {
    @autoreleasepool {
        if (argc!=3 || (strcmp(argv[1],"list") && strcmp(argv[1],"encode-hardware"))) {
            fprintf(stderr,"Usage: neo-audio-probe list OUTPUT.json | encode-hardware NEW_DIRECTORY\n");return 2;
        }
        char model[64]={0};size_t size=sizeof(model);
        if (sysctlbyname("hw.model",model,&size,NULL,0) || strcmp(model,"Mac17,5")) {
            fprintf(stderr,"Target must be the Neo, not the controller\n");return 2;
        }
        UInt32 bytes=0;
        OSStatus status=AudioFormatGetPropertyInfo(kAudioFormatProperty_EncodeFormatIDs,0,NULL,&bytes);
        if (status || bytes>65536 || bytes%sizeof(UInt32)) return 2;
        NSMutableData *formats=[NSMutableData dataWithLength:bytes];
        status=AudioFormatGetProperty(kAudioFormatProperty_EncodeFormatIDs,0,NULL,&bytes,formats.mutableBytes);
        if (status) return 2;
        NSMutableArray *rows=[NSMutableArray array],*hardware=[NSMutableArray array];
        UInt32 *ids=formats.mutableBytes;
        for (unsigned i=0;i<bytes/sizeof(UInt32);i++) {
            UInt32 n=0,format=ids[i];
            OSStatus info=AudioFormatGetPropertyInfo(kAudioFormatProperty_Encoders,sizeof(format),&format,&n);
            NSMutableArray *classes=[NSMutableArray array];
            OSStatus query=info;
            if (!info && n<=65536 && !(n%sizeof(AudioClassDescription))) {
                NSMutableData *data=[NSMutableData dataWithLength:n];
                query=AudioFormatGetProperty(kAudioFormatProperty_Encoders,sizeof(format),&format,&n,data.mutableBytes);
                if (!query) for (unsigned j=0;j<n/sizeof(AudioClassDescription);j++) {
                    AudioClassDescription d=((AudioClassDescription *)data.mutableBytes)[j];
                    BOOL hw=d.mManufacturer==NeoHardwareManufacturer;
                    NSDictionary *item=@{@"type":tag(d.mType),@"subtype":tag(d.mSubType),
                        @"manufacturer":tag(d.mManufacturer),@"type_id":@(d.mType),
                        @"subtype_id":@(d.mSubType),@"manufacturer_id":@(d.mManufacturer),
                        @"classification":hw?@"hardware_advertised":d.mManufacturer==NeoSoftwareManufacturer?@"software_advertised":@"unknown"};
                    [classes addObject:item]; if (hw && d.mType==kAudioEncoderComponentType) [hardware addObject:item];
                }
            }
            [rows addObject:@{@"format":tag(format),@"format_id":@(format),@"info_status":@(info),@"query_status":@(query),@"encoders":classes}];
        }
        NSDictionary *inventory=@{@"model":@(model),@"system_version":NSProcessInfo.processInfo.operatingSystemVersionString,
            @"status":@(status),@"formats":rows,@"hardware_encoder_count":@(hardware.count),
            @"scope":@"AudioFormat encoder inventory; hardware execution requires a separate test"};
        if (!strcmp(argv[1],"list")) return !save(inventory,@(argv[2]));
        NSString *root=@(argv[2]);
        if ([NSFileManager.defaultManager fileExistsAtPath:root] ||
            ![NSFileManager.defaultManager createDirectoryAtPath:root withIntermediateDirectories:YES attributes:nil error:NULL]) return 2;
        if (!save(inventory,[root stringByAppendingPathComponent:@"inventory.json"])) return 2;
        // Also make the explicit AAC-LC hardware request when the inventory
        // does not advertise one. Absence from enumeration is not a probe.
        NSMutableArray *candidates=[hardware mutableCopy];
        BOOL hasAAC=NO;
        for (NSDictionary *item in candidates)
            if ([item[@"subtype_id"] unsignedIntValue]==kAudioFormatMPEG4AAC) hasAAC=YES;
        if (!hasAAC) [candidates addObject:@{@"type":tag(kAudioEncoderComponentType),
            @"subtype":tag(kAudioFormatMPEG4AAC),@"manufacturer":tag(NeoHardwareManufacturer),
            @"type_id":@(kAudioEncoderComponentType),@"subtype_id":@(kAudioFormatMPEG4AAC),
            @"manufacturer_id":@(NeoHardwareManufacturer),@"classification":@"candidate_not_advertised"}];
        // AAC first; every other hardware candidate comes from the inventory.
        [candidates sortUsingComparator:^NSComparisonResult(NSDictionary *a,NSDictionary *b) {
            BOOL aa=[a[@"subtype_id"] unsignedIntValue]==kAudioFormatMPEG4AAC;
            BOOL bb=[b[@"subtype_id"] unsignedIntValue]==kAudioFormatMPEG4AAC;
            return aa==bb?NSOrderedSame:aa?NSOrderedAscending:NSOrderedDescending;
        }];
        unsigned ordinal=0;
        for (NSDictionary *item in candidates) {
            alarm(45);
            NSString *dir=[root stringByAppendingPathComponent:[NSString stringWithFormat:@"encoder-%03u",ordinal++]];
            [NSFileManager.defaultManager createDirectoryAtPath:dir withIntermediateDirectories:NO attributes:nil error:NULL];
            AudioClassDescription cls={[item[@"type_id"] unsignedIntValue],[item[@"subtype_id"] unsignedIntValue],[item[@"manufacturer_id"] unsignedIntValue]};
            AudioStreamBasicDescription in={.mSampleRate=48000,.mFormatID=kAudioFormatLinearPCM,
                .mFormatFlags=kAudioFormatFlagIsSignedInteger|kAudioFormatFlagIsPacked,
                .mBytesPerPacket=4,.mFramesPerPacket=1,.mBytesPerFrame=4,.mChannelsPerFrame=2,.mBitsPerChannel=16};
            AudioStreamBasicDescription out={.mSampleRate=48000,.mFormatID=cls.mSubType,.mChannelsPerFrame=2};
            UInt32 n=sizeof(out);
            OSStatus formatStatus=AudioFormatGetProperty(kAudioFormatProperty_FormatInfo,0,NULL,&n,&out);
            AudioConverterRef converter=NULL;
            OSStatus create=formatStatus?:AudioConverterNewSpecific(&in,&out,1,&cls,&converter);
            NSMutableDictionary *result=[@{@"encoder":item,@"input":asbd(in),@"format_status":@(formatStatus),
                @"create_status":@(create),@"hardware_required":@YES,@"passed":@NO} mutableCopy];
            if (!create) {
                AudioClassDescription invalid=cls;invalid.mManufacturer='zzzz';
                AudioConverterRef negative=NULL;
                OSStatus negativeStatus=AudioConverterNewSpecific(&in,&out,1,&invalid,&negative);
                result[@"invalid_manufacturer_create_status"]=@(negativeStatus);
                result[@"selection_enforced"]=@(negativeStatus!=0);
                if (negative) AudioConverterDispose(negative);
                if (!negativeStatus) {
                    result[@"qualification"]=@"Selection accepts a nonexistent manufacturer; encoding refused because software fallback cannot be excluded";
                    AudioConverterDispose(converter);converter=NULL;
                }
            }
            if (!create && converter) {
                n=sizeof(out);AudioConverterGetProperty(converter,kAudioConverterCurrentOutputStreamDescription,&n,&out);
                result[@"output"]=asbd(out);
                NSMutableData *pcm=[NSMutableData dataWithLength:480000*4];
                int16_t *samples=pcm.mutableBytes;
                for (unsigned frame=0;frame<480000;frame++) for (unsigned ch=0;ch<2;ch++) {
                    double t=frame/48000.0,frequency=ch?880:440;
                    samples[frame*2+ch]=(frame>=192000 && frame<240000)?0:(int16_t)(12000*sin(2*M_PI*(frequency*t+3*t*t)));
                }
                [pcm writeToFile:[dir stringByAppendingPathComponent:@"input.s16le"] options:NSDataWritingWithoutOverwriting error:NULL];
                PCM inputState={pcm.bytes,480000,0,2};
                UInt32 maxPacket=0;n=sizeof(maxPacket);
                OSStatus encode=AudioConverterGetProperty(converter,kAudioConverterPropertyMaximumOutputPacketSize,&n,&maxPacket);
                if (!encode && (!maxPacket || maxPacket>1024*1024)) encode=-1;
                NSMutableData *payload=[NSMutableData data];NSMutableArray *packets=[NSMutableArray array];
                for (unsigned iteration=0;!encode && iteration<16384;iteration++) {
                    NSMutableData *buffer=[NSMutableData dataWithLength:maxPacket];
                    AudioBufferList list={1,{{2,maxPacket,buffer.mutableBytes}}};
                    UInt32 count=1;AudioStreamPacketDescription packet={0};
                    encode=AudioConverterFillComplexBuffer(converter,input,&inputState,&count,&list,&packet);
                    if (encode || !count) break;
                    UInt32 length=packet.mDataByteSize?:list.mBuffers[0].mDataByteSize;
                    if (length>maxPacket || packet.mStartOffset<0 || packet.mStartOffset+length>maxPacket) {encode=-2;break;}
                    [packets addObject:@{@"offset":@(payload.length),@"size":@(length),@"variable_frames":@(packet.mVariableFramesInPacket)}];
                    [payload appendBytes:(uint8_t *)buffer.bytes+packet.mStartOffset length:length];
                    if (iteration==16383) encode=-3;
                }
                n=0;
                if (!AudioConverterGetPropertyInfo(converter,kAudioConverterCompressionMagicCookie,&n,NULL) && n && n<=1024*1024) {
                    NSMutableData *cookie=[NSMutableData dataWithLength:n];
                    if (!AudioConverterGetProperty(converter,kAudioConverterCompressionMagicCookie,&n,cookie.mutableBytes))
                        [cookie writeToFile:[dir stringByAppendingPathComponent:@"magic-cookie.bin"] options:NSDataWritingWithoutOverwriting error:NULL];
                }
                [payload writeToFile:[dir stringByAppendingPathComponent:@"encoded.bin"] options:NSDataWritingWithoutOverwriting error:NULL];
                save(packets,[dir stringByAppendingPathComponent:@"packets.json"]);
                result[@"encode_status"]=@(encode);result[@"input_frames_consumed"]=@(inputState.position);
                result[@"encoded_packets"]=@(packets.count);result[@"encoded_bytes"]=@(payload.length);
                result[@"passed"]=@(!encode && packets.count && inputState.position==480000);
                result[@"qualification"]=@"Independent decode and engine evidence still required";
                AudioConverterDispose(converter);
            }
            save(result,[dir stringByAppendingPathComponent:@"result.json"]);alarm(0);
        }
        printf("NEO_AUDIO_INVENTORY formats=%lu hardware=%lu experiments=%u\n",(unsigned long)rows.count,(unsigned long)hardware.count,ordinal);
        return 0;
    }
}
