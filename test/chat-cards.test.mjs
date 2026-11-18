import test from 'node:test';
import assert from 'node:assert/strict';
await import('../public/chat/cards.js');
const C = globalThis.cdashCards;
const ask = { id: 'a1', kind: 'question', tool: 'claude', questions: [
  { id: 'q0', header: 'Color', question: 'Pick a color', multi: false, other: true, secret: false, options: [{ label: 'Red', description: '' }, { label: 'Blue', description: '' }] },
  { id: 'q1', header: 'Fruits', question: 'Pick fruits', multi: true, other: false, secret: false, options: [{ label: 'Apple', description: '' }, { label: 'Pear', description: '' }] }] };
test('Submit waits for every question, and the frame carries choices and trimmed text', () => {
  assert.equal(C.complete(ask, {}), false);
  assert.equal(C.complete(ask, { q0: { choices: ['Red'], text: '' } }), false);
  assert.equal(C.complete(ask, { q0: { choices: [], text: '  ' }, q1: { choices: ['Pear'], text: '' } }), false);
  const picks = { q0: { choices: [], text: ' Green ' }, q1: { choices: ['Apple', 'Pear'], text: '' } };
  assert.equal(C.complete(ask, picks), true);
  assert.deepEqual(C.frame(ask, picks), { t: 'answer', id: 'a1', answers: { q0: { choices: [], text: 'Green' }, q1: { choices: ['Apple', 'Pear'], text: '' } } });
});

test('a question card renders with its Submit button (DOM append returns nothing)', () => {
  const node = tag => ({
    tag, kids: [], dataset: {}, style: {}, className: '', value: '', checked: false, disabled: false, own: '',
    set textContent(v) { this.own = String(v); this.kids = []; },
    get textContent() { return this.own + this.kids.map(k => k.textContent).join(''); },
    get firstChild() { return this.kids[0] ?? null; },
    append(...xs) { this.kids.push(...xs.map(x => typeof x === 'string' ? { textContent: x, kids: [] } : x)); }, // like the DOM: undefined
    setAttribute() {}, addEventListener() {}, classList: { toggle() {}, add() {} },
  });
  globalThis.document = { createElement: node, createTextNode: t => ({ textContent: t, kids: [] }) };
  const box = node('div');
  C.render(box, { asks: [ask], answered: new Map() }, () => true);
  const all = n => [n, ...(n.kids || []).flatMap(all)];
  assert.ok(all(box).some(n => n.tag === 'button' && n.textContent === 'Submit'), 'Submit is shown');
  assert.ok(all(box).some(n => n.textContent === 'Pick a color'));
});
