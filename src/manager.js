'use strict';
const $ = selector => document.querySelector(selector);
let token = '', platformLogin = false, devices = [], workloads = [], endpoints = [], selectedId = null, fresh = false;
let generation = 0, controller = null, lastRefresh = null;
const dockerViews = new Map();
const workspaceViews = new Map();
const pipelineViews = new Map();
let dockerEpoch = 0;
let pendingSchedule = null, submittingSchedule = false;
const observedCatalogs = new Map();
const observedLogs = new Map();
const observedTargets = new Map();
let observeController = null, observeSession = 0;
let endpointBusy = false, accessMode = 'legacy';
const osNames = { linux: 'Linux', windows: 'Windows', macos: 'macOS' };

function el(tag, className, text) {
  const element = document.createElement(tag);
  if (className) element.className = className;
  if (text !== undefined) element.textContent = text;
  return element;
}
// Local interface state never travels with a server snapshot.
const deviceViews = new Map();
let mobileDetail = false;
function deviceView(id) {
  if (!deviceViews.has(id)) deviceViews.set(id, { tab: 'overview', confirmation: null });
  return deviceViews.get(id);
}
// Reconcile in place so polling does not replace focused fields, disclosures or log scrollers.
function reconcile(target, source) {
  for (const attr of [...target.attributes]) if (!source.hasAttribute(attr.name)) target.removeAttribute(attr.name);
  for (const attr of [...source.attributes]) if (target.getAttribute(attr.name) !== attr.value) target.setAttribute(attr.name, attr.value);
  for (const event of ['onclick', 'oninput', 'onchange', 'onkeydown', 'ontoggle', 'onscroll']) target[event] = source[event];
  if (target instanceof HTMLInputElement) { if (target.value !== source.value) target.value = source.value; target.checked = source.checked; }
  const selectValue = source instanceof HTMLSelectElement ? source.value : null;
  const old = [...target.childNodes], children = [...source.childNodes];
  children.forEach((next, index) => {
    const key = next.nodeType === 1 && (next.id || next.dataset.key);
    let node = key ? old.find(child => child.nodeType === 1 && (child.id || child.dataset.key) === key) : target.childNodes[index];
    if (!node || node.nodeType !== next.nodeType || node.nodeName !== next.nodeName || (node.nodeType === 1 && (node.id || node.dataset.key || '') !== (key || ''))) {
      target.insertBefore(next, target.childNodes[index] || null); node = next;
    } else {
      if (node !== target.childNodes[index]) target.insertBefore(node, target.childNodes[index] || null);
      if (node.nodeType === 1) reconcile(node, next);
      else if (node.textContent !== next.textContent) node.textContent = next.textContent;
    }
  });
  while (target.childNodes.length > children.length) target.lastChild.remove();
  if (target instanceof HTMLSelectElement) target.value = selectValue;
}
function commit(target, source) {
  const active = target.contains(document.activeElement) ? document.activeElement : null;
  reconcile(target, source);
  if (active?.isConnected && document.activeElement !== active) active.focus({ preventScroll: true });
}
function workspaceStatus(device) {
  if (!fresh) return '状态待确认';
  if (!device.online) return '设备离线';
  const workspace = device.workspace;
  return !workspace ? '未启用开发空间' : !workspace.desired_running ? '开发空间已停止' : workspace.healthy ? '开发空间就绪' : '开发空间启动中';
}
function unavailableReason(device) {
  return !fresh ? '连接待确认，恢复连接后可操作。' : !device.online ? '设备离线，请在电脑上启动 Agent 并等待重新连接。' : !device.accepting_tasks ? '这台电脑已暂停接收新任务，无法启动或构建；请在本机恢复接收任务。' : '';
}
function workspaceEntry(device) {
  const box = el('div', 'workspace-entry');
  const copy = el('div'); copy.append(el('h3', '', workspaceStatus(device)), el('p', '', '项目、依赖和编辑器设置保存在这台电脑上。'));
  box.append(copy);
  const ready = fresh && device.online && device.workspace?.healthy && device.workspace.desired_running;
  if (ready) {
    const link = el('a', 'workspace-link', '进入开发空间'); link.href = 'workspaces/' + device.device_id + '/'; link.target = '_blank'; link.rel = 'noopener noreferrer'; box.append(link);
  } else {
    const button = el('button', 'secondary', '管理开发空间'); button.onclick = () => { deviceView(device.device_id).tab = 'workspace'; render(); }; box.append(button);
  }
  return box;
}
function confirmAction(device, key, description, action) {
  const state = deviceView(device.device_id);
  state.confirmation = { key, description, action }; render();
  $('#details .confirmation button')?.focus();
}
function renderConfirmation(area, device) {
  const state = deviceView(device.device_id), pending = state.confirmation;
  if (!pending) return;
  const box = el('div', 'confirmation'); box.dataset.key = 'confirmation'; box.setAttribute('role', 'group'); box.setAttribute('aria-label', '确认操作');
  box.append(el('p', '', pending.description));
  const yes = el('button', 'danger', '确认' + pending.key), cancel = el('button', 'secondary', '取消');
  yes.disabled = !fresh || !device.online || (pending.key === '回滚上一版' && !device.accepting_tasks);
  yes.onclick = () => { state.confirmation = null; pending.action(); };
  cancel.onclick = () => { state.confirmation = null; render(); $('#tab-' + state.tab)?.focus({ preventScroll: true }); };
  box.append(yes, cancel);
  const anchor = area.querySelector('[data-key=deploy-actions], [data-key=actions]');
  if (anchor) anchor.after(box); else area.append(box);
}
function appendLogs(section, view, label) {
  if (view.logs === null) return;
  const disclosure = el('details', 'logs-disclosure'); disclosure.dataset.key = 'logs'; disclosure.open = view.logsOpen !== false;
  disclosure.ontoggle = event => { view.logsOpen = event.currentTarget.open; if (view.logsOpen) event.currentTarget.querySelector('pre').scrollTop = view.logsScroll || 0; };
  disclosure.append(el('summary', '', label));
  const logs = el('pre', 'container-logs', view.logs || '暂无日志。'); logs.onscroll = event => { if (event.currentTarget.closest('details').open) view.logsScroll = event.currentTarget.scrollTop; }; logs.tabIndex = 0; logs.setAttribute('aria-label', label); disclosure.append(logs); section.append(disclosure);
}
function showPage(page) {
  $('#computers-page').hidden = page !== 'computers'; $('#applications-page').hidden = page !== 'applications'; $('#scheduler-page').hidden = page !== 'scheduler';
  for (const name of ['computers', 'applications', 'scheduler']) { const button = $('#nav-' + name); if (page === name) button.setAttribute('aria-current', 'page'); else button.removeAttribute('aria-current'); }
}
function bytes(value) {
  if (!Number.isFinite(value) || value < 0) return '—';
  if (value === 0) return '0 MiB';
  const unit = value >= 1024 ** 3 ? 1024 ** 3 : 1024 ** 2;
  return (value / unit).toLocaleString('zh-CN', { maximumFractionDigits: 1 }) + (unit === 1024 ** 3 ? ' GiB' : ' MiB');
}
function relative(seconds) {
  if (!Number.isFinite(seconds)) return '—';
  seconds = Math.max(0, Math.floor(seconds));
  if (seconds < 5) return '刚刚';
  if (seconds < 60) return seconds + ' 秒前';
  if (seconds < 3600) return Math.floor(seconds / 60) + ' 分钟前';
  if (seconds < 86400) return Math.floor(seconds / 3600) + ' 小时前';
  return Math.floor(seconds / 86400) + ' 天前';
}
function seen(device) { return relative(Date.now() / 1000 - device.last_seen_unix); }
function duration(seconds) {
  if (!Number.isFinite(seconds)) return '—';
  if (seconds >= 86400) return Math.floor(seconds / 86400) + ' 天 ' + Math.floor(seconds % 86400 / 3600) + ' 小时';
  if (seconds >= 3600) return Math.floor(seconds / 3600) + ' 小时 ' + Math.floor(seconds % 3600 / 60) + ' 分钟';
  return Math.floor(seconds / 60) + ' 分钟';
}
function availability(device) {
  if (!fresh) return { text: '待确认', kind: 'warning' };
  return device.online ? { text: '在线', kind: 'online' } : { text: '离线', kind: 'offline' };
}
function setConnection(text, kind) {
  $('#connection-label').textContent = text;
  $('#connection-dot').className = 'dot ' + (kind || '');
}
function setNotice(text, error = false) {
  $('#notice').setAttribute('aria-live', error ? 'polite' : 'off');
  $('#notice').textContent = text;
  $('#notice').className = 'notice' + (error ? ' error' : '');
}
function metric(title, value, caption, percent, warnWhenLow = false) {
  const box = el('div', 'metric');
  box.append(el('div', 'metric-title', title), el('div', 'metric-value', value));
  if (Number.isFinite(percent)) {
    const bar = el('div', 'bar');
    const fill = el('div', 'bar-fill' + ((warnWhenLow ? percent <= 15 : percent >= 85) ? ' high' : ''));
    fill.style.width = Math.max(0, Math.min(100, percent)) + '%';
    bar.setAttribute('role', 'meter');
    bar.setAttribute('aria-label', title);
    bar.setAttribute('aria-valuemin', '0');
    bar.setAttribute('aria-valuemax', '100');
    bar.setAttribute('aria-valuenow', String(Math.round(percent)));
    bar.append(fill); box.append(bar);
  }
  box.append(el('div', 'metric-caption', caption));
  return box;
}
function pair(list, term, value) {
  const item = el('div'); item.append(el('dt', '', term), el('dd', '', value)); list.append(item);
}
function renderDetails(device) {
  const previous = $('#details'), previousLog = previous.querySelector('.container-logs');
  const viewMaps = { workspace: workspaceViews, pipeline: pipelineViews, docker: dockerViews };
  const previousView = viewMaps[previous.querySelector('.device-panel')?.dataset.key]?.get(previous.dataset.key);
  if (previousView && previousLog?.closest('details').open) previousView.logsScroll = previousLog.scrollTop;
  const area = el('section', 'details'); area.id = 'details';
  area.dataset.key = device?.device_id || 'empty'; area.setAttribute('aria-label', '设备详情');
  const finish = () => {
    commit($('#details'), area);
    const tab = device && deviceView(device.device_id).tab;
    const views = { workspace: workspaceViews, pipeline: pipelineViews, docker: dockerViews };
    const log = $('#details .container-logs');
    if (log) log.scrollTop = views[tab]?.get(device.device_id)?.logsScroll || 0;
  };
  if (!device) {
    const empty = el('div', 'detail-empty');
    empty.append(el('h2', '', '选择一台电脑'), el('p', '', '查看实时资源、连接状态与规划额度。'));
    area.append(empty); finish(); return;
  }
  const state = deviceView(device.device_id);
  const back = el('button', 'mobile-back quiet', '返回电脑列表'); back.onclick = () => { mobileDetail = false; render(); $('#search').focus(); }; area.append(back);
  const status = availability(device), resources = device.resources;
  const heading = el('div', 'detail-heading'), identity = el('div');
  identity.append(el('h2', '', device.name), el('p', '', (osNames[device.os] || device.os) + ' / ' + device.arch + (resources?.environment === 'wsl' ? ' · WSL 环境' : '')));
  const badge = el('span', 'badge ' + (status.kind === 'online' ? '' : status.kind), status.text);
  identity.append(badge); heading.append(identity); area.append(heading);
  area.append(workspaceEntry(device));
  const tabs = el('div', 'detail-tabs'); tabs.setAttribute('role', 'tablist'); tabs.setAttribute('aria-label', '设备工作区');
  const names = [['overview', '概览'], ['workspace', '开发空间'], ['pipeline', '构建发布'], ['docker', '容器']];
  for (const [key, label] of names) {
    const button = el('button', '', label); button.id = 'tab-' + key; button.type = 'button'; button.setAttribute('role', 'tab'); button.setAttribute('aria-selected', String(state.tab === key)); button.tabIndex = state.tab === key ? 0 : -1; button.setAttribute('aria-controls', 'device-panel');
    button.onclick = () => { state.tab = key; state.confirmation = null; render(); };
    button.onkeydown = event => { let index = names.findIndex(([name]) => name === key); if (event.key === 'ArrowRight') index++; else if (event.key === 'ArrowLeft') index--; else if (event.key === 'Home') index = 0; else if (event.key === 'End') index = names.length - 1; else return; event.preventDefault(); state.tab = names[(index + names.length) % names.length][0]; state.confirmation = null; render(); $('#tab-' + state.tab).focus(); };
    tabs.append(button);
  }
  area.append(tabs);
  const panel = el('div', 'device-panel'); panel.id = 'device-panel'; panel.dataset.key = state.tab; panel.setAttribute('role', 'tabpanel'); panel.setAttribute('aria-labelledby', 'tab-' + state.tab);
  area.append(panel);
  const reason = unavailableReason(device); if (reason && (state.tab !== 'overview' || (fresh && device.online))) panel.append(el('p', 'resource-warning', reason));
  if (state.tab !== 'overview') {
    ({ workspace: renderWorkspace, pipeline: renderPipeline, docker: renderDocker })[state.tab](panel, device);
    renderConfirmation(panel, device); finish(); return;
  }
  if (!fresh) panel.append(el('p', 'resource-warning', '暂时无法连接管理中心。以下为上次获取的快照，设备状态待确认。'));
  else if (!device.online) panel.append(el('p', 'resource-warning', '设备已离线，以下为最后一次上报的数据。'));
  else if (resources && resources.age_secs > 15) panel.append(el('p', 'resource-warning', '资源采集暂未更新，当前显示上次采集的数据。'));
  else if (!resources) panel.append(el('p', 'resource-warning', '尚未收到资源信息。请确认这台电脑已更新并运行新版 Agent。'));
  if (resources?.environment === 'wsl') panel.append(el('p', 'resource-warning', '这是 WSL 执行环境的资源，请勿与同一台电脑的 Windows 宿主重复累计。'));

  const grid = el('div', 'resource-grid');
  const cpu = resources?.cpu_usage_percent;
  grid.append(metric('CPU 使用率', Number.isFinite(cpu) ? cpu.toFixed(1) + '%' : resources ? '采集中' : '—', resources ? resources.logical_cpus + ' 个逻辑线程' : '等待资源上报', cpu));
  const memoryAvailable = resources ? 100 * resources.available_memory_bytes / resources.total_memory_bytes : null;
  grid.append(metric('当前可用内存', resources ? bytes(resources.available_memory_bytes) : '—', resources ? '总内存 ' + bytes(resources.total_memory_bytes) : '等待资源上报', memoryAvailable, true));
  panel.append(grid);
  const storage = el('div', 'storage-row'), disk = resources?.storage;
  storage.append(metric('Agent 所在磁盘', disk ? bytes(disk.available_bytes) + ' 可用' : '暂无数据', disk ? '文件系统总容量 ' + bytes(disk.total_bytes) + '，不合并其他磁盘' : '未能读取 Agent 数据目录所在文件系统', disk ? 100 * disk.available_bytes / disk.total_bytes : null, true));
  panel.append(storage);

  const policy = device.resource_policy || { reserved_cpu_threads: 0, reserved_memory_mib: 0 };
  const capacity = device.planning_capacity;
  const section = el('section', 'policy-section'), head = el('div', 'section-head');
  head.append(el('h3', '', '预留与规划'), el('span', 'section-note', device.accepting_tasks ? '本机允许接收新任务' : '本机已暂停接收新任务'));
  const list = el('dl');
  pair(list, '为本机预留', policy.reserved_cpu_threads + ' 线程 / ' + bytes(policy.reserved_memory_mib * 1024 ** 2));
  pair(list, '剩余规划额度', capacity ? capacity.cpu_threads + ' 线程 / ' + bytes(capacity.memory_bytes) : '等待资源上报');
  section.append(head, list, el('p', 'policy-tip', '预留资源由这台电脑的本地配置管理。额度仅作规划，尚未作为任务限制执行。'));
  if (capacity && (capacity.cpu_threads === 0 || capacity.memory_bytes === 0)) section.append(el('p', 'resource-warning', '预留已覆盖全部资源，当前没有剩余规划额度。'));
  panel.append(section);

  const info = el('dl', 'identity');
  pair(info, '最近上报', seen(device));
  pair(info, 'Agent 版本', device.version || '—');
  pair(info, '操作系统', resources?.os_version || (osNames[device.os] || device.os));
  pair(info, '系统运行时间', resources ? duration(resources.uptime_secs) : '—');
  pair(info, '设备 ID', device.device_id);
  const capabilities = device.capabilities || {};
  pair(info, '物理主机 ID', capabilities.physical_host_id || device.device_id);
  pair(info, 'GPU', capabilities.gpu ? capabilities.gpu.name + ' / ' + capabilities.gpu.memory_mib + ' MiB' : '未声明');
  pair(info, '数据位置', capabilities.data_locations?.join('、') || '未声明');
  pair(info, '在线偏好', capabilities.availability === 'always_on' ? '长期在线' : '按需在线');
  pair(info, '资源采集时间', resources ? new Date(resources.sampled_at_unix * 1000).toLocaleString('zh-CN') : '—');
  const disclosure = el('details', 'technical-details'); disclosure.open = !!state.technicalOpen;
  disclosure.ontoggle = event => { state.technicalOpen = event.currentTarget.open; };
  disclosure.append(el('summary', '', '设备与采集详情'), info); panel.append(disclosure);
  finish();
}

