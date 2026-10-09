// Bounded native ANE reference using the interface demonstrated by ANEMLL:
// https://gist.github.com/Anemll/49e219448ad350ef67ff4bfdcb9ebd8c
// Compile-only is the default. --run-neo allows exactly one invocation on
// Mac17,5, through Apple's driver; this program never accesses MMIO itself.
#import <Foundation/Foundation.h>
#import <IOSurface/IOSurface.h>
#import <objc/message.h>
#import <dlfcn.h>
#import <sys/sysctl.h>
#import <signal.h>
#import <unistd.h>

static void timeout_handler(int unused) { _exit(124); }
static void require(BOOL ok, NSString *message) {
    if (!ok) @throw [NSException exceptionWithName:@"ANEReference" reason:message userInfo:nil];
}
static IOSurfaceRef make_surface(NSUInteger size) {
    return IOSurfaceCreate((__bridge CFDictionaryRef)@{
        (id)kIOSurfaceWidth:@(size), (id)kIOSurfaceHeight:@1,
        (id)kIOSurfaceBytesPerElement:@1, (id)kIOSurfaceBytesPerRow:@(size),
        (id)kIOSurfaceAllocSize:@(size), (id)kIOSurfacePixelFormat:@0});
}

int main(int argc, const char **argv) {
    signal(SIGALRM, timeout_handler); alarm(180);
    @autoreleasepool { @try {
        require(argc == 5 || argc == 6,
            @"usage: direct-reference compiled.mlmodelc input.bin output.bin receipt.json [--run-neo]");
        BOOL series = argc == 6 && strcmp(argv[5], "--run-neo-series") == 0;
        BOOL execute = argc == 6 && (strcmp(argv[5], "--run-neo") == 0 || series);
        require(argc == 5 || execute, @"Unknown action");
        if (execute) {
            char machine[64] = {0}; size_t length = sizeof(machine);
            require(sysctlbyname("hw.model", machine, &length, NULL, 0) == 0 &&
                    strcmp(machine, "Mac17,5") == 0, @"Execution is restricted to the Neo");
        }
        require(dlopen("/System/Library/PrivateFrameworks/AppleNeuralEngine.framework/AppleNeuralEngine",RTLD_NOW) != NULL,
                @"AppleNeuralEngine framework unavailable");
        NSString *root = [NSString stringWithUTF8String:argv[1]];
        NSData *mil = [NSData dataWithContentsOfFile:[root stringByAppendingPathComponent:@"model.mil"]];
        NSData *weights = [NSData dataWithContentsOfFile:[root stringByAppendingPathComponent:@"weights/weight.bin"]];
        NSString *inputPath=[NSString stringWithUTF8String:argv[2]];
        NSData *input = [NSData dataWithContentsOfFile:series ? [inputPath stringByAppendingPathComponent:@"0.bin"] : inputPath];
        require(mil.length > 0 && mil.length <= 0x40000 && weights.length > 0 &&
                weights.length <= 0x100000 && input.length == 4096, @"Unexpected model/input size");
        Class descriptorClass = NSClassFromString(@"_ANEInMemoryModelDescriptor");
        Class modelClass = NSClassFromString(@"_ANEInMemoryModel");
        require(descriptorClass && modelClass, @"In-memory compiler classes unavailable");
        id descriptor = ((id(*)(Class,SEL,id,id,id))objc_msgSend)(descriptorClass,
            @selector(modelWithMILText:weights:optionsPlist:), mil,
            @{@"@model_path/weights/weight.bin":@{@"offset":@0,@"data":weights}}, nil);
        require(descriptor != nil, @"Descriptor creation failed");
        id model = ((id(*)(Class,SEL,id))objc_msgSend)(modelClass,
            @selector(inMemoryModelWithDescriptor:), descriptor);
        require(model != nil, @"Model creation failed");
        NSString *identifier = ((id(*)(id,SEL))objc_msgSend)(model,@selector(hexStringIdentifier));
        require([identifier isKindOfClass:NSString.class] && identifier.length > 0 && identifier.length <= 256 &&
                [identifier rangeOfCharacterFromSet:[[NSCharacterSet characterSetWithCharactersInString:@"0123456789abcdefABCDEF_"] invertedSet]].location == NSNotFound,
                [NSString stringWithFormat:@"Invalid compiler identifier: class=%@ value=%@",[identifier class],identifier]);
        NSString *temp = [NSTemporaryDirectory() stringByAppendingPathComponent:identifier];
        NSFileManager *fm = NSFileManager.defaultManager;
        require([fm createDirectoryAtPath:[temp stringByAppendingPathComponent:@"weights"]
            withIntermediateDirectories:YES attributes:nil error:nil], @"Cannot prepare compiler directory");
        NSDictionary *files = @{@"model.mil":mil,@"weights/weight.bin":weights};
        for (NSString *name in files) {
            NSString *path = [temp stringByAppendingPathComponent:name];
            NSData *prior = [NSData dataWithContentsOfFile:path];
            require(!prior || [prior isEqualToData:files[name]], @"Compiler directory content conflict");
            require([files[name] writeToFile:path atomically:YES], @"Cannot stage compiler input");
        }
        NSError *error = nil;
        BOOL compiled = ((BOOL(*)(id,SEL,unsigned int,id,NSError**))objc_msgSend)(model,
            @selector(compileWithQoS:options:error:),0,@{},&error);
        require(compiled, [NSString stringWithFormat:@"ANE compiler rejected graph: %@",error]);
        printf("DIRECT_ANE_COMPILE_PASS\n"); fflush(stdout);
        NSMutableDictionary *receipt = [@{@"compiled":@YES,@"executed":@NO,@"linux_ane_tested":@NO,
            @"compiler_directory":temp,
            @"compiler_files":[fm subpathsAtPath:temp] ?: @[]} mutableCopy];
        if (execute) {
            BOOL loaded = ((BOOL(*)(id,SEL,unsigned int,id,NSError**))objc_msgSend)(model,
                @selector(loadWithQoS:options:error:),0,@{},&error);
            require(loaded,[NSString stringWithFormat:@"ANE load failed: %@",error]);
            IOSurfaceRef inSurface = make_surface(4096), outSurface = make_surface(32768);
            require(inSurface && outSurface,@"IOSurface allocation failed");
            Class wrapperClass=NSClassFromString(@"_ANEIOSurfaceObject");
            id wi=((id(*)(Class,SEL,IOSurfaceRef))objc_msgSend)(wrapperClass,@selector(objectWithIOSurface:),inSurface);
            id wo=((id(*)(Class,SEL,IOSurfaceRef))objc_msgSend)(wrapperClass,@selector(objectWithIOSurface:),outSurface);
            id request=((id(*)(Class,SEL,id,id,id,id,id,id,id))objc_msgSend)(NSClassFromString(@"_ANERequest"),
                @selector(requestWithInputs:inputIndices:outputs:outputIndices:weightsBuffer:perfStats:procedureIndex:),
                @[wi],@[@0],@[wo],@[@0],nil,nil,@0);
            require(request != nil,@"Request creation failed");
            printf("DIRECT_ANE_READY_WAITING seconds=15\n"); fflush(stdout); sleep(15);
            for (unsigned sample=0; sample<(series ? 3U : 1U);sample++) {
            if (series) input=[NSData dataWithContentsOfFile:[inputPath stringByAppendingPathComponent:[NSString stringWithFormat:@"%u.bin",sample]]];
            require(input.length==4096,@"Unexpected series input size");
            IOSurfaceLock(inSurface,0,NULL);
            memcpy(IOSurfaceGetBaseAddress(inSurface),input.bytes,input.length);
            IOSurfaceUnlock(inSurface,0,NULL);
            IOSurfaceLock(outSurface,0,NULL);
            uint16_t *out = IOSurfaceGetBaseAddress(outSurface);
            for (unsigned i=0;i<16384;i++) out[i]=0x7e00;
            IOSurfaceUnlock(outSurface,0,NULL);
            BOOL done=((BOOL(*)(id,SEL,unsigned int,id,id,NSError**))objc_msgSend)(model,
                @selector(evaluateWithQoS:options:request:error:),0,@{},request,&error);
            require(done,[NSString stringWithFormat:@"ANE execution failed: %@",error]);
            IOSurfaceLock(outSurface,kIOSurfaceLockReadOnly,NULL);
            NSData *result=[NSData dataWithBytes:IOSurfaceGetBaseAddress(outSurface) length:32768];
            NSString *outputPath=[NSString stringWithUTF8String:argv[3]];
            if(series) outputPath=[outputPath stringByAppendingFormat:@"-%u.bin",sample];
            require([result writeToFile:outputPath atomically:YES],@"Output save failed");
            IOSurfaceUnlock(outSurface,kIOSurfaceLockReadOnly,NULL);
            printf("DIRECT_ANE_SAMPLE_PASS sample=%u values=16384 numerical_validation_pending=1\n",sample);fflush(stdout);
            }
            BOOL unloaded=((BOOL(*)(id,SEL,unsigned int,NSError**))objc_msgSend)(model,@selector(unloadWithQoS:error:),0,&error);
            require(unloaded,[NSString stringWithFormat:@"ANE unload failed: %@",error]);
            CFRelease(inSurface); CFRelease(outSurface);
            receipt[@"executed"]=@YES;
            receipt[@"samples"]=@(series ? 3 : 1);
            printf("DIRECT_ANE_EXECUTION_AND_UNLOAD_PASS values=16384 numerical_validation_pending=1\n");
        }
        NSData *json=[NSJSONSerialization dataWithJSONObject:receipt options:NSJSONWritingPrettyPrinted error:&error];
        require([json writeToFile:[NSString stringWithUTF8String:argv[4]] atomically:YES],@"Receipt save failed");
        return 0;
    } @catch (NSException *e) {
        fprintf(stderr,"DIRECT_ANE_FAILED: %s\n",e.reason.UTF8String); return 1;
    } }
}
