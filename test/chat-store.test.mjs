import test from 'node:test';
import assert from 'node:assert/strict';
await import('../public/chat/store.js');
const S = globalThis.cdashChatStore;
const ev = (seq, e) => ({ t: 'ev', seq, ev: e });
const prompt = (uuid, text, extra = {}) => ({ type: 'user', uuid, message: { role: 'user', content: text }, src: 'typed', ...extra });
const say = (uuid, id, text) => ({ type: 'assistant', uuid, message: { id, role: 'assistant', content: [{ type: 'text', text }] } });
const use = (uuid, id, name, input) => ({ type: 'assistant', uuid, message: { id: 'm' + id, role: 'assistant', content: [{ type: 'tool_use', id, name, input }] } });
const done = (uuid, id, text, is_error = false) => ({ type: 'user', uuid, message: { role: 'user', content: [{ type: 'tool_result', tool_use_id: id, content: text, is_error }] } });
const hello = (o = {}) => ({ t: 'hello', epoch: 'e1', tool: 'claude', first: 1, head: 0, reset: true, ...o });
const WORKING = { t: 'state', working: true, waiting: false, ended: false, held: [] };
const feed = frames => frames.reduce(S.apply, S.initial());

test('events become rows; repeats and gaps are not shown twice', () => {
  const s = feed([hello(), ev(1, prompt('u1', 'hi')), ev(2, say('k1', 'm1', 'hello')), ev(2, say('k1', 'm1', 'hello')), ev(1, prompt('u1', 'hi'))]);
  assert.deepEqual(S.rows(s).map(r => [r.kind, r.text]), [['user', 'hi'], ['text', 'hello']]);
  assert.equal(s.head, 2);
});
test('tool calls pair with results, group by kind, and show a running state', () => {
  const s = feed([hello(), ev(1, use('a', 't1', 'Read', { file_path: '/p/README.md' })), ev(2, done('b', 't1', 'ok')),
    ev(3, use('c', 't2', 'Read', { file_path: '/p/a.js' })), ev(4, done('d', 't2', 'ok')), ev(5, use('e', 't3', 'Bash', { command: 'npm test' }))]);
  const rows = S.rows(s);
  assert.deepEqual(rows.map(r => [r.kind, r.label, r.running]), [['tools', 'Read 2 files', false], ['tools', 'Running `npm test`', true]]);
  assert.equal(rows[0].calls[1].result, 'ok');
  assert.equal(S.toolLabel('Bash', { command: 'npm test' }, true), 'Ran `npm test`');
  assert.equal(S.toolLabel('Edit', { file_path: '/p/README.md' }, true), 'Edited README.md');
  assert.equal(S.toolLabel('mcp__beeper__search', {}, true), 'mcp__beeper__search');
});
test('my message goes sending, sent by its client id, and is not doubled', () => {
  let s = feed([hello()]);
  S.addMine(s, 'c1', 'do it');
  assert.deepEqual(S.rows(s).map(r => [r.text, r.status]), [['do it', 'sending']]);
  s = S.apply(s, { t: 'ack', cid: 'c1', status: 'sent' });
  s = S.apply(s, ev(1, prompt('u1', 'do it', { cid: 'c1' })));
  assert.deepEqual(S.rows(s).map(r => [r.text, r.status]), [['do it', 'sent']]);
});
test('held, returned, refused and unconfirmed messages keep their text', () => {
  let s = feed([hello()]);
  for (const [cid, status] of [['c1', 'held'], ['c2', 'returned'], ['c3', 'unconfirmed']]) { S.addMine(s, cid, cid); s = S.apply(s, { t: 'ack', cid, status, id: 'h1' }); }
  S.addMine(s, 'c4', 'c4'); s = S.apply(s, { t: 'ack', cid: 'c4', status: 'refused', error: 'session ended' });
  const mine = S.rows(s).filter(r => r.kind === 'user');
  assert.deepEqual(mine.map(r => r.status), ['held', 'returned', 'unconfirmed', 'refused']);
  assert.equal(mine[3].error, 'session ended');
});
test('a withdrawn prompt disappears and a queued one shows until Claude takes it', () => {
  let s = feed([hello(), WORKING, ev(1, prompt('u4', 'handed back')), ev(2, { type: 'cdash', subtype: 'retract', uuid: 'u4' }),
    ev(3, { type: 'cdash', subtype: 'queued', text: 'use -j4' })]);
  assert.deepEqual(S.rows(s).map(r => [r.text, r.status]), [['use -j4', 'queued']]);
  s = S.apply(s, ev(4, prompt('q1', 'use -j4', { src: 'steer' })));
  assert.deepEqual(S.rows(s).map(r => [r.text, r.status]), [['use -j4', 'sent']]);
});
test('a reset drops what came before, and my held message the agent forgot is marked not sent', () => {
  let s = feed([hello(), ev(1, prompt('u1', 'old'))]);
  S.addMine(s, 'c1', 'waiting'); s = S.apply(s, { t: 'ack', cid: 'c1', status: 'held', id: 'h1' });
  s = S.apply(s, hello({ epoch: 'e2', head: 0 }));
  s = S.apply(s, { t: 'state', working: false, waiting: false, ended: false, held: [] });
  assert.deepEqual(S.rows(s).map(r => [r.text, r.status]), [['waiting', 'refused']]);
  s = S.apply(feed([hello(), ev(1, prompt('u1', 'old'))]), { t: 'reset', epoch: 'e3' });
  assert.equal(S.rows(s).length, 0);
  assert.equal(s.head, 0);
});
test('streamed text shows until the finished block replaces it; older pages go on top', () => {
  let s = feed([hello({ first: 3, head: 2 }), { t: 'delta', id: 'm9', text: 'Hel' }, { t: 'delta', id: 'm9', text: 'lo' }]);
  assert.deepEqual(S.rows(s).map(r => [r.text, r.streaming]), [['Hello', true]]);
  s = S.apply(s, ev(3, say('k9', 'm9', 'Hello.')));
  assert.deepEqual(S.rows(s).map(r => [r.text, r.streaming]), [['Hello.', false]]);
  s = S.apply(s, { t: 'page', events: [{ seq: 1, ev: prompt('u0', 'first') }, { seq: 2, ev: say('k0', 'm0', 'reply') }], more: false });
  assert.deepEqual(S.rows(s).map(r => r.text), ['first', 'reply', 'Hello.']);
  assert.equal(s.more, false);
});
test('turn ends, shell commands and both prompt content shapes are understood', () => {
  const s = feed([hello(), ev(1, { type: 'user', uuid: 'u', message: { role: 'user', content: [{ type: 'text', text: 'block form' }] } }),
    ev(2, { type: 'cdash', subtype: 'shell', uuid: 'b1', command: 'echo hi' }), ev(3, { type: 'cdash', subtype: 'shell_output', uuid: 'b2', stdout: 'hi', stderr: '' }),
    ev(4, { type: 'result', subtype: 'interrupted' }), ev(5, { type: 'result', subtype: 'success' }), ev(6, { type: 'something-new' })]);
  assert.deepEqual(S.rows(s).map(r => [r.kind, r.text ?? r.command]), [['user', 'block form'], ['shell', 'echo hi'], ['note', 'Stopped']]);
  assert.equal(S.rows(s)[1].stdout, 'hi');
});
test('a message Claude queued shows once, as queued, until Claude takes it', () => {
  let s = feed([hello(), WORKING]);
  S.addMine(s, 'c1', 'later');
  s = S.apply(s, { t: 'ack', cid: 'c1', status: 'sent' });
  s = S.apply(s, ev(1, { type: 'cdash', subtype: 'queued', text: 'later', cid: 'c1' }));
  assert.deepEqual(S.rows(s).map(r => [r.text, r.status]), [['later', 'queued']]);
  s = S.apply(s, ev(2, prompt('q1', 'later', { src: 'steer', cid: 'c1' })));
  assert.deepEqual(S.rows(s).map(r => [r.text, r.status]), [['later', 'sent']]);
});

