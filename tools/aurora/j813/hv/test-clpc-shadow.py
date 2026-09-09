"""Exercise the production MSR handler: readback, CPU isolation and opt-in."""
import ast
from pathlib import Path
import sys
from types import SimpleNamespace

root = Path(__file__).resolve().parent
sys.path.insert(0, str(root / 'm1n1/proxyclient'))
import m1n1.hv as module

tree = ast.parse((root / 'm1n1/proxyclient/m1n1/hv/__init__.py').read_text())
cls = next(n for n in tree.body if isinstance(n, ast.ClassDef) and n.name == 'HV')
method = next(n for n in cls.body if isinstance(n, ast.FunctionDef) and n.name == 'handle_msr')
ns = dict(vars(module))
exec(compile(ast.Module(body=[method], type_ignores=[]), str(root), 'exec'), ns)
handler = ns['handle_msr']


def forbidden(*args, **kwargs):
    raise RuntimeError('physical access requested')


def context(cpu, read=False, value=0, rt=8, crm=7):
    iss = module.ESR_ISS_MSR(0)
    iss.Op0, iss.Op1, iss.CRn, iss.CRm, iss.Op2 = 3, 7, 15, crm, 3
    iss.Rt = rt
    iss.DIR = module.MSR_DIR.READ if read else module.MSR_DIR.WRITE
    ctx = SimpleNamespace(cpu_id=cpu, elr=0x1000,
                          esr=SimpleNamespace(ISS=int(iss)), regs=[0] * 32)
    ctx.regs[rt] = value
    return ctx


hv = SimpleNamespace(u=SimpleNamespace(adt={'/chosen': SimpleNamespace(chip_id=0x8142)},
    mrs=forbidden, msr=forbidden, cpu_features=SimpleNamespace(apple_sysregs_unlocked=False)),
    p=SimpleNamespace(), _bps=[], _wps=[], sysreg={6: {}, 7: {}},
    j813_kernelcache_verified=True, m5_clpc_sampler_shadow=True,
    MSR_REDIRECTS={}, hook_exceptions=False, log=lambda *a: None)
hv.ctx = context(6, value=0x8a00)
assert handler(hv, hv.ctx) and hv.ctx.elr == 0x1004
hv.ctx = context(6, read=True)
assert handler(hv, hv.ctx) and hv.ctx.regs[8] == 0x8a00
hv.ctx = context(7, read=True)
assert handler(hv, hv.ctx) and hv.ctx.regs[8] == 0
hv.ctx = context(6, value=0x8200, crm=9)
assert handler(hv, hv.ctx)
hv.ctx = context(6, read=True, crm=9)
assert handler(hv, hv.ctx) and hv.ctx.regs[8] == 0x8200
hv.ctx = context(6, read=True)
assert handler(hv, hv.ctx) and hv.ctx.regs[8] == 0x8a00
for field, value in [('m5_clpc_sampler_shadow', False), ('j813_kernelcache_verified', False)]:
    setattr(hv, field, value)
    hv.ctx = context(6, read=True)
    try:
        handler(hv, hv.ctx)
    except RuntimeError as error:
        assert str(error) == 'physical access requested'
    else:
        raise AssertionError('shadow enabled without qualification')
    setattr(hv, field, True)
hv.u.adt['/chosen'].chip_id = 0x8140
hv.ctx = context(6, read=True)
try:
    handler(hv, hv.ctx)
except RuntimeError as error:
    assert str(error) == 'physical access requested'
else:
    raise AssertionError('M5 shadow applied to Neo')
print('Production CLPC shadow: coherent readback, per-CPU state, opt-in and exact-profile/chip isolation passed')
