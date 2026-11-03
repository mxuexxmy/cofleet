// Real-browser M0 checks. No persistent browser profile, exported cookies or real account logout.
import {createRequire} from 'node:module';
import {readFile,writeFile,open} from 'node:fs/promises';
import {spawn} from 'node:child_process';
import {once} from 'node:events';
import assert from 'node:assert/strict';
const {chromium}=createRequire(import.meta.url)('playwright');
const root=new URL('../../',import.meta.url);
const state=JSON.parse(await readFile(new URL('.local/m0/current.json',root),'utf8'));
const core=JSON.parse(await readFile(process.env.HOME+'/.config/lzc-client-desktop/core.cfg','utf8'));
const local=async path=>(await fetch('http://127.0.0.1:'+core.http_port+path,{headers:core.extra_headers})).json();
const box=await local('/default-box');
const ext=await local('/box-ext-info?'+new URLSearchParams({url:'https://'+box.boxDomain+'/'}));
const token=ext.cookies.find(c=>c.name==='Lzc-Auth-Token').value;
const domain=state.settings.domain,base='https://'+domain,login='https://login-'+domain,staticUrl='https://static-'+domain;
assert.ok(domain.endsWith('.'+box.boxDomain));
const log=await open(state.directory+'/browser-node.log','w');
const node=spawn(new URL('target/x86_64-unknown-linux-musl/release/cofleet-access-probe',root).pathname,['node','--config',state.directory+'/content/config.json','--node-config',new URL('.local/runtime/node',root).pathname],{stdio:['ignore',log.fd,log.fd]});
const browser=await chromium.launch({headless:true,executablePath:process.env.CHROMIUM_PATH||'/usr/bin/chromium',args:['--no-sandbox']});
try {
 const context=await browser.newContext();
 await context.addCookies([{name:'Lzc-Auth-Token',value:token,domain:'.'+box.boxDomain,path:'/',httpOnly:true,secure:true,sameSite:'Lax'}]);
 let ready=false;
 for(let n=0;n<30;n++){const r=await context.request.get(base+'/api/m0/status',{maxRedirects:0});if(r.ok()&&(await r.json()).node_connected){ready=true;break;}await new Promise(r=>setTimeout(r,500));}
 assert.ok(ready,'node connected');
 const page=await context.newPage();page.setDefaultTimeout(10000);
 await page.goto(login);await page.getByRole('heading',{name:'Application login',exact:true}).waitFor();
 await page.locator('[name=password]').fill(state.password);await page.getByRole('button',{name:'Sign in',exact:true}).click();
 await page.getByRole('heading',{name:'Application signed in',exact:true}).waitFor();await page.waitForFunction(()=>document.querySelector('#ws')?.textContent==='m0-echo');
 await page.screenshot({path:state.directory+'/browser-login.png'});
 const initial=await context.cookies(login);assert.ok(initial.some(c=>c.name==='m0_session'&&c.httpOnly&&c.secure));assert.ok(!(await context.cookies(staticUrl)).some(c=>c.name==='m0_session'));
 await page.goto(login+'/cookies');const cookies=await context.cookies(login);assert.ok(cookies.some(c=>c.name==='m0_domain'&&c.domain==='login-'+domain));assert.ok(cookies.some(c=>c.name==='m0_second'));assert.equal(cookies.find(c=>c.name==='Lzc-Auth-Token').value,token);
 // Independent origins do not necessarily isolate cookies scoped to their common parent.
 const parentCookie='m0_parent_scope_probe';
 await page.evaluate(({parentCookie,parent})=>{document.cookie=parentCookie+'=harmless-marker; Domain=.'+parent+'; Path=/; Secure; SameSite=Lax';},{parentCookie,parent:box.boxDomain});
 const parentCookieBlocked=!(await context.cookies(base)).some(c=>c.name===parentCookie);
 await context.clearCookies({name:parentCookie});
 const main=await context.newPage();await main.goto(base+'/api/m0/status');
 const cross=await page.evaluate(async base=>{try{await fetch(base+'/api/m0/status',{credentials:'include'});return 'readable';}catch{return 'blocked';}},base);assert.equal(cross,'blocked');
 assert.equal((await context.request.get(login+'/api/m0/status',{maxRedirects:0})).status(),404);
 await page.goto(login);await page.waitForFunction(()=>document.querySelector('#ws')?.textContent==='m0-echo');
 await context.clearCookies();
 const response=await context.request.get(login,{maxRedirects:0});assert.ok([302,303,307,401,403].includes(response.status()));
 const wsDenied=await page.evaluate(()=>new Promise(resolve=>{const ws=new WebSocket('wss://'+location.host+'/ws');const timer=setTimeout(()=>{ws.close();resolve(false)},5000);ws.onopen=()=>{clearTimeout(timer);ws.close();resolve(false)};ws.onerror=()=>{clearTimeout(timer);resolve(true)}}));assert.ok(wsDenied);
 const report={stage:'M0-browser',strict_cookie_isolation_passed:parentCookieBlocked,findings:parentCookieBlocked?[]:['Application JavaScript can set a parent-domain cookie visible to the management origin; response Set-Cookie filtering does not prevent this.'],checks:{application_login_form:true,application_cookie_session:true,websocket_after_login:true,host_only_application_session_not_sent_to_other_origin:true,host_only_set_cookie:true,platform_cookie_not_overwritten:true,cross_origin_management_read_blocked:true,management_path_on_application_host_rejected:true,removed_browser_login_cookie_blocks_new_http_and_ws:true},limitations:['Cookie removal only: real platform session revocation and a second real account were not tested.']};
 await writeFile(state.directory+'/browser-verification.json',JSON.stringify(report,null,2));console.log(JSON.stringify(report));
 if(process.env.M0_REQUIRE_STRICT_ISOLATION==='1') assert.ok(parentCookieBlocked,'M0 gate: parent-domain cookie write remains possible');
 await context.close();
} finally {
 await browser.close();node.kill('SIGTERM');if(node.exitCode===null)await once(node,'exit');await log.close();
}
