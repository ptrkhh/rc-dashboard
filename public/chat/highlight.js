/* Syntax colours for fenced code. Output goes into innerHTML, so every byte of
   the code is escaped and the only markup is <span class="hl-c|hl-s|hl-n|hl-k">.
   `lang` is only ever a Map key, never part of markup or a pattern. One left to
   right pass with sticky regexes that have no nested quantifiers, so it stays
   linear. Free of DOM; the IIFE form makes it a classic script and a module. */
(function (g) {
  const ESC = { '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' };
  const esc = s => s.replace(/[&<>"']/g, c => ESC[c]);
  const words = s => new Set(s.split(' '));
  const NUM = /0[xX][0-9a-fA-F_]+|\d[\d_]*(?:\.\d[\d_]*)?(?:[eE][+-]?\d+)?/y;
  const WORD = /[A-Za-z_$][\w$]*/y;

  const JS = words('async await break case catch class const continue debugger default delete do else export extends false finally for from function if import in instanceof let new null of return static super switch this throw true try typeof undefined var void while with yield interface type enum implements public private protected readonly as declare namespace abstract');
  const CLIKE = words('abstract auto bool break case catch char class const continue default defer delete do double else enum extends extern false final finally float fn for func go goto if impl implements import in include int interface let long loop match mod move mut namespace new nil null override package private protected pub public readonly ref return select self short signed sizeof static struct super switch template this throw trait true try type typedef union unsafe unsigned use using var virtual void volatile where while yield fun val when object data string byte println echo function foreach array');
  const HASH = words('and as assert async await begin break case class continue def defer do elif else elsif end ensure esac except false fi finally for from function global if import in is lambda let local module next nil none not null or pass raise rescue return select self then throw true try undef unless until when while with yield echo export source yes no on off True False None');
  const JSON_KW = words('true false null');

  const C = { line: '//', block: true, quotes: '"\'', kw: CLIKE };
  const H = { line: '#', block: false, quotes: '"\'', kw: HASH };
  const LANGS = new Map();
  for (const l of 'java c h cpp cc cs go rust rs swift kotlin kt php css'.split(' ')) LANGS.set(l, C);
  for (const l of 'js javascript mjs jsx ts typescript tsx'.split(' ')) LANGS.set(l, { ...C, quotes: '"\'`', kw: JS });
  for (const l of 'py python sh bash shell zsh rb ruby yaml yml toml'.split(' ')) LANGS.set(l, H);
  LANGS.set('json', { line: '', block: false, quotes: '"', kw: JSON_KW });

  const span = (cls, s) => '<span class="hl-' + cls + '">' + esc(s) + '</span>';

  // A string ends at its closing quote or the end of the line.
  function strEnd(s, i, q) {
    for (let j = i + 1; j < s.length; j++) {
      const c = s[j];
      if (c === '\\') { if (s[j + 1] !== '\n') j++; } else if (c === q) return j + 1; else if (c === '\n') return j;
    }
    return s.length;
  }
  const sticky = (re, s, i) => { re.lastIndex = i; const m = re.exec(s); return m ? m[0] : ''; };

  g.cdashHighlight = {
    html(code, lang) {
      code = String(code ?? '');
      const cfg = typeof lang === 'string' ? LANGS.get(lang.toLowerCase()) : undefined;
      if (!cfg) return esc(code);
      let out = '', plain = 0;
      const flush = i => { if (i > plain) out += esc(code.slice(plain, i)); };
      const emit = (i, end, cls) => { flush(i); out += span(cls, code.slice(i, end)); plain = end; return end; };
      for (let i = 0; i < code.length;) {
        const c = code[i];
        if (cfg.line && code.startsWith(cfg.line, i)) {
          const nl = code.indexOf('\n', i);
          i = emit(i, nl < 0 ? code.length : nl, 'c');
        } else if (cfg.block && c === '/' && code[i + 1] === '*') {
          const e = code.indexOf('*/', i + 2);
          i = emit(i, e < 0 ? code.length : e + 2, 'c');
        } else if (cfg.quotes.includes(c)) {
          i = emit(i, strEnd(code, i, c), 's');
        } else if (c >= '0' && c <= '9') {
          i = emit(i, i + sticky(NUM, code, i).length, 'n');
        } else if (/[A-Za-z_$]/.test(c)) {
          const w = sticky(WORD, code, i);
          if (cfg.kw.has(w)) i = emit(i, i + w.length, 'k'); else i += w.length;
        } else i++;
      }
      flush(code.length);
      return out;
    },
  };
})(typeof globalThis === 'undefined' ? this : globalThis);
