"""Install the reviewed MSR handler on a paused exact M5 sampling trap."""
import ast
import builtins
from pathlib import Path
from types import MethodType
import m1n1.hv as module

if int(hv.adt['/chosen'].chip_id) != 0x8142 or not hv.j813_kernelcache_verified:
    raise RuntimeError('Requires exact M5 kernel profile')
sites = {0xfffffe0009bb6ad0: bytes.fromhex('68f71fd5'),
         0xfffffe0009bb6ad8: bytes.fromhex('68f91fd5')}
if hv.ctx is None or int(hv.ctx.elr) not in sites:
    raise RuntimeError('Requires the captured CorePerfSampler trap')
original_pc = int(hv.ctx.elr)
if hv.readmem(hv.ctx.elr, 4) != sites[original_pc]:
    raise RuntimeError('CorePerfSampler instruction mismatch')
source = Path(__file__).resolve().parent / 'm1n1/proxyclient/m1n1/hv/__init__.py'
tree = ast.parse(source.read_text())
cls = next(n for n in tree.body if isinstance(n, ast.ClassDef) and n.name == 'HV')
method = next(n for n in cls.body if isinstance(n, ast.FunctionDef) and n.name == 'handle_msr')
ns = dict(vars(module))
builtins.exec(compile(ast.Module(body=[method], type_ignores=[]), str(source), 'exec'), ns)
hv.handle_msr = MethodType(ns['handle_msr'], hv)
hv.m5_clpc_sampler_shadow = True
if not hv.handle_msr(hv.ctx) or int(hv.ctx.elr) != original_pc + 4:
    raise RuntimeError('Failed to handle the captured sampler trap')
print('M5_CLPC_SAMPLER_TRAP_HANDLED: guest remains paused')