test('held rows the latest state no longer lists are dropped, or kept as not sent when the session ended', () => {
  let s = feed([hello(), { t: 'state', working: false, waiting: false, ended: false, held: [] }]);
  S.addMine(s, 'c1', 'one'); s = S.apply(s, { t: 'ack', cid: 'c1', status: 'held', id: 'h1' });
  S.addMine(s, 'c2', 'two'); s = S.apply(s, { t: 'ack', cid: 'c2', status: 'held', id: 'h2' });
  s = S.apply(s, { t: 'state', working: true, waiting: false, ended: false, held: [{ id: 'h2', text: 'two' }] });
  assert.deepEqual(s.mine.map(m => m.cid), ['c2'], 'h1 left the queue while this client was away');
  s = S.apply(s, { t: 'state', working: false, waiting: false, ended: true, held: [] });
  assert.deepEqual(S.rows(s).map(r => [r.text, r.status, r.error]), [['two', 'refused', 'session ended']]);
});

test('every row carries a signature that changes when what the view shows changes', () => {
  let s = feed([hello(), ev(1, prompt('u1', 'hi')), ev(2, use('a', 't1', 'Bash', { command: 'ls' })), { t: 'delta', id: 'm9', text: 'Hel' }]);
  S.addMine(s, 'c1', 'mine');
  const sigs = st => S.rows(st).map(r => r.sig);
  const before = sigs(s);
  assert.deepEqual(sigs(s), before, 'stable for the same state');
  assert.ok(before.every(x => typeof x === 'string'));
  s = S.apply(s, { t: 'delta', id: 'm9', text: 'lo' });
  s = S.apply(s, ev(3, done('b', 't1', 'out')));
  s = S.apply(s, { t: 'ack', cid: 'c1', status: 'unconfirmed' });
  const after = sigs(s);                                   // rows: prompt, tools, streaming text, mine
  assert.equal(after[0], before[0], 'an unchanged row keeps its signature');
  for (const i of [1, 2, 3]) assert.notEqual(after[i], before[i], i);
});

