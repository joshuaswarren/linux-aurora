#!/usr/bin/env python3
"""Run isolated, bounded application checks on J700; retain every test receipt."""
import argparse
import base64
import contextlib
import datetime
import http.server
import hashlib
import json
import os
from pathlib import Path
import re
import secrets
import shutil
import signal
import subprocess
import threading
import time
import urllib.parse

HERE = Path(__file__).resolve().parent

def run(command, output, timeout=30, env=None):
    with output.open('wb') as stream:
        proc = subprocess.Popen(command, stdout=stream, stderr=subprocess.STDOUT,
                                env=env, start_new_session=True)
        try:
            return proc.wait(timeout=timeout)
        except subprocess.TimeoutExpired:
            os.killpg(proc.pid, signal.SIGTERM)
            try:
                proc.wait(timeout=5)
            except subprocess.TimeoutExpired:
                os.killpg(proc.pid, signal.SIGKILL)
                proc.wait()
            return 124

def inventory(out):
    receipts = {}
    for name, command in {
        'kernel': ['uname', '-a'], 'packages': ['pacman', '-Q'],
        'opengl': ['glxinfo', '-B'], 'vulkan': ['vulkaninfo', '--summary'],
        'chrome': ['google-chrome-stable', '--version'],
        'firefox': ['firefox', '--version'], 'blender': ['blender', '--version'],
        'obs': ['obs', '--version'],
    }.items():
        receipts[name] = {'available': shutil.which(command[0]) is not None}
        if receipts[name]['available']:
            receipts[name]['exit'] = run(command, out / (name + '.log'))
    (out / 'environment.json').write_text(json.dumps({k: os.environ.get(k) for k in
        ['DISPLAY', 'XDG_SESSION_TYPE', 'GALLIUM_DRIVER', 'MESA_LOADER_DRIVER_OVERRIDE',
         'VK_DRIVER_FILES', 'LIBGL_DRIVERS_PATH', 'LD_LIBRARY_PATH']}, indent=2) + '\n')
    return receipts

def browser(name, out, angle='default'):
    token = secrets.token_urlsafe(24)
    result = {}
    received = threading.Event()
    class Handler(http.server.BaseHTTPRequestHandler):
        def log_message(self, *_): pass
        def do_GET(self):
            body = (HERE / 'browser.html').read_bytes()
            self.send_response(200); self.send_header('Content-Type', 'text/html')
            self.send_header('Content-Length', str(len(body))); self.end_headers(); self.wfile.write(body)
        def do_POST(self):
            if self.path != '/result?token=' + token:
                self.send_error(403); return
            length = int(self.headers.get('Content-Length', '0'))
            if not 0 < length < 100000:
                self.send_error(400); return
            try:
                data = json.loads(self.rfile.read(length))
                assert isinstance(data, dict) and isinstance(data['tests'], list)
            except (ValueError, KeyError, AssertionError):
                self.send_error(400); return
            result.update(data); self.send_response(204); self.end_headers(); received.set()
    server = http.server.HTTPServer(('127.0.0.1', 0), Handler)
    thread = threading.Thread(target=server.serve_forever, daemon=True); thread.start()
    profile = out / 'profile'; profile.mkdir()
    url = f'http://127.0.0.1:{server.server_port}/?token={token}'
    if name == 'chrome':
        command = ['google-chrome-stable', '--user-data-dir=' + str(profile), '--no-first-run',
                   '--disable-background-networking', '--new-window', url]
        if angle != 'default':
            command += ['--use-gl=angle', '--use-angle=' + angle]
    else:
        command = ['firefox', '--no-remote', '--profile', str(profile), '--new-window', url]
    try:
        with (out / 'browser.log').open('wb') as stream:
            proc = subprocess.Popen(command, stdout=stream, stderr=subprocess.STDOUT, start_new_session=True)
            try:
                ready = received.wait(90)
                result['report_received'] = ready
                result['launch_exit'] = proc.poll()
                result['requested_angle_backend'] = angle if name == 'chrome' else None
                result['render_pass'] = ready and len(result.get('tests', [])) == 2 and all(t.get('passed') for t in result['tests'])
                result['hardware_renderer_reported'] = ready and all(
                    not t.get('softwareRenderer', True) and re.search('Apple|AGX|Asahi|G17|T8140', t.get('unmaskedRenderer', ''), re.I)
                    is not None for t in result.get('tests', []))
                (out / 'browser-result.json').write_text(json.dumps(result, indent=2) + '\n')
            finally:
                if proc.poll() is None:
                    os.killpg(proc.pid, signal.SIGTERM)
                    try: proc.wait(timeout=10)
                    except subprocess.TimeoutExpired: os.killpg(proc.pid, signal.SIGKILL); proc.wait()
    finally:
        server.shutdown(); server.server_close(); thread.join()
    return result