function renderDocker(area, device) {
  const section = el('section', 'docker-section');
  const head = el('div', 'section-head');
  head.append(el('h3', '', 'Docker 容器'), el('span', 'section-note', 'Linux 容器 · 固定测试负载'));
  section.dataset.key = 'docker';
  section.append(head); area.append(section);
  const docker = device.docker;
  if (!docker) {
    section.append(el('p', '', '未启用 Docker 管理。请在电脑上使用 --docker 启动新版 Agent。'));
    return;
  }
  let view = dockerViews.get(device.device_id);
  if (!view) { view = { busy: false, pending: null, logs: null, message: '' }; dockerViews.set(device.device_id, view); }
  if (view.busy) section.append(el('p', 'policy-tip', '正在处理容器请求，请等待结果。'));
  section.append(el('p', 'policy-tip', '测试容器：Alpine 3.22，32 MiB 内存、0.25 CPU，无网络和宿主目录挂载。其他容器仅展示。'));
  section.append(el('p', '', '期望状态：' + (docker.desired_running ? '运行' : '停止') + ' · 第 ' + docker.generation + ' 次变更 · 采集于 ' + (docker.sampled_at_unix ? new Date(docker.sampled_at_unix * 1000).toLocaleTimeString('zh-CN') : '等待采集')));
  if (docker.error) section.append(el('p', 'resource-warning', docker.error));
  const actions = el('div', 'docker-actions'); actions.dataset.key = 'actions';
  for (const [label, running] of [['启动测试容器', true], ['停止测试容器', false]]) {
    const retry = view.pending?.running === running;
    const button = el('button', running ? '' : 'secondary', retry ? '重试' + label : label);
    button.type = 'button';
    button.disabled = view.busy || !fresh || !device.online || (running && !device.accepting_tasks && !retry) || (!!view.pending && !retry);
    button.onclick = () => {
      if (!retry) view.pending = { action: 'set_test', request_id: crypto.randomUUID(), running };
      dockerCall(device.device_id, view.pending);
    };
    actions.append(button);
  }
  for (const [label, action] of [['刷新容器', 'list'], ['读取测试日志', 'test_logs']]) {
    const button = el('button', 'quiet', label); button.type = 'button';
    button.disabled = view.busy || !fresh || !device.online;
    button.onclick = () => dockerCall(device.device_id, { action }); actions.append(button);
  }
  section.append(actions);
  if (view.message) { const note = el('p', 'policy-tip', view.message); note.setAttribute('role', 'status'); section.append(note); }
  if (view.pending) section.append(el('p', 'policy-tip', '请求 ID：' + view.pending.request_id + '。若超时，请使用重试按钮查询并恢复同一次操作。'));
  const table = el('div', 'container-list');
  for (const container of docker.containers) {
    const row = el('div', 'container-row');
    row.append(el('strong', '', container.name), el('span', '', container.state + (container.managed ? ' · 受管测试容器' : ' · 只读')), el('code', '', container.image));
    table.append(row);
  }
  if (!docker.containers.length) table.append(el('p', '', '当前未发现容器。'));
  section.append(table);
  if (docker.truncated) section.append(el('p', 'policy-tip', '仅展示前 32 个容器。'));
  appendLogs(section, view, '测试容器日志');
}
async function dockerCall(deviceId, request) {
  const view = dockerViews.get(deviceId), epoch = dockerEpoch;
  if (!view || view.busy || !fresh || !devices.find(device => device.device_id === deviceId)?.online) return;
  view.busy = true; view.message = '正在请求电脑…'; render();
  const abort = new AbortController(), timeout = setTimeout(() => abort.abort(), 15000);
  try {
    const response = await fetch('api/v1/devices/' + deviceId + '/docker', {
      method: 'POST', headers: { 'Content-Type': 'application/json', 'X-CoFleet-Request': '1', ...(platformLogin ? {} : { Authorization: 'Bearer ' + token }) },
      body: JSON.stringify(request), signal: abort.signal, redirect: 'error'
    });
    if (!response.ok) throw new Error(response.status === 504 ? '操作结果尚未确认，请用相同请求 ID 重试。' : '请求失败（' + response.status + '），请检查登录、设备连接和 Docker 状态。');
    const reply = await response.json();
    if (epoch !== dockerEpoch) return;
    const device = devices.find(d => d.device_id === deviceId);
    if (device && reply.snapshot) device.docker = reply.snapshot;
    if (reply.logs !== null && reply.logs !== undefined) { view.logs = reply.logs; view.logsOpen = true; }
    if (reply.error) { view.message = reply.error + '；可用相同请求 ID 重试。'; }
    else {
      if (request.action === 'set_test') view.pending = null;
      view.message = reply.duplicate ? '该请求已处理，已核对当前期望状态。' : request.action === 'set_test' ? '期望状态已保存，实际运行状态将自动更新。' : '已读取最新状态。';
    }
  } catch (error) {
    if (epoch === dockerEpoch) view.message = error.name === 'AbortError' ? '等待超时，操作可能已经执行。请用相同请求 ID 重试。' : error.message;
  } finally {
    clearTimeout(timeout);
    if (epoch === dockerEpoch) { view.busy = false; render(); }
  }
}
function renderWorkspace(area, device) {
  const section = el('section', 'workspace-section'), head = el('div', 'section-head');
  head.append(el('h3', '', '持久开发空间'), el('span', 'section-note', '浏览器编辑器 · 项目与依赖持久保存'));
  section.dataset.key = 'workspace';
  section.append(head); area.append(section);
  const workspace = device.workspace;
  if (!workspace) {
    section.append(el('p', '', '本机尚未启用开发空间。请在节点批准开发镜像后启动 Agent。')); return;
  }
  let view = workspaceViews.get(device.device_id);
  if (!view) { view = { busy: false, pending: null, message: '', logs: null }; workspaceViews.set(device.device_id, view); }
  if (view.busy) section.append(el('p', 'policy-tip', '正在处理开发空间请求，请等待结果。'));
  const status = !workspace.desired_running ? '已停止' : workspace.healthy ? '编辑器就绪' : workspace.state === 'running' ? '编辑器正在启动' : '等待恢复';
  section.append(el('p', '', status + ' · 项目服务 ' + (workspace.project_service_healthy ? '健康' : '未就绪')));
  section.append(el('p', 'policy-tip', 'code-server · Node.js / Python / Git · 2 CPU / 2 GiB。关闭页面不停止开发空间；停止容器保留项目、依赖和编辑器设置。'));
  const actions = el('div', 'docker-actions'); actions.dataset.key = 'actions';
  const ready = fresh && device.online && workspace.healthy && workspace.desired_running;
  if (!ready) section.append(el('p', 'policy-tip', !workspace.desired_running ? '启动开发空间，编辑器就绪后即可进入。' : '编辑器尚未就绪，请检查设备连接、当前状态和日志。'));
  for (const [label, running] of [['启动开发空间', true], ['停止开发空间', false]]) {
    const retry = view.pending?.running === running;
    const button = el('button', running && !workspace.desired_running ? '' : 'secondary', retry ? '重试' + label : label); button.type = 'button';
    button.disabled = view.busy || !fresh || !device.online || (running && !device.accepting_tasks && !retry) || (!!view.pending && !retry);
    button.onclick = () => {
      const send = () => { if (!retry) view.pending = { action: 'set_running', request_id: crypto.randomUUID(), running }; workspaceCall(device.device_id, view.pending); };
      if (!running && !retry) confirmAction(device, '停止开发空间', '停止「' + device.name + '」的编辑器？项目、依赖和编辑器设置会保留。', send); else send();
    };
    actions.append(button);
  }
  const logs = el('button', 'quiet', '开发空间日志'); logs.type = 'button'; logs.disabled = view.busy || !fresh || !device.online;
  logs.onclick = () => workspaceCall(device.device_id, { action: 'logs' }); actions.append(logs); section.append(actions);
  const info = el('dl'); pair(info, '项目目录', workspace.project_path); pair(info, '持久卷', workspace.volume); section.append(info);
  section.append(el('p', 'policy-tip', '项目的 .cofleet/start.sh 会在容器启动时执行。默认预览端口为 3000，健康检查路径为 /health。'));
  if (ready) { const preview = el('a', '', '打开项目预览'); preview.href = 'workspaces/' + device.device_id + '/proxy/3000/'; preview.target = '_blank'; preview.rel = 'noopener noreferrer'; section.append(preview); }
  if (workspace.error) section.append(el('p', 'resource-warning', workspace.error));
  if (view.message) { const message = el('p', 'policy-tip', view.message); message.setAttribute('role', 'status'); section.append(message); }
  appendLogs(section, view, '开发空间日志');
}
async function workspaceCall(deviceId, request) {
  const view = workspaceViews.get(deviceId), epoch = dockerEpoch;
  if (!view || view.busy || !fresh || !devices.find(device => device.device_id === deviceId)?.online) return;
  view.busy = true; view.message = '正在保存并核对开发空间…'; render();
  const abort = new AbortController(), timeout = setTimeout(() => abort.abort(), 15000);
  try {
    const response = await fetch('api/v1/devices/' + deviceId + '/docker', {
      method: 'POST', headers: { 'Content-Type': 'application/json', 'X-CoFleet-Request': '1', ...(platformLogin ? {} : { Authorization: 'Bearer ' + token }) },
      body: JSON.stringify({ action: 'workspace', request }), signal: abort.signal, redirect: 'error'
    });
    if (!response.ok) throw new Error('开发空间请求未确认（' + response.status + '），请用重试按钮继续核对。');
    const reply = await response.json();
    if (epoch !== dockerEpoch) return;
    const device = devices.find(d => d.device_id === deviceId);
    if (device && reply.workspace) device.workspace = reply.workspace;
    if (reply.logs !== null && reply.logs !== undefined) { view.logs = reply.logs; view.logsOpen = true; }
    if (reply.error) view.message = reply.error;
    else { if (request.action === 'set_running') view.pending = null; view.message = reply.duplicate ? '已核对同一次请求的当前状态。' : '已保存，健康状态会自动更新。'; }
  } catch (error) {
    if (epoch === dockerEpoch) view.message = error.name === 'AbortError' ? '等待超时，后台可能已执行。重试会沿用同一次操作。' : error.message;
  } finally { clearTimeout(timeout); if (epoch === dockerEpoch) { view.busy = false; render(); } }
}
function renderPipeline(area, device) {
  const section = el('section', 'pipeline-section'), head = el('div', 'section-head');
  head.append(el('h3', '', '构建与发布'), el('span', 'section-note', '输入快照 · 不可变镜像 · 健康检查'));
  section.dataset.key = 'pipeline';
  section.append(head); area.append(section);
  const pipeline = device.pipeline;
  if (!pipeline) { section.append(el('p', '', '本机尚未启用构建发布。请使用 --build-deploy 启动已批准开发镜像的 Agent。')); return; }
  let view = pipelineViews.get(device.device_id);
  if (!view) { view = { project: 'cofleet-example/release-demo', build: '', busy: false, pending: null, logs: null, message: '' }; pipelineViews.set(device.device_id, view); }
  if (view.busy) section.append(el('p', 'policy-tip', '正在处理构建发布请求，请等待结果。'));
  if (view.pending) section.append(el('p', 'resource-warning', '上次操作结果待确认，请重试原请求后再进行其他变更。'));
  const current = pipeline.current, previous = pipeline.previous;
  section.append(el('p', '', current ? '当前版本 ' + current.id.slice(0, 8) + ' · ' + (!pipeline.desired_running ? '已停止' : current.healthy ? '健康' : '待恢复') : '尚未发布应用'));
  section.append(el('p', 'policy-tip', '构建前自动冻结项目快照，使用项目目录中的 Dockerfile 和 .cofleet-release.json。首版只发布无状态 HTTP 应用：8080 端口、/health 检查，不挂载持久数据。'));
  const label = el('label', 'search-label', '开发空间内的项目相对目录'); label.htmlFor = 'pipeline-project';
  const input = el('input'); input.id = 'pipeline-project'; input.value = view.project; input.dataset.device = device.device_id; input.maxLength = 128;
  input.oninput = event => { view.project = event.currentTarget.value; }; section.append(label, input);
  const actions = el('div', 'docker-actions'); actions.dataset.key = 'actions';
  const make = (title, request, allow = true) => {
    const button = el('button', 'secondary', title); button.type = 'button';
    button.disabled = view.busy || !!view.pending || !fresh || !device.online || !allow;
    button.onclick = () => pipelineCall(device.device_id, { ...request, request_id: crypto.randomUUID() }); actions.append(button);
  };
  make('构建项目', { action: 'build', project_dir: view.project }, device.accepting_tasks);
  // Read the current input value at click time, rather than a stale render closure.
  actions.lastChild.onclick = () => pipelineCall(device.device_id, { action: 'build', project_dir: view.project, request_id: crypto.randomUUID() });
  const builds = pipeline.jobs.filter(j => j.request.action === 'build' && j.status === 'succeeded');
  for (const release of [current, previous]) if (release && !builds.some(b => b.id === release.build_id)) builds.push({ id: release.build_id, input_digest: release.input_digest });
  if (!builds.some(j => j.id === view.build)) view.build = builds[0]?.id || '';
  section.append(actions, el('h3', 'subheading', '选择构建产物并发布'));
  const select = el('select'); select.id = 'pipeline-build'; select.setAttribute('aria-label', '选择构建产物'); select.disabled = view.busy;
  for (const job of builds) { const option = el('option', '', job.id.slice(0, 8) + ' · ' + job.input_digest?.slice(7, 19)); option.value = job.id; option.selected = job.id === view.build; select.append(option); }
  if (!builds.length) section.append(el('p', 'policy-tip', '没有可发布产物，请先完成一次构建。'));
  select.onchange = event => { view.build = event.currentTarget.value; }; section.append(select);
  const deployActions = el('div', 'docker-actions'); deployActions.dataset.key = 'deploy-actions';
  const operation = (title, action, enabled, extra = {}) => {
    const button = el('button', action === 'deploy' ? '' : 'secondary', title); button.type = 'button'; button.disabled = view.busy || !!view.pending || !fresh || !device.online || !enabled;
    button.onclick = () => {
      const send = () => pipelineCall(device.device_id, { action, request_id: crypto.randomUUID(), expected_current: current?.id || null, ...extra, ...(action === 'deploy' ? { build_id: view.build } : {}) });
      if (action === 'stop' || action === 'rollback') confirmAction(device, title, action === 'stop' ? '停止「' + device.name + '」上的应用版本 ' + current.id.slice(0, 8) + '？' : '将「' + device.name + '」的应用从 ' + current.id.slice(0, 8) + ' 回滚至 ' + previous.id.slice(0, 8) + '？', send); else send();
    };
    deployActions.append(button);
  };
  operation('发布所选产物', 'deploy', !!view.build && device.accepting_tasks);
  operation('回滚上一版', 'rollback', !!previous && !!current && device.accepting_tasks);
  operation('停止应用', 'stop', !!current);
  if (current && current.healthy && pipeline.desired_running && fresh && device.online) { const link = el('a', 'workspace-link', '访问应用'); link.href = 'applications/' + device.device_id + '/'; link.target = '_blank'; link.rel = 'noopener noreferrer'; deployActions.append(link); }
  section.append(deployActions);
  if (current) section.append(el('p', 'policy-tip', '镜像：' + current.image_id + '；输入：' + current.input_digest));
  if (!previous) section.append(el('p', 'policy-tip', '尚无上一版本，发布新版本后可回滚。'));
  if (previous) section.append(el('p', 'policy-tip', '上一版：' + previous.id.slice(0, 8) + ' · ' + previous.image_id));
  if (pipeline.error) section.append(el('p', 'resource-warning', pipeline.error));
  if (view.pending) { const retry = el('button', 'secondary', '重试未确认请求'); retry.disabled = view.busy || !fresh || !device.online; retry.onclick = () => pipelineCall(device.device_id, view.pending); section.append(retry); }
  if (view.message) { const message = el('p', 'policy-tip', view.message); message.setAttribute('role', 'status'); section.append(message); }
  const statuses = { capturing: '冻结输入中', queued: '排队中', running: '执行中', succeeded: '成功', failed: '失败', interrupted: '已中断，需检查当前状态' };
  const history = el('section', 'build-history'); history.dataset.key = 'history'; history.tabIndex = 0; history.setAttribute('aria-label', '构建与操作记录'); history.append(el('h3', 'subheading', '构建与操作记录'));
  section.insertBefore(history, section.querySelector('.subheading'));
  if (!pipeline.jobs.length) history.append(el('p', '', '尚无记录。填写项目目录后开始构建。'));
  for (const job of pipeline.jobs) {
    const row = el('div', 'container-row'); row.dataset.key = job.id; row.append(el('strong', '', ({ build: '构建', deploy: '发布', rollback: '回滚', stop: '停止' }[job.request.action] || job.request.action) + ' · ' + job.id.slice(0, 8)), el('span', '', statuses[job.status] || job.status));
    if (job.error) row.append(el('p', 'resource-warning', job.error));
    const log = el('button', 'quiet', '读取日志 ' + job.id.slice(0, 8)); log.disabled = view.busy || !fresh || !device.online; log.onclick = () => pipelineCall(device.device_id, { action: 'logs', job_id: job.id }); row.append(log); history.append(row);
  }
  appendLogs(section, view, '构建与应用日志');
}
async function pipelineCall(deviceId, request) {
  const view = pipelineViews.get(deviceId), epoch = dockerEpoch;
  if (!view || view.busy || !fresh || !devices.find(device => device.device_id === deviceId)?.online) return;
  view.busy = true; if (request.request_id) view.pending = request; view.message = '正在提交请求…'; render();
  const abort = new AbortController(), timer = setTimeout(() => abort.abort(), 15000);
  try {
    const response = await fetch('api/v1/devices/' + deviceId + '/docker', {
      method: 'POST', headers: { 'Content-Type': 'application/json', 'X-CoFleet-Request': '1', ...(platformLogin ? {} : { Authorization: 'Bearer ' + token }) },
      body: JSON.stringify({ action: 'pipeline', request }), signal: abort.signal, redirect: 'error'
    });
    if (!response.ok) throw new Error('请求未确认（' + response.status + '），可用相同请求重试。');
    const reply = await response.json(); if (epoch !== dockerEpoch) return;
    const device = devices.find(d => d.device_id === deviceId); if (device && reply.pipeline) device.pipeline = reply.pipeline;
    if (reply.logs !== null && reply.logs !== undefined) { view.logs = reply.logs; view.logsOpen = true; }
    if (reply.error) view.message = reply.error;
    else { view.pending = null; view.message = reply.duplicate ? '已定位原请求，未重复执行。' : '请求已接受。任务状态和日志将保留在节点。'; }
  } catch (error) { if (epoch === dockerEpoch) view.message = error.name === 'AbortError' ? '提交超时，结果未知；请重试原请求。' : error.message; }
  finally { clearTimeout(timer); if (epoch === dockerEpoch) { view.busy = false; render(); } }
}
function render() {
  const query = $('#search').value.trim().toLocaleLowerCase();
  const visible = devices.filter(d => (!$('#online-only').checked || !fresh || d.online) && [d.name, d.os, d.arch, d.device_id].some(v => String(v).toLocaleLowerCase().includes(query)));
  if (!visible.some(d => d.device_id === selectedId)) selectedId = visible[0]?.device_id || null;
  const list = el('div', 'device-list'); list.id = 'devices';
  for (const device of visible) {
    const item = el('button', 'device'); item.type = 'button';
    item.dataset.key = device.device_id;
    item.setAttribute('aria-pressed', String(device.device_id === selectedId));
    const top = el('span', 'device-top'), status = availability(device), state = el('span', 'device-status');
    state.append(el('span', 'dot ' + (status.kind === 'offline' ? '' : status.kind)), document.createTextNode(status.text));
    top.append(el('span', 'device-name', device.name), state);
    item.append(top, el('span', 'device-meta', (osNames[device.os] || device.os) + ' / ' + device.arch + ' · ' + seen(device)));
    item.append(el('span', 'device-workspace', workspaceStatus(device)));
    item.onclick = () => { selectedId = device.device_id; mobileDetail = true; render(); if (matchMedia('(max-width: 650px)').matches) $('#details').querySelector('.mobile-back')?.focus(); };
    list.append(item);
  }
  commit($('#devices'), list);
  $('#computers-page').classList.toggle('show-detail', mobileDetail && visible.length > 0);
  $('#empty').hidden = visible.length > 0;
  $('#empty').textContent = devices.length ? '没有符合当前筛选条件的设备。' : '设备上线后，会显示在这里。请在电脑上启动 Agent。';
  const online = devices.filter(d => d.online).length;
  $('#summary').textContent = devices.length ? devices.length + ' 台设备，' + (fresh ? online + ' 台在线' : '连接状态待确认') : '等待电脑接入。';
  renderWorkloads();
  renderApplications();
  renderDetails(visible.find(d => d.device_id === selectedId));
}

