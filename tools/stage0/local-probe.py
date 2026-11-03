#!/usr/bin/env python3
"""Temporary Linux loopback probe; credentials remain in memory and stdin."""
import argparse,os
import json,urllib.request,urllib.parse,struct,secrets,threading,subprocess,uuid
from pathlib import Path
from http.server import ThreadingHTTPServer,BaseHTTPRequestHandler

def varint(n):
 out=bytearray()
 while n>127:out.append((n&127)|128);n>>=7
 out.append(n);return bytes(out)
def field(n,value):
 value=value.encode() if isinstance(value,str) else value
 return varint((n<<3)|2)+varint(len(value))+value
def fields(b):
 i=0
 def read():
  nonlocal i
  n=0
  for shift in range(0,70,7):
   x=b[i];i+=1;n|=(x&127)<<shift
   if x<128:return n
  raise ValueError('varint overflow')
 result={}
 while i<len(b):
  tag=read();wire=tag&7
  if wire==0:v=read()
  elif wire==2:n=read();v=b[i:i+n];i+=n
  elif wire==1:v=b[i:i+8];i+=8
  elif wire==5:v=b[i:i+4];i+=4
  else:raise ValueError('wire type')
  result.setdefault(tag>>3,[]).append(v)
 return result
def frame(b):return b'\0'+len(b).to_bytes(4,'big')+b
def unframe(b):
 i=0;data=b'';status=None
 while i<len(b):
  if len(b)-i<5:raise ValueError('truncated RPC frame')
  flag=b[i];n=int.from_bytes(b[i+1:i+5],'big');i+=5
  if n>len(b)-i:raise ValueError('truncated RPC payload')
  chunk=b[i:i+n];i+=n
  if flag&128:
   for line in chunk.decode().splitlines():
    if line.startswith('grpc-status:'):status=line.partition(':')[2].strip()
  else:data=chunk
 if i!=len(b) or status!='0':raise ValueError(f'RPC status {status}')
 return data
parser=argparse.ArgumentParser(description=__doc__)
parser.add_argument('--container',required=True)
parser.add_argument('--cases',nargs='+',choices=['baseline','probe-secret-only','platform-token-only','anonymous','wrong-platform-token','wrong-probe-secret','foreign-owner'],default=['baseline','probe-secret-only','platform-token-only','anonymous','wrong-platform-token','wrong-probe-secret','foreign-owner'])
parser.add_argument('--data-dir',default='.local/runtime/node')
parser.add_argument('--ctl',default='target/release/cofleetctl')
parser.add_argument('--output',default='.local/phase0/gateway-proof.json')
args=parser.parse_args()
core=json.loads((Path.home()/'.config/lzc-client-desktop/core.cfg').read_text())
def local(path):
 with urllib.request.urlopen(urllib.request.Request(f'http://127.0.0.1:{core["http_port"]}'+path,headers=core['extra_headers']),timeout=10) as r:return json.load(r)
box=local('/default-box');ext=local('/box-ext-info?'+urllib.parse.urlencode({'url':'https://'+box['boxDomain']+'/'}))
def shell(method,payload=b''):
 req=urllib.request.Request('http://'+ext['grpc']['addr']+'/space.heiyu.hportal.shell.'+method,data=frame(payload),headers={'content-type':'application/grpc-web+proto','x-grpc-web':'1','lzc-shellapi-cred':ext['grpc']['cred']})
 with urllib.request.urlopen(req,timeout=10) as r:return unframe(r.read())
info=fields(shell('ShellCore/QueryShellCoreInfo'))
peer=info[1][0].decode()
print('Core version:',info.get(6,[b''])[0].decode(),'OS:',info.get(9,[b''])[0].decode())
secret=secrets.token_urlsafe(32);nonce=uuid.uuid4().hex
class Handler(BaseHTTPRequestHandler):
 def log_message(self,*a):pass
 def do_GET(self):
  if self.headers.get('X-CoFleet-Probe-Key')!=secret:self.send_response(403);self.end_headers();return
  agent=json.loads(subprocess.check_output([args.ctl,'--data-dir',args.data_dir,'status'],stderr=subprocess.PIPE))
  data=json.dumps({'probe':'cofleet-stage0','nonce':nonce,'agent':agent}).encode()
  self.send_response(200);self.send_header('Content-Type','application/json');self.send_header('Content-Length',str(len(data)));self.end_headers();self.wfile.write(data)
server=ThreadingHTTPServer(('127.0.0.1',0),Handler);threading.Thread(target=server.serve_forever,daemon=True).start()
service='xyz.mxue.lzc.app.cofleet.stage0.'+uuid.uuid4().hex
payload=b'\x08\x01'+field(2,service)+field(3,f'http://127.0.0.1:{server.server_port}')+field(4,secrets.token_urlsafe(32))+field(5,'X-CoFleet-Gateway-Marker')+field(6,b'\x18\x01')
shell('LocalServiceGateway/RegisterLocalService',payload)
print('Local HTTP test service registered')



agent=json.loads(subprocess.check_output([args.ctl,'--data-dir',args.data_dir,'status'],stderr=subprocess.PIPE))
params={'Owner':box['loginUser'],'Peer':peer,'Service':service,'Secret':secret,'Nonce':nonce,'ExpectedDeviceID':agent['device_id']}
cmd=['lzc-cli','docker','exec','-i',args.container,'/tmp/cofleet-stage0-probe']
checks={}
try:
 for case in args.cases:
  values=dict(params,Case=case)
  if case=='foreign-owner':values['Owner']='cofleet-unowned-'+uuid.uuid4().hex
  result=subprocess.run(cmd,input=json.dumps(values).encode(),capture_output=True,timeout=40)
  if result.returncode:raise RuntimeError(case+': '+result.stderr.decode()[:160])
  checks[case]=json.loads(result.stdout);print(case,checks[case])
 output=Path(args.output);output.parent.mkdir(parents=True,exist_ok=True)
 with os.fdopen(os.open(output,os.O_WRONLY|os.O_CREAT|os.O_TRUNC,0o600),'w') as handle:json.dump(checks,handle,indent=2)
finally:server.shutdown();server.server_close()