test('rows are reused until something they show changes', () => {
  let s = feed([hello(), ev(1, prompt('u1', 'hi')), { t: 'state', working: false, waiting: false, ended: false, held: [] }]);
  const a = S.rows(s);
  assert.equal(S.rows(s), a, 'same array with no change');
  s = S.apply(s, { t: 'state', working: true, waiting: true, ended: false, held: [] });
  assert.equal(S.rows(s), a, 'a working/waiting-only state reuses the rows');
  s = S.apply(s, ev(2, prompt('u2', 'again')));
  const b = S.rows(s);
  assert.notEqual(b, a);
  assert.equal(b.length, 2);
  S.addMine(s, 'c1', 'mine');
  assert.equal(S.rows(s).length, 3, 'a new own message recomputes');
  s = S.apply(s, { t: 'delta', id: 'm9', text: 'x' });
  assert.equal(S.rows(s).length, 4, 'a delta recomputes');
});

test('dropMine removes an own row even after rows() was memoized', () => {
  let s = feed([hello()]);
  S.addMine(s, 'c1', 'mine');
  s = S.apply(s, { t: 'ack', cid: 'c1', status: 'unconfirmed' });
  assert.equal(S.rows(s).length, 1);
  S.dropMine(s, 'c1');
  assert.equal(S.rows(s).length, 0);
});

