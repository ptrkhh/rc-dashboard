import test from 'node:test';
import assert from 'node:assert/strict';
import { createConnection } from 'node:net';
import { mkdtemp, stat, rm, readdir, utimes } from 'node:fs/promises';
import { createHash } from 'node:crypto';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { ChatFeed, withAttached, splitAttached, sniffImage, stashMedia, MAX_MEDIA } from '../crates/agent/src/collect/chat-feed.mjs';

const user = (uuid, text) => ({ type: 'user', uuid, message: { role: 'user', content: [{ type: 'text', text }] } });
async function client(path, first) {                              // returns { lines, write, close }
  const socket = createConnection(path); const lines = []; let buf = '';
  socket.on('data', d => { buf += d; let i; while ((i = buf.indexOf('\n')) >= 0) { lines.push(JSON.parse(buf.slice(0, i))); buf = buf.slice(i + 1); } });
  await new Promise(r => socket.once('connect', r));
  socket.write(JSON.stringify(first) + '\n');
  return { lines, write: o => socket.write(JSON.stringify(o) + '\n'), close: () => socket.destroy() };
}
const until = async (f, ms = 30000) => { const t = Date.now(); while (!f()) { if (Date.now() - t > ms) throw Error('timed out'); await new Promise(r => setTimeout(r, 10)); } };

