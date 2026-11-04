// Run with Playwright installed (NODE_PATH is supported); uses only local HTTP fixtures.
import assert from 'node:assert/strict';
import { createRequire } from 'node:module';
import { readFile, mkdir } from 'node:fs/promises';
import { existsSync } from 'node:fs';
import http from 'node:http';
const { chromium } = createRequire(import.meta.url)('playwright');
const root = new URL('../', import.meta.url);
const now = Math.floor(Date.now() / 1000);
const node = (id, name, extra = {}) => ({ device_id: id, name, os: 'linux', arch: 'x86_64', online: true, accepting_tasks: true, last_seen_unix: now, version: '0.4.0',
  docker_observe: { environment_id: 'local-fixture', catalog_page_limit: 50, log_line_limit: 500, endpoint_tunnel: true },
  resources: { cpu_usage_percent: 23.4, logical_cpus: 16, available_memory_bytes: 24 * 1024 ** 3, total_memory_bytes: 32 * 1024 ** 3, storage: { available_bytes: 180 * 1024 ** 3, total_bytes: 512 * 1024 ** 3 }, age_secs: 1, sampled_at_unix: now, uptime_secs: 24000 },
  planning_capacity: { cpu_threads: 12, memory_bytes: 24 * 1024 ** 3 }, resource_policy: { reserved_cpu_threads: 4, reserved_memory_mib: 8192 },
  workspace: { desired_running: true, healthy: true, project_service_healthy: true, state: 'running', project_path: '/projects', volume: 'workspace-projects' },
  docker: { desired_running: false, generation: 1, sampled_at_unix: now, containers: [] },
  pipeline: { desired_running: true, current: { id: 'release-current', build_id: 'build-one', healthy: true, image_id: 'sha256:current', input_digest: 'sha256:abcdef' }, previous: { id: 'release-previous', build_id: 'build-two', image_id: 'sha256:previous' }, jobs: [
    { id: 'build-one', request: { action: 'build' }, status: 'succeeded', input_digest: 'sha256:111111111111' },
    { id: 'build-two', request: { action: 'build' }, status: 'succeeded', input_digest: 'sha256:222222222222' },
    { id: 'build-failed', request: { action: 'build' }, status: 'failed', error: 'Dockerfile 构建失败，请查看日志。' }
  ] }, ...extra });
