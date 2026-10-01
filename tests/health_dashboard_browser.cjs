// Run with node --test tests/health_dashboard_browser.cjs. Browser boundaries are injected;
// the actual script served to users drives the refresh, timeout and disconnect scenarios.
const {test} = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const vm = require('node:vm');
test('dashboard preserves stale data on disconnect and recovers automatically', async () => {
  const elements = new Map();
  const element = id => { if (!elements.has(id)) elements.set(id, {textContent: '', hidden: false, replaceChildren() {}, append() {}}); return elements.get(id); };
  const created = [];
  let response = new Error('offline');
  let scheduled;
  const sandbox = {
    document: {getElementById: element, createElement: () => { const node = {textContent: '', append() {}}; Object.defineProperty(node, 'innerHTML', {set() {throw new Error('external text must not be rendered as HTML');}}); created.push(node); return node; }},
    fetch: async () => {if(response instanceof Error) throw response; return {ok: true, json: async () => response};},
    AbortSignal: {timeout: () => ({})},
    setInterval: (callback, milliseconds) => {scheduled = {callback, milliseconds};},
  };
  const source = fs.readFileSync('src/health.html', 'utf8').match(/<script>([\s\S]*?)<\/script>/);
  assert.ok(source, 'dashboard must have an automatic refresh script');
  vm.runInNewContext(source[1], sandbox);
  await new Promise(setImmediate);
  assert.equal(scheduled.milliseconds, 10000);
  assert.equal(element('status').textContent, '未知');
  assert.match(element('connection').textContent, /不可达/);
  response = {status:'normal', updated_at:'2026-10-01T01:00:00Z', started_at:'2026-10-01T00:00:00Z', uptime_seconds:3600, components:{github:{status:'normal',details:{error:'<img src=x onerror=alert(1)>'}}}};
  await scheduled.callback();
  assert.equal(element('status').textContent, '正常');
  assert.equal(element('updated').textContent, '2026-10-01T01:00:00Z');
  assert.ok(created.some(node => node.textContent.includes('<img src=x onerror=alert(1)>')), 'external error is displayed as literal text');
  response = new Error('offline');
  await scheduled.callback();
  assert.equal(element('status').textContent, '未知');
  assert.equal(element('updated').textContent, '2026-10-01T01:00:00Z');
  assert.match(element('connection').textContent, /连接失败/);
  response = {status:'warning',updated_at:'2026-10-01T01:00:20Z', started_at:'2026-10-01T00:00:00Z', uptime_seconds:3620, components:{}};
  await scheduled.callback();
  assert.equal(element('status').textContent, '警告');
  assert.equal(element('updated').textContent, '2026-10-01T01:00:20Z');
  assert.equal(element('connection').hidden, true);
});

