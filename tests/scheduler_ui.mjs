// Execute the shipped UI handler with a lost response, without a live browser.
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { randomUUID } from 'node:crypto';
import vm from 'node:vm';
const fields = new Map();
const field = key => {
  if (!fields.has(key)) fields.set(key, { value: '', checked: false, setAttribute() {} });
  return fields.get(key);
};
let send;
const context = vm.createContext({
  document: { querySelector: field }, crypto: { randomUUID }, setInterval() {}, AbortController, setTimeout, clearTimeout,
  fetch: (url, options) => url === 'api/v1/auth-mode'
    ? Promise.resolve({ json: async () => ({ auth: 'development' }) }) : send(url, options)
});
vm.runInContext(readFileSync(new URL('../src/manager.js', import.meta.url), 'utf8'), context);
vm.runInContext('render = () => {}; fresh = true;', context);
field('#schedule-kind').value = 'batch';
field('#schedule-cpu').value = '1';
field('#schedule-memory').value = '512';
const submit = () => field('#schedule-form').onsubmit({ preventDefault() {} });
const accepted = new Map();
const calls = [];
let loseResponse = true;
send = async (_, options) => {
  const request = JSON.parse(options.body);
  calls.push(request);
  accepted.set(request.workload_id, request);
  if (loseResponse) { loseResponse = false; throw new TypeError('response lost'); }
  return { ok: true, json: async () => ({ workload_id: request.workload_id, reason: 'matching node selected' }) };
};
await submit();
assert.match(field('#schedule-notice').textContent, /复用同一请求 ID/);
field('#schedule-memory').value = '1024';
await submit();
assert.equal(calls.length, 1, 'changing uncertain input must not create a second plan');
field('#schedule-memory').value = '512';
await submit();
assert.equal(calls.length, 2);
assert.equal(calls[0].workload_id, calls[1].workload_id);
assert.equal(accepted.size, 1, 'lost-response retry must have only one server plan');
assert.match(field('#schedule-notice').textContent, /已选择匹配节点/);
console.log('PASS lost response retry keeps ID; changed input rejected; one accepted plan');
let resolve;
send = (_, options) => {
  calls.push(JSON.parse(options.body));
  return new Promise(done => { resolve = done; });
};
const inFlight = submit();
await submit();
assert.equal(calls.length, 3, 'concurrent submit must not send again');
vm.runInContext('dockerEpoch++; pendingSchedule = null; submittingSchedule = false; workloads = [];', context);
resolve({ ok: true, json: async () => ({ workload_id: calls[2].workload_id, reason: 'matching node selected' }) });
await inFlight;
assert.equal(vm.runInContext('workloads.length', context), 0, 'old login response must not change current plans');
assert.equal(vm.runInContext('scheduleReason("no matching node: data location home is not present; build/deploy executor is not enabled")', context),
  '没有匹配节点：节点不含数据位置 home；尚未启用构建与发布执行器');
console.log('PASS concurrent submit blocked; old login response ignored; candidate reasons localized');
