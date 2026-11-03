#!/usr/bin/env python3
"""Verify an approved workspace through the real LazyCat app; optionally seed a new example.
Leaves the workspace running. Never deletes an existing project or saves login cookies.
"""
import argparse
import concurrent.futures
import json
from pathlib import Path
import statistics
import subprocess
import sys
import time
import urllib.error
import urllib.parse
import urllib.request
import uuid

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('--manager', required=True)
parser.add_argument('--data-dir', type=Path, required=True)
parser.add_argument('--bin-dir', type=Path, default=Path('target/release'))
parser.add_argument('--output', type=Path, required=True)
parser.add_argument('--seed-example', action='store_true', help='Create only a new cofleet-example folder and project startup script')
parser.add_argument('--container', default='xyzmxuelzcappcofleet-app-1')
args = parser.parse_args()
root = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(root / 'tests'))
from inspector_client import Inspector

core = json.loads((Path.home() / '.config/lzc-client-desktop/core.cfg').read_text())
local_opener = urllib.request.build_opener(urllib.request.ProxyHandler({}))
def local(path):
    req = urllib.request.Request(f'http://127.0.0.1:{core["http_port"]}' + path, headers=core['extra_headers'])
    with local_opener.open(req, timeout=10) as response:
        return json.load(response)
box = local('/default-box')
manager = args.manager.rstrip('/')
target = urllib.parse.urlsplit(manager)
assert target.scheme == 'https' and target.hostname.endswith('.' + box['boxDomain']) and (target.port or 443) == 443
assert not target.username and not target.password and not target.query and not target.fragment
ext = local('/box-ext-info?' + urllib.parse.urlencode({'url': 'https://' + box['boxDomain'] + '/'}))
cookie = next(c['value'] for c in ext['cookies'] if c['name'] == 'Lzc-Auth-Token')
class NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, *args):
        return None
opener = urllib.request.build_opener(NoRedirect())
headers = {'Cookie': 'Lzc-Auth-Token=' + cookie}
def request(path, value=None):
    req = urllib.request.Request(manager + path, data=json.dumps(value).encode() if value is not None else None,
        headers=dict(headers, **{'Content-Type': 'application/json', 'X-CoFleet-Request': '1'}))
    with opener.open(req, timeout=35) as response:
        return response.read()
def ctl(*values):
    return json.loads(subprocess.check_output([str(args.bin_dir.resolve() / 'cofleetctl'), '--data-dir', str(args.data_dir), *values], timeout=95))
def wait(predicate, seconds=120):
    deadline = time.monotonic() + seconds
    while time.monotonic() < deadline:
        try:
            value = predicate()
            if value:
                return value
        except (OSError, ValueError, StopIteration):
            pass
        time.sleep(.5)
    raise AssertionError('verification condition timed out')
wait(lambda: ctl('status')['connected'])
device = ctl('status')['device_id']
entry = f'/workspaces/{device}/'
command_path = f'/api/v1/devices/{device}/docker'
def command(action, **values):
    return json.loads(request(command_path, {'action': 'workspace', 'request': dict(action=action, **values)}))
def device_record():
    return next(d for d in json.loads(request('/api/v1/devices')) if d['device_id'] == device)
report = {'version': json.loads(request('/api/whoami'))['version'],
    'scope': 'current native omarchy through authenticated microserver domain; no independent off-LAN device', 'checks': {}}
def passed(name):
    report['checks'][name] = True
    print('PASS', name, flush=True)
start_id = str(uuid.uuid4())
reply = command('set_running', request_id=start_id, running=True)
assert not reply['error'], reply
wait(lambda: device_record().get('workspace', {}).get('healthy'))
identifier = command('status')['workspace']['container_id']
assert json.loads(request(entry + 'healthz'))['status'] in ('alive', 'expired')
passed('real_platform_login_and_workspace_http_proxy')
assert command('set_running', request_id=start_id, running=True)['duplicate']
passed('workspace_start_retry_deduplicated')
if args.seed_example:
    files = {str(p.relative_to(root / 'tests/fixtures/workspace')): p.read_text()
        for p in (root / 'tests/fixtures/workspace').rglob('*') if p.is_file()}
    files['README.md'] = '# CoFleet persistent example\n\nNode.js service: app.js; preview port 3000; debugger port 9229.\nDependencies are installed in this volume. Edit this example or create your own project.\n'
    writer = """import json, pathlib, sys
base=pathlib.Path('/home/coder/project'); dest=base/'cofleet-example'; startup=base/'.cofleet/start.sh'
assert not dest.exists() and not startup.exists(), 'example or startup script already exists; refusing overwrite'
for key,value in json.load(sys.stdin).items():
 path=dest/key;path.parent.mkdir(parents=True,exist_ok=True);path.write_text(value)
startup.parent.mkdir(parents=True,exist_ok=True)
startup.write_text(chr(10).join(['cd /home/coder/project/cofleet-example','exec sh .cofleet/start.sh','']))
"""
    subprocess.run(['docker', 'exec', '-i', identifier, 'python3', '-c', writer], input=json.dumps(files).encode(), check=True)
    subprocess.run(['docker', 'exec', '-w', '/home/coder/project/cofleet-example', identifier, 'npm', 'install', '--ignore-scripts', '--offline', '--no-audit', '--no-fund'], check=True, stdout=subprocess.DEVNULL)
    subprocess.run(['docker', 'exec', '-d', identifier, 'sh', '-c', 'exec sh /home/coder/project/.cofleet/start.sh > /home/coder/.local/share/cofleet/project.log 2>&1'], check=True)
    passed('new_example_seeded_without_overwriting_existing_files')