function endpointNotice(message, error = false) { const note = $('#endpoint-notice'); note.textContent = message; note.className = 'notice' + (error ? ' error' : ''); }
function renderApplications() {
  const oldDevice = $('#endpoint-device').value;
  const deviceSelect = el('select'); deviceSelect.id = 'endpoint-device'; deviceSelect.required = true;
  for (const device of devices) { const option = el('option', '', device.name + (device.docker_observe ? '' : '（未启用观察）')); option.value = device.device_id; option.disabled = !device.docker_observe; deviceSelect.append(option); }
  if (oldDevice && devices.some(device => device.device_id === oldDevice && device.docker_observe)) deviceSelect.value = oldDevice;
  deviceSelect.onchange = () => { renderApplications(); };
  commit($('#endpoint-device'), deviceSelect);
  const deviceId = $('#endpoint-device').value, catalog = observedCatalogs.get(deviceId);
  const targetSelect = el('select'); targetSelect.id = 'endpoint-target'; targetSelect.required = true;
  const placeholder = el('option', '', catalog ? '选择已发布的 TCP 端口' : '先读取容器'); placeholder.value = ''; targetSelect.append(placeholder);
  for (const container of catalog?.containers || []) for (const port of container.ports.filter(value => value.supported)) {
    const option = el('option', '', (container.compose_project ? container.compose_project + ' / ' + container.compose_service : container.name) + ' · ' + port.container_port + '→' + port.host_port);
    option.value = container.id + ':' + port.container_port; targetSelect.append(option);
  }
  const previousTarget = observedTargets.get(deviceId) || ''; if ([...targetSelect.options].some(option => option.value === previousTarget)) targetSelect.value = previousTarget;
  targetSelect.onchange = () => { observedTargets.set(deviceId, $('#endpoint-target').value); const selected = selectedObserved(); if (selected && !selected.container.compose_project) $('#endpoint-binding').value = 'container'; renderApplications(); };
  commit($('#endpoint-target'), targetSelect);
  const available = fresh && devices.some(device => device.device_id === deviceId && device.online && device.docker_observe);
  $('#endpoint-discover').disabled = endpointBusy || !available;
  $('#endpoint-logs').disabled = endpointBusy || !available || !selectedObserved();
  $('#endpoint-trust-label').hidden = accessMode !== 'trusted_prefix';
  $('#access-policy').textContent = accessMode === 'trusted_prefix' ? '自动前缀入口仅用于你主动添加的受信任应用。应用使用独立来源，与微服共享父域，不能隔离普通父域 Cookie。' : accessMode === 'disabled' ? '应用访问入口尚未配置；可以继续查看容器与日志。' : '';
  $('#endpoint-submit').disabled = endpointBusy || !available || !catalog || accessMode === 'disabled' || (accessMode === 'trusted_prefix' && !$('#endpoint-trust').checked);
  const area = el('div', 'endpoint-list'); area.id = 'endpoint-list';
  if (!endpoints.length) area.append(el('p', '', fresh ? '尚未添加应用入口。' : '连接后读取入口。'));
  for (const endpoint of endpoints) {
    const row = el('article', 'endpoint-row'); row.dataset.key = endpoint.id;
    const copy = el('div'); const device = devices.find(value => value.device_id === endpoint.device_id);
    const verification = { verified: '已核验', pending: '等待节点核验', error: '核验失败', disabled: '已禁用' }[endpoint.verification] || '状态待确认';
    copy.append(el('strong', '', endpoint.name + (endpoint.enabled ? '' : '（已禁用）')), el('small', '', (device?.name || endpoint.device_id) + ' · ' + endpoint.protocol.toUpperCase() + ' · ' + verification + ' · revision ' + endpoint.revision), el('code', '', endpoint.public_url));
    const actions = el('div', 'docker-actions');
    if (endpoint.enabled) { const open = el('a', 'workspace-link', '打开'); open.href = endpoint.public_url; open.target = '_blank'; open.rel = 'noopener noreferrer'; actions.append(open); }
    const toggle = el('button', 'secondary', endpoint.enabled ? '禁用' : '启用'); toggle.disabled = endpointBusy; toggle.onclick = () => updateEndpoint(endpoint, { enabled: !endpoint.enabled }); actions.append(toggle);
    const remove = el('button', 'danger', '删除入口'); remove.disabled = endpointBusy; remove.onclick = () => removeEndpoint(endpoint); actions.append(remove);
    if (endpoint.management_url) { const management = el('a', '', '原管理工具'); management.href = endpoint.management_url; management.target = '_blank'; management.rel = 'noopener noreferrer'; actions.append(management); }
    row.append(copy, actions); if (endpoint.detail) row.append(el('p', 'resource-warning', endpoint.detail)); area.append(row);
  }
  commit($('#endpoint-list'), area);
  renderObserved(deviceId, catalog, available);
}
function selectedObserved() { const [id, port] = $('#endpoint-target').value.split(':'); const container = observedCatalogs.get($('#endpoint-device').value)?.containers.find(value => value.id === id); return container && port ? { container, port: Number(port) } : null; }
function renderObserved(deviceId, catalog, available) {
  const area = el('div'); area.id = 'observe-containers';
  const query = $('#observe-search').value.trim().toLowerCase(), state = $('#observe-state').value;
  const containers = (catalog?.containers || []).filter(c => (!state || c.state === state) && [c.name, c.image, c.compose_project, c.compose_service, c.id].join(' ').toLowerCase().includes(query));
  $('#observe-summary').textContent = !available ? '设备离线、连接待确认或未启用观察。请在本机使用 --docker-observe 启动 Agent。' : !catalog ? '读取容器后，可查看所有容器，包括没有发布端口的应用。' : '上次读取 ' + new Date(catalog.sampled_at_unix * 1000).toLocaleTimeString() + ' · ' + catalog.containers.length + ' 个容器（手动刷新）';
  if (catalog && !containers.length) area.append(el('p', '', catalog.containers.length ? '没有符合筛选条件的容器。' : '当前 Docker 环境没有容器。'));
  let group = null;
  for (const c of containers) {
    const title = c.compose_project ? 'Compose 项目：' + c.compose_project : '独立容器 / 缺少 Compose 标签';
    if (group !== title) { area.append(el('h3', '', title)); group = title; }
    const row = el('article', 'endpoint-row'); row.dataset.key = c.id;
    const copy = el('div'); copy.append(el('strong', '', c.name), el('small', '', c.state + (c.health ? ' / ' + c.health : '') + (c.compose_service ? ' · 服务 ' + c.compose_service : '')), el('code', '', c.image));
    copy.append(el('p', '', c.ports.length ? c.ports.map(p => p.host_ip + ':' + p.host_port + ' → ' + p.container_port + (p.supported ? '' : '（不支持转发：指定网卡）')).join('；') : '没有已发布的 TCP 端口；仍可查看日志。'));
    const button = el('button', 'secondary', '查看日志'); button.disabled = endpointBusy || !available; button.onclick = () => readObservedLogs(deviceId, c.id, c.name); row.append(copy, button); area.append(row);
  }
  commit($('#observe-containers'), area);
  const saved = observedLogs.get(deviceId), details = $('#observed-logs');
  details.hidden = !saved;
  if (saved) { details.querySelector('summary').textContent = saved.name + ' · ' + new Date(saved.collected_at_unix * 1000).toLocaleTimeString() + (saved.truncated ? '（已截断，仅显示末尾）' : ''); const pre = details.querySelector('pre'); if (pre.textContent !== saved.logs) { pre.textContent = saved.logs; pre.scrollTop = saved.scroll || 0; } pre.onscroll = () => { saved.scroll = pre.scrollTop; }; }
  else details.querySelector('pre').textContent = '';
}
async function observeFetch(path, signal) {
  const response = await fetch(path, { headers: platformLogin ? {} : { Authorization: 'Bearer ' + token }, signal, cache: 'no-store', redirect: 'error' });
  if (!response.ok) throw new Error('读取失败（' + response.status + '）。请检查设备连接和观察权限后重试。');
  const result = await response.json(); if (result.result === 'error') throw new Error(result.message); return result;
}
async function discoverContainers() {
  const deviceId = $('#endpoint-device').value; if (!deviceId || endpointBusy) return;
  const epoch = observeSession; endpointBusy = true; observeController = new AbortController(); const signal = AbortSignal.any([observeController.signal, AbortSignal.timeout(60000)]);
  endpointNotice('正在读取容器与已发布端口…'); renderApplications();
  try { let cursor = null, containers = [], environment = null, sampled = null, snapshot = null;
    do { const url = new URL('api/v1/devices/' + deviceId + '/docker/catalog', location.href); url.searchParams.set('limit', '50'); if (cursor) url.searchParams.set('cursor', cursor);
      const result = await observeFetch(url, signal); if (epoch !== observeSession) return;
      if (result.result !== 'catalog' || (snapshot && snapshot !== result.page.snapshot_id)) throw new Error('容器快照变化，请重新读取。');
      environment = result.page.environment_id; sampled = result.page.sampled_at_unix; snapshot = result.page.snapshot_id; containers.push(...result.page.containers); cursor = result.page.next_cursor;
    } while (cursor && containers.length < 500);
    observedCatalogs.set(deviceId, { environment_id: environment, sampled_at_unix: sampled, containers }); if ($('#endpoint-device').value === deviceId) endpointNotice('已读取 ' + containers.length + ' 个容器。');
  } catch (error) { if (epoch === observeSession) endpointNotice(error.message, true); }
  finally { if (epoch === observeSession) { endpointBusy = false; renderApplications(); } }
}
async function readObservedLogs(deviceId, containerId, name) {
  if (endpointBusy) return; const epoch = observeSession; endpointBusy = true; observeController = new AbortController();
  endpointNotice('正在读取 ' + name + ' 的最近日志…'); renderApplications();
  try { const result = await observeFetch('api/v1/devices/' + deviceId + '/docker/containers/' + containerId + '/logs?tail=' + $('#observe-tail').value, AbortSignal.any([observeController.signal, AbortSignal.timeout(20000)]));
    if (epoch !== observeSession) return; if (result.result !== 'logs' || result.value.container_id !== containerId) throw new Error('日志响应与所选容器不匹配。');
    observedLogs.set(deviceId, { ...result.value, name, logs: result.value.logs || '暂无日志。', scroll: 0 });
    if ($('#endpoint-device').value === deviceId) { $('#observed-logs').open = true; endpointNotice('日志读取完成。'); }
  } catch (error) { if (epoch === observeSession) endpointNotice(error.message, true); }
  finally { if (epoch === observeSession) { endpointBusy = false; renderApplications(); } }
}
$('#observe-search').oninput = () => renderApplications();
$('#observe-state').onchange = () => renderApplications();
async function updateEndpoint(endpoint, change) { endpointBusy = true; endpointNotice('正在保存入口…'); renderApplications(); try { const response = await fetch('api/v1/access-endpoints/' + endpoint.id, { method: 'PATCH', headers: requestHeaders(true), body: JSON.stringify({ revision: endpoint.revision, ...change }), redirect: 'error' }); if (!response.ok) throw new Error(await response.text() || '保存失败（' + response.status + '）。'); const saved = await response.json(); endpoints = endpoints.map(value => value.id === saved.id ? saved : value); endpointNotice('入口已保存；在线 Agent 将按新 revision 重新核验。'); } catch (error) { endpointNotice(error.message, true); } finally { endpointBusy = false; renderApplications(); } }
async function removeEndpoint(endpoint) { if (!confirm('只删除 CoFleet 入口，不会删除原容器和数据。继续？')) return; endpointBusy = true; endpointNotice('正在删除入口…'); renderApplications(); try { const response = await fetch('api/v1/access-endpoints/' + endpoint.id + '?revision=' + endpoint.revision, { method: 'DELETE', headers: requestHeaders(), redirect: 'error' }); if (!response.ok) throw new Error(await response.text() || '删除失败（' + response.status + '）。'); endpoints = endpoints.filter(value => value.id !== endpoint.id); endpointNotice('入口已删除；原应用和数据未更改。'); } catch (error) { endpointNotice(error.message, true); } finally { endpointBusy = false; renderApplications(); } }
$('#endpoint-trust').onchange = () => renderApplications();
$('#endpoint-discover').onclick = discoverContainers;
$('#endpoint-logs').onclick = () => { const selected = selectedObserved(); if (selected) readObservedLogs($('#endpoint-device').value, selected.container.id, selected.container.name); };
$('#endpoint-form').onsubmit = async event => { event.preventDefault(); const selected = selectedObserved(); if (!selected || endpointBusy) return; const deviceId = $('#endpoint-device').value, catalog = observedCatalogs.get(deviceId), mode = $('#endpoint-binding').value; if (mode === 'compose' && (!selected.container.compose_project || !selected.container.compose_service)) { endpointNotice('这个容器没有 Compose 项目/服务标签，请选择当前容器绑定。', true); return; } endpointBusy = true; endpointNotice('正在保存并要求 Agent 核验入口…'); renderApplications(); const protocol = $('#endpoint-protocol').value; const body = { trusted_application: accessMode === 'trusted_prefix' && $('#endpoint-trust').checked, id: crypto.randomUUID(), device_id: deviceId, environment_id: catalog.environment_id, binding: mode === 'compose' ? { kind: 'compose', project: selected.container.compose_project, service: selected.container.compose_service } : { kind: 'container', container_id: selected.container.id }, container_port: selected.port, protocol, tls_server_name: protocol === 'https' ? $('#endpoint-tls').value.trim() : null, name: $('#endpoint-name').value.trim(), management_url: $('#endpoint-management').value.trim() || null }; try { const response = await fetch('api/v1/access-endpoints', { method: 'POST', headers: requestHeaders(true), body: JSON.stringify(body), redirect: 'error' }); if (!response.ok) throw new Error(await response.text() || '创建失败（' + response.status + '）。'); endpoints.push(await response.json()); $('#endpoint-name').value = ''; endpointNotice('入口配置已持久保存。打开时仍会重新核对容器、端口和当前 Agent 会话。'); } catch (error) { endpointNotice(error.message, true); } finally { endpointBusy = false; renderApplications(); } };

