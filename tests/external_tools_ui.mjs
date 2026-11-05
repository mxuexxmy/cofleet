// Application-level entry contract; no upstream app or physical host is controlled.
import assert from 'node:assert/strict';
import {createRequire} from 'node:module';
import {readFile,mkdir} from 'node:fs/promises';
import http from 'node:http';
const {chromium}=createRequire(import.meta.url)('playwright');
let configured={webshell_url:'https://terminal.example/',remote_desktop_url:'https://desktop.example/'},unavailable=false;
const server=http.createServer(async(req,res)=>{
 const path=new URL(req.url,'http://localhost').pathname;
 const json=(v,status=200)=>{res.writeHead(status,{'Content-Type':'application/json','X-CoFleet-Scope':'fixture'});res.end(JSON.stringify(v));};
 if(path==='/api/v1/auth-mode')return json({auth:'development'});
 if(path==='/api/whoami')return json({user:'fixture',scope:'fixture',external_tools:configured});
 if(path==='/api/v1/devices')return json([],unavailable?503:200);
 if(path==='/api/v1/workloads'||path==='/api/v1/access-endpoints')return json([]);
 const files={'/':['src/manager.html','text/html'],'/manager.js':['src/manager.js','text/javascript'],'/manager.css':['src/manager.css','text/css'],'/app-icon.png':['assets/cofleet-icon-256.png','image/png'],'/favicon.ico':['assets/favicon.ico','image/x-icon']};
 if(!files[path]){res.writeHead(404);return res.end();}
 const [file,type]=files[path];res.writeHead(200,{'Content-Type':type});res.end(await readFile(new URL('../'+file,import.meta.url)));
});
await new Promise(r=>server.listen(0,'127.0.0.1',r));
const browser=await chromium.launch({headless:true,executablePath:'/usr/bin/chromium',args:['--no-sandbox']});
try{
 const context=await browser.newContext(),page=await context.newPage({viewport:{width:1440,height:1000}}),errors=[];page.on('pageerror',e=>errors.push(e.message));
 let requestHeaders;
 await context.route('https://terminal.example/',route=>{requestHeaders=route.request().headers();return route.fulfill({contentType:'text/html',body:'<h1>Upstream target selector</h1>'});});
 await page.goto(`http://127.0.0.1:${server.address().port}/`);await page.locator('#token').fill('fixture-token');await page.locator('#login button').click();await page.waitForFunction(()=>fresh);
 await page.locator('#nav-tools').click();assert.equal(await page.locator('#nav-tools').getAttribute('aria-current'),'page');assert.equal(await page.locator('#computers-page').isVisible(),false);
 for(const [name,url] of [['打开物理机终端','https://terminal.example/'],['打开远程桌面','https://desktop.example/']]){
  const link=page.getByRole('link',{name,exact:true});assert.equal(await link.getAttribute('href'),url);assert.equal(await link.getAttribute('target'),'_blank');assert.equal(await link.getAttribute('rel'),'noopener noreferrer');
 }
 const popupPromise=context.waitForEvent('page');await page.getByRole('link',{name:'打开物理机终端',exact:true}).click();const popup=await popupPromise;await popup.getByRole('heading').waitFor();assert.equal(await popup.evaluate(()=>window.opener),null);assert.equal(await popup.evaluate(()=>document.referrer),'');assert.ok(!requestHeaders.authorization);await popup.close();
 assert.match(await page.locator('#tools-page').innerText(),/不会直接连接.*当前选中/);
 await page.locator('.external-tool-help summary').first().click();
 await page.evaluate(()=>refresh());assert.equal(await page.locator('#tools-page').isVisible(),true);assert.equal(await page.locator('.external-tool-help').first().evaluate(d=>d.open),true);
 await page.locator('.external-tool-help summary').first().click();
 await mkdir('.local/ui-preview/p1',{recursive:true});await page.screenshot({path:'.local/ui-preview/p1/desktop.png',fullPage:true});
 configured={};await page.evaluate(()=>refresh());assert.equal(await page.getByRole('button',{name:'打开远程桌面',exact:true}).isDisabled(),true);assert.match(await page.locator('#tools-page').innerText(),/尚未配置应用地址/);
 configured={webshell_url:'javascript:alert(1)',remote_desktop_url:'https://user:secret@desktop.example/?token=secret'};await page.evaluate(()=>refresh());assert.equal(await page.locator('#external-tools a.workspace-link').count(),0);
 configured={webshell_url:'https://terminal.example/'};await page.evaluate(()=>refresh());unavailable=true;await page.evaluate(()=>refresh());assert.equal(await page.locator('#external-tools a.workspace-link').count(),0);assert.match(await page.locator('#tools-page').innerText(),/连接管理中心/);unavailable=false;await page.evaluate(()=>refresh());
 await page.setViewportSize({width:390,height:844});await page.locator('#nav-tools').scrollIntoViewIfNeeded();assert.ok(await page.evaluate(()=>document.documentElement.scrollWidth<=innerWidth));await page.screenshot({path:'.local/ui-preview/p1/mobile.png',fullPage:true});
 await page.locator('#logout').click();await page.locator('#nav-tools').click();assert.equal(await page.locator('#external-tools a.workspace-link').count(),0);assert.deepEqual(errors,[]);
 console.log('PASS application-level navigation, upstream selector, noopener/noreferrer, no forwarded credential, missing/unsafe config, stale/login clearing, polling stability, mobile layout');
}finally{await browser.close();await new Promise(r=>server.close(r));}
