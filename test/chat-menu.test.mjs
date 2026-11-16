import test from 'node:test';
import assert from 'node:assert/strict';
await import('../public/chat/menu.js');
const { filter } = globalThis.cdashMenu;
const cmds = [{ name: 'compact' }, { name: 'clear' }, { name: 'config' }];
test('the slash menu filters by the typed prefix and closes once arguments start', () => {
  assert.deepEqual(filter(cmds, '/').map(c => c.name), ['compact', 'clear', 'config']);
  assert.deepEqual(filter(cmds, '/CO').map(c => c.name), ['compact', 'config']);
  assert.deepEqual(filter(cmds, '/compact x'), []);
  assert.deepEqual(filter(cmds, 'hello'), []);
  assert.equal(filter(Array.from({ length: 80 }, (_, i) => ({ name: 'c' + i })), '/c').length, 50);
});
test('a value the list lacks gets a disabled placeholder first', () => {
  const { withPlaceholder } = globalThis.cdashMenu;
  const opts = [['opus', 'Opus'], ['haiku', 'Haiku']];
  assert.deepEqual(withPlaceholder(opts, 'opus'), opts);
  assert.deepEqual(withPlaceholder(opts, null)[0], ['', '—', true]);
  assert.equal(withPlaceholder(opts, undefined).length, 3);
});