const st = o => ({ t: 'state', working: false, waiting: false, ended: false, unreadable: false, held: [], ...o });
test('asks, settings and hooks come from state frames; menu, answered and set replies are kept', () => {
  let s = S.initial();
  assert.deepEqual([s.asks, s.settings, s.hooks, s.waiting_for, s.menu, s.setReply], [[], {}, true, null, null, null]);
  s = S.apply(s, st({ waiting: true, waiting_for: 'input needed', asks: [{ id: 'a1', kind: 'plan', tool: 'claude', plan: '# P' }], settings: { model: 'claude-opus-5-5' }, hooks: false }));
  assert.deepEqual([s.asks[0].id, s.settings.model, s.hooks, s.waiting_for], ['a1', 'claude-opus-5-5', false, 'input needed']);
  s = S.apply(s, { t: 'answered', id: 'a1', ok: false, error: 'already answered' });
  assert.deepEqual(s.answered.get('a1'), { ok: false, error: 'already answered' });
  s = S.apply(s, st({ asks: [] }));
  assert.equal(s.answered.size, 0, 'replies about asks that are gone are forgotten');
  s = S.apply(s, { t: 'menu', commands: [{ name: 'compact' }] });
  assert.deepEqual(s.menu, { commands: [{ name: 'compact' }], models: [], efforts: [], agents: [] });
  s = S.apply(s, { t: 'set', ok: false, error: 'Stop the turn or wait for it to finish' });
  assert.deepEqual(s.setReply, { ok: false, error: 'Stop the turn or wait for it to finish' });
});
test('a state frame without the new keys uses defaults', () => {
  const s = S.apply(S.initial(), st({}));
  assert.deepEqual([s.asks, s.settings, s.hooks, s.waiting_for], [[], {}, true, null]);
});
test('an ack with status done drops the own row and refreshes rows()', () => {
  const s = S.initial();
  S.addMine(s, 'c1', '/compact');
  assert.equal(S.rows(s).length, 1);
  S.apply(s, { t: 'ack', cid: 'c1', status: 'done' });
  assert.equal(S.rows(s).length, 0);
});
test('the Terminal banner waits 500 ms for a card, except where no card can come', () => {
  const w = o => ({ waiting: true, asks: [], hooks: true, waiting_for: null, ...o });
  assert.equal(S.needsTerminal(w({}), 100), false);
  assert.equal(S.needsTerminal(w({}), 600), true);
  assert.equal(S.needsTerminal(w({ waiting_for: 'dialog open' }), 0), true);
  assert.equal(S.needsTerminal(w({ hooks: false }), 0), true);
  assert.equal(S.needsTerminal(w({ asks: [{ id: 'a' }] }), 9999), false);
  assert.equal(S.needsTerminal(w({ waiting: false }), 9999), false);
});
test('the header chip reads model, effort, Plan and a non-default agent', () => {
  assert.equal(S.chipText({ tool: 'claude', settings: { model: 'claude-opus-5-5', effort: 'high', plan: true } }), 'Opus · high · Plan');
  assert.equal(S.chipText({ tool: 'codex', settings: { model: 'gpt-5.4', effort: 'low', plan: false } }), 'gpt-5.4 · low');
  assert.equal(S.chipText({ tool: 'opencode', settings: { model: 'anthropic/claude-sonnet-5', variant: 'max', agent: 'plan' } }), 'claude-sonnet-5 · max · plan');
  assert.equal(S.chipText({ tool: 'opencode', settings: { model: 'a/b', variant: null, agent: 'build' } }), 'b');
  assert.equal(S.chipText({ tool: 'claude', settings: {} }), '');
});

