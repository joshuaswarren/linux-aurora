"""Explicit shader-cache policy for reproducible native CTS iterations."""
from pathlib import Path
import re


def resolve_cache_policy(requested, trace=False):
    if requested not in ('auto', 'off', 'isolated'):
        raise ValueError('Unknown shader cache policy')
    resolved = ('off' if trace else 'isolated') if requested == 'auto' else requested
    if trace and resolved != 'off':
        raise ValueError('Shader tracing requires --shader-cache off')
    return resolved


def shader_cache_environment(policy, mesa_sha, kernel_sha, home='/home/mcruz'):
    if policy not in ('off', 'isolated'):
        raise ValueError('Unknown shader cache policy')
    if not all(re.fullmatch(r'[0-9a-f]{64}', value) for value in (mesa_sha, kernel_sha)):
        raise ValueError('Cache namespaces require exact binary SHA-256 hashes')
    if policy == 'off':
        return {'MESA_SHADER_CACHE_DISABLE': 'true'}
    directory = Path(home) / '.cache/neo-cts-shaders' / mesa_sha / kernel_sha
    return {
        'MESA_SHADER_CACHE_DISABLE': 'false',
        'MESA_SHADER_CACHE_DIR': str(directory),
        'MESA_SHADER_CACHE_SHOW_STATS': 'true',
    }
