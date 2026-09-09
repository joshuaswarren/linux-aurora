// SPDX-License-Identifier: MIT
// Tight deterministic source images; CVPixelBuffer row padding is never input.
static OSType coverageFormat(NSString *name) {
    NSDictionary *formats=@{@"nv12":@(kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange),
        @"2vuy":@(kCVPixelFormatType_422YpCbCr8),@"p010":@(kCVPixelFormatType_420YpCbCr10BiPlanarVideoRange),
        @"p210":@(kCVPixelFormatType_422YpCbCr10BiPlanarVideoRange),@"y416":@(kCVPixelFormatType_4444AYpCbCr16),
        @"bgra":@(kCVPixelFormatType_32BGRA),@"fdep":@(kCVPixelFormatType_DepthFloat32),
        @"fdis":@(kCVPixelFormatType_DisparityFloat32)};
    return [formats[name] unsignedIntValue];
}
static NSString *coverageFFmpegFormat(NSString *name) {
    return @{@"nv12":@"nv12",@"2vuy":@"uyvy422",@"p010":@"p010le",@"p210":@"p210le",
        @"y416":@"ayuv64le",@"bgra":@"bgra",@"fdep":@"grayf32le",@"fdis":@"grayf32le"}[name];
}
static NSData *coveragePixels(CVPixelBufferRef pixel, NSString *name, unsigned frame, BOOL gradient) {
    const unsigned w=320,h=240;
    OSType format=coverageFormat(name);
    if (CVPixelBufferGetWidth(pixel)!=w || CVPixelBufferGetHeight(pixel)!=h ||
        CVPixelBufferGetPixelFormatType(pixel)!=format || CVPixelBufferLockBaseAddress(pixel,0)) return nil;
    BOOL planar=[name isEqual:@"nv12"] || [name isEqual:@"p010"] || [name isEqual:@"p210"];
    BOOL ten=[name isEqual:@"p010"] || [name isEqual:@"p210"];
    unsigned rowBytes=w*(ten?2:[name isEqual:@"2vuy"]?2:[name isEqual:@"y416"]?8:planar?1:4);
    unsigned chromaHeight=[name isEqual:@"p210"]?h:h/2;
    NSMutableData *data=[NSMutableData dataWithLength:rowBytes*(h+(planar?chromaHeight:0))];
    uint8_t *b=data.mutableBytes;
    const uint8_t ys[]={235,210,170,145,106,81,41,16},us[]={128,16,166,54,202,90,240,128},vs[]={128,146,16,34,222,240,110,128};
    for (unsigned y=0;y<h;y++) for (unsigned x=0;x<w;x++) {
        unsigned column=(x+frame*8)%w,bar=column/40;
        if ([name isEqual:@"2vuy"]) {
            size_t off=y*rowBytes+x*2;
            b[off]=(x&1)?vs[bar]:us[bar];b[off+1]=gradient?(16+(column+y)%220):ys[bar];
        } else if ([name isEqual:@"nv12"]) {
            b[y*w+x]=gradient?(16+(column+y)%220):ys[bar];
            if (!(y&1)) b[w*h+(y/2)*w+x]=(x&1)?vs[bar]:us[bar];
        } else if (ten) {
            ((uint16_t *)b)[y*w+x]=(64+(column*7+y*3+frame)%877)<<6;
            if (chromaHeight==h || !(y&1)) {
                unsigned cy=chromaHeight==h?y:y/2;
                ((uint16_t *)b)[w*h+cy*w+x]=(64+(column*3+cy*5+(x&1)*271)%897)<<6;
            }
        } else if ([name isEqual:@"y416"]) {
            uint16_t *p=(uint16_t *)(b+y*rowBytes+x*8);
            p[0]=(uint16_t)((column*193+y*71+frame*503)&65535);
            p[1]=(256+(column*13+y*7+frame)%3505)<<4;
            p[2]=(256+(column*9+y*3)%3585)<<4;p[3]=(256+(column*5+y*11)%3585)<<4;
        } else if ([name isEqual:@"bgra"]) {
            uint8_t *p=b+y*rowBytes+x*4;p[0]=column%256;p[1]=y;p[2]=(column+y)%256;p[3]=(column*3+y+frame)%256;
        } else {
            float depth=0.5f+column/160.0f+y/240.0f;
            ((float *)b)[y*w+x]=[name isEqual:@"fdis"]?1.0f/depth:depth;
        }
    }
    BOOL valid=!planar || CVPixelBufferGetPlaneCount(pixel)==2;
    for (unsigned plane=0;valid && plane<(planar?2:1);plane++) {
        unsigned rows=plane?chromaHeight:h;
        uint8_t *dest=planar?CVPixelBufferGetBaseAddressOfPlane(pixel,plane):CVPixelBufferGetBaseAddress(pixel);
        size_t stride=planar?CVPixelBufferGetBytesPerRowOfPlane(pixel,plane):CVPixelBufferGetBytesPerRow(pixel);
        if (!dest || stride<rowBytes) {valid=NO;break;}
        for (unsigned y=0;y<rows;y++) memcpy(dest+y*stride,b+(plane?rowBytes*h:0)+y*rowBytes,rowBytes);
    }
    CVPixelBufferUnlockBaseAddress(pixel,0);
    return valid?data:nil;
}
