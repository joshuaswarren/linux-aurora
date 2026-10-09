#!/usr/bin/env python3
"""Review GL33 NotSupported results against the pinned CTS source predicates.

This is an applicability review, not the official verdict or session-accounting
gate. It neither changes QPA files nor converts NotSupported results to Pass.
See GL33-APPLICABILITY-AUDIT.md for source references and reasoning.
"""
import argparse
from collections import Counter
import hashlib
import json
from pathlib import Path
import re
import xml.etree.ElementTree as ET

CTS_COMMIT = '067e8832315e79817ede1c4863804e440f5d1c80'
PREFIX = 'KHR-GL33.'
LATER_FORMATS = {8, 9, 10, 13, 14, 17, 18, 19, 24, 69, 70}
MINMAX = {
    'TextureFilterMinmaxParameterQueries',
    'TextureFilterMinmaxMinificationFiltering',
    'TextureFilterMinmaxMagnificationFiltering',
    'TextureFilterMinmaxMipmapMinificationFiltering',
    'TextureFilterMinmaxSupport',
}


def swizzle_exclusion(case):
    """gl3cTextureSwizzleTests.cpp: tables and guards, pinned revision only."""
    m = re.fullmatch(PREFIX + r'texture_swizzle\.smoke_access_idx_(\d+)_channel_idx_(\d+)', case)
    if m:
        access, channel = map(int, m.groups())
        if channel < 4 and access in {1, 6, 8, 9, 12, 13}:
            return 'swizzle_projective_array_access'
        return None
    m = re.fullmatch(PREFIX + r'texture_swizzle\.functional_format_idx_(\d+)_target_idx_(\d+)', case)
    if not m:
        return None
    fmt, target = map(int, m.groups())
    if not (0 <= fmt <= 70 and 0 <= target <= 8):
        return None
    if fmt in LATER_FORMATS:
        return 'swizzle_format_version_metadata'
    if 63 <= fmt <= 68 and target in {2, 7, 8}:
        return 'swizzle_excluded_depth_target'
    if fmt == 38 and target in {7, 8}:
        return 'swizzle_excluded_shared_exponent_msaa'
    if target in {5, 6}:
        return 'swizzle_fixed_texel_fetch_target'
    return None


def classify(case, message, depth_bits, extensions):
    exclusion = swizzle_exclusion(case)
    if exclusion:
        expected_message = ('Format not supported' if exclusion == 'swizzle_format_version_metadata'
                            else 'Target not supported')
        return exclusion if message == expected_message else None
    fixed = {
        PREFIX + 'CommonBugs.CommonBug_PerVertexValidation':
            ('Test implemented for OpenGL 4.1 contexts or newer.', 'gl41_test_metadata'),
        PREFIX + 'shaders.shader_integer_mix.prototypes':
            ('NotSupported', 'glsl450_core_prototype_metadata'),
        PREFIX + 'pipeline_statistics_query_tests_ARB.functional_tess_queries':
            ('OpenGL 4.0+ is required to run this test.', 'gl40_test_metadata'),
    }
    if case in fixed:
        expected_message, reason = fixed[case]
        return reason if message == expected_message else None
    if case in {PREFIX + 'packed_depth_stencil.stencil_texturing.' + fmt
                for fmt in ('depth24_stencil8', 'depth32f_stencil8')}:
        return ('gl43_stencil_test_metadata'
                if message == 'stencil_texturing extension is not supported' else None)
    if re.fullmatch(PREFIX + r'cull_distance\.functional_test_item_8_primitive_mode_(lines|triangles)_max_culldist_[0-7]', case):
        return 'cull_fetch_nonpoint_combination' if message == 'Not supported' else None
    if case.startswith(PREFIX + 'shaders.fragdepth.') and depth_bits == 0:
        if re.fullmatch(r"Depth buffer is required: '' at glcFragDepthTests.cpp:(155|312)", message):
            return 'configuration_has_no_depth'
    if case in {PREFIX + 'texture_filter_minmax_tests.' + name for name in MINMAX}:
        if message == 'Not Supported' and 'GL_ARB_texture_filter_minmax' not in extensions:
            return 'unadvertised_optional_texture_filter_minmax'
    return None