test('prompt rows carry their attachments and drop malformed ones', () => {
  const ref = 'ab'.repeat(32);
  const s = feed([hello(), ev(1, prompt('u1', 'see', { files: [{ kind: 'image', ref, mime: 'image/png' }, { kind: 'image', ref: '../../api/kill' },
    { kind: 'file', name: 'r.pdf', size: 9 }, { kind: 'file' }] }))]);
  assert.deepEqual(S.rows(s)[0].files, [{ kind: 'image', ref, mime: 'image/png' }, { kind: 'file', name: 'r.pdf', size: 9 }]);
});
test('my attachments show until the prompt event replaces them', () => {
  let s = feed([hello()]);
  const files = [{ id: '0123456789abcdef', name: 'a.png', kind: 'image', size: 8, mime: 'image/png' }];
  S.addMine(s, 'c1', '', files);
  assert.deepEqual(S.rows(s).map(r => [r.text, r.status, r.files]), [['', 'sending', files]]);
  s = S.apply(s, ev(1, prompt('u1', '', { cid: 'c1', files: [{ kind: 'image', ref: 'ab'.repeat(32), mime: 'image/png' }] })));
  assert.deepEqual(S.rows(s).map(r => [r.status, r.files[0].ref]), [['sent', 'ab'.repeat(32)]]);
});

