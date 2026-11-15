import test from 'node:test';
import assert from 'node:assert/strict';
await import('../public/chat/md.js');
await import('../public/chat/cards.js');
await import('../public/chat/sheets.js');
const C = globalThis.cdashSheets;

test('diff lines get classes by how they start', () => {
  assert.deepEqual(C.diffLines('diff --git a/x b/x\nindex 1..2\n--- a/x\n+++ b/x\n@@ -1 +1 @@\n-a\n+b\n c\n\\ No newline at end of file'),
    [['meta', 'diff --git a/x b/x'], ['meta', 'index 1..2'], ['meta', '--- a/x'], ['meta', '+++ b/x'], ['hunk', '@@ -1 +1 @@'], ['del', '-a'], ['add', '+b'], ['ctx', ' c'], ['ctx', '\\ No newline at end of file']]);
});
test('inside a hunk, removed and added lines that look like headers stay lines', () => {
  assert.deepEqual(C.diffLines('--- a/x\n+++ b/x\n@@ -1 +1 @@\n--- old\n+++ new\n'),
    [['meta', '--- a/x'], ['meta', '+++ b/x'], ['hunk', '@@ -1 +1 @@'], ['del', '--- old'], ['add', '+++ new']]);
  assert.deepEqual(C.diffLines(''), []);
  assert.deepEqual(C.diffLines(null), []);
});
test('elapsed time reads as seconds, minutes or hours', () => {
  const at = s => Date.parse('2026-11-13T10:00:00Z') + s * 1000;
  assert.deepEqual([45, 185, 3720].map(s => C.elapsed('2026-11-13T10:00:00Z', at(s))), ['45s', '3m 05s', '1h 02m']);
  assert.equal(C.elapsed(Date.parse('2026-11-13T10:00:00Z'), at(7)), '7s', 'milliseconds work too');
  assert.equal(C.elapsed('2026-11-13T10:00:00Z', at(-5)), '0s', 'a clock behind the start');
});
test('a missing or odd start reads as nothing and never throws', () => {
  for (const s of [undefined, null, '', 'nope', {}, [], NaN, true, '1789000000000'])
    assert.equal(C.elapsed(s, Date.now()), '', String(s));
});
test('a Claude side question over 100,000 context tokens asks first', () => {
  assert.equal(C.asideConfirm({ tool: 'claude', usage: { context: { used: 120000 } } }), 120000);
  for (const s of [{ tool: 'claude', usage: { context: { used: 100000 } } }, { tool: 'codex', usage: { context: { used: 500000 } } }, { tool: 'codex', usage: null }])
    assert.equal(C.asideConfirm(s), 0);
});
test('a Claude side question of unknown size asks first too', () => {
  for (const s of [{ tool: 'claude', usage: null }, { tool: 'claude', usage: { context: null } }, { tool: 'claude' }])
    assert.equal(C.asideConfirm(s), -1);
});
test('/btw becomes a side question', () => {
  assert.deepEqual(['/btw what changed?', '/btw', '/btwx', 'hi /btw x'].map(C.btw), ['what changed?', '', null, null]);
});

// Just enough DOM for the sheet: nodes keep their children, text and markup.
function fakeDom() {
  const node = tag => ({
    tag, kids: [], dataset: {}, className: '', value: '', disabled: false, hidden: false, html: '', own: '',
    set textContent(v) { this.own = String(v); this.kids = []; this.html = ''; },
    get textContent() { return this.own + this.kids.map(k => k.textContent).join(''); },
    set innerHTML(v) { this.html = v; this.kids = []; this.own = ''; },
    get innerHTML() { return this.html; },
    append(...xs) { this.kids.push(...xs.map(x => typeof x === 'string' ? { textContent: x, kids: [] } : x)); },
    setAttribute() {}, focus() {},
    addEventListener(t, f) { this['on' + t] = f; },
    showModal() { this.open = true; },
    close() { if (!this.open) return; this.open = false; this.onclose?.(); },
  });
  const ids = { '#chat-sheet': node('dialog'), '#chat-sheet-title': node('span'), '#chat-sheet-body': node('div'), '#chat-running': node('button') };
  globalThis.document = { querySelector: s => ids[s] ?? null, createElement: node };
  const all = n => [n, ...(n.kids || []).flatMap(all)];
  return {
    sheet: ids['#chat-sheet'], body: ids['#chat-sheet-body'],
    find: pred => all(ids['#chat-sheet-body']).find(pred),
    button: text => all(ids['#chat-sheet-body']).find(n => n.tag === 'button' && n.textContent === text),
  };
}
const chat = (state, opts = {}) => {
  const sent = [];
  return { sent, c: { state: { tool: 'codex', usage: null, tasks: [], working: false, ...state }, send: f => (sent.push(f), true), opts: { name: 's', ...opts } } };
};

