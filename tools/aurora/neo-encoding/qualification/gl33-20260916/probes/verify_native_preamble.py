#!/usr/bin/env python3
"""Check selected native G17P encoder witnesses, not whole-program correctness."""
import json
import importlib.util
from pathlib import Path

from native_metal import NativeMetal

schema_spec = importlib.util.spec_from_file_location(
    'native_shader_schema', Path(__file__).with_name('inspect-native-shader-schema.py'))
schema_module = importlib.util.module_from_spec(schema_spec)
schema_spec.loader.exec_module(schema_module)
schema = schema_module.inspect(schema_module.default_compiler())
for entry in schema['schemas']:
    phase = {field['id']: field for field in
             entry['objects']['AGCCodeGenerator.ShaderPhase']}
    assert phase[2]['name'] == 'temporary_register_count'
    assert phase[3]['name'] == 'cprog_uniform_inputs'
    assert phase[3]['base_type'] == 14 and phase[3]['element_type'] == 8

reader = NativeMetal()
witnesses = {
    '33setConstantCalculationProgramInfo': {
        0x216b5c110: ('ldp', 'w10, w9, [x1, #4]'),
        0x216b5c114: ('ldr', 'x11, [x1, #0x20]'),
        0x216b5c120: ('csel', 'x21, x9, x11, hi'),
        0x216b5c13c: ('b.hi', '#0x216b5c220'),
        0x216b5c140: ('ldrb', 'w9, [x20, #0x28]'),
        0x216b5c144: ('tbnz', 'w9, #0, #0x216b5c220'),
        0x216b5c178: ('ldrh', 'w8, [x24, #-2]!'),
        0x216b5c184: ('b.eq', '#0x216b5c168'),
        0x216b5c214: ('str', 'x21, [x19, #0x110]'),
        0x216b5c228: ('str', 'x9, [x19, #0x108]'),
        0x216b5c230: ('ldrb', 'w8, [x20, #0x29]'),
        0x216b5c240: ('strb', 'w8, [x19, #0x11c]'),
        0x216b5c274: ('mov', 'w8, #0x100'),
    },
    'EE6finishEv': {
        0x216b5b4e4: ('ldp', 'x1, x8, [x28, #0x18]!'),
        0x216b5b4ec: ('b.eq', '#0x216b5b57c'),
        0x216b5b57c: ('ldr', 'x8, [x19, #0x108]'),
        0x216b5b5a0: ('mov', 'w9, #0xe'),
        0x216b5b5b8: ('mov', 'w4, #4'),
        0x216b5b5dc: ('ldrb', 'w9, [x19, #0x11c]'),
        0x216b5b61c: ('bl', '#0x216b0ed78'),
        0x216b5b654: ('bfi', 'x9, x8, #1, #0x30'),
        0x216b5b674: ('bl', '#0x216b123fc'),
    },
    # The compute variant embeds its ESL object at +0x9d8. The metadata
    # below is therefore at owner +0xef8, not a directly initialized field
    # of the outer constructor. Do not infer its meaning from the offset.
    '21ComputeProgramVariantINS_6HAL2008EncodersENS1_7ClassesEEC2': {
        0x216ba7780: ('add', 'x0, x26, #0x9d8'),
        0x216ba77cc: ('bl', '#0x216be7de0'),
    },
    '30UserComputeArgumentTableLayoutELb1ELm8EEC2': {
        0x216be7e18: ('mov', 'x26, x0'),
        0x216beb2e8: ('ldrh', 'w8, [x8, #0x38]'),
        0x216beb308: ('ldr', 'w9, [x8]'),
        0x216beb30c: ('cbz', 'w9, #0x216beb418'),
        0x216beb374: ('ldr', 'w9, [x8, #4]!'),
        0x216beb3ac: ('ldr', 'w9, [x8]'),
        0x216beb3b0: ('add', 'x8, x8, x9'),
        0x216beb3b4: ('ldrsw', 'x9, [x8]'),
        0x216beb3b8: ('neg', 'x9, x9'),
        0x216beb3bc: ('add', 'x10, x8, x9'),
        0x216beb3c0: ('ldrh', 'w9, [x10, #0xa]'),
        0x216beb3c4: ('add', 'x8, x8, x9'),
        0x216beb3c8: ('ldr', 'w9, [x8]'),
        0x216beb3cc: ('add', 'x8, x8, x9'),
        0x216beb3d0: ('ldr', 'w8, [x8]'),
        0x216beb3d4: ('str', 'w8, [x26, #0x520]'),
    },
    '21ComputeProgramVariantINS_6HAL2008EncodersENS1_7ClassesEE8finalize': {
        0x216bab574: ('ldr', 'w8, [x19, #0xef8]'),
        0x216bab578: ('cmp', 'w8, #0'),
        0x216bab57c: ('cset', 'w8, ne'),
        0x216bab580: ('strb', 'w8, [sp, #0x59]'),
        0x216bab588: ('add', 'x1, sp, #0x30'),
        0x216bab58c: ('bl', '#0x216b5c0dc'),
    },
    # The vertex launch word is not the AGX2 Counts layout. Its overlapping
    # 12-bit field comes from serialized scalar 37 via ESL state metadata.
    '29UserVertexArgumentTableLayoutELb1ELm8EEC2': {
        0x216bdc660: ('mov', 'x26, x0'),
        0x216bdce2c: ('ldrsw', 'x8, [x10]'),
        0x216bdce30: ('neg', 'x9, x8'),
        0x216bdce34: ('add', 'x8, x10, x9'),
        0x216bdce38: ('ldrh', 'w9, [x8]'),
        0x216bdce3c: ('cmp', 'w9, #0x43'),
        0x216bdce40: ('b.lo', '#0x216bdce58'),
        0x216bdce44: ('ldrh', 'w8, [x8, #0x42]'),
        0x216bdce48: ('cbz', 'x8, #0x216bdce5c'),
        0x216bdce4c: ('ldr', 'x9, [sp, #0x80]'),
        0x216bdce50: ('ldr', 'w8, [x9, x8]'),
        0x216bdce58: ('mov', 'w8, #0'),
        0x216bdce5c: ('str', 'w8, [x26, #0x260]'),
        0x216bdcebc: ('ldr', 'w8, [x26, #0x260]'),
        0x216bdcec0: ('str', 'w8, [x26, #0x26c]'),
        0x216bdcf34: ('ldr', 'x10, [sp, #0x80]'),
        0x216bdcf38: ('ldrsw', 'x8, [x10]'),
        0x216bdcf3c: ('neg', 'x9, x8'),
        0x216bdcf40: ('add', 'x8, x10, x9'),
        0x216bdcf44: ('ldrh', 'w9, [x8]'),
        0x216bdcf48: ('cmp', 'w9, #0x4f'),
        0x216bdcf4c: ('b.lo', '#0x216bdcf64'),
        0x216bdcf50: ('ldrh', 'w8, [x8, #0x4e]'),
        0x216bdcf54: ('cbz', 'x8, #0x216bdcf68'),
        0x216bdcf58: ('ldr', 'x9, [sp, #0x80]'),
        0x216bdcf5c: ('ldr', 'w8, [x9, x8]'),
        0x216bdcf64: ('mov', 'w8, #0'),
        0x216bdcf68: ('str', 'w8, [x26, #0x278]'),
    },
    '20VertexProgramVariantINS_6HAL2008EncodersENS1_7ClassesEE8finalize': {
        0x216b8b5f0: ('ldr', 'x8, [x19, #0x9d8]'),
        0x216b8b5f8: ('ldr', 'w10, [x19, #0xc44]'),
        0x216b8b600: ('ldr', 'w12, [x19, #0xc50]'),
        0x216b8b638: ('add', 'w9, w10, #3'),
        0x216b8b63c: ('lsr', 'w9, w9, #2'),
        0x216b8b640: ('str', 'w9, [x8, #0x128]'),
        0x216b8b644: ('add', 'w9, w12, #3'),
        0x216b8b648: ('lsr', 'w9, w9, #2'),
        0x216b8b64c: ('add', 'w9, w9, #1'),
        0x216b8b650: ('and', 'w9, w9, #0x7ffffffe'),
        0x216b8b654: ('str', 'w9, [x8, #0x12c]'),
        0x216b8b810: ('ldr', 'x11, [x19, #0x9d8]'),
        0x216b8b814: ('ldr', 'w9, [x11, #0x128]'),
        0x216b8b818: ('ldr', 'w12, [x11, #0x12c]'),
        0x216b8b838: ('bfi', 'w15, w12, #8, #0xc'),
        0x216b8b83c: ('ubfiz', 'w12, w9, #3, #0x12'),
        0x216b8b884: ('mov', 'v1.s[1], w15'),
        0x216b8b888: ('str', 'd1, [x19, #0xf30]'),
        0x216b8b88c: ('str', 'w12, [x19, #0xf38]'),
    },
}
checked = 0
for fragment, expected in witnesses.items():
    name, instructions = reader.function(fragment)
    decoded = {i.address: (i.mnemonic, i.op_str) for i in instructions}
    for address, value in expected.items():
        assert decoded.get(address) == value, (hex(address), value,
                                                decoded.get(address))
        checked += 1