wait(lambda: json.loads(request(entry + 'proxy/3000/health'))['marker'] == 'persistent-dependency-ok')
passed('project_service_reachable_through_microserver')
assert not any(key in json.loads(request(entry + 'proxy/3000/health'))['headers'] for key in ['cookie','authorization','x-hc-user-id'])
passed('real_platform_credentials_not_forwarded_to_project')
inspector_path = urllib.parse.urlsplit(json.loads(request(entry + 'proxy/9229/json/list'))[0]['webSocketDebuggerUrl']).path
inspector_url = manager + entry + 'proxy/9229' + inspector_path
inspector = Inspector(inspector_url, headers)
try:
    inspector.command('Runtime.enable')
    inspector.command('Debugger.enable')
    breakpoint = inspector.command('Debugger.setBreakpointByUrl', {'lineNumber': 3, 'urlRegex': '.*/app[.]js$'})
    assert breakpoint['locations']
    with concurrent.futures.ThreadPoolExecutor(max_workers=1) as pool:
        response = pool.submit(request, entry + 'proxy/3000/debug')
        paused = inspector.event('Debugger.paused')
        frame = paused['params']['callFrames'][0]['callFrameId']
        assert inspector.command('Debugger.evaluateOnCallFrame', {'callFrameId': frame, 'expression': 'a + b'})['result']['value'] == 42
        inspector.command('Debugger.resume')
        assert json.loads(response.result(timeout=30))['answer'] == 42
    passed('real_breakpoint_variable_inspection_and_resume_via_microserver')
    expression = "process.mainModule.require('node:fs').writeFileSync('/home/coder/project/cofleet-example/remote-edited.txt','saved-through-microserver\\n'); true"
    assert inspector.command('Runtime.evaluate', {'expression': expression})['result']['value']
    expression = "process.mainModule.require('node:child_process').execFileSync('/bin/sh',['-c','cat /home/coder/project/cofleet-example/remote-edited.txt']).toString()"
    assert inspector.command('Runtime.evaluate', {'expression': expression})['result']['value'] == 'saved-through-microserver\n'
    passed('remote_file_write_and_shell_read_inside_workspace_via_inspector')
finally:
    inspector.close()
# A fresh authenticated connection must still see the same file and dependency.
inspector = Inspector(inspector_url, headers)
try:
    result = inspector.command('Runtime.evaluate', {'expression': "globalThis.fixtureMarker + ':' + process.mainModule.require('node:fs').readFileSync('/home/coder/project/cofleet-example/remote-edited.txt','utf8').trim()"})['result']['value']
    assert result == 'persistent-dependency-ok:saved-through-microserver'
finally:
    inspector.close()
passed('fresh_connection_reenters_same_project_and_dependencies')

def stats():
    result = subprocess.run(['lzc-cli', 'docker', 'stats', '--no-stream', '--format', '{{json .}}', args.container], capture_output=True, text=True, timeout=20)
    if result.returncode:
        return {'unavailable': True}
    for line in result.stdout.splitlines():
        if line.startswith('{'):
            data = json.loads(line)
            return {key: data.get(key) for key in ['CPUPerc', 'MemUsage', 'MemPerc', 'NetIO', 'PIDs']}
    return {'unavailable': True}
report['lc02_idle_snapshot'] = stats()
latencies = []
for _ in range(10):
    before = time.perf_counter(); request(entry + 'proxy/3000/health'); latencies.append((time.perf_counter() - before) * 1000)
with concurrent.futures.ThreadPoolExecutor(max_workers=1) as pool:
    active = pool.submit(stats)
    before = time.perf_counter(); count = 0
    for _ in range(8):
        payload = request(entry + 'proxy/3000/download')
        assert len(payload) == 4 * 1024 * 1024
        count += len(payload)
    elapsed = time.perf_counter() - before
    report['lc02_during_transfer_snapshot'] = active.result()
report['latency_ms'] = {'samples': len(latencies), 'median': round(statistics.median(latencies), 2), 'max': round(max(latencies), 2)}
report['download'] = {'bytes': count, 'seconds': round(elapsed, 3), 'mib_per_second': round(count / 1024 ** 2 / elapsed, 2)}
report['lc02_after_transfer_snapshot'] = stats()
report['network_limit'] = 'Uses the current LazyCat route, which may be LAN/direct; not a measured independent WAN link. Docker snapshots are observations, not controlled peak measurements.'
passed('lc02_proxy_resource_and_network_observations_recorded')
args.output.parent.mkdir(parents=True, exist_ok=True)
args.output.write_text(json.dumps(report, ensure_ascii=False, indent=2) + '\n')
print('Report saved without login cookies or private device IDs', flush=True)