test('a side answer appends its chunks in order, and closing it mid-answer cancels it', () => {
  const dom = fakeDom();
  const { c, sent } = chat({});
  C.openAside(c, 'what changed?');
  assert.equal(sent.length, 1);
  const { id } = sent[0];
  assert.deepEqual(sent[0], { t: 'aside', id, text: 'what changed?' });
  C.onFrame(c, { t: 'aside', id, text: '**a', done: false });
  C.onFrame(c, { t: 'aside', id: 'other', text: 'x', done: false });
  C.onFrame(c, { t: 'state' });
  C.onFrame(c, { t: 'aside', id, text: 'b**', done: false });
  assert.equal(dom.find(n => n.className.includes('chat-aside-answer')).innerHTML, globalThis.cdashMd.render('**ab**'));
  dom.sheet.close();
  assert.deepEqual(sent[1], { t: 'aside_cancel', id });
});

test('a finished side answer is not cancelled; an error shows as text; Ask another clears the sheet', () => {
  const dom = fakeDom();
  const { c, sent } = chat({});
  C.openAside(c, 'q');
  const { id } = sent[0];
  C.onFrame(c, { t: 'aside', id, text: '', done: true, error: '<b>side question failed</b>' });
  assert.ok(dom.body.textContent.includes('<b>side question failed</b>'));
  assert.ok(!dom.find(n => n.className.includes('chat-aside-answer')).innerHTML.includes('<b>'));
  dom.button('Ask another').onclick();
  assert.equal(dom.find(n => n.tag === 'textarea').value, '');
  assert.ok(!dom.body.textContent.includes('side question failed'));
  dom.sheet.close();
  assert.equal(sent.length, 1, 'nothing to cancel');
});

test('a Claude side question of unknown size waits for Ask anyway', () => {
  const dom = fakeDom();
  const { c, sent } = chat({ tool: 'claude', usage: null });
  C.openAside(c, 'q');
  assert.equal(sent.length, 0);
  assert.match(dom.body.textContent, /not known/);
  dom.button('Ask anyway').onclick();
  assert.equal(sent[0].t, 'aside');
});

test('a big Claude side question names its size before asking', () => {
  const dom = fakeDom();
  const { c, sent } = chat({ tool: 'claude', usage: { context: { used: 120000 } } });
  C.openAside(c, 'q');
  assert.equal(sent.length, 0);
  assert.ok(dom.body.textContent.includes('This question re-reads about ' + (120000).toLocaleString() + ' tokens of the conversation. Ask anyway?'));
});

test('Changes shows the withheld reason, never "no changes"', async () => {
  const dom = fakeDom();
  const why = 'Changes are not shown: this repository has a clean filter.';
  const { c } = chat({}, { api: async () => ({ folder: '/w', repo: true, files: [], truncated: false, withheld: why }) });
  await C.openDiff(c);
  assert.ok(dom.body.textContent.includes(why));
  assert.ok(dom.body.textContent.includes('/w'));
  assert.doesNotMatch(dom.body.textContent, /no changes/i);
});

test('Changes says when the folder is not a repository, or clean', async () => {
  const dom = fakeDom();
  let answer = { folder: '/w', repo: false, files: [], truncated: false };
  const { c } = chat({}, { api: async () => answer });
  await C.openDiff(c);
  assert.match(dom.body.textContent, /Not a git repository\./);
  answer = { folder: '/w', repo: true, files: [], truncated: false };
  await C.openDiff(c);
  assert.match(dom.body.textContent, /No changes\./);
});

test('Changes lists each file with its counts, binary ones as binary', async () => {
  const dom = fakeDom();
  const { c } = chat({}, { api: async () => ({ folder: '/w', repo: true, truncated: true, files: [
    { path: 'a.txt', status: 'modified', additions: 2, deletions: 1, patch: '@@ -1 +1 @@\n-a\n+b\n' },
    { path: 'img.png', status: 'modified', additions: null, deletions: null, patch: null }] }) });
  await C.openDiff(c);
  const t = dom.body.textContent;
  assert.ok(t.includes('a.txt') && t.includes('+2') && t.includes('−1'), t);
  assert.ok(t.includes('img.png') && t.includes('binary'), t);
  assert.ok(t.includes('Over 2 MB of changes'), t);
});

test('the running list reads each task, odd starts included', () => {
  const dom = fakeDom();
  const { c } = chat({ tasks: [{ id: '1', kind: 'shell', title: 'sleep 60', started: new Date(Date.now() - 5000).toISOString() }, { id: '2', kind: 'agent', title: 'Review' }] });
  C.openRunning(c);
  const t = dom.body.textContent;
  assert.match(t, /Shell.*sleep 60.*5s/);
  assert.match(t, /Agent.*Review/);
});

test('usage reads context, session tokens and the tool\'s plan limits', () => {
  const dom = fakeDom();
  const { c } = chat({ tool: 'claude', usage: { model: 'claude-opus-5-5', context: { used: 50000, window: 200000 }, tokens: { input: 1, output: 2, cache_read: 3, cache_write: 4 }, cost: null } },
    { limits: () => ({ claude: [{ short: '5h', pct: 25, resetsAt: null }], codex: [{ short: 'Codex 5h', pct: 9 }] }) });
  C.openUsage(c);
  const t = dom.body.textContent;
  assert.ok(t.includes((50000).toLocaleString() + ' of ' + (200000).toLocaleString() + ' tokens (25%)'), t);
  assert.ok(t.includes('claude-opus-5-5'), t);
  assert.ok(t.includes('5h: 25% used'), t);
  assert.ok(!t.includes('Codex'), t);
  assert.ok(!t.includes('Cost'), t);
});