let nodes = [node('dev-a', '家中的开发主机'), node('dev-b', '随身电脑'), node('dev-c', '一台名称很长的 Windows 与 WSL 协作电脑用于测试换行', { online: false, workspace: null })];
let unavailable = false, loseOperation = false, logDelay = 0;
const requests = [], plans = [], endpoints = [];
const server = http.createServer(async (req, res) => {
  const pathname = new URL(req.url, 'http://localhost').pathname;
  const json = (value, status = 200) => { res.writeHead(status, { 'Content-Type': 'application/json' }); res.end(JSON.stringify(value)); };
  if (pathname === '/api/v1/auth-mode') return json({ auth: 'development' });
  if (pathname === '/api/v1/workspace-session') return json({});
  if (pathname === '/api/v1/devices') return json(nodes, unavailable ? 503 : 200);
  if (pathname === '/api/v1/workloads') {
    if (req.method === 'POST') { let body = ''; for await (const chunk of req) body += chunk; const plan = { ...JSON.parse(body), reason: 'matching node selected', device_id: 'dev-a', action: 'start', attempt: 1 }; plans.push(plan); return json(plan); }
    return json(plans);
  }
  if (pathname.startsWith('/api/v1/workloads/')) { plans.length = 0; return json({}); }
  if (pathname === '/api/v1/access-endpoints') {
    if (req.method === 'POST') { let body = ''; for await (const chunk of req) body += chunk; const value = JSON.parse(body); const endpoint = { ...value, owner: 'development', enabled: true, revision: 1, public_url: 'https://e-' + value.id + '.apps.example/', created_unix: now, updated_unix: now }; endpoints.push(endpoint); return json(endpoint, 201); }
    return json(endpoints);
  }
  if (pathname.startsWith('/api/v1/access-endpoints/')) {
    const id = pathname.split('/').pop(), index = endpoints.findIndex(value => value.id === id);
    if (index < 0) return json({}, 404);
    if (req.method === 'DELETE') { endpoints.splice(index, 1); res.writeHead(204); return res.end(); }
    let body = ''; for await (const chunk of req) body += chunk; const change = JSON.parse(body); endpoints[index] = { ...endpoints[index], ...change, revision: endpoints[index].revision + 1 }; return json(endpoints[index]);
  }
  if (pathname.endsWith('/docker/catalog')) return json({ result: 'catalog', page: { environment_id: 'local-fixture', snapshot_id: '00000000-0000-4000-8000-000000000001', sampled_at_unix: now, next_cursor: null, containers: [{ id: 'a'.repeat(64), name: 'photos', image: 'photos:latest', state: 'running', health: 'healthy', compose_project: 'home', compose_service: 'photos', compose_oneoff: false, ports: [{ container_port: 8080, host_ip: '127.0.0.1', host_port: 18080, supported: true }] }, { id: 'b'.repeat(64), name: 'background-worker', image: 'worker:1', state: 'exited', health: null, compose_project: null, compose_service: null, ports: [] }] } });
  if (pathname.includes('/docker/containers/') && pathname.endsWith('/logs')) { if (logDelay) await new Promise(resolve => setTimeout(resolve, logDelay)); return json({ result: 'logs', value: { container_id: 'a'.repeat(64), collected_at_unix: now, logs: 'existing application log', truncated: false } }); }
  if (pathname.endsWith('/docker')) {
    let body = ''; for await (const chunk of req) body += chunk;
    const action = JSON.parse(body); requests.push(action);
    if (loseOperation) { loseOperation = false; return json({ error: 'response unavailable after acceptance' }, 504); }
    return json({ workspace: nodes[0].workspace, pipeline: nodes[0].pipeline, snapshot: nodes[0].docker, logs: action.request?.action === 'logs' || action.action === 'test_logs' ? '测试日志\n'.repeat(100) : null });
  }
  const files = { '/': ['src/manager.html', 'text/html'], '/manager.js': ['src/manager.js', 'text/javascript'], '/manager.css': ['src/manager.css', 'text/css'], '/app-icon.png': ['assets/cofleet-icon.png', 'image/png'], '/favicon.ico': ['assets/favicon.ico', 'image/x-icon'] };
  if (!files[pathname]) { res.writeHead(404); return res.end(); }
  const [path, type] = files[pathname]; res.writeHead(200, { 'Content-Type': type }); res.end(await readFile(new URL(path, root)));
});
await new Promise(resolve => server.listen(0, '127.0.0.1', resolve));
const browser = await chromium.launch({ headless: true, executablePath: process.env.CHROMIUM_PATH || (existsSync('/usr/bin/chromium') ? '/usr/bin/chromium' : undefined), args: ['--no-sandbox'] });
try {
  const page = await browser.newPage({ viewport: { width: 1440, height: 1100 } });
  page.setDefaultTimeout(8000);
  const errors = []; page.on('pageerror', error => errors.push(error.message));
  await page.goto(`http://127.0.0.1:${server.address().port}`);
  await page.locator('#token').fill('fixture-token'); await page.locator('#login button').click();
  await page.locator('.device').first().waitFor();
  assert.equal(await page.locator('#scheduler-page').isVisible(), false);
  assert.equal(await page.locator('#details .workspace-link').getAttribute('href'), 'workspaces/dev-a/');
  const output = new URL('../.local/ui-preview/redesign/', import.meta.url); await mkdir(output, { recursive: true });
  for (const width of [1440, 1024]) { await page.setViewportSize({ width, height: 1100 }); await page.screenshot({ path: new URL(`desktop-${width}.png`, output).pathname, fullPage: true }); assert.ok(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)); }
  await page.locator('#tab-pipeline').click();
  await page.screenshot({ path: new URL('pipeline.png', output).pathname, fullPage: true });
  await page.locator('#pipeline-project').fill('my-project'); await page.locator('#pipeline-project').evaluate(input => { window.originalInput = input; input.setSelectionRange(3, 6); });
  // Real timer ticks, then an explicit changed snapshot.
  await page.waitForTimeout(10500);
  nodes[0].resources.cpu_usage_percent = 51;
  await page.evaluate(() => refresh());
  assert.ok(await page.locator('#pipeline-project').evaluate(input => input === window.originalInput && document.activeElement === input && input.selectionStart === 3 && input.selectionEnd === 6));
  assert.equal(await page.locator('#pipeline-project').inputValue(), 'my-project');
  await page.locator('#pipeline-build').selectOption('build-two'); await page.evaluate(() => refresh());
  assert.equal(await page.locator('#pipeline-build').inputValue(), 'build-two');
  await page.getByRole('button', { name: '发布所选产物', exact: true }).click();
  await page.waitForFunction(() => !pipelineViews.get('dev-a').busy);
  assert.equal(requests.at(-1).request.build_id, 'build-two');
  await page.locator('.device').nth(1).click(); await page.locator('#tab-pipeline').click();
  assert.equal(await page.locator('#pipeline-project').inputValue(), 'cofleet-example/release-demo');
  await page.locator('.device').first().click(); assert.equal(await page.locator('#pipeline-project').inputValue(), 'my-project');
  await page.getByRole('button', { name: '回滚上一版', exact: true }).click();
  assert.match(await page.locator('.confirmation').innerText(), /release-/);
  const before = requests.length; await page.getByRole('button', { name: '取消', exact: true }).click(); assert.equal(requests.length, before);
  await page.getByRole('button', { name: '读取日志 build-on', exact: true }).click();
  await page.locator('#details .container-logs').waitFor();
  await page.locator('#details .container-logs').evaluate(log => { window.originalLog = log; log.scrollTop = 120; });
  await page.evaluate(() => refresh());
  assert.ok(await page.locator('#details .container-logs').evaluate(log => log === window.originalLog && log.scrollTop === 120));
  await page.locator('#details .logs-disclosure summary').click(); await page.evaluate(() => refresh());
  assert.equal(await page.locator('#details .logs-disclosure').evaluate(detail => detail.open), false);
  await page.locator('#tab-overview').click(); await page.locator('#tab-pipeline').click();
  assert.equal(await page.locator('#details .logs-disclosure').evaluate(detail => detail.open), false);
  await page.locator('#details .logs-disclosure summary').click();
  assert.equal(await page.locator('#details .container-logs').evaluate(log => log.scrollTop), 120);
  await page.locator('#tab-workspace').click();
  await page.getByRole('button', { name: '停止开发空间', exact: true }).click();
  assert.match(await page.locator('.confirmation').innerText(), /保留/);
  await page.getByRole('button', { name: '取消', exact: true }).click();
  loseOperation = true;
  await page.getByRole('button', { name: '启动开发空间', exact: true }).click();
  await page.getByRole('button', { name: '重试启动开发空间', exact: true }).waitFor();
  const lostId = requests.at(-1).request.request_id;
  await page.waitForFunction(() => !workspaceViews.get('dev-a').busy);
  await page.getByRole('button', { name: '重试启动开发空间', exact: true }).click();
  await page.waitForFunction(() => !workspaceViews.get('dev-a').busy);
  assert.equal(requests.at(-1).request.request_id, lostId);
  await page.locator('#tab-docker').click(); await page.getByRole('button', { name: '启动测试容器', exact: true }).click();
  await page.waitForFunction(() => !dockerViews.get('dev-a').busy); assert.equal(requests.at(-1).action, 'set_test');
  await page.getByRole('button', { name: '读取测试日志', exact: true }).click(); await page.locator('#details .container-logs').waitFor();
  await page.locator('#tab-overview').click(); await page.locator('.technical-details summary').click(); await page.evaluate(() => refresh());
  assert.equal(await page.locator('.technical-details').evaluate(detail => detail.open), true);
  await page.locator('#online-only').check();
  unavailable = true; await page.evaluate(() => refresh());
  assert.match(await page.locator('#connection-label').innerText(), /待确认/);
  assert.equal(await page.locator('#details .workspace-link').count(), 0);
  assert.match(await page.locator('#details').innerText(), /快照/);
  assert.equal(await page.locator('.device').count(), 3);
  unavailable = false; nodes[0].resources.age_secs = 20; await page.evaluate(() => refresh()); assert.match(await page.locator('#details').innerText(), /采集暂未更新/);
  await page.locator('#tab-overview').focus(); await page.keyboard.press('ArrowRight'); assert.equal(await page.locator('#tab-workspace').getAttribute('aria-selected'), 'true');
  await page.locator('#nav-applications').click(); await page.locator('#endpoint-discover').click(); await page.locator('#endpoint-target option').nth(1).waitFor({ state: 'attached' }); await page.locator('#endpoint-target').selectOption({ index: 1 });
  await page.locator('#endpoint-logs').click(); await page.waitForFunction(() => document.querySelector('#observed-logs').textContent.includes('existing application log')); assert.match(await page.locator('#observed-logs').innerText(), /existing application log/);
  assert.match(await page.locator('#observe-containers').innerText(), /background-worker/);
  assert.match(await page.locator('#observe-containers').innerText(), /Compose 项目：home/);
  await page.locator('#observe-search').fill('background');
  assert.equal(await page.locator('#observe-containers .endpoint-row').count(), 1);
  assert.equal(await page.locator('#observe-containers button').isEnabled(), true);
  await page.evaluate(() => refresh());
  assert.equal(await page.locator('#observe-search').inputValue(), 'background');
  await page.locator('#observe-search').fill('no-match');
  assert.match(await page.locator('#observe-containers').innerText(), /没有符合/);
  await page.locator('#observe-search').fill('');
  await page.locator('#observe-state').selectOption('exited');
  assert.equal(await page.locator('#observe-containers .endpoint-row').count(), 1);
  await page.locator('#observe-state').selectOption('');
  await page.locator('#endpoint-device').selectOption('dev-b');
  assert.equal(await page.locator('#observed-logs').isVisible(), false);
  await page.locator('#endpoint-device').selectOption('dev-a');
  assert.equal(await page.locator('#observed-logs').isVisible(), true);
  logDelay = 350;
  await page.locator('#endpoint-logs').click();
  await page.evaluate(() => refresh());
  await page.waitForFunction(() => !endpointBusy);
  assert.match(await page.locator('#observed-logs').innerText(), /existing application log/);
  logDelay = 0;
  for (const width of [1440, 1024, 390]) { await page.setViewportSize({ width, height: 1000 }); await page.screenshot({ path: new URL('docker-observe-' + width + '.png', output).pathname, fullPage: true }); assert.ok(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)); }
  await page.setViewportSize({ width: 1440, height: 1100 });

  await page.locator('#endpoint-name').fill('家庭相册'); await page.locator('#endpoint-submit').click(); await page.locator('#endpoint-list .endpoint-row').waitFor(); assert.match(await page.locator('#endpoint-list .endpoint-row').innerText(), /家庭相册/);
  await page.getByRole('button', { name: '禁用', exact: true }).click(); await page.getByRole('button', { name: '启用', exact: true }).waitFor();
  await page.locator('#nav-scheduler').click(); await page.locator('#schedule-memory').fill('2048'); await page.evaluate(() => refresh()); assert.equal(await page.locator('#schedule-memory').inputValue(), '2048');
  await page.locator('#schedule-submit').click(); await page.locator('.workload-row').waitFor(); assert.match(await page.locator('#schedule-notice').innerText(), /已选择匹配节点/);
  await page.evaluate(() => refresh()); assert.match(await page.locator('#schedule-notice').innerText(), /已选择匹配节点/);
  await page.getByRole('button', { name: '释放', exact: true }).click(); await page.waitForFunction(() => document.querySelectorAll('.workload-row').length === 0);
  await page.locator('#nav-computers').click();
  await page.locator('#search').fill('nothing'); assert.match(await page.locator('#empty').innerText(), /没有符合/); await page.locator('#search').fill('');
  await page.setViewportSize({ width: 390, height: 844 }); await page.reload(); await page.locator('#token').fill('fixture-token'); await page.locator('#login button').click(); await page.locator('.device').first().waitFor();
  assert.equal(await page.locator('#details').isVisible(), false);
  await page.screenshot({ path: new URL('mobile-list.png', output).pathname, fullPage: true });
  await page.locator('.device').first().click(); assert.equal(await page.locator('.fleet').isVisible(), false);
  await page.screenshot({ path: new URL('mobile-detail.png', output).pathname, fullPage: true });
  assert.ok(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth));
  await page.locator('.mobile-back').click(); assert.equal(await page.locator('.fleet').isVisible(), true);
  nodes = [node('bare', '未配置的设备', { workspace: null, pipeline: null, docker: null })]; await page.evaluate(() => refresh()); await page.locator('.device').click(); await page.locator('#tab-workspace').click(); assert.match(await page.locator('#device-panel').innerText(), /未启用/);
  await page.locator('#logout').click(); assert.equal(await page.evaluate(() => observedCatalogs.size + observedLogs.size + observedTargets.size), 0); assert.equal(await page.locator('#search').inputValue(), '');
  nodes = []; await page.locator('#token').fill('fixture-token'); await page.locator('#login button').click(); await page.waitForFunction(() => fresh); assert.match(await page.locator('#empty').innerText(), /启动 Agent/);
  assert.deepEqual(errors, []);
  console.log('PASS navigation, live polling focus/caret, per-device drafts, build selection, log scroll/disclosure, confirmations, request-ID retry, stale/offline states, schedule feedback, mobile, empty states, logout');
} finally { await browser.close(); await new Promise(resolve => server.close(resolve)); }