def blender(out, cpu, gpu_backend='default'):
    env = dict(os.environ, NEO_SUITE_OUTPUT=str(out))
    if cpu: env['NEO_SUITE_CPU_CONTROL'] = '1'
    backend_args = [] if gpu_backend == 'default' else ['--gpu-backend', gpu_backend]
    code = run(['blender'] + backend_args + ['--background', '--factory-startup', '--python-exit-code', '1',
                '--python', str(HERE / 'blender-scene.py')], out / 'blender.log', 180, env)
    receipt = {'exit': code, 'cpu_control': cpu, 'requested_gpu_backend': gpu_backend,
               'render_exists': (out / 'render.png').is_file()}
    info = out / 'blender-result.json'
    if info.exists(): receipt.update(json.loads(info.read_text()))
    receipt['passed'] = code == 0 and receipt['render_exists']
    return receipt

def cts(out, binary, case, deadline):
    executable = Path(binary).resolve(strict=True)
    # CTS data is relative to its module directory, never run from the user's home.
    log = out / 'results.qpa'
    command = [str(executable), '--deqp-case=' + case, '--deqp-log-filename=' + str(log),
               '--deqp-watchdog=enable', '--deqp-surface-width=256', '--deqp-surface-height=256']
    with contextlib.chdir(executable.parent):
        code = run(command, out / 'cts.log', deadline)
    text = log.read_text(errors='replace') if log.exists() else ''
    statuses = re.findall(r'<Result\s+StatusCode="([^"]+)"', text)
    counts = {status: statuses.count(status) for status in sorted(set(statuses))}
    return {'exit': code, 'binary': str(executable), 'case': case, 'counts': counts,
            'executed': len(statuses), 'passed': code == 0 and counts.get('Pass', 0) > 0 and
            all(s in ('Pass', 'NotSupported', 'QualityWarning', 'CompatibilityWarning') for s in statuses),
            'conformance_claim': False}

