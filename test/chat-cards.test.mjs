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
