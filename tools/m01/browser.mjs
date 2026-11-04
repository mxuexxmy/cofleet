// Local HTTPS experiment with sibling hosts; never uses platform credentials or production state.
import {createRequire} from 'node:module';
import {mkdir,writeFile,mkdtemp} from 'node:fs/promises';
import {spawn,execFileSync} from 'node:child_process';
import {createInterface} from 'node:readline';
import {once} from 'node:events';
import assert from 'node:assert/strict';
const {chromium}=createRequire(import.meta.url)('playwright');
const root=new URL('../../',import.meta.url);
const parent=new URL('.local/m01/',root);await mkdir(parent,{recursive:true,mode:0o700});
const dir=await mkdtemp(parent.pathname+'probe-');
execFileSync('openssl',['req','-x509','-newkey','rsa:2048','-nodes','-days','1','-keyout',dir+'/key.pem','-out',dir+'/cert.pem','-subj','/CN=*.cofleet.test','-addext','subjectAltName=DNS:manager.cofleet.test,DNS:one.cofleet.test,DNS:two.cofleet.test'],{stdio:'ignore'});
const server=spawn('python3',[new URL('tools/m01/probe.py',root).pathname,'--cert',dir+'/cert.pem','--key',dir+'/key.pem'],{stdio:['ignore','pipe','inherit']});
const lines=createInterface({input:server.stdout});const [{port}]=[JSON.parse((await once(lines,'line'))[0])];
const manager=`https://manager.cofleet.test:${port}`,one=`https://one.cofleet.test:${port}`,two=`https://two.cofleet.test:${port}`;
const browser=await chromium.launch({headless:true,executablePath:process.env.CHROMIUM_PATH||'/usr/bin/chromium',args:['--no-sandbox','--no-proxy-server','--host-resolver-rules=MAP *.cofleet.test 127.0.0.1']});
const checks={};const pass=name=>{checks[name]=true;};
try {
 const context=await browser.newContext({ignoreHTTPSErrors:true});const page=await context.newPage();
 // Browser fetch, not APIRequestContext: DNS resolver mapping and actual cookie rules apply.
 const visit=async (p,url)=>{await p.goto(url);};
 const post=async (p,path,body)=>p.evaluate(async({path,body})=>{const r=await fetch(path,{method:'POST',headers:{'Content-Type':'application/json'},body:JSON.stringify(body)});return {status:r.status,value:await r.json()};},{path,body});
 await visit(page,manager);assert.equal((await post(page,'/login',{user:'alice'})).status,200);
 const managerCookie=(await context.cookies(manager)).find(c=>c.name==='__Host-cofleet_manager');assert.ok(managerCookie.httpOnly&&managerCookie.secure&&!managerCookie.domain.startsWith('.'));pass('host_only_secure_httponly_manager_session');
 const app=await context.newPage();await visit(app,one+'/.cofleet/begin');const challenge=JSON.parse(await app.locator('body').innerText()).challenge;
 const issued=await post(page,'/grant',{challenge});assert.equal(issued.status,200);
 // Copying a grant into another browser fails without its host-only bootstrap cookie.
 const outsider=await browser.newContext({ignoreHTTPSErrors:true});const other=await outsider.newPage();await visit(other,manager);
 await other.evaluate(async({one,ticket})=>{const f=document.createElement('form');f.method='POST';f.action=one+'/.cofleet/accept';const i=document.createElement('input');i.name='ticket';i.value=ticket;f.append(i);document.body.append(f);f.submit();},{one,ticket:issued.value.ticket});
 await other.waitForURL(one+'/.cofleet/accept');assert.equal(JSON.parse(await other.locator('body').innerText()).application,undefined);pass('copied_ticket_without_browser_binding_rejected');await outsider.close();
 // A failed redemption burns the one-use ticket; restart the bootstrap for legitimate access.
 await visit(app,one+'/.cofleet/begin');const challenge2=JSON.parse(await app.locator('body').innerText()).challenge;
 const grant=await post(page,'/grant',{challenge:challenge2});assert.equal(grant.status,200);assert.equal((await post(page,'/grant',{challenge:challenge2})).status,409);pass('duplicate_grant_issue_rejected');
 await page.evaluate(({one,ticket})=>{const f=document.createElement('form');f.method='POST';f.action=one+'/.cofleet/accept';const i=document.createElement('input');i.name='ticket';i.value=ticket;f.append(i);document.body.append(f);f.submit();},{one,ticket:grant.value.ticket});
 await page.waitForURL(one+'/.cofleet/accept');await visit(page,one);assert.equal(JSON.parse(await page.locator('body').innerText()).application,'one.cofleet.test');pass('browser_form_redemption_and_application_access');
 const accessCookie=(await context.cookies(one)).find(c=>c.name==='__Host-cofleet_access');assert.ok(accessCookie.httpOnly&&accessCookie.secure&&!accessCookie.domain.startsWith('.'));assert.ok(!(await context.cookies(two)).some(c=>c.name==='__Host-cofleet_access'));pass('access_session_not_sent_to_sibling');
 await context.addCookies([{...accessCookie,domain:'two.cofleet.test'}]);await visit(app,two);assert.deepEqual(JSON.parse(await app.locator('body').innerText()),{});await context.clearCookies({name:'__Host-cofleet_access',domain:'two.cofleet.test'});pass('copied_session_rejected_on_wrong_host');
 await visit(app,manager);await app.evaluate(({one,ticket})=>{const f=document.createElement('form');f.method='POST';f.action=one+'/.cofleet/accept';const i=document.createElement('input');i.name='ticket';i.value=ticket;f.append(i);document.body.append(f);f.submit();},{one,ticket:grant.value.ticket});await app.waitForURL(one+'/.cofleet/accept');assert.equal(JSON.parse(await app.locator('body').innerText()).reason,'grant_binding');pass('redeemed_ticket_replay_rejected');

 await page.evaluate(()=>{document.cookie='__Host-cofleet_manager=forged; Domain=cofleet.test; Secure; Path=/';document.cookie='__Host-cofleet_access=forged; Domain=cofleet.test; Secure; Path=/';document.cookie='m01_parent_probe=marker; Domain=cofleet.test; Secure; Path=/';});
 assert.equal((await context.cookies(manager)).find(c=>c.name==='__Host-cofleet_manager').value,managerCookie.value);assert.equal((await context.cookies(one)).find(c=>c.name==='__Host-cofleet_access').value,accessCookie.value);pass('parent_domain_host_cookie_forgery_rejected');
 assert.ok((await context.cookies(manager)).some(c=>c.name==='m01_parent_probe'));pass('shared_parent_cookie_contamination_reproduced');
 assert.ok(!(await page.evaluate(()=>document.cookie)).includes('__Host-cofleet_access'));pass('app_javascript_cannot_read_httponly_access_session');
 const cross=await page.evaluate(async manager=>{try{await fetch(manager+'/api/private',{credentials:'include'});return false;}catch{return true;}},manager);assert.ok(cross);pass('cross_origin_management_response_unreadable');
 const csrf=await post(page,manager+'/disable',{}).catch(()=>({status:0}));assert.notEqual(csrf.status,200);await page.evaluate(manager=>{const f=document.createElement('form');f.method='POST';f.action=manager+'/disable';const i=document.createElement('input');i.name='test';i.value='1';f.append(i);document.body.append(f);f.submit();},manager);await page.waitForURL(manager+'/disable');assert.deepEqual(JSON.parse(await page.locator('body').innerText()),{});await visit(page,one);assert.equal(JSON.parse(await page.locator('body').innerText()).application,'one.cofleet.test');pass('application_origin_management_mutation_denied');
 await visit(app,one+'/api/private');assert.deepEqual(JSON.parse(await app.locator('body').innerText()),{});pass('management_path_on_application_host_denied');
 const bob=await browser.newContext({ignoreHTTPSErrors:true});const bobPage=await bob.newPage();await visit(bobPage,manager);await post(bobPage,'/login',{user:'bob'});
 await visit(app,two+'/.cofleet/begin');const c3=JSON.parse(await app.locator('body').innerText()).challenge;
 assert.equal((await post(bobPage,'/grant',{challenge:c3})).status,403);pass('fixture_other_account_cannot_authorize_owner_endpoint');await bob.close();
 await visit(app,manager);assert.equal((await post(app,'/disable',{})).status,200);await visit(page,one);assert.deepEqual(JSON.parse(await page.locator('body').innerText()),{});pass('revision_change_invalidates_existing_session');
 const fresh=await post(app,'/grant',{challenge:c3});assert.equal(fresh.status,200);
 assert.equal((await post(app,'/logout',{})).status,200);
 // Logout invalidates both existing sessions and issued, unredeemed grants.
 await visit(page,one);assert.deepEqual(JSON.parse(await page.locator('body').innerText()),{});pass('manager_logout_invalidates_application_session');
 await visit(app,manager);await app.evaluate(({two,ticket})=>{const f=document.createElement('form');f.method='POST';f.action=two+'/.cofleet/accept';const i=document.createElement('input');i.name='ticket';i.value=ticket;f.append(i);document.body.append(f);f.submit();},{two,ticket:fresh.value.ticket});await app.waitForURL(two+'/.cofleet/accept');assert.equal(JSON.parse(await app.locator('body').innerText()).reason,'grant_binding');pass('logout_invalidates_unredeemed_grant');
 const report={stage:'M0.1-local',checks,strict_parent_cookie_isolation:false,production_ready:false,limitations:['Local self-signed TLS trusted only by isolated browser context; no platform TLS validation.','Fixture accounts only; no real second LazyCat account or platform logout tested.','Shared parent cookie writes remain possible; platform cookie poisoning and availability impact are unresolved.','This probe is separate from the production Manager; no production access gate is bypassed.']};
 await writeFile(dir+'/results.json',JSON.stringify(report,null,2));console.log(JSON.stringify({...report,output:dir+'/results.json'}));await context.close();
}finally{await browser.close();server.kill('SIGTERM');if(server.exitCode===null)await once(server,'exit');lines.close();}