test('a client gets the log, state, ready and then live events; reconnecting resumes', async t => {
  const dir = await mkdtemp(join(tmpdir(), 'cdash-feed-')); t.after(() => rm(dir, { recursive: true, force: true }));
  const feed = new ChatFeed({ send: async () => {}, stop: async () => {} }); t.after(() => feed.close());
  const path = await feed.listen(dir);
  assert.equal((await stat(path)).mode & 0o777, 0o600);
  feed.accept(user('u1', 'one')); feed.accept(user('u1', 'one')); feed.accept(user('u2', 'two'));
  const a = await client(path, { after: 0, epoch: null });
  await until(() => a.lines.length >= 6);
  assert.deepEqual(a.lines.map(l => l.t), ['hello', 'ev', 'ev', 'state', 'asks', 'tasks'], 'duplicates by uuid are dropped; not ready yet');
  feed.ready(); feed.setState({ working: true, waiting: false }); feed.delta('m1', 'Hel'); feed.accept(user('u3', 'three'));
  await until(() => a.lines.length >= 10);
  assert.deepEqual(a.lines.slice(6).map(l => l.t), ['ready', 'state', 'delta', 'ev']);
  assert.equal(a.lines.at(-1).seq, 3);
  const b = await client(path, { after: 2, epoch: a.lines[0].epoch });
  await until(() => b.lines.some(l => l.t === 'ready'));
  assert.deepEqual(b.lines.filter(l => l.t === 'ev').map(l => l.seq), [3]);
  const c = await client(path, { after: 2, epoch: 'someone-else' });
  await until(() => c.lines.some(l => l.t === 'ready'));
  assert.deepEqual(c.lines.filter(l => l.t === 'ev').map(l => l.seq), [1, 2, 3]);
  for (const x of [a, b, c]) x.close();
});
test('send and stop reach the supervisor and failures come back in the ack', async t => {
  const dir = await mkdtemp(join(tmpdir(), 'cdash-feed-')); t.after(() => rm(dir, { recursive: true, force: true }));
  const calls = [];
  const feed = new ChatFeed({ send: async (text, o) => { calls.push(['send', text, o.steer, o.cid]); if (text === 'bad') throw Error('no turn'); }, stop: async () => { calls.push(['stop']); } });
  t.after(() => feed.close());
  const a = await client(await feed.listen(dir), { after: 0, epoch: null });
  a.write({ t: 'send', cid: 'c1', text: 'go', steer: true }); a.write({ t: 'send', cid: 'c2', text: 'bad', steer: false }); a.write({ t: 'stop' }); a.write('garbage');
  await until(() => a.lines.filter(l => l.t === 'ack').length === 2 && calls.length === 3);
  assert.deepEqual(calls, [['send', 'go', true, 'c1'], ['send', 'bad', false, 'c2'], ['stop']]);
  assert.deepEqual(a.lines.filter(l => l.t === 'ack'), [{ t: 'ack', cid: 'c1', ok: true }, { t: 'ack', cid: 'c2', ok: false, error: 'no turn' }]);
  a.close();
});
test('only conversation messages are kept and approval requests set waiting', async () => {
  const feed = new ChatFeed({ send: async () => {}, stop: async () => {} });
  feed.accept({ type: 'stream_event', event: { type: 'message_start' } });
  feed.accept({ type: 'system', subtype: 'init' });
  feed.accept({ type: 'control_request', request_id: 'r1', request: { subtype: 'can_use_tool' } });
  assert.deepEqual([feed.head, feed.state.waiting], [0, true]);
  feed.accept({ type: 'control_cancel_request', request_id: 'r1' });
  feed.accept({ type: 'result', subtype: 'success', is_error: false, result: 'x', num_turns: 1 });
  assert.deepEqual([feed.head, feed.state.waiting], [1, false]);
  assert.deepEqual(feed.log[0].ev, { type: 'result', subtype: 'success' });
});
test('every req gets exactly one res, matched by rid; unknown ops and errors come back as errors', async t => {
  const dir = await mkdtemp(join(tmpdir(), 'cdash-feed-')); t.after(() => rm(dir, { recursive: true, force: true }));
  const feed = new ChatFeed({ send: async () => {}, stop: async () => {}, ops: {
    menu: async () => ({ commands: [{ name: 'compact' }] }),
    set: async ({ what }) => { if (what === 'bad') throw Error('nope'); },
    big: async () => ({ blob: 'x'.repeat(70 * 1024) }),
  } });
  t.after(() => feed.close());
  const a = await client(await feed.listen(dir), { after: 0, epoch: null });
  a.write({ t: 'req', rid: 1, op: 'menu' }); a.write({ t: 'req', rid: 2, op: 'set', what: 'bad', value: 1 });
  a.write({ t: 'req', rid: 3, op: 'rewind' }); a.write({ t: 'req', rid: 4, op: 'big' });
  a.write({ t: 'req', rid: 5, op: 'set', what: 'plan', value: true });
  await until(() => a.lines.filter(l => l.t === 'res').length === 5);
  assert.deepEqual(a.lines.filter(l => l.t === 'res').sort((x, y) => x.rid - y.rid), [
    { t: 'res', rid: 1, ok: true, data: { commands: [{ name: 'compact' }] } },
    { t: 'res', rid: 2, ok: false, error: 'nope' },
    { t: 'res', rid: 3, ok: false, error: 'not supported' },
    { t: 'res', rid: 4, ok: false, error: 'reply too large' },
    { t: 'res', rid: 5, ok: true, data: null }]);
  a.close();
});
test('asks and settings go out live and are replayed after hello; a huge plan is cut to fit a line', async t => {
  const dir = await mkdtemp(join(tmpdir(), 'cdash-feed-')); t.after(() => rm(dir, { recursive: true, force: true }));
  const feed = new ChatFeed({ send: async () => {}, stop: async () => {} }); t.after(() => feed.close());
  const path = await feed.listen(dir);
  const settings = { model: 'm', effort: 'low', plan: false, agent: null, variant: null };
  feed.putAsk({ id: '7', kind: 'question', tool: 'codex', questions: [] });
  feed.setSettings(settings);
  const a = await client(path, { after: 0, epoch: null });
  await until(() => a.lines.some(l => l.t === 'settings'));
  assert.deepEqual(a.lines.map(l => l.t), ['hello', 'state', 'asks', 'settings', 'tasks']);
  assert.equal(a.lines[2].asks[0].id, '7');
  feed.putAsk({ id: 'implement:t1', kind: 'plan', tool: 'codex', plan: '# P\n' + 'y'.repeat(200 * 1024) });
  feed.dropAsk('7'); feed.dropAsk('nothing');
  await until(() => a.lines.filter(l => l.t === 'asks').length === 3);
  const last = a.lines.filter(l => l.t === 'asks').at(-1);
  assert.deepEqual(last.asks.map(x => x.id), ['implement:t1']);
  assert.ok(Buffer.byteLength(JSON.stringify(last)) < 64 * 1024 && last.asks[0].plan.startsWith('# P\nyyy'));
  feed.setSettings({ ...settings });
  await new Promise(r => setTimeout(r, 50));
  assert.equal(a.lines.filter(l => l.t === 'settings').length, 1, 'unchanged settings are not sent again');
  a.close();
});
test('a send handler that returns done gets done in the ack', async t => {
  const dir = await mkdtemp(join(tmpdir(), 'cdash-feed-')); t.after(() => rm(dir, { recursive: true, force: true }));
  const feed = new ChatFeed({ send: async () => ({ done: true }), stop: async () => {} }); t.after(() => feed.close());
  const a = await client(await feed.listen(dir), { after: 0, epoch: null });
  a.write({ t: 'send', cid: 'c1', text: '/x' });
  await until(() => a.lines.some(l => l.t === 'ack'));
  assert.deepEqual(a.lines.find(l => l.t === 'ack'), { t: 'ack', cid: 'c1', ok: true, done: true });
  a.close();
});
test('a hanging op does not delay send or stop, and times out once; inherited names are unsupported', async t => {
  const dir = await mkdtemp(join(tmpdir(), 'cdash-feed-')); t.after(() => rm(dir, { recursive: true, force: true }));
  let stopped = false;
  const feed = new ChatFeed({ send: async () => {}, stop: async () => { stopped = true; }, opTimeout: 200, ops: { hang: () => new Promise(() => {}) } });
  t.after(() => feed.close());
  const a = await client(await feed.listen(dir), { after: 0, epoch: null });
  a.write({ t: 'req', rid: 1, op: 'hang' }); a.write({ t: 'req', rid: 2, op: 'constructor' });
  a.write({ t: 'send', cid: 'c1', text: 'hi' }); a.write({ t: 'stop' });
  await until(() => a.lines.some(l => l.t === 'ack') && stopped);
  assert.ok(!a.lines.some(l => l.rid === 1), 'the op is still pending');
  await until(() => a.lines.some(l => l.rid === 1));
  assert.deepEqual(a.lines.filter(l => l.t === 'res'), [{ t: 'res', rid: 2, ok: false, error: 'not supported' }, { t: 'res', rid: 1, ok: false, error: 'timed out' }]);
  a.close();
});