function scheduleReason(reason) {
  const messages = {
    'matching node selected': '已选择匹配节点，等待提交具体任务',
    'assigned node is online': '已分配节点在线，保持当前计划',
    'stateful workload lost contact with its node; waiting to avoid a second writer': '有状态负载与原节点失联；等待恢复，避免出现第二个写入者',
    'previous node is offline; retry on a matching node': '原节点离线，计划在匹配节点重试',
    'previous node is offline; rebuild stateless service on a matching node': '原节点离线，计划在匹配节点重建无状态服务',
    'no nodes are registered': '尚无已注册节点',
    'node is offline': '节点离线',
    'node paused new tasks': '节点已暂停接收新任务',
    'build/deploy executor is not enabled': '尚未启用构建与发布执行器',
    'resource report is unavailable': '尚未收到资源报告',
    'insufficient execution-environment CPU capacity': '执行环境 CPU 额度不足',
    'insufficient execution-environment memory capacity': '执行环境内存额度不足',
    'insufficient physical CPU capacity': '物理主机剩余 CPU 额度不足',
    'insufficient physical memory capacity': '物理主机剩余内存额度不足',
    'GPU is required but not reported': '负载需要 GPU，节点尚未声明',
    'insufficient GPU memory': 'GPU 显存不足',
    'insufficient uncommitted physical GPU memory': '物理主机剩余 GPU 显存额度不足'
  };
  if (messages[reason]) return messages[reason];
  if (reason.startsWith('no matching node: ')) return '没有匹配节点：' + reason.slice(18).split('; ').map(scheduleReason).join('；');
  const data = /^data location (.+) is not present$/.exec(reason);
  return data ? '节点不含数据位置 ' + data[1] : reason;
}

