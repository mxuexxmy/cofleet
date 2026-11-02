#!/usr/bin/env python3
"""Verify a deployed hub using the real local LazyCat login; never saves cookies.
The application stays installed. The temporary test Agent is stopped on exit.
"""
import argparse
from datetime import datetime
import json
from pathlib import Path
import socket
import subprocess
import tempfile
import time
import urllib.error
import urllib.parse
import urllib.request

parser = argparse.ArgumentParser()
parser.add_argument('--manager', required=True)
parser.add_argument('--bin-dir', type=Path, default=Path('target/release'))
parser.add_argument('--output', type=Path, required=True)
parser.add_argument('--restart-container', help='Optional: restart only this deployed test container with lzc-cli')
args = parser.parse_args()
binaries = args.bin_dir.resolve()
core = json.loads((Path.home()/'.config/lzc-client-desktop/core.cfg').read_text())
local_url = f'http://127.0.0.1:{core["http_port"]}'
local_opener = urllib.request.build_opener(urllib.request.ProxyHandler({}))

def local(path):
    req = urllib.request.Request(local_url+path, headers={'LZC_MISC_AUTH_TOKEN': core['extra_headers']['LZC_MISC_AUTH_TOKEN']})
    with local_opener.open(req, timeout=10) as r:
        return json.load(r)

box = local('/default-box')
manager = args.manager.rstrip('/')
u = urllib.parse.urlparse(manager)
assert u.scheme == 'https' and u.hostname.endswith('.'+box['boxDomain']) and (u.port or 443)==443, 'manager must belong to the current default box'
ext = local('/box-ext-info?'+urllib.parse.urlencode({'url':f'https://{box["boxDomain"]}/'}))
cookie = next(c['value'] for c in ext['cookies'] if c['name']=='Lzc-Auth-Token')
class NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, *unused):
        return None
opener = urllib.request.build_opener(NoRedirect())

def request(path, method='GET', authenticated=True, headers=None):
    values = dict(headers or {})
    if authenticated:
        values['Cookie'] = 'Lzc-Auth-Token='+cookie
    req = urllib.request.Request(manager+path, headers=values, data=b'' if method=='POST' else None, method=method)
    with opener.open(req, timeout=10) as r:
        return r.status, json.load(r)

def wait_for(fn, timeout=60):
    deadline = time.monotonic()+timeout
    while time.monotonic()<deadline:
        try:
            result=fn()
            if result:
                return result
        except (OSError, ValueError, subprocess.CalledProcessError):
            pass
        time.sleep(.5)
    raise AssertionError('verification condition timed out')

report = {'measured_at':datetime.now().astimezone().isoformat(), 'manager':manager, 'box':box['boxName'], 'checks':{}}
status, who = request('/api/whoami')
assert status==200 and who['auth']=='lazycat'
report['checks']['real_platform_login']=True
report['platform_user']=who['user']
for label, headers in [('anonymous_rejected',{}),('forged_user_header_rejected',{'X-HC-User-ID':who['user']})]:
    try:
        request('/api/v1/devices', authenticated=False, headers=headers)
        raise AssertionError('unauthenticated request reached device API')
    except urllib.error.HTTPError as error:
        assert error.code in (301,302,303,307,308,401,403), error.code
        report['checks'][label]=error.code
with tempfile.TemporaryDirectory(prefix='cf-lzc-') as temp:
    data=Path(temp)/'node'
    def ctl(*commands):
        return subprocess.check_output([binaries/'cofleetctl','--data-dir',data,*commands], text=True, stderr=subprocess.PIPE, timeout=10)
    ctl('init','--auth','lazycat','--manager',manager,'--name','verify-'+socket.gethostname(),'--heartbeat-secs','1')
    def status():
        return json.loads(ctl('status'))
    with tempfile.TemporaryFile() as log:
        agent=subprocess.Popen([binaries/'cofleet-agent','--data-dir',data], stdout=log,stderr=log)
        try:
            wait_for(lambda:status()['connected'])
            device=status()['device_id']
            report['device_id']=device
            def device_record():
                return next((d for d in request('/api/v1/devices')[1] if d['device_id']==device),None)
            wait_for(lambda:device_record() and device_record()['online'])
            report['checks']['real_wss_agent_connection']=True
            ctl('pause')
            wait_for(lambda:not device_record()['accepting_tasks'])
            ctl('resume')
            wait_for(lambda:device_record()['accepting_tasks'])
            report['checks']['pause_resume_propagated']=True
            code, refresh=request(f'/api/v1/devices/{device}/refresh',method='POST')
            assert code==202
            wait_for(lambda:device_record()['last_report_id']==refresh['request_id'])
            report['checks']['hub_command_and_reply']=True
            if args.restart_container:
                command=['mise','exec','node@26.8.2','--','lzc-cli','docker','restart',args.restart_container]
                result=subprocess.run(command,text=True,capture_output=True,timeout=90)
                assert result.returncode==0, 'test container restart failed'
                wait_for(lambda:device_record() and device_record()['online'],timeout=90)
                assert status()['device_id']==device
                code, refresh=request(f'/api/v1/devices/{device}/refresh',method='POST')
                assert code==202
                wait_for(lambda:device_record()['last_report_id']==refresh['request_id'])
                report['checks']['hub_restart_reconnect_same_device']=True
        finally:
            if agent.poll() is None:
                agent.terminate()
                try:agent.wait(timeout=5)
                except subprocess.TimeoutExpired:agent.kill();agent.wait()
    wait_for(lambda:device_record() and not device_record()['online'])
    report['checks']['agent_exit_marks_offline']=True
    report['test_agent_running']=False
args.output.parent.mkdir(parents=True,exist_ok=True)
args.output.write_text(json.dumps(report,ensure_ascii=False,indent=2)+'\n')
print(json.dumps(report,ensure_ascii=False,indent=2))