const PNG = Buffer.concat([Buffer.from('89504e470d0a1a0a', 'hex'), Buffer.from('feed')]);
test('upload lines round-trip and nothing else is touched', () => {
  const up = '/home/u/.cache/cdash/uploads/cdash-a/0123456789abcdef';
  const sent = withAttached('look at these', [`${up}/r.pdf`, `${up}/notes 1.txt`]);
  assert.equal(sent, `look at these\nAttached: ${up}/r.pdf\nAttached: ${up}/notes 1.txt`);
  assert.deepEqual(splitAttached(sent), { text: 'look at these', files: [{ kind: 'file', name: 'r.pdf' }, { kind: 'file', name: 'notes 1.txt' }] });
  assert.deepEqual(splitAttached(withAttached('', [`${up}/r.pdf`])), { text: '', files: [{ kind: 'file', name: 'r.pdf' }] });
  for (const kept of ['Attached: /etc/passwd', 'Attached: rel/cdash/uploads/x', `Attached: ${up}/a.txt\nthen more`])
    assert.deepEqual(splitAttached(kept), { text: kept, files: [] });
});
test('images are stored once by content in a private folder, and nothing else is', async t => {
  const root = await mkdtemp(join(tmpdir(), 'cdash-media-')); t.after(() => rm(root, { recursive: true, force: true }));
  const dir = join(root, 'cdash', 'media');
  const a = await stashMedia(dir, PNG);
  assert.deepEqual(a, { ref: createHash('sha256').update(PNG).digest('hex'), mime: 'image/png' });
  const file = join(dir, a.ref + '.png');
  assert.deepEqual([(await stat(file)).mode & 0o777, (await stat(dir)).mode & 0o777], [0o600, 0o700]);
  const past = new Date(Date.now() - 86_400_000);
  await utimes(file, past, past);
  await stashMedia(dir, PNG);
  assert.ok((await stat(file)).mtimeMs > past.getTime() + 60_000, 'a rewrite refreshes its age');
  assert.deepEqual(await readdir(dir), [a.ref + '.png'], 'one file, no temp left');
  assert.equal(await stashMedia(dir, Buffer.from('%PDF-1.7')), null);
  assert.equal(await stashMedia(undefined, PNG), null, 'no folder from the agent, no media');
  assert.equal(await stashMedia(dir, Buffer.concat([PNG, Buffer.alloc(MAX_MEDIA)])), null, 'over 20 MB is not kept');
  assert.equal(sniffImage(Buffer.from('RIFF\0\0\0\0WEBPVP8 ')), 'image/webp');
});
test('a send line hands its attachments to the supervisor', async t => {
  const dir = await mkdtemp(join(tmpdir(), 'cdash-feed-')); t.after(() => rm(dir, { recursive: true, force: true }));
  const got = [];
  const feed = new ChatFeed({ send: async (text, o) => { got.push(o.files); }, stop: async () => {} }); t.after(() => feed.close());
  const a = await client(await feed.listen(dir), { after: 0, epoch: null });
  const f = { path: '/u/up/a.png', kind: 'image', mime: 'image/png', name: 'a.png', size: 8 };
  a.write({ t: 'send', cid: 'c1', text: 'x', steer: false, files: [f, { kind: 'file' }] });
  a.write({ t: 'send', cid: 'c2', text: 'y', steer: false });
  await until(() => got.length === 2);
  assert.deepEqual(got, [[f], []]);
  a.close();
});

