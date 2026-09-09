// SPDX-License-Identifier: MIT
// Negative control: a requested manufacturer alone is not hardware evidence.
#import <Foundation/Foundation.h>
#import <AudioToolbox/AudioToolbox.h>
#include <sys/sysctl.h>
int main(int argc,const char **argv) {
    @autoreleasepool {
        char model[64]={0};size_t size=sizeof(model);
        if (argc!=2 || sysctlbyname("hw.model",model,&size,NULL,0) || strcmp(model,"Mac17,5")) return 2;
        AudioStreamBasicDescription in={.mSampleRate=48000,.mFormatID=kAudioFormatLinearPCM,
            .mFormatFlags=kAudioFormatFlagIsSignedInteger|kAudioFormatFlagIsPacked,
            .mBytesPerPacket=4,.mFramesPerPacket=1,.mBytesPerFrame=4,.mChannelsPerFrame=2,.mBitsPerChannel=16};
        NSMutableArray *results=[NSMutableArray array];
        for (NSString *manufacturer in @[@"appl",@"aphw",@"zzzz"]) {
            const unsigned char *s=(const unsigned char *)manufacturer.UTF8String;
            UInt32 value=(UInt32)s[0]<<24|(UInt32)s[1]<<16|(UInt32)s[2]<<8|s[3];
            AudioClassDescription cls={kAudioEncoderComponentType,kAudioFormatMPEG4AAC,value};
            AudioStreamBasicDescription out={.mSampleRate=48000,.mFormatID=kAudioFormatMPEG4AAC,.mChannelsPerFrame=2};
            UInt32 n=sizeof(out);OSStatus format=AudioFormatGetProperty(kAudioFormatProperty_FormatInfo,0,NULL,&n,&out);
            AudioConverterRef converter=NULL;OSStatus create=format?:AudioConverterNewSpecific(&in,&out,1,&cls,&converter);
            NSMutableDictionary *row=[@{@"requested_manufacturer":manufacturer,@"create_status":@(create)} mutableCopy];
            if (!create) {
                UInt32 resumes=0;n=sizeof(resumes);
                OSStatus status=AudioConverterGetProperty(converter,'crfi',&n,&resumes);
                row[@"hardware_resume_property_status"]=@(status);
                if (!status) row[@"hardware_resume_property"]=@(resumes);
                AudioConverterDispose(converter);
            }
            [results addObject:row];
        }
        NSDictionary *report=@{@"model":@(model),@"system_version":NSProcessInfo.processInfo.operatingSystemVersionString,
            @"scope":@"AAC-LC manufacturer-selection negative control; no encoding submitted",@"results":results};
        NSData *json=[NSJSONSerialization dataWithJSONObject:report options:NSJSONWritingPrettyPrinted error:NULL];
        return ![json writeToFile:@(argv[1]) options:NSDataWritingWithoutOverwriting error:NULL];
    }
}
