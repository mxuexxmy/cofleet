#!/usr/bin/env python3
"""Exercise scheduling over HTTP with two real Agents on one native physical host.
Creates isolated configs only; it does not start Docker workloads or interrupt user Agents.
"""
import argparse
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
parser.add_argument('--image', required=True, help='Approved immutable workspace image ID')
parser.add_argument('--bin-dir', type=Path, default=Path('target/release'))
args = parser.parse_args()
binaries = args.bin_dir.resolve()
root = Path('.local/phase4') / uuid.uuid4().hex
root.mkdir(parents=True, mode=0o700)
env = dict(os.environ, COFLEET_TOKEN=secrets.token_urlsafe(32))
with socket.socket() as listener:
    listener.bind(('127.0.0.1', 0))
    port = listener.getsockname()[1]
base = f'http://127.0.0.1:{port}'
processes = []
checks = {}
log = (root / 'processes.log').open('ab')

def api(path, value=None, method=None, authorized=True):
    headers = {'Content-Type': 'application/json', 'X-CoFleet-Request': '1'}
    if authorized:
        headers['Authorization'] = 'Bearer ' + env['COFLEET_TOKEN']
    req = urllib.request.Request(base + path, data=json.dumps(value).encode() if value is not None else None,
                                 headers=headers, method=method)
    with urllib.request.urlopen(req, timeout=10) as response:
        body = response.read()
        return json.loads(body) if body else None

def wait(predicate):
    deadline = time.monotonic() + 30
    while time.monotonic() < deadline:
        try:
            result = predicate()
            if result:
                return result
        except (OSError, ValueError, subprocess.CalledProcessError):
            pass
        time.sleep(.2)
    raise AssertionError('condition timed out')

def ctl(data, *values):
    return subprocess.check_output([str(binaries / 'cofleetctl'), '--data-dir', str(data), *values], env=env, timeout=10, stderr=subprocess.DEVNULL)

def spawn(binary, *values):
    process = subprocess.Popen([str(binaries / binary), *values], env=env, stdout=log, stderr=log)
    processes.append(process)
    return process

def submit(kind='batch', **requirements):
    request = {'workload_id': str(uuid.uuid4()), 'kind': kind, 'requirements': requirements}
    return request, api('/api/v1/workloads', request)

def remove(request):
    api('/api/v1/workloads/' + request['workload_id'], method='DELETE')

def passed(name):
    checks[name] = True
    print('PASS', name, flush=True)

try:
    spawn('cofleet-manager', '--listen', f'127.0.0.1:{port}')
    wait(lambda: api('/api/whoami'))
    host = str(uuid.uuid4())
    agents = {}
    for index in range(2):
        data = root / f'node-{index}'
        ctl(data, 'init', '--name', f'scheduler-test-{index}', '--manager', base, '--heartbeat-secs', '1', '--physical-host-id', host)
        process = spawn('cofleet-agent', '--data-dir', str(data), '--docker', '--workspace-image', args.image, '--build-deploy')
        status = wait(lambda: json.loads(ctl(data, 'status')))
        device = status['device_id']
        ctl(data, 'capabilities', '--physical-host-id', host, '--data-location', 'test-local')
        agents[device] = (data, process)
    nodes = wait(lambda: (items if len(items) == 2 and all(n['online'] and n.get('resources') and n.get('pipeline') is not None and 'test-local' in n['capabilities']['data_locations'] for n in items) else None)
                 if (items := api('/api/v1/devices')) is not None else None)
    assert all(n['version'] == '0.6.0' and n['capabilities']['physical_host_id'] == host for n in nodes)
    passed('two_real_agents_report_version_capabilities_and_same_physical_host')
    try:
        api('/api/v1/workloads', authorized=False)
        raise AssertionError('anonymous schedule access allowed')
    except urllib.error.HTTPError as error:
        assert error.code == 401
    passed('scheduler_requires_authentication')
    cpus = max(n['resources']['logical_cpus'] for n in nodes)
    first, placement = submit(cpu_threads=cpus, memory_mib=128)
    assert placement['action'] == 'start'
    assert api('/api/v1/workloads', first)['attempt'] == 1
    conflict = dict(first, requirements={'cpu_threads': 1, 'memory_mib': 129})
    try:
        api('/api/v1/workloads', conflict)
        raise AssertionError('conflicting ID allowed')
    except urllib.error.HTTPError as error:
        assert error.code == 409
    second, result = submit(cpu_threads=1, memory_mib=128)
    assert result['action'] == 'wait' and 'physical CPU' in result['reason']
    remove(first)
    result = api('/api/v1/workloads', second)
    assert result['action'] == 'start'
    remove(second)
    passed('shared_host_commitments_prevent_double_counting_and_release_frees_capacity')
    passed('request_retry_is_idempotent_and_conflicting_requirements_rejected')
    missing, result = submit(gpu_memory_mib=1, data_location='unavailable-test-data')
    assert result['action'] == 'wait' and 'GPU' in result['reason'] and 'unavailable-test-data' in result['reason']
    remove(missing)
    passed('gpu_and_data_mismatch_return_explicit_candidate_reasons')
    for kind, expected in [('batch', 'retry'), ('stateless_service', 'rebuild'), ('stateful_service', 'wait')]:
        request, assigned = submit(kind, data_location='test-local', memory_mib=128)
        assert assigned['action'] == 'start'
        chosen = assigned['device_id']
        data, process = agents[chosen]
        process.terminate(); process.wait(timeout=10)
        wait(lambda: not next(n for n in api('/api/v1/devices') if n['device_id'] == chosen)['online'])
        result = api('/api/v1/workloads', request)
        assert result['action'] == expected
        assert (result['device_id'] == chosen) == (kind == 'stateful_service')
        if kind == 'stateful_service':
            assert 'second writer' in result['reason']
        remove(request)
        agents[chosen] = (data, spawn('cofleet-agent', '--data-dir', str(data), '--docker', '--workspace-image', args.image, '--build-deploy'))
        wait(lambda: next(n for n in api('/api/v1/devices') if n['device_id'] == chosen)['online'])
        passed(kind + '_disconnect_policy_' + expected)
    for data, _ in agents.values():
        ctl(data, 'pause')
    wait(lambda: all(not n['accepting_tasks'] for n in api('/api/v1/devices')))
    paused, result = submit()
    assert result['action'] == 'wait' and 'paused' in result['reason']
    remove(paused)
    passed('paused_nodes_reject_new_placement')
    report = {'version': '0.6.0', 'checks': checks,
              'scope': 'two native Agent processes on one real physical host; no second machine or Docker workloads simulated'}
    (root / 'results.json').write_text(json.dumps(report, indent=2) + '\n')
    print('Results:', root / 'results.json', flush=True)
finally:
    for process in reversed(processes):
        if process.poll() is None:
            process.terminate()
            try:
                process.wait(timeout=10)
            except subprocess.TimeoutExpired:
                process.kill(); process.wait()
    log.close()