test('a reset gives every client a new hello and the history after it, then ready again', async t => {
  const dir = await mkdtemp(join(tmpdir(), 'cdash-feed-')); t.after(() => rm(dir, { recursive: true, force: true }));
  const feed = new ChatFeed({ send: async () => {}, stop: async () => {} }); t.after(() => feed.close());
  feed.accept(user('u1', 'one')); feed.ready(); feed.setUsage({ cwd: '/w' });
  const a = await client(await feed.listen(dir), { after: 0, epoch: null });
  await until(() => a.lines.some(l => l.t === 'ready'));
  const before = a.lines.length;
  feed.reset(); feed.accept(user('u1', 'one again')); feed.ready();
  await until(() => a.lines.slice(before).some(l => l.t === 'ready'));
  const after = a.lines.slice(before);
  assert.deepEqual([after[0].t, after[0].head], ['hello', 0]);
  assert.notEqual(after[0].epoch, a.lines[0].epoch);
  assert.deepEqual(after.filter(l => l.t === 'ev').map(l => [l.seq, l.ev.message.content[0].text]), [[1, 'one again']], 'a uuid from before the reset is accepted again');
  assert.ok(after.some(l => l.t === 'usage' && l.usage.cwd === '/w') && after.some(l => l.t === 'asks'));
  a.close();
});
test('usage and tasks are replayed after hello; aside lines are not; cdash events are kept', async t => {
  const dir = await mkdtemp(join(tmpdir(), 'cdash-feed-')); t.after(() => rm(dir, { recursive: true, force: true }));
  const feed = new ChatFeed({ send: async () => {}, stop: async () => {} }); t.after(() => feed.close());
  feed.setUsage({ cwd: '/w' }); feed.setTasks([{ id: 'b1' }]);
  feed.accept({ type: 'cdash', subtype: 'todo', items: [] }); feed.pushAside('q1', 'Hi', false);
  const a = await client(await feed.listen(dir), { after: 0, epoch: null });
  await until(() => a.lines.some(l => l.t === 'usage'));
  assert.deepEqual(a.lines.find(l => l.t === 'tasks').tasks, [{ id: 'b1' }]);
  assert.equal(a.lines.find(l => l.t === 'usage').usage.cwd, '/w');
  assert.deepEqual(a.lines.filter(l => l.t === 'ev').map(l => l.ev.subtype), ['todo']);
  assert.ok(!a.lines.some(l => l.t === 'aside'), 'nothing of a side answer is stored');
  feed.pushAside('q1', 'lo', true);
  await until(() => a.lines.some(l => l.t === 'aside'));
  assert.deepEqual(a.lines.find(l => l.t === 'aside'), { t: 'aside', id: 'q1', text: 'lo', done: true });
  feed.setTasks([]);
  await until(() => a.lines.filter(l => l.t === 'tasks').length === 2);
  assert.deepEqual(a.lines.filter(l => l.t === 'tasks').at(-1).tasks, []);
  feed.pushAside('q2', 'x'.repeat(100 * 1024), true);
  await until(() => a.lines.filter(l => l.t === 'aside' && l.id === 'q2').length >= 7);
  const q2 = a.lines.filter(l => l.t === 'aside' && l.id === 'q2');
  assert.deepEqual(q2.map(l => l.done), [false, false, false, false, false, false, true], 'a long answer is split under the line limit, done only on the last');
  assert.equal(q2.map(l => l.text).join('').length, 100 * 1024);
  a.close();
});
