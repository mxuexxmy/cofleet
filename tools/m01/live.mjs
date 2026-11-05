// Explicit live acceptance: creates/reuses ONLY the HTTP validation endpoint, then leaves it enabled.
import {createRequire} from 'node:module';
import {readFile,writeFile,mkdir} from 'node:fs/promises';
import assert from 'node:assert/strict';
const {chromium}=createRequire(import.meta.url)('playwright');
const root=new URL('../../',import.meta.url);
const core=JSON.parse(await readFile(process.env.HOME+'/.config/lzc-client-desktop/core.cfg','utf8'));
const local=async path=>(await fetch('http://127.0.0.1:'+core.http_port+path,{headers:core.extra_headers})).json();
const box=await local('/default-box');const ext=await local('/box-ext-info?'+new URLSearchParams({url:'https://'+box.boxDomain+'/'}));
const token=ext.cookies.find(c=>c.name==='Lzc-Auth-Token').value;
const base=(process.env.COFLEET_M01_MANAGER_URL || (await readFile(new URL('.local/current-manager-url',root),'utf8'))).trim().replace(/\/$/,'');assert.ok(new URL(base).hostname.endsWith('.'+box.boxDomain));
const browser=await chromium.launch({headless:true,executablePath:process.env.CHROMIUM_PATH||'/usr/bin/chromium',args:['--no-sandbox']});
const output=new URL(process.env.COFLEET_ACCESS_VERIFY_OUTPUT_DIR || '.local/release-0.7.1/',root);await mkdir(output,{recursive:true});
try{
 const context=await browser.newContext();await context.addCookies([{name:'Lzc-Auth-Token',value:token,domain:'.'+box.boxDomain,path:'/',secure:true,httpOnly:true,sameSite:'Lax'}]);
 const page=await context.newPage();const errors=[];page.on('pageerror',e=>errors.push(e.message));await page.goto(base);await page.locator('.device').first().waitFor();await page.locator('#nav-applications').click();
 const who=await (await context.request.get(base+'/api/whoami')).json();assert.equal(who.access_mode,'trusted_prefix');
 const list=await (await context.request.get(base+'/api/v1/access-endpoints')).json();let endpoint=list.find(e=>e.name==='HTTP 验证');
 if(!endpoint){
  await page.locator('#endpoint-discover').click();await page.waitForFunction(()=>[...document.querySelector('#endpoint-target').options].some(o=>o.textContent.includes('cofleet-http-test')));
  const value=await page.locator('#endpoint-target option').evaluateAll(options=>options.find(o=>o.textContent.includes('cofleet-http-test')).value);
  await page.locator('#endpoint-target').selectOption(value);await page.locator('#endpoint-binding').selectOption('container');await page.locator('#endpoint-name').fill('HTTP 验证');await page.locator('#endpoint-trust').check();
  const pending=page.waitForResponse(r=>r.url()===base+'/api/v1/access-endpoints'&&r.request().method()==='POST');await page.locator('#endpoint-submit').click();const created=await pending;assert.equal(created.status(),201,await created.text());endpoint=await created.json();
 }
 if(endpoint&&!endpoint.enabled) {const result=await page.evaluate(async e=>{const r=await fetch('api/v1/access-endpoints/'+e.id,{method:'PATCH',headers:{'Content-Type':'application/json','X-CoFleet-Request':'1'},body:JSON.stringify({revision:e.revision,enabled:true})});return {status:r.status,value:await r.json()};},endpoint);assert.equal(result.status,200);endpoint=result.value;}
 const app=await context.newPage();let response=await app.goto(endpoint.public_url);assert.equal(response.status(),200);
 await app.getByRole('heading',{name:'打开受信任应用',exact:true}).waitFor();await app.getByRole('button',{name:'继续打开',exact:true}).click();await app.getByRole('heading',{name:'授权已就绪',exact:true}).waitFor();await app.getByRole('button',{name:'打开应用',exact:true}).click();await app.getByRole('heading',{name:'HTTP 服务访问成功',exact:true}).waitFor();
 assert.ok(new URL(app.url()).hostname.startsWith('e-'));const cookies=await context.cookies(endpoint.public_url);assert.ok(cookies.some(c=>c.name==='__Host-cofleet_access'&&c.httpOnly&&c.secure));assert.ok(!cookies.some(c=>c.name==='__Host-cofleet_manager'));
 const health=await context.request.get(endpoint.public_url+'health');assert.equal(health.status(),200);assert.match(await health.text(),/^ok/);
 const forbidden=await context.request.get(endpoint.public_url+'api/v1/devices',{maxRedirects:0});assert.equal(forbidden.status(),404); // Upstream HTTP fixture, not management API.
 // Reuse the existing, self-controlled M0 fixture for application login, headers and WebSocket.
 const fixture=JSON.parse(await readFile(new URL('.local/m0/current.json',root),'utf8'));
 let all=await (await context.request.get(base+'/api/v1/access-endpoints')).json();let loginEndpoint=all.find(e=>e.name==='登录与 WebSocket 验证'&&e.enabled);
 if(!loginEndpoint){await page.locator('#endpoint-discover').click();await page.waitForFunction(name=>[...document.querySelector('#endpoint-target').options].some(o=>o.textContent.includes(name)&&o.textContent.includes('8800→')),fixture.fixture);
 const value=await page.locator('#endpoint-target option').evaluateAll((options,name)=>options.find(o=>o.textContent.includes(name)&&o.textContent.includes('8800→')).value,fixture.fixture);
 await page.locator('#endpoint-target').selectOption(value);await page.locator('#endpoint-binding').selectOption('container');await page.locator('#endpoint-name').fill('登录与 WebSocket 验证');await page.locator('#endpoint-trust').check();const pending=page.waitForResponse(r=>r.url()===base+'/api/v1/access-endpoints'&&r.request().method()==='POST');await page.locator('#endpoint-submit').click();const response=await pending;assert.equal(response.status(),201);loginEndpoint=await response.json();}
 const login=await context.newPage();await login.goto(loginEndpoint.public_url);await login.getByRole('button',{name:'继续打开',exact:true}).click();await login.getByRole('button',{name:'打开应用',exact:true}).click();await login.getByRole('heading',{name:'Application login',exact:true}).waitFor();await login.locator('[name=password]').fill(fixture.password);await login.getByRole('button',{name:'Sign in',exact:true}).click();await login.getByRole('heading',{name:'Application signed in',exact:true}).waitFor();await login.waitForFunction(()=>document.querySelector('#ws')?.textContent==='m0-echo');
 const headers=await (await context.request.get(loginEndpoint.public_url+'headers',{headers:{Authorization:'Bearer m01-fixture-only'}})).json();assert.equal(headers.identity_leaked,false);assert.equal(headers.authorization,'Bearer m01-fixture-only');assert.ok(!String(headers.cookie).match(/Lzc-Auth-Token|HC-Auth-Token|__Host-cofleet/));
 const change=async enabled=>page.evaluate(async({id,enabled})=>{const list=await (await fetch('api/v1/access-endpoints')).json();const old=list.find(e=>e.id===id);const r=await fetch('api/v1/access-endpoints/'+id,{method:'PATCH',headers:{'Content-Type':'application/json','X-CoFleet-Request':'1'},body:JSON.stringify({revision:old.revision,enabled})});return r.status;},{id:endpoint.id,enabled});
 assert.equal(await change(false),200);response=await app.goto(endpoint.public_url);assert.ok(!(await app.locator('body').innerText()).includes('HTTP 服务访问成功'));const deniedHealth=await context.request.get(endpoint.public_url+'health');assert.notEqual((await deniedHealth.text()).trim(),'ok');assert.equal(await change(true),200);
 await app.goto(endpoint.public_url);await app.getByRole('heading',{name:'打开受信任应用',exact:true}).waitFor();await app.getByRole('button',{name:'继续打开',exact:true}).click();await app.getByRole('button',{name:'打开应用',exact:true}).click();await app.getByRole('heading',{name:'HTTP 服务访问成功',exact:true}).waitFor();
 await app.screenshot({path:new URL('http-access.png',output).pathname,fullPage:true});
 const logout=await page.evaluate(async()=>{const r=await fetch('api/v1/access-session',{method:'DELETE',headers:{'X-CoFleet-Request':'1'}});return r.status;});assert.equal(logout,204);await login.waitForFunction(()=>ws.readyState===WebSocket.CLOSED,{},{timeout:12000});
 await app.goto(endpoint.public_url);await app.getByRole('heading',{name:'请先打开管理中心',exact:true}).waitFor();
 assert.deepEqual(errors,[]);
 const report={version:who.version,public_url:endpoint.public_url,checks:{trusted_consent_and_creation:true,automatic_prefix_host_tls_and_platform_identity:true,browser_bound_authorization:true,http_service_access:true,health_path:true,host_only_access_cookie:true,management_cookie_not_sent_to_app:true,application_host_not_management_api:true,disable_denies_access:true,enable_requires_fresh_grant:true,manager_logout_revokes_access:true,browser_errors_absent:true,application_cookie_login:true,application_authorization_preserved:true,identity_and_gateway_cookies_stripped:true,websocket_echo:true,logout_closes_live_websocket:true},limitations:['Shared parent ordinary cookies remain possible: trusted applications only.','Second real account and platform-side logout were not tested.']};
 await writeFile(new URL('trusted-access-verification.json',output),JSON.stringify(report,null,2));console.log(JSON.stringify(report));await context.close();
}finally{await browser.close();}
