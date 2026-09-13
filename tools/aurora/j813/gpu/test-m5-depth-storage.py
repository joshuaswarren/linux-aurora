#!/usr/bin/env python3
"""Host-test the actual pure depth validator without kernel-only G17 tests."""
from pathlib import Path
import re
import subprocess
import tempfile

root = Path(__file__).resolve().parents[4]
source = (root / 'drivers/gpu/drm/asahi/g17_uapi.rs').read_text()


def item(kind, name):
    start = source.index(f'{kind} {name}')
    opening = source.index('{', start)
    depth, end = 1, opening + 1
    while depth:
        depth += (source[end] == '{') - (source[end] == '}')
        end += 1
    return source[start:end]


structs = {}


def add_struct(name):
    if name in structs:
        return
    text = item('struct', name)
    structs[name] = text
    for child in re.findall(r':\s*(Uapi\w+)\s*[,\n]', text):
        add_struct(child)


add_struct('UapiRenderCommand')
parts = ['#![allow(dead_code)]']
parts += ['#[derive(Debug, Copy, Clone, Default)]\n' + text for text in structs.values()]
parts += ['#[derive(Debug, Copy, Clone)]\n' + item('enum', 'GpuAccess'),
          '#[derive(Debug, PartialEq, Eq)] enum UapiTranslateError { AddressRange, ZlsFields }',
          item('trait', 'GpuAddressSpace'), item('fn', 'require_range'),
          re.search(r'pub\(crate\) const M5_MAX_RENDER_DIMENSION[^;]+;', source).group(),
          item('fn', 'validate_m5_reference_depth')]
parts.append(r'''
struct Space { data: u64, metadata: u64 }
impl GpuAddressSpace for Space {
    fn covers(&self, address: u64, size: u64, _access: GpuAccess) -> bool {
        (address == 0x10000 && size <= self.data) ||
        (address == 0x800000 && size <= self.metadata)
    }
}
fn command(width: u16, height: u16, bytes: u64) -> UapiRenderCommand {
    UapiRenderCommand {
        width, height, layers: 1, depth_clear: 0x3f800000, zls_control: 0x80044,
        depth_dimensions: u32::from(width - 1) | (u32::from(height - 1) << 16),
        depth: UapiZlsBuffer { base: 0x10000, compression_base: 0x800000,
            stride: if bytes < 16384 { 0 } else { (((bytes / 16384 - 1) << 14) | 1) as u32 },
            compression_stride: 0 },
        ..Default::default()
    }
}
#[test]
fn exact_resize_footprints_and_truncated_mappings() {
    for (w,h,data,metadata) in [(16,16,0x400,0x80), (33,17,0x2000,0x80),
        (63,129,0x10000,0x200), (64,64,0x4000,0x80), (192,192,0x24000,0x800),
        (256,256,0x40000,0x800), (319,191,0x3c000,0x1000),
        (889,719,0x2a0000,0x8000), (1024,1024,0x400000,0x8000),
        (2560,1664,0x1040000,0x40000), (3840,2160,0x1fe0000,0x80000),
        (16384,16384,0x40000000,0x800000)] {
        let c = command(w,h,data);
        assert_eq!(validate_m5_reference_depth(&c,&Space{data,metadata}),Ok(()));
        assert_eq!(validate_m5_reference_depth(&c,&Space{data:data-1,metadata}),
            Err(UapiTranslateError::AddressRange));
        assert_eq!(validate_m5_reference_depth(&c,&Space{data,metadata:metadata-1}),
            Err(UapiTranslateError::AddressRange));
    }
}
#[test]
fn tiny_uncompressed_depth_and_cacheline_metadata() {
    for (w,h,data) in [(1,1,128), (1,16384,65536), (15,65,8192)] {
        let mut c = command(w,h,data);
        c.depth.compression_base = 0; c.zls_control = 0x80000;
        assert_eq!(validate_m5_reference_depth(&c,&Space{data,metadata:0}),Ok(()));
        assert_eq!(validate_m5_reference_depth(&c,&Space{data:data-1,metadata:0}),
            Err(UapiTranslateError::AddressRange));
        c.depth.compression_base = 0x800000; c.zls_control = 0x80044;
        assert_eq!(validate_m5_reference_depth(&c,&Space{data,metadata:128}),
            Err(UapiTranslateError::ZlsFields));
    }
    let mut c = command(16,16,1024);
    c.depth.stride = 0xffffc001;
    assert_eq!(validate_m5_reference_depth(&c,&Space{data:1024,metadata:128}),
        Err(UapiTranslateError::ZlsFields));
    c = command(16385,1,131072); c.depth.compression_base = 0; c.zls_control = 0x80000;
    assert_eq!(validate_m5_reference_depth(&c,&Space{data:131072,metadata:0}),
        Err(UapiTranslateError::ZlsFields));
}
#[test]
fn small_surface_metadata_can_share_its_data_page() {
    struct Adjacent;
    impl GpuAddressSpace for Adjacent {
        fn covers(&self, address: u64, size: u64, _: GpuAccess) -> bool {
            (address == 0x10080 && size <= 1024) ||
            (address == 0x10480 && size <= 128)
        }
    }
    let mut c = command(16,16,1024);
    c.depth.base = 0x10080;
    c.depth.compression_base = 0x10480;
    assert_eq!(validate_m5_reference_depth(&c,&Adjacent),Ok(()));
    c.depth.compression_base += 1;
    assert_eq!(validate_m5_reference_depth(&c,&Adjacent),Err(UapiTranslateError::ZlsFields));
}
#[test]
fn stale_dimensions_and_strides_are_rejected() {
    let valid = command(319,191,0x3c000);
    let space = Space {data:0x3c000,metadata:0x4000};
    let mut c = valid;
    c.depth.compression_stride = 0x7c000;
    assert_eq!(validate_m5_reference_depth(&c,&space),Ok(()));
    c.depth.compression_stride = 0x3c000;
    assert_eq!(validate_m5_reference_depth(&c,&space),Err(UapiTranslateError::ZlsFields));
    c = valid; c.depth.stride = 0x3c001;
    assert_eq!(validate_m5_reference_depth(&c,&space),Err(UapiTranslateError::ZlsFields));
    c = valid; c.depth_dimensions = 0x00ff00ff;
    assert_eq!(validate_m5_reference_depth(&c,&space),Err(UapiTranslateError::ZlsFields));
    c = valid; c.depth.base += 1;
    assert_eq!(validate_m5_reference_depth(&c,&space),Err(UapiTranslateError::ZlsFields));
}
''')
with tempfile.TemporaryDirectory(prefix='m5-depth-resize-') as directory:
    path = Path(directory) / 'depth.rs'
    path.write_text('\n'.join(parts))
    binary = path.with_suffix('')
    subprocess.run(['rustc', '--edition=2021', '--test', str(path), '-o', str(binary)], check=True)
    subprocess.run([str(binary)], check=True)
