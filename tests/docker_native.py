#!/usr/bin/env python3
"""Opt-in integration checks against the native Linux Docker engine; no daemon restart."""
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
import uuid

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('--allow-native-docker', action='store_true', required=True)
parser.add_argument('--bin-dir', default='target/debug')
args = parser.parse_args()
binaries = Path(args.bin_dir).resolve()
root = Path('.local/phase1') / uuid.uuid4().hex
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
    return subprocess.Popen([str(binaries / 'cofleet-agent'), '--data-dir', str(data), '--docker'],
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


try:
    manager = start_manager()
    subprocess.run([str(binaries / 'cofleetctl'), '--data-dir', str(data), 'init', '--name',
        'native-docker-integration', '--manager', base, '--heartbeat-secs', '1'],
        env=env, check=True, stdout=subprocess.DEVNULL)
    agent = start_agent()
    wait(lambda: ctl('status')['connected'])
    device = ctl('status')['device_id']
    name = 'cofleet-test-' + uuid.UUID(device).hex
    path = f'/api/v1/devices/{device}/docker'
    wait(lambda: api('/api/v1/devices')[0].get('docker'))
    image = 'alpine@sha256:5291449c3df73caf6ed85e649dec1b9e818b39a5d8c871e97afc13e9cd5e8fa8'

    def command(action, **values):
        return api(path, dict(action=action, **values))

    def inspect(identifier):
        return json.loads(docker('inspect', identifier))[0]

    def current_id():
        return docker('ps', '-aq', '--no-trunc', '--filter', f'name=^/{name}$')

    for headers in [{'Content-Type': 'application/json', 'X-CoFleet-Request': '1'},
                    {'Authorization': 'Bearer ' + token, 'Content-Type': 'application/json'}]:
        try:
            api(path, {'action': 'list'}, headers)
            raise AssertionError('unauthorized request accepted')
        except urllib.error.HTTPError as error:
            assert error.code in (401, 403)
    record('authentication_and_csrf_header_required')
    assert command('list')['snapshot'] is not None
    record('container_inventory')

    # Create a known test-owned collision, without CoFleet labels; it must not be started.
    collision = docker('create', '--pull=never', '--name', name, '--label', 'cofleet.integration=' + root.name,
                       '--network=none', image, 'sleep', '300')
    owned_ids.add(collision)
    request_a = str(uuid.uuid4())
    rejected = command('set_test', request_id=request_a, running=True)
    assert rejected['error'] and not inspect(collision)['State']['Running']
    docker('rm', collision)
    owned_ids.remove(collision)
    record('unmanaged_name_collision_not_mutated')

    # Discard the HTTP response deliberately, then retry the same persisted intent.
    request_b = str(uuid.uuid4())
    body = json.dumps({'action': 'set_test', 'request_id': request_b, 'running': True}).encode()
    with socket.create_connection(('127.0.0.1', port), timeout=5) as connection:
        connection.sendall((f'POST {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\n'
            f'Authorization: Bearer {token}\r\nContent-Type: application/json\r\nX-CoFleet-Request: 1\r\n'
            f'Content-Length: {len(body)}\r\nConnection: close\r\n\r\n').encode() + body)
        timed_out = False
        connection.settimeout(0.001)
        try:
            connection.recv(1)
        except TimeoutError:
            timed_out = True  # Abandon an operation whose result is still unknown.
    assert timed_out, 'expected a real caller-side timeout'
    identifier = wait(current_id)
    owned_ids.add(identifier)
    wait(lambda: inspect(identifier)['State']['Running'])
    retry = command('set_test', request_id=request_b, running=True)
    assert retry['duplicate'] and current_id() == identifier and '\n' not in current_id()
    with concurrent.futures.ThreadPoolExecutor(max_workers=2) as pool:
        responses = list(pool.map(lambda _: command('set_test', request_id=request_b, running=True), range(2)))
    assert all(reply['duplicate'] for reply in responses)
    record('lost_response_and_concurrent_retries_use_one_instance')
    assert 'CoFleet test container started' in command('test_logs')['logs']
    info = inspect(identifier)
    assert info['HostConfig']['NetworkMode'] == 'none' and not info['Mounts']
    assert info['HostConfig']['Memory'] == 32 * 1024 ** 2 and info['HostConfig']['ReadonlyRootfs']
    assert info['Config']['User'] == '65534:65534'
    record('bounded_logs_and_fixed_container_limits')

    stop(manager)
    wait(lambda: not ctl('status')['connected'])
    assert inspect(identifier)['State']['Running']
    record('manager_disconnect_keeps_container_running')
    manager = start_manager()
    wait(lambda: ctl('status')['connected'])
    wait(lambda: api('/api/v1/devices')[0].get('docker'))
    assert current_id() == identifier
    record('manager_restart_restores_observed_state')

    stop(agent)
    docker('stop', '--time=3', identifier)
    agent = start_agent()
    wait(lambda: inspect(identifier)['State']['Running'])
    wait(lambda: ctl('status')['connected'])
    record('agent_restart_reconciles_persisted_running_intent')

    request_stop = str(uuid.uuid4())
    stopped = command('set_test', request_id=request_stop, running=False)
    assert not stopped['error'] and not inspect(identifier)['State']['Running']
    stop(agent)
    agent = start_agent()
    wait(lambda: ctl('status')['connected'])
    local = ctl('docker', 'list')
    assert not local['snapshot']['desired_running'] and not inspect(identifier)['State']['Running']
    delayed = command('set_test', request_id=request_b, running=True)
    assert delayed['duplicate'] and not delayed['snapshot']['desired_running']
    assert not inspect(identifier)['State']['Running']
    conflict = command('set_test', request_id=request_b, running=False)
    assert conflict['error']
    record('explicit_stop_survives_restart_and_delayed_start_retry')

    ctl('pause')
    paused = command('set_test', request_id=str(uuid.uuid4()), running=True)
    assert paused['error'] and not paused['snapshot']['desired_running']
    ctl('resume')
    record('paused_node_rejects_new_start')
    state_path = data / 'docker-state.json'
    assert state_path.stat().st_mode & 0o777 == 0o600
    record('desired_state_is_private_and_durable')
    (root / 'results.json').write_text(json.dumps({'checks': checks, 'docker_server': docker('version', '--format', '{{.Server.Version}}'), 'scope': 'native Linux, isolated development manager and node'}, indent=2) + '\n')
    print('Results:', root / 'results.json', flush=True)
finally:
    stop(agent)
    stop(manager)
    # Only immutable IDs created and tracked by this test may be removed.
    for identifier in owned_ids:
        subprocess.run(['docker', '--host', 'unix:///var/run/docker.sock', 'rm', '-f', identifier],
                       stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    log.close()