function renderWorkloads() {
  const area = el('div', 'workload-list'); area.id = 'workloads';
  $('#schedule-submit').disabled = submittingSchedule || !fresh || (!token && !platformLogin);
  if (!workloads.length) { area.append(el('p', '', fresh ? '尚无调度计划。' : '连接后可创建调度计划。')); commit($('#workloads'), area); return; }
  const actions = { start: '已分配', keep: '保持分配', retry: '计划重试', rebuild: '计划重建', wait: '等待' };
  const kinds = { batch: '批任务', stateless_service: '无状态服务', stateful_service: '有状态服务' };
  for (const workload of workloads) {
    const row = el('div', 'workload-row'), summary = el('div'); row.dataset.key = workload.workload_id;
    const device = devices.find(value => value.device_id === workload.device_id);
    summary.append(el('strong', '', (kinds[workload.kind] || workload.kind) + ' · ' + (actions[workload.action] || workload.action)), el('div', '', scheduleReason(workload.reason)), el('small', '', '节点：' + (device?.name || workload.device_id || '未分配') + ' · 尝试 ' + workload.attempt), el('code', '', workload.workload_id));
    const remove = el('button', 'quiet', '释放'); remove.type = 'button'; remove.disabled = !fresh; remove.onclick = () => removeWorkload(workload.workload_id);
    row.append(summary, remove);
    const failures = (workload.candidates || []).filter(candidate => !candidate.eligible).map(candidate => {
      const node = devices.find(value => value.device_id === candidate.device_id);
      return (node?.name || candidate.device_id) + '：' + candidate.reasons.map(scheduleReason).join('、');
    });
    if (failures.length) row.append(el('div', 'candidate-reasons', failures.join('；')));
    area.append(row);
  }
  commit($('#workloads'), area);
}

