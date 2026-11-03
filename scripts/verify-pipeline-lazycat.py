#!/usr/bin/env python3
"""Validate build/releases through the current LazyCat app; seed a new sample only.
Leaves the sample app running. Refuses to replace any existing application.
Credentials stay in memory and are restricted to the current box HTTPS domain.
"""
import argparse
import json
from pathlib import Path
import subprocess
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
parser.add_argument('--resume-first-release', action='store_true', help='Resume a verifier-created v1 sample after a transient proxy failure')
args = parser.parse_args()
root = Path(__file__).resolve().parents[1]
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
assert not target.username and not target.password and not target.query and not target.fragment and not target.path
ext = local('/box-ext-info?' + urllib.parse.urlencode({'url': 'https://' + box['boxDomain'] + '/'}))
cookie = next(c['value'] for c in ext['cookies'] if c['name'] == 'Lzc-Auth-Token')
class NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, *args):
        return None
opener = urllib.request.build_opener(NoRedirect())
def request(path, value=None):
    req = urllib.request.Request(manager + path, data=json.dumps(value).encode() if value is not None else None,
        headers={'Cookie': 'Lzc-Auth-Token=' + cookie, 'Content-Type': 'application/json', 'X-CoFleet-Request': '1'})
    with opener.open(req, timeout=35) as response:
        return json.load(response)
def ctl(*values):
    return json.loads(subprocess.check_output([str(args.bin_dir.resolve() / 'cofleetctl'), '--data-dir', str(args.data_dir), *values], timeout=95))
def wait(predicate, seconds=360):
    deadline = time.monotonic() + seconds
    while time.monotonic() < deadline:
        try:
            value = predicate()
            if value:
                return value
        except urllib.error.HTTPError as error:
            if error.code not in (409, 503):
                raise
        time.sleep(1)
    raise AssertionError('verification condition timed out')
wait(lambda: ctl('status')['connected'], 60)
device = ctl('status')['device_id']
path = f'/api/v1/devices/{device}/docker'
def command(action, **values):
    reply = request(path, {'action': 'pipeline', 'request': dict(action=action, **values)})
    assert not reply['error'], reply['error']
    return reply
initial = command('status')['pipeline']
assert initial['current'] is None or args.resume_first_release, 'refusing to replace existing application'
workspace = request(path, {'action': 'workspace', 'request': {'action': 'status'}})['workspace']
assert workspace['healthy'], 'start workspace before running verification'
container = workspace['container_id']
files = {str(p.relative_to(root / 'tests/fixtures/release')): p.read_text()
    for p in (root / 'tests/fixtures/release').rglob('*') if p.is_file()}
writer = """import json,pathlib,sys
base=pathlib.Path('/home/coder/project/cofleet-example/release-demo')
assert not base.exists(), 'refusing to overwrite existing sample'
base.mkdir(parents=True)
for name,text in json.load(sys.stdin).items():
 path=base/name;path.parent.mkdir(parents=True,exist_ok=True);path.write_text(text)
"""
if not args.resume_first_release:
    subprocess.run(['docker','exec','-i',container,'python3','-c',writer], input=json.dumps(files).encode(),check=True)
report = {'version': request('/api/whoami')['version'],
    'scope': 'native omarchy via current authenticated LazyCat domain; no independent WAN or low-end hardware', 'checks': {}}
def passed(name):
    report['checks'][name] = True
    print('PASS',name,flush=True)
def finish(identifier):
    def poll():
        job = next(j for j in command('status')['pipeline']['jobs'] if j['id'] == identifier)
        if job['status'] in ('failed','interrupted'):
            raise AssertionError(job.get('error') or job['status'])
        return job if job['status'] == 'succeeded' else None
    return wait(poll)
def build():
    identifier = str(uuid.uuid4())
    command('build',request_id=identifier,project_dir='cofleet-example/release-demo')
    assert command('build',request_id=identifier,project_dir='cofleet-example/release-demo')['duplicate']
    job = finish(identifier)
    assert job['image_id'].startswith('sha256:') and job['input_digest'].startswith('sha256:')
    assert command('logs',job_id=identifier)['logs']
    return identifier,job
if args.resume_first_release:
    assert initial['current'] and initial['previous'] is None, 'resume requires only the first verifier release'
    d1 = initial['current']['id']; v1 = initial['current']['build_id']
    j1 = next(j for j in initial['jobs'] if j['id'] == v1)
    assert j1['request']['project_dir'] == 'cofleet-example/release-demo'
    # Require byte-for-byte original fixture files before modifying the sample.
    verifier = "import json,pathlib,sys; b=pathlib.Path('/home/coder/project/cofleet-example/release-demo'); assert all((b/k).read_text()==v for k,v in json.load(sys.stdin).items())"
    subprocess.run(['docker','exec','-i',container,'python3','-c',verifier],input=json.dumps(files).encode(),check=True)
    assert command('build',request_id=v1,project_dir='cofleet-example/release-demo')['duplicate']
    assert command('logs',job_id=v1)['logs']
else:
    v1,j1 = build()
    d1 = str(uuid.uuid4()); command('deploy',request_id=d1,build_id=v1,expected_current=None); finish(d1)
passed('real_microserver_build_frozen_digest_logs_and_deduplication')
entry = f'/applications/{device}/'
assert wait(lambda: request(entry).get('version') == 'v1',60)
passed('immutable_v1_deployed_and_served_through_microserver')
# Edit only the newly created verifier sample, never a pre-existing user project.
subprocess.run(['docker','exec',container,'python3','-c',
    "from pathlib import Path;Path('/home/coder/project/cofleet-example/release-demo/version.txt').write_text('v2\\n')"],check=True)
v2,j2 = build(); assert j1['input_digest'] != j2['input_digest']
d2 = str(uuid.uuid4()); command('deploy',request_id=d2,build_id=v2,expected_current=d1); finish(d2)
assert wait(lambda: request(entry).get('version') == 'v2',60)
state=command('status')['pipeline'];assert state['previous']['id'] == d1
passed('v2_health_gated_update_and_previous_release_retained')
r1 = str(uuid.uuid4()); command('rollback',request_id=r1,expected_current=d2); finish(r1)
assert wait(lambda: request(entry).get('version') == 'v1',60)
state=command('status')['pipeline'];assert state['current']['image_id'] == j1['image_id'] and state['previous']['image_id'] == j2['image_id']
assert command('rollback',request_id=r1,expected_current=d2)['duplicate']
passed('rollback_restores_exact_v1_image_and_retry_does_not_repeat')
assert request(path, {'action':'workspace','request':{'action':'status'}})['workspace']['healthy']
passed('existing_workspace_remains_healthy')
args.output.parent.mkdir(parents=True,exist_ok=True)
args.output.write_text(json.dumps(report,ensure_ascii=False,indent=2)+'\n')
print('Saved sanitized report; sample app remains running',flush=True)
