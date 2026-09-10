#!/usr/bin/env python3
# SPDX-License-Identifier: MIT
"""Compare Linux codecs with pre-handoff M5 roots and the real host reset code.

Requires the private local captures, rustc, and unicorn 2.1.4. No hardware
connection is opened. The ARM64 reset routine runs only in an emulator with
synthetic channel allocations. Captured firmware is never executed here.
"""
import argparse
import hashlib
import json
from pathlib import Path
import struct
import subprocess
import tempfile

from unicorn import Uc, UC_ARCH_ARM64, UC_MODE_ARM
from unicorn.arm64_const import UC_ARM64_REG_X0, UC_ARM64_REG_CPACR_EL1


def read_va(image, address, size):
    if struct.unpack_from('<I', image)[0] != 0xfeedfacf:
        raise ValueError('Expected extracted ARM64 Mach-O')
    offset = 32
    for _ in range(struct.unpack_from('<I', image, 16)[0]):
        command, length = struct.unpack_from('<II', image, offset)
        if command == 0x19:
            vm, _, file_offset, file_size = struct.unpack_from('<QQQQ', image, offset + 24)
            if vm <= address and address + size <= vm + file_size:
                start = file_offset + address - vm
                return image[start:start + size]
        if length < 8:
            raise ValueError('Invalid Mach-O command')
        offset += length
    raise ValueError('Requested code is outside file-backed segments')


def rust_item(source, name, kind='fn'):
    marker = f'{kind} {name}'
    start = source.index(marker)
    body = source.index('{', start)
    depth = 1
    end = body + 1
    while depth:
        depth += (source[end] == '{') - (source[end] == '}')
        end += 1
    # Preserve pub(crate), needed by the harness, without copying the other
    # kernel-only dependencies of this otherwise pure submission module.
    return 'pub(crate) ' + source[start:end]


