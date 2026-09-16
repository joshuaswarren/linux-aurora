"""Deterministic, small EEVEE render; CPU Cycles is a separately labelled control."""
import bpy
import json
import os
from pathlib import Path

out = Path(os.environ['NEO_SUITE_OUTPUT'])
bpy.ops.wm.read_factory_settings(use_empty=True)
bpy.ops.mesh.primitive_cube_add(location=(0, 0, 0.6))
cube = bpy.context.object
cube.rotation_euler = (0.2, 0.1, 0.35)
mat = bpy.data.materials.new('Neo green')
mat.diffuse_color = (0.05, 0.7, 0.3, 1)
cube.data.materials.append(mat)
bpy.ops.mesh.primitive_plane_add(size=200)
bpy.ops.object.light_add(type='AREA', location=(2, -3, 6))
bpy.context.object.data.energy = 800
bpy.context.object.data.shape = 'DISK'
bpy.context.object.data.size = 5
bpy.ops.object.camera_add(location=(5, -7, 5))
camera = bpy.context.object
from mathutils import Vector
camera.rotation_euler = (Vector((0, 0, 0.5)) - camera.location).to_track_quat('-Z', 'Y').to_euler()
scene = bpy.context.scene
scene.camera = camera
scene.render.resolution_x = 320
scene.render.resolution_y = 240
scene.render.resolution_percentage = 100
scene.render.image_settings.file_format = 'PNG'
scene.render.engine = 'CYCLES' if os.getenv('NEO_SUITE_CPU_CONTROL') else 'BLENDER_EEVEE'
if scene.render.engine == 'CYCLES':
    scene.cycles.device = 'CPU'
    scene.cycles.samples = 8
scene.render.filepath = str(out / 'render.png')
bpy.ops.wm.save_as_mainfile(filepath=str(out / 'neo-test.blend'))
bpy.ops.render.render(write_still=True)
import gpu
info = {'blender': bpy.app.version_string, 'engine': scene.render.engine,
        'vendor': gpu.platform.vendor_get(), 'renderer': gpu.platform.renderer_get(),
        'backend': gpu.platform.backend_type_get(), 'render_exists': (out / 'render.png').is_file()}
(out / 'blender-result.json').write_text(json.dumps(info, indent=2) + '\n')
