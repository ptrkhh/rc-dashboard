import test from 'node:test';
import assert from 'node:assert/strict';
await import('../public/chat/attach.js');
const A = globalThis.cdashAttach, MiB = 1024 * 1024;
const until = async (f, ms = 2000) => { const t = Date.now(); while (!f()) { if (Date.now() - t > ms) throw Error('timed out'); await new Promise(r => setTimeout(r, 10)); } };
function rig() {
  const sent = []; let open = true;
  const u = new A.Uploads({ send: d => open && (sent.push(d), true), buffered: () => 0, onChange: () => {} });
  return { u, sent, setOpen: v => { open = v; } };
}
const frames = sent => sent.filter(d => typeof d === 'string').map(d => JSON.parse(d));

test('files go one at a time: the upload frame, 1 MiB frames, then the answer', async () => {
  const { u, sent } = rig();
  const a = u.add(new File([new Uint8Array(2.5 * MiB)], 'a.bin')), b = u.add(new File(['hi'], 'b.txt'));
  await until(() => sent.length === 4);
  assert.deepEqual(JSON.parse(sent[0]), { t: 'upload', ref: a.key, name: 'a.bin', size: 2.5 * MiB });
  assert.deepEqual(sent.slice(1).map(d => d.byteLength), [MiB, MiB, MiB / 2]);
  assert.deepEqual([a.state, a.progress, b.state], ['uploading', 1, 'queued'], 'the next waits for the answer');
  u.onFrame({ t: 'uploaded', ref: a.key, id: '0123456789abcdef', name: 'a.bin', size: 2.5 * MiB, kind: 'file', mime: 'application/octet-stream' });
  await until(() => frames(sent).length === 2);
  u.onFrame({ t: 'uploaded', ref: b.key, error: 'upload failed' });
  assert.deepEqual([b.state, b.error, u.pending()], ['failed', 'upload failed', true]);
  u.remove(b.key);
  assert.deepEqual([u.pending(), u.ready()], [false, [{ id: '0123456789abcdef', name: 'a.bin', kind: 'file', size: 2.5 * MiB, mime: 'application/octet-stream' }]]);
});
test('nothing of the next file goes out until the server answered the open one, even an error', async () => {
  const { u, sent } = rig();
  const a = u.add(new File(['aa'], 'a.txt')), b = u.add(new File(['bb'], 'b.txt'));
  await until(() => sent.length === 2);
  await new Promise(r => setTimeout(r, 150));
  assert.equal(sent.length, 2, 'a is unanswered, so b sends neither its frame nor its bytes');
  u.onFrame({ t: 'uploaded', ref: a.key, error: 'one upload at a time' });
  await until(() => sent.length === 4);
  assert.equal(frames(sent)[1].ref, b.key);
  assert.equal(a.error, 'one upload at a time');
});
test('an error reply mid-stream stops the slices and the next file starts', async () => {
  const sent = []; let backed = true;
  const u = new A.Uploads({ send: d => (sent.push(d), true), buffered: () => backed && sent.length >= 2 ? 2 * MiB : 0, onChange: () => {} });  // backed up after the first slice
  const a = u.add(new File([new Uint8Array(5 * MiB)], 'a.bin')), b = u.add(new File(['hi'], 'b.txt'));
  await until(() => sent.length >= 2);
  u.onFrame({ t: 'uploaded', ref: a.key, error: 'upload failed' });
  backed = false;
  await until(() => frames(sent).length === 2);
  assert.deepEqual([a.state, a.error], ['failed', 'upload failed']);
  assert.equal(frames(sent)[1].ref, b.key);
  assert.equal(sent.filter(d => typeof d !== 'string' && d.byteLength === MiB).length, 1, 'only one slice of a went out');
});
test('over 20 MB, an eleventh attachment and empty files are refused on the chip and never sent', async () => {
  const { u, sent } = rig();
  const big = u.add(new File([new Uint8Array(A.LIMIT + 1)], 'big.bin')), empty = u.add(new File([], 'e.txt'));
  for (let i = 0; i < A.MAX; i++) u.add(new File(['x'], `f${i}.txt`));
  const extra = u.add(new File(['x'], 'eleven.txt'));
  assert.deepEqual([big.error, empty.error, extra.error], ['Over 20 MB', 'Empty file', '10 attachments at most']);
  await new Promise(r => setTimeout(r, 50));
  assert.ok(!frames(sent).some(f => ['big.bin', 'e.txt', 'eleven.txt'].includes(f.name)));
});
test('a dropped socket fails what was not finished, Retry sends it again, and restore brings sent ones back', async () => {
  const { u, sent, setOpen } = rig();
  const a = u.add(new File(['abc'], 'a.txt'));
  await until(() => sent.length === 2);
  setOpen(false); u.lost();
  assert.deepEqual([a.state, a.error], ['failed', 'Connection lost']);
  setOpen(true); u.retry(a.key);
  await until(() => frames(sent).length === 2);
  assert.equal(frames(sent)[1].ref, a.key);
  u.clear();
  u.restore([{ id: 'feedfeedfeedfeed', name: 'shot.png', kind: 'image', size: 8, mime: 'image/png' }]);
  assert.deepEqual([u.ready().map(f => f.id), u.pending()], [['feedfeedfeedfeed'], false]);
});
test('a closed socket fails the item as Offline', async () => {
  const { u, setOpen } = rig();
  setOpen(false);
  const a = u.add(new File(['abc'], 'a.txt'));
  await until(() => a.state === 'failed');
  assert.equal(a.error, 'Offline');
});
test('sizes read the way the chips show them', () => {
  assert.deepEqual([900, 1536, 2.5 * MiB, 20 * MiB].map(A.fmtSize), ['900 B', '1.5 KB', '2.5 MB', '20 MB']);
});
