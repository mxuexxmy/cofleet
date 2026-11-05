#!/usr/bin/env python3
"""Real Manager/Agent with a read-only fake engine; no user's Docker daemon is changed."""
import hashlib
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
import os
from pathlib import Path
import secrets
import socket
import subprocess
import tempfile
import threading
import time
import tomllib
import urllib.error
import urllib.request
import uuid

bin_dir = Path('target/debug').resolve()
class Fixture(BaseHTTPRequestHandler):
    def log_message(self, *args): pass
    def do_GET(self):
        self.send_response(200); self.end_headers()
        try:
            if self.path == '/stream':
                while True:
                    self.wfile.write(b'fixture stream\n'); self.wfile.flush(); time.sleep(.05)
            else: self.wfile.write(b'fixture')
        except (BrokenPipeError, ConnectionResetError): pass
server = ThreadingHTTPServer(('127.0.0.1', 0), Fixture)
threading.Thread(target=server.serve_forever, daemon=True).start()
with tempfile.TemporaryDirectory(prefix='cofleet-engine-') as temporary:
    root = Path(temporary); node = root/'node'; bins = root/'bin'; bins.mkdir()
    mode = root/'engine'; mode.write_text('a')
    token = secrets.token_urlsafe(32)
    env = dict(os.environ, COFLEET_TOKEN=token, PATH=str(bins)+os.pathsep+os.environ['PATH'])
    with socket.socket() as sock: sock.bind(('127.0.0.1',0)); port=sock.getsockname()[1]
    base=f'http://127.0.0.1:{port}'
    (bins/'docker').write_text(f'''#!{__import__('sys').executable}
import json,sys
from pathlib import Path
args=sys.argv[3:]
assert args[0] in ('info','ps','inspect','logs'), 'Mutation forbidden'
mode=Path({str(mode)!r}).read_text()
if mode=='down': print('Cannot connect to Docker daemon',file=sys.stderr);sys.exit(1)
if args[0]=='info': print('fixture-engine-'+mode+'-000001')
elif args[0]=='ps': print('a'*64)
elif args[0]=='logs': print('fixture logs')
else:
 print(json.dumps({{'Id':'a'*64,'Name':'/fixture','Config':{{'Image':'fixture','Labels':{{}}}},'State':{{'Status':'running'}},'HostConfig':{{'PortBindings':{{'8080/tcp':[{{'HostIp':'127.0.0.1','HostPort':str({server.server_port})}}]}}}},'NetworkSettings':{{'Ports':{{'8080/tcp':[{{'HostIp':'127.0.0.1','HostPort':str({server.server_port})}}]}}}}}}))
'''); (bins/'docker').chmod(0o700)
    processes=[];log=open(root/'processes.log','ab')
    def start_agent():
        p=subprocess.Popen([str(bin_dir/'cofleet-agent'),'--data-dir',str(node),'--docker-observe'],env=env,stdout=log,stderr=log);processes.append(p);return p
    def api(path, body=None, method=None, host=None):
        req=urllib.request.Request(base+path,data=json.dumps(body).encode() if body is not None else None,method=method,headers={'Authorization':'Bearer '+token,'X-CoFleet-Request':'1','Content-Type':'application/json',**({'Host':host} if host else {})})
        with urllib.request.urlopen(req,timeout=20) as r:return json.load(r)
    def wait(fn):
        end=time.monotonic()+25
        while time.monotonic()<end:
            try:
                value=fn()
                if value:return value
            except (OSError,ValueError):pass
            time.sleep(.1)
        raise AssertionError('condition did not become true')
    def stop(p):
        if p.poll() is None:p.terminate();p.wait(timeout=10)
    try:
        processes.append(subprocess.Popen([str(bin_dir/'cofleet-manager'),'--listen',f'127.0.0.1:{port}','--state-dir',str(root/'manager'),'--access-domain','apps.example.net'],env=env,stdout=log,stderr=log))
        wait(lambda:api('/api/v1/devices')==[])
        subprocess.run([str(bin_dir/'cofleetctl'),'--data-dir',str(node),'init','--manager',base,'--name','engine fixture','--heartbeat-secs','1'],env=env,check=True,stdout=subprocess.DEVNULL)
        device=tomllib.loads((node/'agent.toml').read_text())['device_id']
        legacy=node/'docker-observe-state.json';legacy.write_text(json.dumps({'device_id':device,'environment_id':'local-'+hashlib.sha256(b'cofleet-docker-observe-v1\0'+uuid.UUID(device).bytes+b'linux'+b'unix:///var/run/docker.sock').hexdigest(),'definitions':{}}));original=legacy.read_bytes()
        agent=start_agent()
        current=lambda:next((d for d in api('/api/v1/devices') if d['device_id']==device),None)
        first=wait(lambda:(d:=current()) and d['online'] and d['docker_observe']['engine_verified'] and d)
        env_a=first['docker_observe']['environment_id'];assert env_a.startswith('engine-v2-')
        id=str(uuid.uuid4());path=f'/api/v1/devices/{device}/docker'
        body={'id':id,'device_id':device,'environment_id':env_a,'binding':{'kind':'container','container_id':'a'*64},'container_port':8080,'protocol':'http','name':'fixture'}
        endpoint=api('/api/v1/access-endpoints',body);authority=f'e-{uuid.UUID(id).hex}.apps.example.net'
        stream=urllib.request.urlopen(urllib.request.Request(base+'/stream',headers={'Host':authority,'Authorization':'Bearer '+token}),timeout=15);assert stream.read(1)
        mode.write_text('b');started=time.monotonic()
        try:
            while stream.read(8192):
                assert time.monotonic()-started<14
        except __import__('http.client').client.IncompleteRead:pass
        finally:stream.close()
        second=wait(lambda:(d:=current()) and d['docker_observe']['engine_verified'] and d['docker_observe']['environment_id']!=env_a and d)
        env_b=second['docker_observe']['environment_id']
        try:api('/',host=authority);raise AssertionError('Old engine binding allowed')
        except urllib.error.HTTPError as e:assert e.code in (409,503)
        rebound=api('/api/v1/access-endpoints/'+id+'/binding',{**body,'environment_id':env_b,'revision':endpoint['revision']},'PUT')
        assert rebound['id']==id and rebound['public_url']==endpoint['public_url'] and rebound['revision']==2
        with urllib.request.urlopen(urllib.request.Request(base+'/',headers={'Host':authority,'Authorization':'Bearer '+token}),timeout=15) as r:assert r.read()==b'fixture'
        mode.write_text('down');wait(lambda:(d:=current()) and d['online'] and not d['docker_observe']['engine_verified'])
        assert api(path+'/catalog')['result']=='error'
        mode.write_text('b');wait(lambda:(d:=current()) and d['docker_observe']['engine_verified'])
        stop(agent);agent=start_agent();wait(lambda:(d:=current()) and d['online'] and d['docker_observe']['engine_verified'] and d['docker_observe']['environment_id']==env_b)
        assert legacy.read_bytes()==original
        print('PASS engine-change revokes live stream and old binding; explicit rebind preserves URL; engine unavailable keeps Agent online; identity stable on restart; v1 state untouched')
    finally:
        for p in reversed(processes):stop(p)
        log.close()
server.shutdown();server.server_close()
