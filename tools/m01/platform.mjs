// Cookie-prefix behavior on actual automatically generated LazyCat domains.
// Synthetic marker cookies only; never overwrites a platform or application session.
import {createRequire} from 'node:module';
import {readFile,writeFile,mkdir} from 'node:fs/promises';
import assert from 'node:assert/strict';
const {chromium}=createRequire(import.meta.url)('playwright');
const root=new URL('../../',import.meta.url);
const state=JSON.parse(await readFile(new URL('.local/m0/current.json',root),'utf8'));
const core=JSON.parse(await readFile(process.env.HOME+'/.config/lzc-client-desktop/core.cfg','utf8'));
const local=async path=>(await fetch('http://127.0.0.1:'+core.http_port+path,{headers:core.extra_headers})).json();
const box=await local('/default-box');
assert.ok(state.settings.domain.endsWith('.'+box.boxDomain));
const ext=await local('/box-ext-info?'+new URLSearchParams({url:'https://'+box.boxDomain+'/'}));
const token=ext.cookies.find(c=>c.name==='Lzc-Auth-Token').value;
const base='https://'+state.settings.domain,app='https://static-'+state.settings.domain;
const browser=await chromium.launch({headless:true,executablePath:process.env.CHROMIUM_PATH||'/usr/bin/chromium',args:['--no-sandbox']});
try{
 const context=await browser.newContext();
 await context.addCookies([{name:'Lzc-Auth-Token',value:token,domain:'.'+box.boxDomain,path:'/',secure:true,httpOnly:true,sameSite:'Lax'}]);
 const page=await context.newPage();const response=await page.goto(base);assert.equal(response.status(),200);
 const markerName='__Host-cofleet_m01_probe';
 await context.addCookies([{name:markerName,value:'synthetic-host-bound-marker',domain:state.settings.domain,path:'/',secure:true,httpOnly:true,sameSite:'Lax'}]);
 const appResponse=await page.goto(app);assert.equal(appResponse.status(),200,'fixed M0 static service ready');
 assert.ok(!(await context.cookies(app)).some(c=>c.name===markerName));
 await page.evaluate(({name,parent})=>{document.cookie=name+'=forged; Domain=.'+parent+'; Secure; Path=/';document.cookie='m01_parent_marker=harmless; Domain=.'+parent+'; Secure; Path=/';},{name:markerName,parent:box.boxDomain});
 const cookies=await context.cookies(base);
 assert.equal(cookies.find(c=>c.name===markerName).value,'synthetic-host-bound-marker');
 assert.ok(cookies.some(c=>c.name==='m01_parent_marker'));
 await page.goto(base);assert.ok(!(await page.evaluate(()=>document.cookie)).includes(markerName));
 const report={stage:'M0.1-platform-cookie-prefix',checks:{actual_generated_hosts_and_platform_tls:true,synthetic_host_session_not_sent_to_sibling:true,parent_domain_host_session_forgery_rejected:true,httponly_synthetic_session_not_readable:true},strict_parent_cookie_isolation:false,production_ready:false,limitations:['Synthetic __Host marker only, not an implemented production authorization flow.','Ordinary parent-domain marker is still visible on management host.','No real platform credential mutation, second-account or platform logout test.']};
 await context.clearCookies({name:'m01_parent_marker'});await context.clearCookies({name:markerName});
 const dir=new URL('.local/m01/',root);await mkdir(dir,{recursive:true});await writeFile(new URL('platform-results.json',dir),JSON.stringify(report,null,2));console.log(JSON.stringify(report));await context.close();
}finally{await browser.close();}
