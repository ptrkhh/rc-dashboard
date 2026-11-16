import test from 'node:test';
import assert from 'node:assert/strict';
await import('../public/chat/highlight.js');
await import('../public/chat/md.js');
const H = globalThis.cdashHighlight;
test('tokens get classes and everything stays escaped', () => {
  const html = H.html('const s = "<script>alert(1)</script>"; // done\nlet n = 42;', 'js');
  assert.ok(!html.includes('<script>'));
  assert.match(html, /<span class="hl-k">const<\/span>/);
  assert.match(html, /<span class="hl-s">&quot;&lt;script&gt;alert\(1\)&lt;\/script&gt;&quot;<\/span>/);
  assert.match(html, /<span class="hl-c">\/\/ done<\/span>/);
  assert.match(html, /<span class="hl-n">42<\/span>/);
});
test('python, rust, go and shell', () => {
  assert.match(H.html('def f(x):  # note\n    return "a"', 'python'), /<span class="hl-k">def<\/span>.*<span class="hl-c"># note<\/span>.*<span class="hl-k">return<\/span>/s);
  assert.match(H.html('fn main() { let x = 0x1f; }', 'rust'), /<span class="hl-k">fn<\/span>.*<span class="hl-n">0x1f<\/span>/s);
  assert.match(H.html('func f() string { return "go" }', 'go'), /<span class="hl-k">func<\/span>/);
  assert.match(H.html('if [ -f x ]; then echo "y"; fi # end', 'bash'), /<span class="hl-k">if<\/span>.*<span class="hl-c"># end<\/span>/s);
  assert.match(H.html('{"a": [1.5e3, true, null]}', 'json'), /<span class="hl-s">&quot;a&quot;<\/span>.*<span class="hl-n">1\.5e3<\/span>.*<span class="hl-k">true<\/span>.*<span class="hl-k">null<\/span>/s);
});
test('unknown languages are only escaped, and markdown code blocks use the highlighter', () => {
  assert.equal(H.html('<b>&', 'cobol'), '&lt;b&gt;&amp;');
  assert.equal(H.html('<b>&', undefined), '&lt;b&gt;&amp;');
  assert.match(globalThis.cdashMd.render('```js\nconst a = "<b>";\n```'), /<code class="lang-js"><span class="hl-k">const<\/span> a = <span class="hl-s">&quot;&lt;b&gt;&quot;<\/span>;/);
});
test('unterminated strings and comments neither swallow the rest wrongly nor hang', () => {
  assert.match(H.html('x = "open\ny = 1', 'python'), /<span class="hl-n">1<\/span>/, 'a string stops at the line end');
  const t = Date.now(); H.html('/*'.repeat(20000) + '"'.repeat(20000), 'js'); assert.ok(Date.now() - t < 1000);
});
// Only escaped text and the four span classes may ever come out.
const strip = h => h.replace(/<span class="hl-[csnk]">/g, '').replace(/<\/span>/g, '');
const clean = h => assert.ok(!/[<>"']/.test(strip(h)), h.slice(0, 200));
test('hostile code never produces other markup', () => {
  const evil = ['</span><img src=x onerror=alert(1)>', '"</span><img onerror=x>', "'\"&<>`", '// </span><b>\n/* <i> ', '`<u>', '0x<a>', '<script>'];
  for (const lang of ['js', 'ts', 'python', 'bash', 'rust', 'json', 'css', 'yaml', 'go', 'cobol'])
    for (const e of evil) { clean(H.html(e, lang)); clean(H.html('x = ' + e + '\n' + e, lang)); }
});
test('lang is untrusted and never reaches markup', () => {
  for (const lang of ['"><img onerror=1>', '__proto__', 'constructor', 'toString', '', null, {}, 42, 'JS<'])
    assert.equal(H.html('<b>&', lang), '&lt;b&gt;&amp;');
  assert.match(H.html('const a', 'JS'), /hl-k/, 'case-insensitive');
});
test('a huge input stays linear', () => {
  const big = 'const a = "x\\"y"; // c\n/* b */ 0x1f 1_000.5e+3 '.repeat(50000);
  const t = Date.now(); const out = H.html(big, 'js'); assert.ok(Date.now() - t < 2000); clean(out);
  for (const s of ['"\\'.repeat(100000), '1'.repeat(200000), '9.'.repeat(100000), 'a'.repeat(200000), '/'.repeat(200000), '#'.repeat(100000)]) {
    const t2 = Date.now(); H.html(s, 'js'); H.html(s, 'python'); assert.ok(Date.now() - t2 < 1500);
  }
});
