"""Bounded passive AVE2 MMIO survey; load after the required J700 boot modules.

Only observes guest accesses. Does not poll, write, initialize or reset AVE.
Host-side ave-phase.json labels phases without opening a second KIS client.
"""
import json
import os
from pathlib import Path
import time
from m1n1.hv.types import TraceMode
from m1n1.utils import irange

if int(hv.u.adt['/chosen'].chip_id) != 0x8140 or hv.u.adt.model != 'Mac17,5':
    raise RuntimeError('AVE survey requires the J700/T8140 target')
_ave_dir = Path(os.environ['J700_AGX_BOOT_TRACE_LOG']).parent
_ave_log = (_ave_dir / 'ave-mmio.jsonl').open('w', buffering=1)
_ave_control = _ave_dir / 'ave-phase.json'
_ave_limit = int(os.environ.get('J700_AVE_EVENT_LIMIT', '200000'))
if not 1 <= _ave_limit <= 2000000:
    raise RuntimeError('Invalid AVE trace event limit')
_ave_sequence = 0
_ave_count = 0
_ave_phase = 'boot'
_ave_mtime = None
_ave_error_reported = False
_ave_ident = 'J700AVE2Survey'

def _ave_emit(kind, **values):
    global _ave_sequence
    _ave_sequence += 1
    _ave_log.write(json.dumps({'seq': _ave_sequence, 'host_monotonic_ns': time.monotonic_ns(),
                              'kind': kind, 'phase': _ave_phase, **values}) + '\n')

def _ave_update_phase():
    global _ave_mtime, _ave_phase
    try:
        stamp = _ave_control.stat().st_mtime_ns
        if stamp != _ave_mtime:
            value = json.loads(_ave_control.read_text())
            phase = value['phase']
            if not isinstance(phase, str) or not 1 <= len(phase) <= 128:
                raise ValueError('Invalid phase label')
            _ave_phase, _ave_mtime = phase, stamp
            _ave_emit('phase')
    except FileNotFoundError:
        pass

def _ave_callback(base, index, node_name):
    def event(evt):
        global _ave_count, _ave_error_reported
        try:
            if _ave_count >= _ave_limit:
                return
            _ave_update_phase()
            _ave_count += 1
            _ave_emit('mmio', node=node_name, region=index, cpu=int(evt.flags.CPU),
                      pc=hex(int(evt.pc)), op='W' if evt.flags.WRITE else 'R',
                      width=1 << int(evt.flags.WIDTH), address=hex(int(evt.addr)),
                      offset=hex(int(evt.addr) - base), data=hex(int(evt.data)))
            if _ave_count == _ave_limit:
                _ave_emit('limit-reached', limit=_ave_limit, complete=False)
                hv.clear_tracers(_ave_ident)
        except Exception as exc:
            if not _ave_error_reported:
                _ave_error_reported = True
                try:
                    _ave_emit('trace-error', error=repr(exc), complete=False)
                except Exception:
                    pass
            # A failed survey must not stop the guest or produce unbounded logs.
            hv.clear_tracers(_ave_ident)
    return event

_ave_nodes = [hv.adt['/arm-io/ave']]
_ave_trace_dart = os.environ.get('J700_AVE_TRACE_DART', '0') == '1'
if _ave_trace_dart:
    _ave_nodes += [n for n in hv.adt['/arm-io'] if n.name.startswith('dart') and 'ave' in n.name]
_ave_topology = {'target': 'J700/T8140', 'nodes': []}
for _ave_node in _ave_nodes:
    _ave_regions = []
    for _ave_index in range(len(_ave_node.reg)):
        _ave_base, _ave_size = map(int, _ave_node.get_reg(_ave_index))
        if not _ave_base or not 0 < _ave_size <= 32 * 1024 * 1024:
            raise RuntimeError('Unexpected AVE/DART register range')
        _ave_regions.append({'index': _ave_index, 'base': hex(_ave_base), 'size': hex(_ave_size)})
        # Keep the first survey off the shared PMGR windows and large SRAM
        # apertures. These are metadata only until individually qualified.
        _ave_selected = (_ave_node.name == 'ave' and _ave_index in (0, 2)) or (
            _ave_node.name.startswith('dart') and _ave_index < 4)
        _ave_regions[-1]['traced'] = _ave_selected
        if not _ave_selected:
            continue
        _ave_event = _ave_callback(_ave_base, _ave_index, _ave_node.name)
        hv.add_tracer(irange(_ave_base, _ave_size), _ave_ident, TraceMode.ASYNC,
                      read=None, write=_ave_event)
    _ave_properties = {}
    for _ave_key in ['compatible', 'interrupts', 'clock-gates', 'power-gates', 'clock-ids',
                     'iommu-parent', 'pre-loaded', 'segment-names', 'segment-ranges']:
        _ave_value = _ave_node.getprop(_ave_key)
        if _ave_value is not None:
            _ave_properties[_ave_key] = _ave_value.hex() if isinstance(_ave_value, bytes) else str(_ave_value)
    _ave_topology['nodes'].append({'name': _ave_node.name, 'regions': _ave_regions,
                                  'properties': _ave_properties})
(_ave_dir / 'ave-topology.json').write_text(json.dumps(_ave_topology, indent=2) + '\n')
_ave_emit('armed', event_limit=_ave_limit, nodes=[n.name for n in _ave_nodes])
print(f'J700_AVE_SURVEY_ARMED nodes={len(_ave_nodes)} limit={_ave_limit}', flush=True)