def native_reset(code, values):
    emulator = Uc(UC_ARCH_ARM64, UC_MODE_ARM)
    emulator.mem_map(0x100000, 0x1000)
    emulator.mem_write(0x100000, code)
    emulator.mem_map(0x200000, 0x5000)
    host, cached, uncached = 0x200000, 0x201000, 0x202000
    emulator.mem_write(cached, bytes([0xa5]) * 0x100)
    emulator.mem_write(uncached, bytes([0x5a]) * 0x80)
    def store(offset, value, width=8):
        emulator.mem_write(host + offset, value.to_bytes(width, 'little'))
    store(0x58, cached)
    store(0x60, uncached)
    for offset, value in zip((0x88, 0x90, 0x98, 0x4c, 0xf8), values):
        store(offset, value, 4 if offset == 0x4c else 8)
    store(0x54, 2048, 4)
    emulator.reg_write(UC_ARM64_REG_X0, host)
    emulator.reg_write(UC_ARM64_REG_CPACR_EL1, 3 << 20)
    emulator.emu_start(0x100000, 0x100000 + len(code), count=100)
    assert bytes(emulator.mem_read(cached + 0xc0, 0x40)) == bytes([0xa5]) * 0x40
    return bytes(emulator.mem_read(cached, 0xc0)), bytes(emulator.mem_read(uncached, 0x80))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('preinit', type=Path)
    parser.add_argument('kext', type=Path)
    parser.add_argument('--linux', type=Path, required=True)
    args = parser.parse_args()
    driver = (args.linux / 'drivers/gpu/drm/asahi').resolve()
    captures = []
    for role in ('gfx-asc', 'gfx1-asc'):
        directory = args.preinit / role
        manifest = json.loads((directory / 'manifest.json').read_text())
        if manifest['phase'] != 'before-INBOX1-publication':
            raise ValueError('Reference is not a pre-handoff capture')
        for entry in manifest['objects']:
            data = (directory / entry['file']).read_bytes()
            if len(data) != entry['size'] or hashlib.sha256(data).hexdigest() != entry['sha256']:
                raise ValueError('Capture integrity check failed')
        captures.append(directory.resolve())

    # This body has no calls, branches, PAC operations or PC-relative operands.
    # It includes every channel reset store, ending before the optional flush.
    code = read_va(args.kext.read_bytes(), 0xfffffe000897d36c, 0xa8)
    code_hash = hashlib.sha256(code).hexdigest()
    if code_hash != 'fb321b76a31d693b3c0ebbeb3a93963e3c0f587be0176e20484833946ce74148':
        raise ValueError('M5 25G76 host reset routine differs from the inspected code')
    native_sizes = struct.unpack('<7Q', read_va(args.kext.read_bytes(), 0xfffffe0007139820, 56))
    if native_sizes != (0x2708, 0xc18, 0x1048, 0xe10, 0x6a30, 0x68, 0x800):
        raise ValueError('M5 shared allocation descriptor sizes changed')
    if struct.unpack('<2Q', read_va(args.kext.read_bytes(), 0xfffffe0007139880, 16)) != (0xe440, 0x6a8):
        raise ValueError('M5 status/config allocation sizes changed')
    source = (driver / 'g17_submission.rs').read_text()
    items = [rust_item(source, n) for n in ('put_u32_le', 'put_u64_le',
              'encode_m5_25g76_channel_state_reset', 'apply_g17_uncached_channel_reset')]
    items += [rust_item(source, n, 'struct') for n in
              ('M5ChannelStateResetValues', 'G17UncachedChannelResetLayout')]
    for name in ('M5_25G76_CHANNEL_STATE_RESET_SIZE', 'G17_UNCACHED_CHANNEL_RESET_WRITE_EXTENT',
                 'G17_UNCACHED_CHANNEL_RESET_LAYOUT'):
        start = source.index('pub(crate) const ' + name + ':')
        items.append(source[start:source.index(';', start) + 1])
    harness = '''
#![allow(dead_code, non_upper_case_globals)]
mod module_parameters {
    pub struct Parameter;
    impl Parameter { pub fn value(&self) -> &'static u64 { &0 } }
    pub static g17p_fw_log: Parameter = Parameter;
    pub static g17p_progress_watchdog: Parameter = Parameter;
    pub static g17p_idle_selector: Parameter = Parameter;
}
#[path = INITDATA_PATH] mod initdata;
mod submission { SUBMISSION_ITEMS }
fn word(b: &[u8], at: usize) -> u64 { u64::from_le_bytes(b[at..at+8].try_into().unwrap()) }
fn main() {
    use initdata::*;
    use submission::*;
    let args: Vec<String> = std::env::args().collect();
    for (index, directory) in args[1..3].iter().enumerate() {
        let p = std::path::Path::new(directory);
        let raw = std::fs::read(p.join("root.bin")).unwrap();
        let main = std::fs::read(p.join("main-config.bin")).unwrap();
        let hardware = std::fs::read(p.join("shared-cluster.bin")).unwrap();
        if p.join("aon-ptd.bin").exists() {
            let mut aon = vec![0xa5; M5_25G76_AON_PTD_SIZE];
            encode_m5_25g76_aon_ptd(&mut aon).unwrap();
            assert_eq!(aon, std::fs::read(p.join("aon-ptd.bin")).unwrap());
        }
        let mut next = 0usize;
        for (offset, size) in M5_25G76_SHARED_ALLOCATIONS {
            assert_eq!(offset, next);
            next = (offset + size + 63) & !63;
        }
        assert_eq!(next, M5_25G76_SHARED_PREFIX_SIZE);
        let bindings = M5PolicyBindings { state: word(&hardware, 0x8ed8),
            aon_ptd: word(&hardware, 0xb6a0), boot_timestamp: word(&hardware, 0xb75c) };
        let mut policy = vec![0xa5; hardware.len()];
        encode_m5_25g76_bundle_policy(bindings, &mut policy).unwrap();
        assert_eq!(&policy[0x2740..M5_25G76_SHARED_PREFIX_SIZE],
                   &hardware[0x2740..M5_25G76_SHARED_PREFIX_SIZE]);
        assert!(policy[..0x2740].iter().all(|&v| v == 0xa5));
        assert!(policy[M5_25G76_SHARED_PREFIX_SIZE..].iter().all(|&v| v == 0xa5));
        let policy_before = policy.clone();
        let collision = M5PolicyBindings { aon_ptd: bindings.state + 0x40, ..bindings };
        assert_eq!(encode_m5_25g76_bundle_policy(collision, &mut policy),
                   Err(InitdataError::PairAliasedObject));
        assert_eq!(policy, policy_before);
        let relocated = M5PolicyBindings { state: 0xfffffc2008000000,
            aon_ptd: 0xfffffc2009000000, boot_timestamp: 0x1234567890 };
        encode_m5_25g76_bundle_policy(relocated, &mut policy).unwrap();
        assert_eq!(word(&policy, 0x8ed8), relocated.state);
        assert_eq!(word(&policy, 0xb6a0), relocated.aon_ptd);
        assert_eq!(word(&policy, 0xb75c), relocated.boot_timestamp);
        let mut region_c = vec![0; REGION_C_SIZE];
        encode_region_c_for_profile(RootProfile::J813G17G25G76, &mut region_c).unwrap();
        assert_eq!(region_c, std::fs::read(p.join("region-c.bin")).unwrap(),
                   "M5 region C differs from pre-handoff capture");
        let mut status_a = vec![0; STATUS_BLOCK_SIZE];
        encode_status_block(&mut status_a).unwrap();
        assert_eq!(status_a, std::fs::read(p.join("status-a.bin")).unwrap());
        if index == 1 {
            let mut extra = vec![0; STATUS_BLOCK_SIZE];
            encode_secondary_root_extra_1_for_profile(RootProfile::J813G17G25G76, &mut extra).unwrap();
            assert_eq!(extra, std::fs::read(p.join("secondary-extra-1.bin")).unwrap());
        } else {
            let mut native_status = std::fs::read(p.join("status-b.bin")).unwrap();
            let mut status = vec![0xa5; PRIMARY_STATUS_B_OBJECT_SIZE];
            encode_m5_25g76_primary_status_b(word(&native_status, PRIMARY_STATUS_B_FWCTL_STATE),
                word(&native_status, PRIMARY_STATUS_B_FWCTL_RING), &mut status).unwrap();
            // GFX1 writes these after its own handoff. A primary capture can
            // occur later, so compare only the pre-start host-owned state.
            native_status[0xeab0..0xeac4].fill(0);
            assert_eq!(status, native_status);
            let secondary_extra = std::fs::read(std::path::Path::new(&args[2])
                .join("secondary-extra-0.bin")).unwrap();
            assert_eq!(&status[0xe440..0xe4c0], &secondary_extra);
        }
        for name in ["region-a.bin", "qos.bin", "completion-0.bin", "completion-1.bin",
                     "completion-2.bin", "completion-3.bin"] {
            let zeroed = p.join(name);
            // Earlier captures did not include the completion/QoS backing.
            if zeroed.exists() {
                assert!(std::fs::read(zeroed).unwrap().iter().all(|&v| v == 0),
                        "M5 pre-handoff backing was not zero-initialized: {name}");
            }
        }
        let mut registers = vec![0; HW_REGISTER_MAP_SLOTS * HW_REGISTER_MAP_ENTRY_SIZE];
        encode_m5_25g76_register_map(&mut registers).unwrap();
        assert_eq!(&registers, &hardware[HW_REGISTER_MAP_OFFSET..HW_REGISTER_MAP_OFFSET + registers.len()],
                   "M5 register table differs from pre-handoff hardware evidence");
        assert_eq!(encode_m5_25g76_register_map(&mut registers[..0x100]), Err(InitdataError::BufferSize));
        let mut aliases = [G17PKsmCompletionAliases {low: 0, high: 0}; 4];
        for (i, alias) in aliases.iter_mut().enumerate() {
            *alias = G17PKsmCompletionAliases { low: word(&hardware,0x2614+i*32),
                                             high: word(&hardware,0x261c+i*32) };
        }
        let mut completion = [0xa5; 0x80];
        encode_m5_25g76_completion_table(aliases, &mut completion).unwrap();
        assert_eq!(&completion, &hardware[0x2614..0x2694]);
        let mut overlapping = aliases;
        overlapping[3].high = overlapping[0].high + 0x4000;
        let before = completion;
        assert_eq!(encode_m5_25g76_completion_table(overlapping, &mut completion),
                   Err(InitdataError::PairAliasedObject));
        assert_eq!(completion, before, "invalid aliases partially rewrote the table");
        let mut overflowing = aliases;
        overflowing[3].high = u64::MAX - 0x4000;
        assert_eq!(encode_m5_25g76_completion_table(overflowing, &mut completion),
                   Err(InitdataError::AddressOutOfRange));
        assert_eq!(completion, before);
        let mut rebuilt_hardware = vec![0; M5_25G76_HW_DATA_SIZE];
        encode_m5_25g76_hw_data(aliases, word(&hardware,0x26ac), &mut rebuilt_hardware).unwrap();
        assert_eq!(rebuilt_hardware, hardware[..M5_25G76_HW_DATA_SIZE],
                   "M5 hardware-data differs from pre-handoff capture");
        let hardware_before = rebuilt_hardware.clone();
        assert_eq!(encode_m5_25g76_hw_data(aliases, aliases[0].high, &mut rebuilt_hardware),
                   Err(InitdataError::PairAliasedObject));
        assert_eq!(rebuilt_hardware, hardware_before);
        let role = if index == 0 { InstanceRole::Primary } else { InstanceRole::Secondary };
        let shared = SharedAddresses { region_a: word(&raw, 8), region_c: word(&raw, 0x20),
            hw_data_bundle: word(&main, 0), hw_data_bundle_alloc: 0x28000 };
        let instance = InstanceAddresses { root: 0xfffffc20001a8000 + index as u64 * 0x8000,
            main_config: word(&raw, 0x18), status_a: word(&raw, 0xa8), status_b: word(&raw, 0xb0),
            secondary_extras: if index == 0 { [0, 0] } else { [word(&raw,0xb8), word(&raw,0xc0)] } };
        let mut channels = [ChannelEntry::EMPTY; CHANNEL_TABLE_ENTRIES];
        for (i, channel) in channels.iter_mut().enumerate() {
            let offset = 0x20 + i * 32;
            *channel = ChannelEntry {
                states: [word(&main,offset), word(&main,offset+8), word(&main,offset+16)],
                ring: word(&main,offset+24),
            };
        }
        let views = PrimaryRegionViews { sentinel_high: word(&main,0x2d0),
            alias_high: [word(&main,0x2e0),word(&main,0x2f0)] };
        let mut rebuilt_main = vec![0; MAIN_CONFIG_SIZE];
        encode_main_config_for_profile(RootProfile::J813G17G25G76, role, &shared,
            &instance, &channels, if index == 0 {Some(&views)} else {None}, &mut rebuilt_main).unwrap();
        assert_eq!(rebuilt_main, main, "M5 main configuration differs from pre-handoff capture");
        let mut encoded = vec![0; raw.len()];
        encode_root_for_profile(RootProfile::J813G17G25G76, role, &shared, &instance, &mut encoded).unwrap();
        assert_eq!(encoded, raw, "M5 root differs from the actual pre-handoff capture");
        check_firmware_root_gates_for_profile(RootProfile::J813G17G25G76, &encoded).unwrap();
        assert_eq!(check_firmware_root_gates(&encoded), Err(InitdataError::InterfaceMismatch));
        encode_root(role, &shared, &instance, &mut encoded).unwrap();
        check_firmware_root_gates(&encoded).unwrap();
        assert_eq!(check_firmware_root_gates_for_profile(RootProfile::J813G17G25G76, &encoded),
                   Err(InitdataError::InterfaceMismatch));
    }
    for case in 0..32u64 {
        let values = M5ChannelStateResetValues { uncached_state: 0xfffffc2000100000 + case * 0x4000,
            command_pointer_ring: 0xfffffc2000300000 + case * 0x4000,
            gpu_va_10: 0xfffffc2000500000 + case * 0x80,
            word_48: 0x12345600 + case as u32, gpu_va_9c: 0xfffffc2000900000 + case * 0x100 };
        let raw = encode_m5_25g76_channel_state_reset(values);
        let mut uncached = [0x5a; G17_UNCACHED_CHANNEL_RESET_WRITE_EXTENT];
        apply_g17_uncached_channel_reset(&mut uncached, 2048);
        for byte in raw.iter().chain(uncached.iter()) { print!("{byte:02x}"); }
        println!();
    }
}
'''.replace('INITDATA_PATH', json.dumps(str(driver / 'g17_initdata.rs'))).replace('SUBMISSION_ITEMS', '\n'.join(items))
    with tempfile.TemporaryDirectory(prefix='m5-gpu-codecs-') as temporary:
        temporary = Path(temporary)
        (temporary / 'main.rs').write_text(harness)
        subprocess.run(['rustc', '--edition=2021', str(temporary / 'main.rs'), '-o', str(temporary / 'verify')], check=True)
        result = subprocess.run([str(temporary / 'verify'), *map(str, captures)], capture_output=True, text=True, check=True)
    lines = result.stdout.splitlines()
    if len(lines) != 32:
        raise ValueError('Missing reset comparison cases')
    for case, line in enumerate(lines):
        values = (0xfffffc2000100000 + case * 0x4000, 0xfffffc2000300000 + case * 0x4000,
                  0xfffffc2000500000 + case * 0x80, 0x12345600 + case,
                  0xfffffc2000900000 + case * 0x100)
        cached, uncached = native_reset(code, values)
        actual = bytes.fromhex(line)
        assert actual[:0xc0] == cached, 'Linux reset differs from emulated M5 host instructions'
        assert actual[0xc0:] == uncached[:0x64], 'Uncached stores differ from M5 host instructions'
        assert uncached[0x64:] == bytes([0x5a]) * (0x80 - 0x64)
    print(json.dumps(dict(roots_matched=2, main_configs_matched=2, hardware_data_blocks_matched=2,
                         bundle_policy_blocks_matched=2, native_allocation_sizes_verified=7,
                         region_c_blocks_matched=2, status_a_blocks_matched=2,
                         secondary_extra_1_blocks_matched=1,
                         primary_status_host_fields_matched=1, secondary_extra_0_alias_matched=1,
                         aon_ptd_blocks_matched=sum((p / 'aon-ptd.bin').exists() for p in captures),
                         completion_queue_descriptors_matched=4, register_table_slots_matched=53,
                         register_table_captures_matched=2, cross_target_version_rejections=4,
                         reset_cases_matched=32, m5_cached_record_bytes=0xc0,
                         host_reset_code_sha256=code_hash, linux_runtime_admitted=False), indent=2))


if __name__ == '__main__':
    main()