def obs(out):
    """Record a generated colour scene, without capturing any personal desktop content."""
    import socket
    import websocket
    import math
    import struct
    import wave
    password = secrets.token_urlsafe(32)
    with socket.socket() as listener:
        listener.bind(('127.0.0.1', 0)); port = listener.getsockname()[1]
    config = out / 'config'
    obsconfig = config / 'obs-studio'
    plugin = obsconfig / 'plugin_config/obs-websocket'; plugin.mkdir(parents=True)
    # A first-run auto-configuration wizard disables recording while open.
    # Seed only this isolated test profile so automated recording is available.
    (obsconfig / 'user.ini').write_text('[General]\nFirstRun=true\n[Basic]\nProfile=Neo Suite\nProfileDir=NeoSuite\n')
    profile = obsconfig / 'basic/profiles/NeoSuite'; profile.mkdir(parents=True)
    (profile / 'basic.ini').write_text('[General]\nName=Neo Suite\n[Output]\nMode=Simple\n'
        '[SimpleOutput]\nFilePath=' + str(out) + '\nRecFormat2=mkv\nRecQuality=HQ\nRecEncoder=x264\n'
        'StreamEncoder=x264\nRecAudioEncoder=aac\nRecTracks=1\n[Video]\nBaseCX=320\nBaseCY=240\n'
        'OutputCX=320\nOutputCY=240\nFPSType=0\nFPSCommon=30\n[Audio]\nSampleRate=48000\nChannelSetup=Stereo\n')
    (plugin / 'config.json').write_text(json.dumps({'server_enabled': True,
        'server_port': port, 'auth_required': True, 'server_password': password,
        'first_load': False, 'alerts_enabled': False}))
    os.chmod(plugin / 'config.json', 0o600)
    env = dict(os.environ, XDG_CONFIG_HOME=str(config))
    counter = 0
    connection = None
    result = {'passed': False, 'recording_encoder': 'x264 software', 'hardware_encoding_claim': False}
    with (out / 'obs.log').open('wb') as stream:
        proc = subprocess.Popen(['obs', '--disable-updater', '--disable-missing-files-check',
                                 '--disable-shutdown-check'], env=env, stdout=stream,
                                stderr=subprocess.STDOUT, start_new_session=True)
        try:
            deadline = time.monotonic() + 60
            while time.monotonic() < deadline and proc.poll() is None:
                try:
                    connection = websocket.create_connection(f'ws://127.0.0.1:{port}', timeout=2,
                                                              http_proxy_host=None)
                    break
                except (OSError, websocket.WebSocketException): time.sleep(0.5)
            if connection is None: raise RuntimeError('OBS WebSocket did not become ready')
            hello = json.loads(connection.recv())['d']
            identify = {'rpcVersion': 1, 'eventSubscriptions': 0}
            if 'authentication' in hello:
                auth = hello['authentication']
                secret = base64.b64encode(hashlib.sha256((password + auth['salt']).encode()).digest()).decode()
                identify['authentication'] = base64.b64encode(hashlib.sha256((secret + auth['challenge']).encode()).digest()).decode()
            connection.send(json.dumps({'op': 1, 'd': identify}))
            assert json.loads(connection.recv())['op'] == 2
            class RequestError(RuntimeError):
                def __init__(self, kind, status):
                    super().__init__(kind + ': ' + str(status))
                    self.code = status.get('code')
            def request(kind, **data):
                nonlocal counter
                counter += 1; request_id = str(counter)
                connection.send(json.dumps({'op': 6, 'd': {'requestType': kind,
                    'requestId': request_id, 'requestData': data}}))
                while True:
                    reply = json.loads(connection.recv())
                    if reply['op'] == 7 and reply['d']['requestId'] == request_id:
                        response = reply['d']
                        if not response['requestStatus']['result']:
                            raise RequestError(kind, response['requestStatus'])
                        return response.get('responseData', {})
            # WebSocket identification can precede OBS frontend readiness.
            # Retry only this read-only query on NotReady (207); mutations
            # and all other errors retain their normal failure behavior.
            while True:
                try:
                    result['version'] = request('GetVersion')
                    break
                except RequestError as error:
                    if error.code != 207 or proc.poll() is not None or time.monotonic() >= deadline:
                        raise
                    time.sleep(0.5)
            request('SetProfileParameter', parameterCategory='Output', parameterName='Mode', parameterValue='Simple')
            for name, value in {'FilePath': str(out), 'RecFormat2': 'mkv', 'RecQuality': 'HQ',
                                'RecEncoder': 'x264'}.items():
                request('SetProfileParameter', parameterCategory='SimpleOutput', parameterName=name, parameterValue=value)
            request('SetRecordDirectory', recordDirectory=str(out))
            result['video_settings'] = request('GetVideoSettings')
            kinds = request('GetInputKindList')['inputKinds']
            kind = next(k for k in ['color_source_v3', 'color_source_v2', 'color_source'] if k in kinds)
            request('CreateScene', sceneName='Neo Suite')
            request('CreateInput', sceneName='Neo Suite', inputName='Generated colour', inputKind=kind,
                    inputSettings={'color': 0xff00ff00, 'width': 320, 'height': 240}, sceneItemEnabled=True)
            # Provide a deterministic audio clock even on a machine without
            # microphones/speakers or a running PulseAudio server.
            tone = out / 'test-tone.wav'
            with wave.open(str(tone), 'wb') as audio:
                audio.setnchannels(1); audio.setsampwidth(2); audio.setframerate(48000)
                audio.writeframes(b''.join(struct.pack('<h', int(2000 * math.sin(i * 2 * math.pi * 440 / 48000))) for i in range(48000)))
            request('CreateInput', sceneName='Neo Suite', inputName='Generated audio', inputKind='ffmpeg_source',
                    inputSettings={'is_local_file': True, 'local_file': str(tone), 'looping': True}, sceneItemEnabled=True)
            request('SetCurrentProgramScene', sceneName='Neo Suite')
            request('StartRecord')
            for _ in range(40):
                if request('GetRecordStatus').get('outputActive'): break
                time.sleep(0.25)
            else:
                if os.environ.get('NEO_SOFTWARE_CONTROL') == '1':
                    run(['ffmpeg', '-y', '-f', 'x11grab', '-video_size', '1024x768',
                         '-i', os.environ['DISPLAY'], '-frames:v', '1', str(out / 'debug-screen.png')],
                        out / 'debug-screen.log', 10)
                raise RuntimeError('OBS accepted StartRecord but recording never became active')
            try:
                for frame in range(12):
                    request('SetInputSettings', inputName='Generated colour',
                            inputSettings={'color': 0xff00ff00 if frame % 2 else 0xff0000ff}, overlay=True)
                    time.sleep(1)
                result['stats'] = request('GetStats')
                shot = request('GetSourceScreenshot', sourceName='Neo Suite', imageFormat='png', imageWidth=320, imageHeight=240)
                (out / 'preview.png').write_bytes(base64.b64decode(shot['imageData'].split(',', 1)[1]))
            finally:
                stopped = request('StopRecord')
            for _ in range(50):
                if not request('GetRecordStatus').get('outputActive'): break
                time.sleep(0.1)
            recording = Path(stopped['outputPath']).resolve()
            if recording.parent != out or not recording.is_file(): raise RuntimeError('Recording path mismatch')
            code = run(['ffprobe', '-v', 'error', '-show_streams', '-show_format', '-of', 'json', str(recording)],
                       out / 'recording-probe.json', 30)
            decode = run(['ffmpeg', '-v', 'error', '-i', str(recording), '-map', '0:v:0', '-f', 'framemd5', '-'], out / 'recording-frames.md5', 30)
            frames = re.findall(r'^\s*\d+,\s*-?\d+,\s*-?\d+,.*?,\s*([0-9a-f]{32})\s*$',
                                (out / 'recording-frames.md5').read_text(), re.M)
            result.update(recording=str(recording), frames=len(frames), unique_frames=len(set(frames)),
                          passed=code == 0 and decode == 0 and len(frames) >= 120 and len(set(frames)) >= 2)
        finally:
            if connection is not None: connection.close()
            if proc.poll() is None:
                os.killpg(proc.pid, signal.SIGTERM)
                try: proc.wait(timeout=10)
                except subprocess.TimeoutExpired: os.killpg(proc.pid, signal.SIGKILL); proc.wait()
            # The credential exists only for this completed, isolated OBS session.
            (plugin / 'config.json').unlink(missing_ok=True)
    return result

