#!/usr/bin/env python3
"""Record Core ML's proposed placement on this host, not Linux hardware proof."""
import argparse
import collections
import json
from pathlib import Path
import shutil

import coremltools as ct
from coremltools.models.compute_plan import MLComputePlan

ap=argparse.ArgumentParser(description=__doc__)
ap.add_argument('model',type=Path)
ap.add_argument('output',type=Path)
args=ap.parse_args()
compiled=ct.models.utils.compile_model(str(args.model.resolve()))
try:
    plan=MLComputePlan.load_from_path(compiled,compute_units=ct.ComputeUnit.CPU_AND_NE)
    rows=[]
    def walk(block):
        for op in block.operations:
            usage=plan.get_compute_device_usage_for_mlprogram_operation(op)
            cost=plan.get_estimated_cost_for_mlprogram_operation(op)
            rows.append({'op':op.operator_name,
                         'device':type(usage.preferred_compute_device).__name__ if usage else None,
                         'cost':cost.weight if cost else None})
            for child in op.blocks:
                walk(child)
    for function in plan.model_structure.program.functions.values():
        walk(function.block)
    result={'counts':dict(collections.Counter(r['device'] for r in rows)),
            'operations':rows,'linux_ane_tested':False}
    args.output.write_text(json.dumps(result,indent=2)+'\n')
    print(json.dumps(result['counts']))
finally:
    shutil.rmtree(compiled)
