#!/usr/bin/env python3
"""Persistent workspace checks using the native Docker engine and actual reverse WSS streams."""
import argparse
import concurrent.futures
import json
import os
from pathlib import Path
import secrets
import socket
import subprocess
import time
import urllib.error
import urllib.request
import urllib.parse
import uuid

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('--allow-native-docker', action='store_true', required=True)
parser.add_argument('--image', required=True, help='Approved local immutable workspace image ID')
parser.add_argument('--bin-dir', default='target/debug')
args = parser.parse_args()
binaries = Path(args.bin_dir).resolve()
root = Path('.local/phase2') / uuid.uuid4().hex
root.mkdir(parents=True, mode=0o700)
data = root / 'node'
token = secrets.token_urlsafe(32)
env = dict(os.environ, COFLEET_TOKEN=token)
with socket.socket() as listener:
    listener.bind(('127.0.0.1', 0))
    port = listener.getsockname()[1]
base = f'http://127.0.0.1:{port}'
manager = agent = None
owned_ids = set()
checks = {}
log = open(root / 'processes.log', 'ab')


def ctl(*values):
    result = subprocess.run([str(binaries / 'cofleetctl'), '--data-dir', str(data), *values],
                            env=env, capture_output=True, timeout=95)
    if result.returncode:
        raise RuntimeError('local control failed')
    return json.loads(result.stdout)


def docker(*values):
    return subprocess.check_output(['docker', '--host', 'unix:///var/run/docker.sock', *values], text=True).strip()


def api(path, value=None, headers=None):
    request = urllib.request.Request(base + path,
        data=json.dumps(value).encode() if value is not None else None,
        headers=headers if headers is not None else {'Authorization': 'Bearer ' + token,
            'Content-Type': 'application/json', 'X-CoFleet-Request': '1'})
    with urllib.request.urlopen(request, timeout=20) as response:
        return json.load(response)


def wait(predicate, seconds=45):
    deadline = time.monotonic() + seconds
    while time.monotonic() < deadline:
        try:
            value = predicate()
            if value:
                return value
        except (OSError, ValueError, RuntimeError):
            pass
        time.sleep(0.3)
    raise AssertionError('condition did not become true')


def start_manager():
    process = subprocess.Popen([str(binaries / 'cofleet-manager'), '--listen', f'127.0.0.1:{port}'],
        env=env, stdout=log, stderr=log)
    wait(lambda: urllib.request.urlopen(base + '/healthz', timeout=1).status == 200)
    return process


def start_agent():
    return subprocess.Popen([str(binaries / 'cofleet-agent'), '--data-dir', str(data), '--docker', '--workspace-image', args.image],
        env=env, stdout=log, stderr=log)


def stop(process):
    if process and process.poll() is None:
        process.terminate()
        try:
            process.wait(timeout=5)
        except subprocess.TimeoutExpired:
            process.kill()
            process.wait()


def record(name):
    checks[name] = True
    print('PASS', name, flush=True)


