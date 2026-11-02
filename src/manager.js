'use strict';
const $ = selector => document.querySelector(selector);
let token = '', platformLogin = false, devices = [], selectedId = null, fresh = false;
let generation = 0, controller = null, lastRefresh = null;
const osNames = { linux: 'Linux', windows: 'Windows', macos: 'macOS' };

function el(tag, className, text) {
  const element = document.createElement(tag);
  if (className) element.className = className;
  if (text !== undefined) element.textContent = text;
  return element;
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
  const area = $('#details'); area.replaceChildren();
  if (!device) {
    const empty = el('div', 'detail-empty');
    empty.append(el('h2', '', '选择一台电脑'), el('p', '', '查看实时资源、连接状态与规划额度。'));
    area.append(empty); return;
  }
  const status = availability(device), resources = device.resources;
  const heading = el('div', 'detail-heading'), identity = el('div');
  identity.append(el('h2', '', device.name), el('p', '', (osNames[device.os] || device.os) + ' / ' + device.arch + (resources?.environment === 'wsl' ? ' · WSL 环境' : '')));
  const badge = el('span', 'badge ' + (status.kind === 'online' ? '' : status.kind), status.text);
  heading.append(identity, badge); area.append(heading);
  if (!fresh) area.append(el('p', 'resource-warning', '暂时无法连接管理中心。以下为上次获取的快照，设备状态待确认。'));
  else if (!device.online) area.append(el('p', 'resource-warning', '设备已离线，以下为最后一次上报的数据。'));
  else if (resources && resources.age_secs > 15) area.append(el('p', 'resource-warning', '资源采集暂未更新，当前显示上次采集的数据。'));
  else if (!resources) area.append(el('p', 'resource-warning', '尚未收到资源信息。请确认这台电脑已更新并运行新版 Agent。'));
  if (resources?.environment === 'wsl') area.append(el('p', 'resource-warning', '这是 WSL 执行环境的资源，请勿与同一台电脑的 Windows 宿主重复累计。'));

  const grid = el('div', 'resource-grid');
  const cpu = resources?.cpu_usage_percent;
  grid.append(metric('CPU 使用率', Number.isFinite(cpu) ? cpu.toFixed(1) + '%' : resources ? '采集中' : '—', resources ? resources.logical_cpus + ' 个逻辑线程' : '等待资源上报', cpu));
  const memoryAvailable = resources ? 100 * resources.available_memory_bytes / resources.total_memory_bytes : null;
  grid.append(metric('当前可用内存', resources ? bytes(resources.available_memory_bytes) : '—', resources ? '总内存 ' + bytes(resources.total_memory_bytes) : '等待资源上报', memoryAvailable, true));
  area.append(grid);
  const storage = el('div', 'storage-row'), disk = resources?.storage;
  storage.append(metric('Agent 所在磁盘', disk ? bytes(disk.available_bytes) + ' 可用' : '暂无数据', disk ? '文件系统总容量 ' + bytes(disk.total_bytes) + '，不合并其他磁盘' : '未能读取 Agent 数据目录所在文件系统', disk ? 100 * disk.available_bytes / disk.total_bytes : null, true));
  area.append(storage);

  const policy = device.resource_policy || { reserved_cpu_threads: 0, reserved_memory_mib: 0 };
  const capacity = device.planning_capacity;
  const section = el('section', 'policy-section'), head = el('div', 'section-head');
  head.append(el('h3', '', '预留与规划'), el('span', 'section-note', device.accepting_tasks ? '本机允许接收新任务' : '本机已暂停接收新任务'));
  const list = el('dl');
  pair(list, '为本机预留', policy.reserved_cpu_threads + ' 线程 / ' + bytes(policy.reserved_memory_mib * 1024 ** 2));
  pair(list, '剩余规划额度', capacity ? capacity.cpu_threads + ' 线程 / ' + bytes(capacity.memory_bytes) : '等待资源上报');
  section.append(head, list, el('p', 'policy-tip', '预留资源由这台电脑的本地配置管理。额度仅作规划，尚未作为任务限制执行。'));
  if (capacity && (capacity.cpu_threads === 0 || capacity.memory_bytes === 0)) section.append(el('p', 'resource-warning', '预留已覆盖全部资源，当前没有剩余规划额度。'));
  area.append(section);

  const info = el('dl', 'identity');
  pair(info, '最近上报', seen(device));
  pair(info, 'Agent 版本', device.version || '—');
  pair(info, '操作系统', resources?.os_version || (osNames[device.os] || device.os));
  pair(info, '系统运行时间', resources ? duration(resources.uptime_secs) : '—');
  pair(info, '设备 ID', device.device_id);
  pair(info, '资源采集时间', resources ? new Date(resources.sampled_at_unix * 1000).toLocaleString('zh-CN') : '—');
  area.append(info);
}
function render() {
  const query = $('#search').value.trim().toLocaleLowerCase();
  const visible = devices.filter(d => (!$('#online-only').checked || (fresh && d.online)) && [d.name, d.os, d.arch, d.device_id].some(v => String(v).toLocaleLowerCase().includes(query)));
  if (!visible.some(d => d.device_id === selectedId)) selectedId = visible[0]?.device_id || null;
  const list = $('#devices'); list.replaceChildren();
  for (const device of visible) {
    const item = el('button', 'device'); item.type = 'button';
    item.setAttribute('aria-pressed', String(device.device_id === selectedId));
    const top = el('span', 'device-top'), status = availability(device), state = el('span', 'device-status');
    state.append(el('span', 'dot ' + (status.kind === 'offline' ? '' : status.kind)), document.createTextNode(status.text));
    top.append(el('span', 'device-name', device.name), state);
    item.append(top, el('span', 'device-meta', (osNames[device.os] || device.os) + ' / ' + device.arch + ' · ' + seen(device)));
    item.onclick = () => { selectedId = device.device_id; render(); };
    list.append(item);
  }
  $('#empty').hidden = visible.length > 0;
  $('#empty').textContent = devices.length ? '没有符合当前筛选条件的设备。' : '设备上线后，会显示在这里。请在电脑上启动 Agent。';
  const online = devices.filter(d => d.online).length;
  $('#summary').textContent = devices.length ? devices.length + ' 台设备，' + (fresh ? online + ' 台在线' : '连接状态待确认') : '等待电脑接入。';
  renderDetails(visible.find(d => d.device_id === selectedId));
}
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
    devices = data; fresh = true; lastRefresh = Date.now();
    $('#login-panel').hidden = true;
    setConnection('已连接', 'online');
    setNotice('最近更新 ' + new Date(lastRefresh).toLocaleTimeString('zh-CN') + '，每 5 秒自动刷新。');
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
  event.preventDefault(); token = $('#token').value; $('#token').value = '';
  devices = []; selectedId = null; fresh = false; lastRefresh = null;
  $('#logout').hidden = false; render(); refresh();
};
$('#logout').onclick = () => {
  generation++; controller?.abort(); token = ''; devices = []; fresh = false; selectedId = null; lastRefresh = null;
  $('#login-panel').hidden = false; $('#logout').hidden = true; $('#refresh').disabled = true;
  setConnection('已断开'); setNotice('凭据已从页面内存清除。'); render();
};
$('#refresh').onclick = refresh;
$('#search').oninput = render;
$('#online-only').onchange = render;
fetch('api/v1/auth-mode', { redirect: 'error' }).then(response => response.json()).then(mode => {
  if (mode.auth === 'lazycat') {
    platformLogin = true; $('#login-panel').hidden = true; $('#refresh').disabled = false;
    setNotice('正在读取已接入的设备…'); refresh();
  }
}).catch(() => setNotice('无法读取登录方式，请刷新页面或检查管理中心。', true));
setInterval(refresh, 5000);