def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--output', type=Path, required=True, help='New directory for this run')
    commands = parser.add_subparsers(dest='command', required=True)
    commands.add_parser('inventory')
    chrome_parser = commands.add_parser('chrome')
    chrome_parser.add_argument('--angle', choices=['default', 'gl', 'vulkan'], default='default')
    commands.add_parser('firefox')
    commands.add_parser('obs')
    bp = commands.add_parser('blender'); bp.add_argument('--cpu-control', action='store_true')
    bp.add_argument('--gpu-backend', choices=['default', 'opengl', 'vulkan'], default='default')
    cp = commands.add_parser('cts'); cp.add_argument('--binary', required=True); cp.add_argument('--case', required=True)
    cp.add_argument('--timeout', type=int, default=180)
    args = parser.parse_args()
    compatible = Path('/proc/device-tree/compatible')
    if not compatible.exists() or b'apple,j700\0' not in compatible.read_bytes():
        parser.error('This runner is scoped to the Neo J700')
    if os.geteuid() == 0: parser.error('Run as the Plasma desktop user, with its display environment')
    out = args.output.resolve(); out.mkdir(parents=True, exist_ok=False)
    started = datetime.datetime.now(datetime.timezone.utc).isoformat()
    try:
        if args.command == 'inventory': result = inventory(out)
        elif args.command in ('chrome', 'firefox'): result = browser(args.command, out, getattr(args, 'angle', 'default'))
        elif args.command == 'blender': result = blender(out, args.cpu_control, args.gpu_backend)
        elif args.command == 'obs': result = obs(out)
        else: result = cts(out, args.binary, args.case, args.timeout)
    except Exception as error:
        result = {'passed': False, 'error': f'{type(error).__name__}: {error}'}
    receipt = {'started': started, 'finished': datetime.datetime.now(datetime.timezone.utc).isoformat(),
               'test': args.command, 'result': result}
    (out / 'receipt.json').write_text(json.dumps(receipt, indent=2) + '\n')
    print(out / 'receipt.json')
    return 1 if result.get('passed') is False or result.get('render_pass') is False or result.get('error') else 0

if __name__ == '__main__':
    raise SystemExit(main())