volume = None
try:
    manager = start_manager()
    subprocess.run([str(binaries / 'cofleetctl'), '--data-dir', str(data), 'init', '--name',
        'persistent-workspace-integration', '--manager', base, '--heartbeat-secs', '1'],
        env=env, check=True, stdout=subprocess.DEVNULL)
    agent = start_agent()
    wait(lambda: ctl('status')['connected'])
    device = ctl('status')['device_id']
    path = f'/api/v1/devices/{device}/docker'
    entry = f'/workspaces/{device}/'
    wait(lambda: api('/api/v1/devices')[0].get('workspace'))

    def command(action, **values):
        return api(path, {'action': 'workspace', 'request': dict(action=action, **values)})

    def snapshot():
        return command('status')['workspace']

    def proxy(path='', headers=None):
        request = urllib.request.Request(base + entry + path, headers=headers or {'Authorization': 'Bearer ' + token})
        with urllib.request.urlopen(request, timeout=30) as response:
            return response.status, response.read()

    request_start = str(uuid.uuid4())
    reply = command('set_running', request_id=request_start, running=True)
    assert not reply['error'], reply
    identifier = reply['workspace']['container_id']
    owned_ids.add(identifier)
    volume = reply['workspace']['volume']
    wait(lambda: snapshot()['healthy'], 120)
    assert command('set_running', request_id=request_start, running=True)['duplicate']
    record('workspace_start_health_and_request_deduplication')
    wait(lambda: api('/api/v1/devices')[0].get('workspace', {}).get('healthy'))
    html = proxy()[1]
    assert b'code-server' in html or b'workbench' in html
    assert json.loads(proxy('healthz')[1])['status'] in ('alive', 'expired')
    record('ide_http_through_authenticated_reverse_data_channel')
    for headers in [{}, {'Authorization': 'Bearer ' + token, 'Origin': 'https://evil.example'}]:
        try:
            request = urllib.request.Request(base + entry, headers=headers)
            urllib.request.urlopen(request, timeout=10)
            raise AssertionError('unauthorized proxy request accepted')
        except urllib.error.HTTPError as error:
            assert error.code in (401, 403)
    record('anonymous_and_cross_origin_workspace_access_rejected')

    files = {str(p.relative_to(Path('tests/fixtures/workspace'))): p.read_text()
             for p in Path('tests/fixtures/workspace').rglob('*') if p.is_file()}
    writer = "import sys,json,pathlib; r=pathlib.Path('/home/coder/project'); [(r.joinpath(k).parent.mkdir(parents=True,exist_ok=True),r.joinpath(k).write_text(v)) for k,v in json.load(sys.stdin).items()]"
    subprocess.run(['docker', 'exec', '-i', identifier, 'python3', '-c', writer],
                   input=json.dumps(files).encode(), check=True)
    docker('exec', '-w', '/home/coder/project', identifier, 'npm', 'install', '--ignore-scripts', '--offline', '--no-audit', '--no-fund')
    docker('exec', '-d', '-w', '/home/coder/project', identifier, 'sh', '.cofleet/start.sh')
    wait(lambda: json.loads(proxy('proxy/3000/health')[1])['marker'] == 'persistent-dependency-ok')
    report_headers = json.loads(proxy('proxy/3000/health', {'Authorization': 'Bearer ' + token,
        'Cookie': 'Lzc-Auth-Token=private-test-marker', 'X-HC-User-ID': 'private-test-owner'})[1])['headers']
    assert not any(key in report_headers for key in ['authorization', 'cookie', 'x-hc-user-id'])
    record('project_source_dependency_and_service_preview')
    record('platform_credentials_not_forwarded_to_project')

    from inspector_client import Inspector
    inspector_path = urllib.parse.urlsplit(json.loads(proxy('proxy/9229/json/list')[1])[0]['webSocketDebuggerUrl']).path
    inspector = Inspector(base + entry + 'proxy/9229' + inspector_path, {'Authorization': 'Bearer ' + token})
    inspector.command('Runtime.enable')
    assert inspector.command('Runtime.evaluate', {'expression': 'globalThis.fixtureMarker'})['result']['value'] == 'persistent-dependency-ok'
    inspector.command('Debugger.enable')
    breakpoint = inspector.command('Debugger.setBreakpointByUrl', {'lineNumber': 3, 'urlRegex': '.*/project/app[.]js$'})
    assert breakpoint['locations']
    with concurrent.futures.ThreadPoolExecutor(max_workers=1) as pool:
        response = pool.submit(proxy, 'proxy/3000/debug')
        paused = inspector.event('Debugger.paused')
        frame = paused['params']['callFrames'][0]['callFrameId']
        assert inspector.command('Debugger.evaluateOnCallFrame', {'callFrameId': frame, 'expression': 'a + b'})['result']['value'] == 42
        inspector.command('Debugger.resume')
        assert json.loads(response.result(timeout=20)[1])['answer'] == 42
    inspector.close()
    record('actual_debugger_breakpoint_inspection_and_resume_over_websocket')

    stop(manager)
    assert json.loads(docker('inspect', identifier))[0]['State']['Running']
    manager = start_manager()
    wait(lambda: ctl('status')['connected'])
    wait(lambda: api('/api/v1/devices')[0].get('workspace', {}).get('healthy'))
    assert json.loads(proxy('proxy/3000/health')[1])['marker'] == 'persistent-dependency-ok'
    record('manager_restart_and_new_http_session_preserve_running_service')

    stop(agent)
    docker('rm', '-f', identifier)
    owned_ids.remove(identifier)
    agent = start_agent()
    wait(lambda: ctl('status')['connected'])
    wait(lambda: snapshot()['healthy'], 120)
    replacement = snapshot()['container_id']
    owned_ids.add(replacement)
    assert replacement != identifier and snapshot()['volume'] == volume
    assert 'persistent-dependency-ok' in docker('exec', '-w', '/home/coder/project', replacement, 'node', '-e', "console.log(require('persist-dep'))")
    wait(lambda: json.loads(proxy('proxy/3000/health')[1])['marker'] == 'persistent-dependency-ok')
    record('container_recreation_preserves_source_and_installed_dependencies')
    record('saved_project_start_script_restores_service')
    stopped = command('set_running', request_id=str(uuid.uuid4()), running=False)
    assert not stopped['error']
    delayed = command('set_running', request_id=request_start, running=True)
    assert delayed['duplicate'] and not delayed['workspace']['desired_running']
    assert not json.loads(docker('inspect', replacement))[0]['State']['Running']
    assert json.loads(docker('volume', 'inspect', volume))[0]['Name'] == volume
    record('explicit_stop_retains_volume_and_cannot_be_undone_by_old_retry')
    assert (data / 'workspace-state.json').stat().st_mode & 0o777 == 0o600
    record('private_durable_workspace_state')
    (root / 'results.json').write_text(json.dumps({'checks': checks, 'scope': 'native Linux; isolated development manager; all IDE traffic uses reverse WebSocket tunnel'}, indent=2) + '\n')
    print('Results:', root / 'results.json', flush=True)
finally:
    stop(agent)
    stop(manager)
    for identifier in owned_ids:
        subprocess.run(['docker', 'rm', '-f', identifier], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    if volume:
        subprocess.run(['docker', 'volume', 'rm', volume], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    log.close()
