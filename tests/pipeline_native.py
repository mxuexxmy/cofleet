#!/usr/bin/env python3
"""Frozen builds and health-gated releases using native Docker and reverse WSS streams."""
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
root = Path('.local/phase3') / uuid.uuid4().hex
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
    return subprocess.Popen([str(binaries / 'cofleet-agent'), '--data-dir', str(data), '--docker', '--workspace-image', args.image, '--build-deploy'],
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
    import tarfile, hashlib
    manager = start_manager()
    subprocess.run([str(binaries / 'cofleetctl'), '--data-dir', str(data), 'init', '--name',
        'pipeline-integration', '--manager', base, '--heartbeat-secs', '1'], env=env, check=True, stdout=subprocess.DEVNULL)
    agent = start_agent()
    wait(lambda: ctl('status')['connected'])
    device = ctl('status')['device_id']
    api_path = f'/api/v1/devices/{device}/docker'
    entry = f'/applications/{device}/'
    def workspace(action, **values):
        return api(api_path, {'action':'workspace','request':dict(action=action, **values)})
    def command(action, **values):
        return api(api_path, {'action':'pipeline','request':dict(action=action, **values)})
    def snapshot(): return command('status')['pipeline']
    def job(identifier): return next(j for j in snapshot()['jobs'] if j['id']==identifier)
    def done(identifier, success=True):
        result=wait(lambda: (j if j['status'] in ('succeeded','failed','interrupted') else None) if (j:=job(identifier)) else None, 180)
        assert (result['status']=='succeeded') == success, result
        return result
    def submit(action, **values):
        identifier=str(uuid.uuid4()); result=command(action, request_id=identifier, **values); assert not result['error'],result
        return identifier
    w=workspace('set_running',request_id=str(uuid.uuid4()),running=True)
    assert not w['error']; volume=w['workspace']['volume']; container=w['workspace']['container_id'];owned_ids.add(container)
    wait(lambda: workspace('status')['workspace']['healthy'],120)
    wait(lambda: api('/api/v1/devices')[0].get('pipeline'))
    fixture=Path('tests/fixtures/release')
    def write(files):
        script="import json,sys,pathlib; r=pathlib.Path('/home/coder/project/release-demo'); [(r.joinpath(k).parent.mkdir(parents=True,exist_ok=True),r.joinpath(k).write_text(v)) for k,v in json.load(sys.stdin).items()]"
        subprocess.run(['docker','exec','-i',container,'python3','-c',script],input=json.dumps(files).encode(),check=True)
    files={str(p.relative_to(fixture)):p.read_text() for p in fixture.rglob('*') if p.is_file()}
    files['.env']='NEVER_CAPTURE_THIS_SECRET=test\n';write(files)
    build1=submit('build',project_dir='release-demo')
    wait(lambda:job(build1)['input_digest'])
    write({'version.txt':'v2\n'})
    first=done(build1)
    assert first['image_id'].startswith('sha256:')
    archive=data/'pipeline'/build1/'input.tar'
    assert first['input_digest']=='sha256:'+hashlib.sha256(archive.read_bytes()).hexdigest()
    with tarfile.open(archive) as tar:
        assert '.env' not in tar.getnames() and tar.extractfile('version.txt').read()==b'v1\n'
    assert (data/'pipeline'/build1/'build.log').stat().st_size>0
    assert command('build',request_id=build1,project_dir='release-demo')['duplicate']
    record('frozen_source_digest_private_archive_logs_and_build_deduplication')
    release1=submit('deploy',build_id=build1,expected_current=None);done(release1)
    def version(): return api(entry)['version']
    wait(lambda:version()=='v1')
    record('immutable_image_deployment_and_authenticated_application_proxy')
    for headers in [{}, {'Authorization':'Bearer '+token,'Origin':'https://evil.example'}]:
        try: api(entry,headers=headers);raise AssertionError('unauthorized application accepted')
        except urllib.error.HTTPError as error: assert error.code in (401,403)
    record('application_origin_and_authentication_checks')
    build2=submit('build',project_dir='release-demo');second=done(build2)
    assert second['image_id']!=first['image_id']
    stale=submit('deploy',build_id=build2,expected_current=None);done(stale,False)
    assert version()=='v1'
    release2=submit('deploy',build_id=build2,expected_current=release1);done(release2)
    wait(lambda:version()=='v2')
    assert snapshot()['previous']['id']==release1
    record('health_gated_update_previous_release_retained_and_stale_update_rejected')
    write({'.cofleet-release.json':'{"data_policy":"persistent","migration":true}\n'})
    build3=submit('build',project_dir='release-demo');done(build3)
    migration=submit('deploy',build_id=build3,expected_current=release2);done(migration,False)
    assert version()=='v2'
    record('data_migration_release_refused_without_touching_active_application')
    write({'.cofleet-release.json':files['.cofleet-release.json'],'health.txt':'bad\n'})
    bad=submit('build',project_dir='release-demo');done(bad)
    failed=submit('deploy',build_id=bad,expected_current=release2)
    for _ in range(3): assert version()=='v2';time.sleep(.5)
    done(failed,False);assert snapshot()['current']['id']==release2 and version()=='v2'
    record('unhealthy_candidate_never_replaces_running_release')
    rollback=submit('rollback',expected_current=release2);done(rollback)
    wait(lambda:version()=='v1')
    assert snapshot()['current']['image_id']==first['image_id']
    record('rollback_uses_preserved_immutable_image')
    docker('exec',container,'ln','-s','/etc/passwd','/home/coder/project/release-demo/escape')
    rejected=submit('build',project_dir='release-demo');done(rejected,False)
    assert version()=='v1';docker('exec',container,'rm','/home/coder/project/release-demo/escape')
    record('symlink_source_rejected_without_disrupting_application')
    write({'Dockerfile':files['Dockerfile']+'\nRUN exit 7\n'})
    broken=submit('build',project_dir='release-demo');done(broken,False)
    assert command('logs',job_id=broken)['logs'] and version()=='v1'
    record('failed_build_preserves_logs_and_current_release')
    active=snapshot()['current']['container_id']
    stop(agent); assert json.loads(docker('inspect',active))[0]['State']['Running']
    agent=start_agent();wait(lambda:ctl('status')['connected']);wait(lambda:version()=='v1')
    assert snapshot()['current']['container_id']==active
    assert command('build',request_id=build1,project_dir='release-demo')['duplicate']
    record('agent_restart_preserves_application_versions_and_request_journal')
    stopped=submit('stop',expected_current=rollback);done(stopped)
    assert command('rollback',request_id=rollback,expected_current=release2)['duplicate']
    assert not snapshot()['desired_running']
    wait(lambda:not json.loads(docker('inspect',active))[0]['State']['Running'])
    record('explicit_stop_is_not_undone_by_replayed_release_operation')
    assert (data/'pipeline'/'state.json').stat().st_mode & 0o777==0o600
    (root/'results.json').write_text(json.dumps({'checks':checks,'scope':'native Docker, isolated node/manager, applications accessed through reverse data tunnels'},indent=2)+'\n')
    print('Results:',root/'results.json',flush=True)
finally:
    stop(agent);stop(manager)
    if 'device' in globals():
        ids=docker('ps','-aq','--filter','label=xyz.mxue.cofleet.device='+device).splitlines()
        for identifier in ids: subprocess.run(['docker','rm','-f',identifier],stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL)
        images=docker('image','ls','-q','--filter','label=xyz.mxue.cofleet.device='+device).splitlines()
        for image in set(images): subprocess.run(['docker','image','rm','-f',image],stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL)
    if volume: subprocess.run(['docker','volume','rm',volume],stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL)
    log.close()