function requestHeaders(json = false) {
  return { ...(json ? { 'Content-Type': 'application/json' } : {}), 'X-CoFleet-Request': '1', ...(platformLogin ? {} : { Authorization: 'Bearer ' + token }) };
}

function setScheduleNotice(message, error = false) { const note = $('#schedule-notice'); note.textContent = message; note.className = 'notice' + (error ? ' error' : ''); }

async function removeWorkload(id) {
  if (!fresh) return;
  const epoch = dockerEpoch;
  try {
    const response = await fetch('api/v1/workloads/' + id, { method: 'DELETE', headers: requestHeaders(), redirect: 'error' });
    if (epoch !== dockerEpoch) return;
    if (!response.ok) throw new Error('释放调度计划失败（' + response.status + '）。');
    workloads = workloads.filter(value => value.workload_id !== id); setScheduleNotice('调度计划已释放。'); render();
  } catch (error) { if (epoch === dockerEpoch) setScheduleNotice(error.message, true); }
}

$('#schedule-form').onsubmit = async event => {
  event.preventDefault();
  if (submittingSchedule || !fresh) return;
  const epoch = dockerEpoch;
  const kind = $('#schedule-kind').value, data = $('#schedule-data').value.trim();
  if (kind === 'stateful_service' && !data) { setScheduleNotice('有状态负载必须填写数据位置，避免错误复制。', true); return; }
  const input = { kind, requirements: { cpu_threads: Number($('#schedule-cpu').value), memory_mib: Number($('#schedule-memory').value), gpu_memory_mib: Number($('#schedule-gpu').value || 0), data_location: data || null, prefer_always_on: $('#schedule-always').checked } };
  if (pendingSchedule && JSON.stringify(input) !== JSON.stringify({ kind: pendingSchedule.kind, requirements: pendingSchedule.requirements })) {
    setScheduleNotice('上次提交结果待确认，请先刷新计划列表，或用原参数重试。', true); return;
  }
  const request = pendingSchedule || { workload_id: crypto.randomUUID(), ...input };
  pendingSchedule = request; submittingSchedule = true; $('#schedule-submit').disabled = true;
  const requestController = new AbortController();
  const requestTimeout = setTimeout(() => requestController.abort(), 15000);
  try {
    const response = await fetch('api/v1/workloads', { method: 'POST', headers: requestHeaders(true), body: JSON.stringify(request), signal: requestController.signal, redirect: 'error' });
    if (epoch !== dockerEpoch) return;
    if (!response.ok) {
      if (response.status >= 400 && response.status < 500) pendingSchedule = null;
      throw new Error(await response.text() || '调度请求失败（' + response.status + '）');
    }
    const placement = await response.json();
    if (epoch !== dockerEpoch) return;
    pendingSchedule = null; workloads = workloads.filter(value => value.workload_id !== placement.workload_id); workloads.push(placement); setScheduleNotice(scheduleReason(placement.reason)); render();
  } catch (error) { if (epoch === dockerEpoch) setScheduleNotice(error.message + (pendingSchedule ? ' 结果待确认，再次提交将复用同一请求 ID。' : ''), true); }
  finally { clearTimeout(requestTimeout); if (epoch === dockerEpoch) { submittingSchedule = false; $('#schedule-submit').disabled = !fresh; } }
};
async function refresh() {
  if (!token && !platformLogin) return;
  const current = ++generation;
  controller?.abort(); const activeController = new AbortController(); controller = activeController;
  const timeout = setTimeout(() => activeController.abort(), 8000);
  $('#refresh').disabled = true;
  try {
    const response = await fetch('api/v1/devices', { headers: platformLogin ? {} : { Authorization: 'Bearer ' + token }, cache: 'no-store', signal: activeController.signal, redirect: 'error' });
    if (current !== generation) return;
    if (!response.ok) {
      const error = new Error(response.status === 401 || response.status === 403 ? (platformLogin ? '登录已失效，请在懒猫客户端重新登录。' : '访问凭据无效，请重新输入。') : '管理中心暂不可用。');
      error.authDenied = response.status === 401 || response.status === 403; throw error;
    }
    const data = await response.json();
    if (current !== generation) return;
    if (!Array.isArray(data)) throw new Error('管理中心返回了无法识别的数据。');
    const workloadResponse = await fetch('api/v1/workloads', { headers: platformLogin ? {} : { Authorization: 'Bearer ' + token }, cache: 'no-store', signal: activeController.signal, redirect: 'error' });
    if (!workloadResponse.ok) throw new Error('无法读取调度状态（' + workloadResponse.status + '）。');
    const workloadData = await workloadResponse.json();
    const endpointResponse = await fetch('api/v1/access-endpoints', { headers: platformLogin ? {} : { Authorization: 'Bearer ' + token }, cache: 'no-store', signal: activeController.signal, redirect: 'error' });
    if (!endpointResponse.ok) throw new Error('无法读取应用入口（' + endpointResponse.status + '）。');
    const endpointData = await endpointResponse.json();
    if (current !== generation) return;
    devices = data; workloads = Array.isArray(workloadData) ? workloadData : []; endpoints = Array.isArray(endpointData) ? endpointData : [];
    if (!submittingSchedule && pendingSchedule && workloads.some(value => value.workload_id === pendingSchedule.workload_id)) pendingSchedule = null;
    fresh = true; lastRefresh = Date.now();
    $('#login-panel').hidden = true;
    setConnection('已连接', 'online');
    $('#updated-at').textContent = '更新于 ' + new Date(lastRefresh).toLocaleTimeString('zh-CN');
    setNotice('');
    render();
  } catch (error) {
    if (current !== generation) return;
    fresh = false;
    if (error.authDenied) { devices = []; if (!platformLogin) $('#login-panel').hidden = false; }
    setConnection(error.authDenied ? '登录失效' : '连接待确认', 'warning');
    setNotice((error.name === 'AbortError' ? '刷新超时。' : error instanceof TypeError ? '暂时无法连接管理中心，或登录已失效。请检查网络并重新登录。' : error.message) + (lastRefresh && !error.authDenied ? ' 保留上次快照，将自动重试。' : ''), true);
    render();
  } finally {
    clearTimeout(timeout);
    if (current === generation) $('#refresh').disabled = false;
  }
}
$('#login').onsubmit = event => {
  dockerEpoch++; pendingSchedule = null; submittingSchedule = false; dockerViews.clear(); workspaceViews.clear(); pipelineViews.clear(); deviceViews.clear(); mobileDetail = false;
  event.preventDefault(); token = $('#token').value; $('#token').value = '';
  fetch('api/v1/workspace-session', { method: 'POST', headers: { Authorization: 'Bearer ' + token, 'X-CoFleet-Request': '1' } }).catch(() => {});
  devices = []; workloads = []; endpoints = []; selectedId = null; fresh = false; lastRefresh = null;
  $('#logout').hidden = false; resetInterface(); render(); refresh();
};
$('#logout').onclick = () => {
  dockerEpoch++; pendingSchedule = null; submittingSchedule = false; dockerViews.clear(); workspaceViews.clear(); pipelineViews.clear(); deviceViews.clear(); mobileDetail = false;
  if (platformLogin && accessMode === 'trusted_prefix') { fetch('api/v1/access-session', { method: 'DELETE', headers: requestHeaders(), keepalive: true }).catch(() => {}); platformLogin = false; }
  if (token) fetch('api/v1/workspace-session', { method: 'DELETE', headers: { Authorization: 'Bearer ' + token, 'X-CoFleet-Request': '1' } }).catch(() => {});
  generation++; controller?.abort(); token = ''; devices = []; workloads = []; endpoints = []; observeSession++; observedCatalogs.clear(); observedLogs.clear(); observedTargets.clear(); observeController?.abort(); endpointBusy = false; fresh = false; selectedId = null; lastRefresh = null;
  $('#login-panel').hidden = false; $('#logout').hidden = true; $('#refresh').disabled = true;
  resetInterface(); setConnection('已断开'); setNotice('凭据已从页面内存清除。'); render();
};
function resetInterface() {
  $('#search').value = ''; $('#online-only').checked = false;
  $('#endpoint-trust').checked = false;
  $('#schedule-form').reset(); $('#schedule-notice').textContent = '';
  $('#updated-at').textContent = '每 5 秒自动更新'; showPage('computers');
}
$('#nav-computers').onclick = () => showPage('computers');
$('#nav-applications').onclick = () => showPage('applications');
$('#nav-scheduler').onclick = () => showPage('scheduler');
$('#refresh').onclick = refresh;
$('#search').oninput = render;
$('#online-only').onchange = render;
fetch('api/v1/auth-mode', { redirect: 'error' }).then(response => response.json()).then(mode => {
  accessMode = mode.access_mode || 'legacy';
  if (mode.auth === 'lazycat') {
    platformLogin = true; $('#login-panel').hidden = true; $('#refresh').disabled = false;
    setNotice('正在读取已接入的设备…'); refresh();
  }
}).catch(() => setNotice('无法读取登录方式，请刷新页面或检查管理中心。', true));
setInterval(refresh, 5000);