test('rows carry time, id and source; days are separated; thinking is its own row', () => {
  const s = feed([hello(), ev(1, prompt('u1', 'hi', { ts: '2026-11-12T12:00:00Z' })),
    ev(2, { type: 'assistant', uuid: 'k1', ts: '2026-11-12T12:00:05Z', message: { id: 'm1', role: 'assistant', content: [{ type: 'thinking', thinking: 'Plan it.' }] } }),
    ev(3, { ...say('k2', 'm1', 'hello'), ts: '2026-11-12T12:00:06Z' }), ev(4, prompt('u2', 'again', { ts: '2026-11-13T12:00:00Z', src: 'steer' }))]);
  assert.deepEqual(S.rows(s).map(r => r.kind), ['user', 'thinking', 'text', 'day', 'user']);
  const [u1, th, , day, u2] = S.rows(s);
  assert.deepEqual([u1.uuid, u1.src, u1.ts, th.text, day.ts, u2.src], ['u1', 'typed', '2026-11-12T12:00:00Z', 'Plan it.', '2026-11-13T12:00:00Z', 'steer']);
});
test('the checklist is the latest todo event; a staged revert is a row', () => {
  const todo = items => ({ type: 'cdash', subtype: 'todo', items });
  const s = feed([hello(), ev(1, todo([{ text: 'a', status: 'pending' }])), ev(2, todo([{ text: 'a', status: 'completed' }, { text: 'b', status: 'in_progress' }])),
    ev(3, { type: 'cdash', subtype: 'staged', messages: 2 })]);
  assert.deepEqual(S.todo(s), { done: 1, total: 2, items: [{ text: 'a', status: 'completed' }, { text: 'b', status: 'in_progress' }] });
  assert.equal(S.todo(feed([hello()])), null);
  assert.deepEqual(S.rows(s).map(r => [r.kind, r.count]), [['staged', 2]], 'todo events are not rows');
});
test('the hub\'s state todo wins and outlives the backlog', () => {
  const items = [{ text: 'a', status: 'completed' }, { text: 'b', status: 'pending' }];
  const st = todo => ({ t: 'state', working: false, waiting: false, ended: false, held: [], todo });
  let s = feed([hello(), st(items)]);
  assert.deepEqual(S.todo(s), { done: 1, total: 2, items }, 'no todo event in the rows');
  s = S.apply(s, ev(1, { type: 'cdash', subtype: 'todo', items: [{ text: 'old', status: 'pending' }] }));
  assert.equal(S.todo(s).items[0].text, 'a', 'state beats an event');
  s = S.apply(s, st(null));
  assert.equal(S.todo(s).items[0].text, 'old', 'no state todo: the latest event');
});
test('a queued row with no later prompt is hidden once the session is idle', () => {
  let s = feed([hello(), WORKING, ev(1, { type: 'cdash', subtype: 'queued', text: 'lost' })]);
  assert.equal(S.rows(s).length, 1);
  s = S.apply(s, { ...WORKING, working: false });
  assert.deepEqual(S.rows(s), []);
});
test('edit diffs and their counts come from each tool\'s own record', () => {
  assert.deepEqual(S.lineDiff('a\nb\nc', 'a\nB\nc'), [{ t: ' ', s: 'a' }, { t: '-', s: 'b' }, { t: '+', s: 'B' }, { t: ' ', s: 'c' }]);
  const n = d => [d.add, d.del];
  assert.deepEqual(n(S.editDiff({ name: 'Edit', input: { file_path: '/p/x.js', old_string: 'a\nb\nc', new_string: 'a\nB\nc' } })), [1, 1]);
  assert.deepEqual(n(S.editDiff({ name: 'MultiEdit', input: { file_path: '/p/x.js', edits: [{ old_string: 'a', new_string: 'b' }, { old_string: 'c', new_string: 'd\ne' }] } })), [3, 2]);
  assert.deepEqual(n(S.editDiff({ name: 'Write', input: { file_path: '/p/n.txt', content: 'x\ny' } })), [2, 0]);
  const codex = S.editDiff({ name: 'Edit', input: { changes: [{ path: 'a.rs', kind: { type: 'update' }, diff: '@@ -1 +1 @@\n-old\n+new\n' }, { path: 'b.rs', kind: { type: 'add' }, diff: 'one\ntwo\n' }] } });
  assert.deepEqual([...n(codex), codex.files.length], [3, 1, 2]);
  assert.deepEqual(n(S.editDiff({ name: 'edit', input: { path: 'a.ts' }, diff: [{ file: 'a.ts', additions: 1, deletions: 1, patch: '--- a.ts\n+++ a.ts\n@@ -1 +1 @@\n-x\n+y\n' }] })), [1, 1]);
  assert.equal(S.editDiff({ name: 'Read', input: { file_path: '/p/x' } }), null);
  const rows = S.rows(feed([hello(), ev(1, use('a', 't1', 'Edit', { file_path: '/p/x.js', old_string: 'a\nb', new_string: 'a\nB' })), ev(2, done('b', 't1', 'ok'))]));
  assert.equal(rows[0].label, 'Edited x.js +1 \u22121');
});
test('an OpenCode tool result\'s diff lands on its call', () => {
  const diff = [{ file: 'a.ts', additions: 1, deletions: 0, patch: '+z\n' }];
  const r = { type: 'user', uuid: 'b', message: { role: 'user', content: [{ type: 'tool_result', tool_use_id: 't1', content: 'ok' }] }, diff };
  const rows = S.rows(feed([hello(), ev(1, use('a', 't1', 'edit', { path: 'a.ts' })), ev(2, { ...r, message: { ...r.message, content: [{ ...r.message.content[0], diff }] } })]));
  assert.deepEqual(rows[0].calls[0].diff, diff);
});
test('rewind is offered on idle prompts backed by an event that did not steer; state carries usage and tasks', () => {
  const idle = { working: false, waiting: false, ended: false, unreadable: false };
  assert.equal(S.canRewind(idle, { kind: 'user', uuid: 'u1', src: 'typed' }), true);
  assert.equal(S.canRewind(idle, { kind: 'user', uuid: 'u1' }), true, 'Codex and OpenCode prompts have no src');
  for (const [st, row] of [[{ ...idle, working: true }, { kind: 'user', uuid: 'u1' }], [idle, { kind: 'user', uuid: 'u1', src: 'steer' }],
    [idle, { kind: 'user', cid: 'c1' }], [idle, { kind: 'text', uuid: 'k1' }]]) assert.equal(S.canRewind(st, row), false);
  const s = feed([hello(), { t: 'state', working: false, waiting: false, ended: false, held: [], usage: { cwd: '/w' }, tasks: [{ id: 'b1' }] }]);
  assert.deepEqual([s.usage, s.tasks], [{ cwd: '/w' }, [{ id: 'b1' }]]);
});
