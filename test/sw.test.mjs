import test from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { runInNewContext } from 'node:vm';

// Runs public/sw.js against a fake cache and network and returns what one
// fetch event answers.
async function answer(path, { cached, network }) {
  const store = new Map(cached ? [[path, cached]] : []);
  const handlers = {};
  const cache = { put: async (k, r) => store.set(typeof k === 'string' ? k : new URL(k.url).pathname, r) };
  const self = { addEventListener: (type, fn) => { handlers[type] = fn; } };
  runInNewContext(readFileSync(new URL('../public/sw.js', import.meta.url), 'utf8'), {
    self, URL,
    caches: { open: async () => cache, keys: async () => [], match: async k => store.get(typeof k === 'string' ? k : new URL(k.url).pathname) },
    fetch: async () => { if (!network) throw new TypeError('offline'); return network; },
  });
  let responded;
  handlers.fetch({ request: { url: 'https://dash.example' + path, mode: 'no-cors' }, respondWith: p => { responded = p; } });
  return responded;
}
const res = body => ({ ok: true, status: 200, redirected: false, text: async () => body, clone: () => res(body) });

test('assets come from the network, so a page never pairs new HTML with an old stylesheet', async () => {
  const r = await answer('/style.css', { cached: res('old'), network: res('new') });
  assert.equal(await r.text(), 'new');
});
test('offline, the cached asset still answers', async () => {
  const r = await answer('/style.css', { cached: res('old'), network: null });
  assert.equal(await r.text(), 'old');
});
test('a new service worker takes over at once, so old browsers pick up the fix on their next load', () => {
  const handlers = {};
  let skipped = false;
  runInNewContext(readFileSync(new URL('../public/sw.js', import.meta.url), 'utf8'), {
    self: { addEventListener: (type, fn) => { handlers[type] = fn; }, skipWaiting: () => { skipped = true; } },
    URL, caches: {}, fetch: async () => {},
  });
  handlers.install?.({ waitUntil() {} });
  assert.ok(skipped);
});

// Runs sw.js with a fake registration and clients; records notifications shown and windows opened.
function worker() {
  const w = { handlers: {}, shown: [], opened: [], clients: [] };
  const self = {
    addEventListener: (type, fn) => { w.handlers[type] = fn; },
    registration: { showNotification: async (title, o) => { w.shown.push({ title, ...o }); } },
    clients: { matchAll: async () => w.clients, openWindow: async u => { w.opened.push(u); } },
  };
  runInNewContext(readFileSync(new URL('../public/sw.js', import.meta.url), 'utf8'), { self, URL, encodeURIComponent, caches: {}, fetch: async () => {} });
  return w;
}
function pushed(w, text) {
  let p;
  const data = text === undefined ? null : { json: () => JSON.parse(text), text: () => text };
  w.handlers.push({ data, waitUntil: x => { p = x; } });
  return p;
}

test('a push shows one notification per session, titled with the project', async () => {
  const w = worker();
  for (const [k, t] of [['done', 1], ['ask', 2]]) await pushed(w, JSON.stringify({ s: 'cdash-a', p: 'proj', k, t }));
  await pushed(w, JSON.stringify({ s: 'cdash-b', p: 'other', k: 'error', t: 3 }));
  assert.deepEqual(w.shown.map(n => [n.title, n.body, n.tag]),
    [['proj', 'Finished', 'cdash-a'], ['proj', 'Needs your answer', 'cdash-a'], ['other', 'Stopped with an error', 'cdash-b']]);
});
test('a push that cannot be read still shows a notification', async () => {
  const w = worker();
  await pushed(w, 'not json');
  await pushed(w, undefined);
  assert.deepEqual(w.shown.map(n => [n.title, n.body]), [['RC Dashboard', 'Open the dashboard'], ['RC Dashboard', 'Open the dashboard']]);
});
test('a tap asks an open dashboard to open the chat, or opens one that will', async () => {
  const w = worker();
  const note = { data: { s: 'cdash-a', p: 'proj' }, close() { this.closed = true; } };
  // posted messages are copied: objects made inside the vm context fail strict deepEqual on prototype
  const tap = () => { let p; w.handlers.notificationclick({ notification: note, waitUntil: x => { p = x; } }); return p; };
  await tap();
  assert.deepEqual(w.opened, ['/?chat=cdash-a&title=proj']);
  const posted = [];
  w.clients.push({ focus: async () => {}, postMessage: m => posted.push(JSON.parse(JSON.stringify(m))) });
  await tap();
  assert.deepEqual(posted, [{ t: 'open-chat', s: 'cdash-a', p: 'proj' }]);
  assert.equal(note.closed, true);
  note.data = { s: 'cdash-a', p: 'a&b #c' };
  w.clients.length = 0;
  await tap();
  assert.equal(w.opened.at(-1), '/?chat=cdash-a&title=a%26b%20%23c');
});