symbols = dict(reader.symbols())
assert symbols[0x216b123fc] == '__ZNK22AGXIotoInstruction_B_04emitEPhb'
assert symbols[0x216b12500] == '__ZNK24AGXIotoInstruction_NOP_04emitEPhb'
assert symbols[0x216b0ed78] == '__ZNK24AGXIotoInstruction_LDIMM4emitEPh'
print(json.dumps({'binary_sha256': reader.sha256, 'instruction_witnesses': checked,
                  'callee_symbols': 3, 'status': 'pass',
                  'compiler_schema_sha256': schema['compiler_sha256'],
                  'compiler_schemas_verified': len(schema['schemas']),
                  'prelude_metadata': {
                      'esl_owner_offset': '0x9d8',
                      'esl_field_offset': '0x520',
                      'owner_field_offset': '0xef8',
                      'source': 'first table in vtable 0x38 vector; length of its vtable 0x0a vector',
                      'vector_field': 'ShaderPhase.cprog_uniform_inputs: vector<uint32>',
                      'vector_element_units': 'not independently proven by schema names',
                  },
                  'vertex_state_count': {
                      'serialized_scalar': 37,
                      'serialized_vtable_offset': '0x4e',
                      'esl_field_offset': '0x278',
                      'owner_field_offset': '0xc50',
                      'encoder_field_offset': '0x12c',
                      'word_bits': '8:19',
                      'rounding': 'round_up(ceil(scalar / 4), 2), for non-overflowing scalar',
                      'higher_level_meaning': 'thread_invariant_largest_ipr_bytes; distinct from temporary_register_count',
                  },
                  'vertex_spill_count': {
                      'serialized_scalar': 31,
                      'serialized_vtable_offset': '0x42',
                      'esl_field_offsets': ['0x260', '0x26c'],
                      'owner_field_offset': '0xc44',
                      'encoder_field_offset': '0x128',
                      'pipeline_word_bits': '3:20',
                      'rounding': 'ceil(scalar / 4), for non-overflowing scalar',
                      'higher_level_meaning': 'thread_invariant_spill_buffer_bytes',
                  },
                  'scope': 'host macOS 27 G17P HAL200 static encoder witnesses'}))
