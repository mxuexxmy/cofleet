#!/usr/bin/env python3
"""Read-only Linux observation smoke: never mutates Docker resources."""
import json
import os
from pathlib import Path
import secrets
import socket
import shutil
import sys
import subprocess
import time
import urllib.request
import urllib.error
import uuid

root = Path('.local/observe-tests') / uuid.uuid4().hex
root.mkdir(parents=True, mode=0o700)
data = root / 'node'
token = secrets.token_urlsafe(32)
env = dict(os.environ, COFLEET_TOKEN=token)
with socket.socket() as sock:
    sock.bind(('127.0.0.1', 0))
    port = sock.getsockname()[1]
base = f'http://127.0.0.1:{port}'
processes = []
log = open(root / 'processes.log', 'wb')
bin_dir = Path('target/debug').resolve()

def docker(*args):
    return subprocess.check_output(['docker', '--host', 'unix:///var/run/docker.sock', *args], text=True).strip()

def snapshot():
    return docker('ps', '-a', '--no-trunc', '--format', '{{.ID}} {{.Names}} {{.State}} {{.Ports}}')

def api(path, auth=token):
    request = urllib.request.Request(base + path, headers={'Authorization': 'Bearer ' + auth})
    with urllib.request.urlopen(request, timeout=20) as response:
        return json.load(response)

def wait(fn):
    for _ in range(100):
        try:
            result = fn()
            if result:
                return result
        except (OSError, ValueError):
            pass
        time.sleep(.15)
    raise RuntimeError('Timed out')

def start_agent(enabled):
    process = subprocess.Popen([str(bin_dir / 'cofleet-agent'), '--data-dir', str(data)] + (['--docker-observe'] if enabled else []), env=env, stdout=log, stderr=log)
    processes.append(process)
    return process

# Only the isolated test Agent sees this guard. Production processes keep their PATH.
real_docker = shutil.which('docker')
wrapper = root / 'bin'
wrapper.mkdir()
mode_file = (root / 'mode').resolve()
mode_file.write_text('real')
(wrapper / 'docker').write_text(f"""#!{sys.executable}
import os, sys
from pathlib import Path
args = sys.argv[3:]
assert args[0] in ('ps', 'inspect', 'logs', 'info'), 'Unexpected Docker mutation'
mode = Path({str(mode_file)!r}).read_text()
if mode == 'empty' and args[0] == 'ps': sys.exit(0)
if mode == 'many' and args[0] == 'ps':
    print('a' * 64 + '\\n' + ('b' * 64 + '\\n') * 500); sys.exit(0)
if mode in ('denied', 'daemon'):
    print('permission denied' if mode == 'denied' else 'Cannot connect to Docker daemon', file=sys.stderr); sys.exit(1)
if mode == 'driver' and args[0] == 'logs':
    print('logging driver does not support reading', file=sys.stderr); sys.exit(1)
if mode == 'long' and args[0] == 'logs':
    sys.stderr.write('日志' * 100000); sys.exit(0)
os.execv({real_docker!r}, [{real_docker!r}] + sys.argv[1:])
""")
(wrapper / 'docker').chmod(0o700)
env['PATH'] = str(wrapper.resolve()) + os.pathsep + env['PATH']
before = snapshot()
checks = []
try:
    processes.append(subprocess.Popen([str(bin_dir / 'cofleet-manager'), '--listen', f'127.0.0.1:{port}', '--state-dir', str(root / 'manager')], env=env, stdout=log, stderr=log))
    wait(lambda: api('/api/v1/devices') == [])
    subprocess.run([str(bin_dir / 'cofleetctl'), '--data-dir', str(data), 'init', '--name', 'observe-readonly-test', '--manager', base, '--heartbeat-secs', '1'], env=env, check=True, stdout=subprocess.DEVNULL)
    agent = start_agent(True)
    device = wait(lambda: next((d for d in api('/api/v1/devices') if d.get('docker_observe')), None))
    device_id = device['device_id']
    assert device.get('docker') is None
    checks.append('independent_opt_in_without_docker_executor')
    path = f'/api/v1/devices/{device_id}/docker'
    cursor, ids, snapshot_id = None, [], None
    while True:
        reply = api(path + '/catalog?limit=1' + ('&cursor=' + cursor if cursor else ''))
        assert reply['result'] == 'catalog', reply
        page = reply['page']
        assert not snapshot_id or snapshot_id == page['snapshot_id']
        snapshot_id = page['snapshot_id']
        ids.extend(c['id'] for c in page['containers'])
        assert all('env' not in c and 'logs' not in c for c in page['containers'])
        cursor = page['next_cursor']
        if not cursor:
            break
    assert set(ids) == set(docker('ps', '-aq', '--no-trunc').splitlines())
    checks.append('real_catalog_pagination_complete_and_metadata_only')
    if ids:
        value = api(path + '/containers/' + ids[0] + '/logs?tail=500')
        assert value['result'] == 'logs', value
        assert len(value['value']['logs'].encode()) <= 16384
        checks.append('real_container_logs_bounded')
    assert api(path + '/containers/not-a-full-id/logs')['result'] == 'error'
    assert api(path + '/containers/' + 'f' * 64 + '/logs')['result'] == 'error'
    assert api(path + '/catalog?cursor=invalid')['result'] == 'error'
    checks.append('invalid_id_missing_container_invalid_cursor_rejected')
    try:
        api(path + '/catalog', 'invalid-token')
        raise AssertionError('Unauthorized catalog allowed')
    except urllib.error.HTTPError as error:
        assert error.code == 401
    checks.append('unauthorized_catalog_rejected')
    mode_file.write_text('empty')
    assert api(path + '/catalog')['page']['containers'] == []
    for mode in ('many', 'denied', 'daemon'):
        mode_file.write_text(mode)
        assert api(path + '/catalog')['result'] == 'error'
    checks.append('simulated_empty_overlimit_permission_and_daemon_errors')
    if ids:
        mode_file.write_text('driver')
        assert api(path + '/containers/' + ids[0] + '/logs')['result'] == 'error'
        mode_file.write_text('long')
        value = api(path + '/containers/' + ids[0] + '/logs')['value']
        assert value['truncated'] and len(value['logs'].encode()) <= 16384
        checks.append('simulated_unsupported_driver_and_large_unicode_stderr')
    mode_file.write_text('real')
    agent.terminate(); agent.wait(timeout=10)
    start_agent(False)
    wait(lambda: next((d for d in api('/api/v1/devices') if d['online'] and not d.get('docker_observe')), None))
    try:
        api(path + '/catalog')
        raise AssertionError('Disabled observation allowed')
    except urllib.error.HTTPError as error:
        assert error.code == 409
    checks.append('disabled_capability_rejects_observation')
    assert snapshot() == before
    checks.append('existing_containers_unchanged')
    (root / 'results.json').write_text(json.dumps({'checks': checks}, indent=2))
    print(json.dumps({'checks': checks, 'result': str(root / 'results.json')}, indent=2))
finally:
    for process in reversed(processes):
        if process.poll() is None:
            process.terminate()
            try:
                process.wait(timeout=10)
            except subprocess.TimeoutExpired:
                process.kill(); process.wait()
    log.close()