def audit(path, expected_cases):
    data = path.read_bytes()
    text = data.decode(errors='replace')
    issues = []
    if not re.search(r'^#sessionInfo releaseName opengl-cts-4\.6\.8\.1-0-g' + CTS_COMMIT + r'\s*$', text, re.M):
        issues.append('CTS release does not match the audited source revision')
    if len(re.findall(r'^#beginSession\s*$', text, re.M)) != 1 or len(re.findall(r'^#endSession\s*$', text, re.M)) != 1:
        issues.append('QPA does not contain one completed session')
    chunks = re.split(r'^#beginTestCaseResult ([^\n]+)\n', text, flags=re.M)
    results = {}
    for i in range(1, len(chunks), 2):
        case, body = chunks[i].strip(), chunks[i + 1]
        if case in results:
            issues.append('Duplicate case: ' + case)
        try:
            if '#endTestCaseResult' not in body:
                raise ValueError('missing result terminator')
            xml = ET.fromstring(body.split('#endTestCaseResult', 1)[0].strip())
            result = xml.find('Result')
            if xml.get('CasePath') != case or result is None:
                raise ValueError('invalid case/result identity')
            results[case] = {
                'status': result.attrib['StatusCode'],
                'message': ''.join(result.itertext()).strip(),
                'texts': [''.join(t.itertext()).strip() for t in xml.findall('Text')],
                'numbers': {n.attrib['Name']: n.text for n in xml.findall('Number')},
            }
        except (ET.ParseError, ValueError, KeyError) as exc:
            issues.append('Malformed case: ' + case + ': ' + str(exc))
    if set(results) != expected_cases:
        issues.append('Case inventory mismatch: missing=%d extra=%d' %
                      (len(expected_cases - set(results)), len(set(results) - expected_cases)))
    depth = results.get(PREFIX + 'info.render_target', {}).get('numbers', {}).get('DepthBits')
    depth = int(depth) if depth is not None and depth.isdigit() else None
    extensions = set(results.get(PREFIX + 'info.extensions', {}).get('texts', []))
    if depth is None or not extensions:
        issues.append('Missing render-target depth or extension capability evidence')
    groups, statuses = Counter(), Counter()
    unknown = []
    depth_results, mix_results, cull_results = Counter(), Counter(), Counter()
    swizzle_checked = 0
    for case, result in results.items():
        status = result['status']
        statuses[status] += 1
        if status not in {'Pass', 'NotSupported'}:
            issues.append(case + ': nonpassing status ' + status)
        if '.texture_swizzle.functional_format_idx_' in case or '.texture_swizzle.smoke_access_idx_' in case:
            swizzle_checked += 1
            expected_status = 'NotSupported' if swizzle_exclusion(case) else 'Pass'
            if status != expected_status:
                issues.append(case + ': swizzle predicate expects ' + expected_status + ', observed ' + status)
        if case.startswith(PREFIX + 'shaders.fragdepth.'):
            depth_results[status] += 1
            if depth and status != 'Pass':
                issues.append(case + ': depth-bearing target requires execution and Pass')
        if case.startswith(PREFIX + 'shaders.shader_integer_mix.'):
            mix_results[status] += 1
        if case.startswith(PREFIX + 'cull_distance.'):
            cull_results[status] += 1
        if status == 'NotSupported':
            reason = classify(case, result['message'], depth, extensions)
            if reason:
                groups[reason] += 1
            else:
                unknown.append({'case': case, 'message': result['message']})
    if unknown:
        issues.append('Unexplained NotSupported cases: ' + str(len(unknown)))
    return {
        'path': str(path.resolve()), 'sha256': hashlib.sha256(data).hexdigest(),
        'counts': dict(statuses), 'unsupported_groups': dict(groups),
        'depth_bits': depth, 'depth_results': dict(depth_results),
        'integer_mix_results': dict(mix_results), 'cull_distance_results': dict(cull_results),
        'swizzle_predicate_cases_checked': swizzle_checked,
        'renderer': results.get(PREFIX + 'info.renderer', {}).get('texts', []),
        'reported_version': results.get(PREFIX + 'info.version', {}).get('texts', []),
        'optional_extensions': {name: name in extensions for name in (
            'GL_EXT_shader_integer_mix', 'GL_ARB_stencil_texturing', 'GL_ARB_texture_filter_minmax')},
        'unknown_unsupported': unknown, 'issues': issues,
    }


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('paths', type=Path, nargs='+', help='completed main QPAs, or final run directories')
    parser.add_argument('--output', type=Path, required=True)
    args = parser.parse_args()
    expected = set((Path(__file__).resolve().parent / 'gl33-main.txt').read_text().splitlines())
    assert len(expected) == 9886
    paths = []
    for path in args.paths:
        paths.extend(sorted(path.glob('config-gl33-main-*.qpa')) if path.is_dir() else [path])
    if not paths or len({p.resolve() for p in paths}) != len(paths):
        parser.error('supply at least one QPA without duplicate paths')
    files = [audit(path, expected) for path in paths]
    totals = Counter()
    groups = Counter()
    for item in files:
        totals.update(item['counts'])
        groups.update(item['unsupported_groups'])
    out = {'cts_commit': CTS_COMMIT, 'files_reviewed': len(files),
           'counts': dict(totals), 'unsupported_groups': dict(groups),
           'no_applicability_issues': not any(f['issues'] for f in files),
           'scope': 'Only supplied files. Does not establish full campaign completion, official conformance, or GL4.6 coverage.',
           'files': files}
    args.output.write_text(json.dumps(out, indent=2) + '\n')
    print(json.dumps({k: out[k] for k in ('files_reviewed', 'counts', 'unsupported_groups', 'no_applicability_issues')}))
    return 0 if out['no_applicability_issues'] else 1


if __name__ == '__main__':
    raise SystemExit(main())
