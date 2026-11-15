/* The chat screen: one WebSocket to the agent, frames through cdashChatStore,
   rows to DOM by key. Conversation text reaches markup only through
   cdashMd.render; everything else is textContent. Touches the DOM only inside
   functions, so node --test can import it for its pure helpers. */
(function (g) {
  const $ = s => document.querySelector(s);
  const RETRY = [1000, 2000, 5000, 10000];
  const NEAR = 80; // px from the bottom that still counts as following
  let c = null;    // the open chat
  let wired = false;

  /* What the composer offers: 'send' while idle, 'stop' while a turn runs or
     a message is still on its way, 'off' when typing would go nowhere useful. */
  function composerMode(state, hasPending) {
    if (state.waiting || state.ended || state.unreadable) return 'off';
    return state.working || hasPending ? 'stop' : 'send';
  }
  // Stop while a turn runs and nothing would go out: an empty box, and no chip but failed ones.
  const stopOffered = (mode, text, u) => mode === 'stop' && !text && u.items.every(i => i.error);
  const pending = s => s.mine.some(m => m.seq == null && (m.status === 'sending' || m.status === 'sent'));

  function el(tag, cls, text) {
    const n = document.createElement(tag);
    if (cls) n.className = cls;
    if (text != null) n.textContent = text;
    return n;
  }

  function wireOnce() {
    if (wired) return;
    wired = true;
    const log = $('#chat-log');
    log.addEventListener('scroll', () => {
      if (!c) return;
      c.follow = log.scrollHeight - log.scrollTop - log.clientHeight < NEAR;
      $('#chat-jump').hidden = c.follow;
      askPage(log);
    });
    $('#chat-jump').addEventListener('click', () => {
      if (!c) return;
      c.follow = true;
      log.scrollTop = log.scrollHeight;
      $('#chat-jump').hidden = true;
    });
    log.addEventListener('click', e => {
      const b = e.target.closest('[data-tools]');
      if (b && c) showTools(b.dataset.tools);
      const v = e.target.closest('[data-view]');
      if (v && c) cdashAttach.view(v.dataset.view);
    });
    log.addEventListener('click', e => {
      if (!c) return;
      const t = e.target;
      const code = t.closest('[data-copy-code]');
      if (code) { c.opts.copy(code.previousSibling.textContent); return; }
      const think = t.closest('[data-think]');
      if (think) { const k = think.dataset.think; if (!c.opened.delete(k)) c.opened.add(k); schedule(); return; }
      const undo = t.closest('[data-undo]');
      if (undo) { if (!c.rewinding && sendFrame({ t: 'rewind_undo' })) c.rewinding = undo; return; }
      const rowEl = t.closest('[data-row]');
      const row = rowEl && (c.rows || []).find(r => r.key === rowEl.dataset.row);
      if (!row) return;
      const act = t.closest('[data-act]')?.dataset.act;
      if (act === 'copy') c.opts.copy(row.text);
      else if (act === 'rewind') { if (cdashChatStore.canRewind(c.state, row)) showRewind(row); }
      else if (!t.closest('button, a, input') && !getSelection().toString()) toggleActions(row.key);
    });
    log.addEventListener('keydown', e => {
      if (!c || (e.key !== 'Enter' && e.key !== ' ') || !e.target.dataset?.row) return;
      e.preventDefault();
      toggleActions(e.target.dataset.row);
    });
    cdashMenu.wire(sendFrame, () => c?.state ?? null);
    $('#chat-sheet-close').addEventListener('click', () => $('#chat-sheet').close());
    $('#chat-changes').addEventListener('click', () => { if (c) cdashSheets.openDiff(c); });
    $('#chat-usage').addEventListener('click', () => { if (c) cdashSheets.openUsage(c); });
    $('#chat-running').addEventListener('click', () => { if (c) cdashSheets.openRunning(c); });
    log.addEventListener('click', e => {
      const b = e.target.closest('[data-edit],[data-again],[data-dismiss]');
      if (!b || !c) return;
      const cid = b.dataset.edit ?? b.dataset.again ?? b.dataset.dismiss;
      const m = c.state.mine.find(x => x.cid === cid);
      if (!m) return;
      if (b.dataset.again !== undefined) resend(c.state, cid, submitText);
      else {
        cdashChatStore.dropMine(c.state, cid);
        if (b.dataset.edit !== undefined) { toBox(m.text); c.uploads.restore(m.files); }
      }
      schedule();
    });
    $('#chat-held').addEventListener('click', e => {
      const b = e.target.closest('[data-op]');
      if (!b || !c) return;
      const h = c.state.held.find(x => x.id === b.dataset.id);
      if (!h || !sendFrame({ t: 'held', op: b.dataset.op === 'now' ? 'now' : 'remove', id: h.id })) return;
      if (b.dataset.op === 'edit') { toBox(h.text); c.uploads.restore(h.files); }
    });
    $('#chat-banner').addEventListener('click', e => {
      if (e.target.closest('[data-open-term]') && c) c.opts.openTerm(c.opts.name, c.opts.title);
    });
    const input = $('#chat-input');
    $('#chat-form').addEventListener('submit', e => {
      e.preventDefault();
      if (!c) return;
      const text = input.value.trim();
      // A side question: answered in its sheet, never sent to the session.
      const q = cdashSheets.btw(text);
      if (q !== null) {
        input.value = '';
        saveDraft();
        grow();
        renderComposer();
        cdashSheets.openAside(c, q);
        return;
      }
      const mode = composerMode(c.state, pending(c.state));
      const u = c.uploads;
      if (mode === 'off') return;
      if (stopOffered(mode, text, u)) { sendFrame({ t: 'stop' }); return; }
      const ready = u.ready();
      if (u.pending() || (!text && !ready.length)) return;
      if (submitText(text, ready)) {
        input.value = '';
        saveDraft();
        c.hint = '';
        // remove(), not clear(): an upload removed mid-flight still holds the queue until the agent answers it.
        for (const i of [...u.items]) u.remove(i.key);
        grow();
        renderComposer();
      }
    });
    input.addEventListener('keydown', e => {
      // Enter inserts a newline on touch keyboards; a hardware keyboard sends.
      if (e.key === 'Enter' && !e.shiftKey && !e.isComposing && matchMedia('(pointer: fine)').matches) {
        e.preventDefault();
        $('#chat-form').requestSubmit();
      }
    });
    input.addEventListener('input', () => { grow(); if (c) { saveDraft(); renderComposer(); } });
    cdashAttach.wire(() => c?.uploads ?? null, schedule);
    document.addEventListener('visibilitychange', () => {
      if (!document.hidden && c && !c.ws && !c.done) { clearTimeout(c.retryTimer); connect(); }
    });
    addEventListener('online', () => { if (c && !c.ws && !c.done) { clearTimeout(c.retryTimer); connect(); } });
    addEventListener('offline', () => { if (c) { c.link = 'Offline'; schedule(); } });
  }

  function open(opts) {
    if (c) close();
    wireOnce();
    c = {
      opts, state: cdashChatStore.initial(), ws: null, nodes: new Map(), raf: 0,
      attempt: 0, retryTimer: 0, done: false, follow: true, paging: false, paged: false,
      link: 'Connecting…', since: 0, clock: 0,
      acted: null, opened: new Set(), todoOpen: false, todoSig: null, hint: '',  // row with its action line, open Thinking rows
      rewinding: null,  // the button whose rewind or undo awaits its `rewound`
    };
    $('#chat-title').textContent = opts.title || opts.name;
    const rc = $('#chat-rc');
    rc.hidden = !opts.rcLink;
    if (opts.rcLink) rc.href = opts.rcLink;
    $('#chat-term').onclick = () => opts.openTerm(opts.name, opts.title);
    $('#chat-log').textContent = '';
    $('#chat-jump').hidden = true;
    $('#chat').hidden = false;
    $('#chat-input').value = opts.store.get(draftKey()) || '';
    grow();
    c.clock = setInterval(() => { if (!c) return; if (c.state.working) renderStatus(); cdashSheets.refresh(c); }, 1000);
    const x = c;
    x.send = f => c === x && sendFrame(f);  // for the sheets: never onto a later chat's socket
    // Read the socket per call: a reconnect replaces x.ws.
    x.uploads = new cdashAttach.Uploads({
      send: d => { if (x.ws?.readyState !== WebSocket.OPEN) return false; x.ws.send(d); return true; },
      buffered: () => x.ws?.bufferedAmount ?? 0,
      onChange: () => { if (c === x) schedule(); },
    });
    x.unwatch = watchFocus(on => { if (x.ws?.readyState === 1) x.ws.send(JSON.stringify({ t: 'focus', on })); });
    schedule();
    connect();
  }

  function close() {
    if (!c) return;
    const x = c;
    c = null;
    clearTimeout(x.retryTimer);
    clearInterval(x.clock);
    x.unwatch?.();
    x.uploads.clear();
    if (x.raf) cancelAnimationFrame(x.raf);
    x.ws?.close();
    $('#chat').hidden = true;
    $('#chat-log').textContent = '';
    $('#chat-held').textContent = '';
    $('#chat-cards').textContent = '';
    if ($('#chat-sheet').open) $('#chat-sheet').close();
    if ($('#chat-viewer').open) $('#chat-viewer').close();
    cdashAttach.renderChips(null);
    cdashMenu.reset();
  }

  async function connect() {
    const x = c;
    const s = x.state;
    const q = '/api/chat?name=' + encodeURIComponent(x.opts.name)
      + (s.epoch ? '&after=' + s.head + '&epoch=' + encodeURIComponent(s.epoch) : '');
    let url;
    try { url = await x.opts.wsUrl(q); }
    catch { if (c === x) retry(x); return; }
    if (c !== x || x.ws) return; // closed, or another attempt won, while resolving the URL
    x.base = httpBase(url);
    const ws = new WebSocket(url);
    x.ws = ws;
    ws.onmessage = e => {
      if (c !== x || x.ws !== ws) return;
      let f;
      try { f = JSON.parse(e.data); } catch { return; }
      if (f.t === 'uploaded') { x.uploads.onFrame(f); return; }
      if (f.t === 'rewound') { rewound(f); return; }
      if (f.t === 'aside') { cdashSheets.onFrame(x, f); return; }
      if (f.t === 'hello') { x.attempt = 0; x.link = ''; ws.send(JSON.stringify({ t: 'focus', on: focusNow() })); }
      if (f.t === 'page') { x.paging = false; x.paged = true; }
      const wasWorking = x.state.working;
      const wasWaiting = x.state.waiting;
      const prevAsks = x.state.asks.length;
      cdashChatStore.apply(x.state, f);
      if (x.state.working && !wasWorking) x.since = Date.now();
      const hadAsks = prevAsks > 0 && x.state.asks.length === 0;
      if (x.state.waiting && (!wasWaiting || hadAsks)) { x.waitSince = Date.now(); setTimeout(() => { if (c === x) schedule(); }, 550); } // the banner waits 500 ms for an ask
      schedule();
    };
    ws.onclose = e => {
      if (c !== x || x.ws !== ws) return; // the UI closed it, or a newer socket replaced it
      x.ws = null;
      x.paging = false;
      x.uploads.lost();
      if (x.rewinding) { x.rewinding.disabled = false; x.rewinding = null; }  // the reply went with the socket
      cdashSheets.lost(x);
      if (e.code === 4000) { x.done = true; x.link = ''; schedule(); return; }
      if (e.code === 4002) { connect(); return; } // fell behind: catch up from the last position now
      retry(x);
    };
  }

  // Older history is wanted at the top, or whenever the list is too short to scroll.
  function needPage(more, paging, top, height, view) {
    return more && !paging && (top < 40 || height <= view);
  }

  function askPage(log) {
    if (!needPage(c.state.more, c.paging, log.scrollTop, log.scrollHeight, log.clientHeight) || c.ws?.readyState !== WebSocket.OPEN) return;
    c.paging = true;
    c.ws.send(JSON.stringify({ t: 'page', before: c.state.first }));
  }

  // Thumbnails load over http(s) from the agent the socket reached: in the apps that is not the page's origin.
  // The socket URL minus its /api/chat part, so a profile's path prefix stays.
  const httpBase = url => url.slice(0, url.indexOf('/api/chat')).replace(/^ws/, 'http');

  // Send again: the row goes only once the text has been sent, so offline it stays.
  function resend(state, cid, send) {
    const m = state.mine.find(x => x.cid === cid);
    if (!m || !send(m.text, m.files ?? [])) return false;
    cdashChatStore.dropMine(state, cid);
    return true;
  }

  function sendFrame(f) {
    if (c?.ws?.readyState !== WebSocket.OPEN) { c?.opts.toast?.('Offline'); return false; }
    c.ws.send(JSON.stringify(f));
    return true;
  }

  function submitText(text, files = []) {
    const cid = crypto.randomUUID();
    if (!sendFrame({ t: 'send', cid, text, files: files.map(f => f.id) })) return false;
    cdashChatStore.addMine(c.state, cid, text, files);
    c.follow = true;
    schedule();
    return true;
  }

  function toBox(text) {
    const input = $('#chat-input');
    input.value = input.value ? input.value + '\n' + text : text;
    saveDraft();
    grow();
    input.focus();
    renderComposer();
  }

  // ponytail: keyed by tmux name and never expire; a later session reusing the name finds the old draft. Expire by age if that shows.
  const draftKey = () => 'cdash-draft:' + c.opts.name;
  // Attachments are not kept: only the text.
  function saveDraft() {
    const v = $('#chat-input').value;
    if (v.trim()) c.opts.store.set(draftKey(), v); else c.opts.store.remove(draftKey());
  }

  // The answer to a rewind or an undo. It comes before the cut's reload, which leaves the box alone.
  function rewound(f) {
    const sheet = $('#chat-sheet');
    const by = c.rewinding;
    c.rewinding = null;
    if (!f.ok) {
      c.opts.toast?.(f.error || 'Rewind failed');
      if (by) by.disabled = false;
      return;
    }
    if (sheet.open && sheet.dataset.kind === 'rewind') sheet.close();
    if (f.text) toBox(f.text);
    const n = f.attachments || 0;
    c.hint = f.uuid && n > 0 ? 'This prompt had ' + n + (n === 1 ? ' attachment' : ' attachments') + '; attach them again' : '';
    schedule();
  }

  function showRewind(row) {
    const sheet = $('#chat-sheet');
    if (sheet.open || c.rewinding) return;
    sheet.dataset.kind = 'rewind';
    $('#chat-sheet-title').textContent = 'Rewind to here?';
    const body = $('#chat-sheet-body');
    body.textContent = '';
    body.append(el('p', 'chat-rewind-text', 'The conversation goes back to before this message. Its text returns to the composer.'));
    let files = null;
    if (c.state.tool === 'codex') body.append(el('p', 'chat-rewind-text', 'Files stay as they are.'));
    else {
      const label = el('label', 'chat-rewind-files');
      files = el('input');
      files.type = 'checkbox';
      label.append(files, ' Also restore files');
      body.append(label);
    }
    const bar = el('div', 'chat-rewind-actions');
    const cancel = el('button', '', 'Cancel');
    const go = el('button', 'chat-rewind-go', 'Rewind');
    cancel.type = go.type = 'button';
    cancel.onclick = () => sheet.close();
    go.onclick = () => { if (!c.rewinding && sendFrame({ t: 'rewind', uuid: row.uuid, files: !!files?.checked })) { go.disabled = true; c.rewinding = go; } };
    bar.append(cancel, go);
    body.append(bar);
    sheet.showModal();
  }

  function grow() {
    const input = $('#chat-input');
    input.style.height = 'auto';
    input.style.height = Math.min(input.scrollHeight, innerHeight * 0.4) + 'px';
  }

  function retry(x) {
    x.link = navigator.onLine === false ? 'Offline' : 'Reconnecting…';
    schedule();
    x.retryTimer = setTimeout(() => { if (c === x && !x.ws) connect(); }, RETRY[Math.min(x.attempt++, RETRY.length - 1)]);
  }

  function schedule() {
    if (c && !c.raf) c.raf = requestAnimationFrame(render);
  }

  const STATUS = { sending: 'Sending…', queued: 'Waiting for Claude', held: 'Held', unconfirmed: 'Not confirmed', returned: 'Not sent', refused: 'Not sent' };

  function build(r) {
    if (r.kind === 'text') {
      const n = el('div', 'chat-text' + (r.streaming ? ' streaming' : ''));
      n.innerHTML = cdashMd.render(r.text);
      return codeCopy(n);
    }
    if (r.kind === 'thinking') {
      const n = el('div', 'chat-thinking');
      const open = c.opened.has(r.key);
      const b = el('button', 'chat-thinking-btn', 'Thinking');
      b.type = 'button';
      b.dataset.think = r.key;
      b.setAttribute('aria-expanded', open);
      n.append(b);
      if (open) {
        const body = el('div', 'chat-text chat-thinking-body');
        body.innerHTML = cdashMd.render(r.text);
        n.append(codeCopy(body));
      }
      return n;
    }
    if (r.kind === 'day') return el('div', 'chat-note chat-day', new Date(r.ts).toLocaleDateString(undefined, { weekday: 'short', day: 'numeric', month: 'short' }));
    if (r.kind === 'staged') {
      const n = el('div', 'chat-note chat-staged');
      const undo = el('button', '', 'Undo');
      undo.type = 'button';
      undo.dataset.undo = '';
      n.append(el('span', '', r.count + (r.count === 1 ? ' message' : ' messages') + ' rewound'), undo);
      return n;
    }
    if (r.kind === 'user') {
      const n = el('div', 'chat-user chat-' + r.status);
      if (r.files?.length) n.append(files(r.files));
      if (r.text || !r.files?.length) n.append(el('div', 'chat-bubble', r.text));
      if (r.status !== 'sent') {
        const meta = el('div', 'chat-meta', STATUS[r.status] + (r.error ? ': ' + r.error : ''));
        if (r.cid && ['unconfirmed', 'returned', 'refused'].includes(r.status)) {
          // Nothing is ever resent on its own: the owner decides.
          const edit = el('button', '', 'Edit');
          edit.type = 'button';
          edit.dataset.edit = r.cid;
          meta.append(edit);
          if (r.status === 'unconfirmed') {
            const again = el('button', '', 'Send again');
            again.type = 'button';
            again.dataset.again = r.cid;
            meta.append(again);
          }
          const dismiss = el('button', '', 'Dismiss');
          dismiss.type = 'button';
          dismiss.dataset.dismiss = r.cid;
          meta.append(dismiss);
        }
        n.append(meta);
      }
      return n;
    }
    if (r.kind === 'tools') {
      const failed = r.calls.some(call => call.error);
      const b = el('button', 'chat-tool' + (r.running ? ' running' : failed ? ' failed' : ''));
      b.type = 'button';
      b.dataset.tools = r.key;
      b.append(el('span', r.running ? 'chat-spinner' : 'chat-tool-dot'), labelled(el('span', 'chat-tool-label'), r.label));
      return b;
    }
    if (r.kind === 'shell') {
      const n = el('div', 'chat-shell');
      n.append(el('pre', 'chat-shell-cmd', '! ' + r.command));
      const out = [r.stdout, r.stderr].filter(Boolean).join('\n');
      if (out) n.append(el('pre', 'chat-shell-out', out));
      return n;
    }
    return el('div', 'chat-note', r.text);
  }

  // Images with a ref as thumbnails, then chips; my own not yet shown images have no ref and show as chips.
  function files(fs) {
    const box = el('div', 'chat-files');
    const thumbs = fs.filter(f => f.kind === 'image' && f.ref);
    for (const f of thumbs) {
      const src = c.base + '/api/chat/media/' + f.ref; // the store let through only 64-hex refs
      const b = el('button', 'chat-thumb');
      b.type = 'button';
      b.dataset.view = src;
      b.setAttribute('aria-label', 'View image');
      const img = el('img');
      Object.assign(img, { loading: 'lazy', decoding: 'async', alt: 'Image', src });
      b.append(img);
      box.append(b);
    }
    for (const f of fs) {
      if (thumbs.includes(f)) continue;
      const chip = el('div', 'chat-file');
      chip.append(el('span', 'chat-file-name', f.name ?? 'Image'));
      if (typeof f.size === 'number') chip.append(el('span', 'chat-file-size', cdashAttach.fmtSize(f.size)));
      box.append(chip);
    }
    return box;
  }

  // A Copy button beside each code block, outside the pre so it stays put while the code scrolls.
  function codeCopy(n) {
    for (const pre of n.querySelectorAll('pre')) {
      const box = el('div', 'chat-code');
      const b = el('button', 'chat-code-copy', 'Copy');
      b.type = 'button';
      b.dataset.copyCode = '';
      pre.replaceWith(box);
      box.append(pre, b);
    }
    return n;
  }

  // Under a tapped message: its time, Copy, and Rewind to here where the store allows it.
  function actions(r) {
    const a = el('div', 'chat-actions');
    if (r.ts) a.append(el('span', 'chat-time', new Date(r.ts).toLocaleTimeString()));
    const add = (label, act) => { const b = el('button', '', label); b.type = 'button'; b.dataset.act = act; a.append(b); };
    add('Copy', 'copy');
    if (cdashChatStore.canRewind(c.state, r)) add('Rewind to here', 'rewind');
    return a;
  }

  // "Ran `npm test`": the command shows as code, without the backticks.
  function labelled(node, text) {
    const m = /^(.*?)`(.*)`$/s.exec(text);
    if (m) node.append(m[1], el('code', '', m[2])); else node.textContent = text;
    return node;
  }

  function render() {
    const x = c;
    if (!x) return;
    x.raf = 0;
    const log = $('#chat-log');
    const height = log.scrollHeight;
    const top = log.scrollTop;
    // Held messages live in the list above the composer, with their controls.
    const rows = cdashChatStore.rows(x.state).filter(r => !(r.kind === 'user' && r.status === 'held'));
    x.rows = rows;
    const keep = new Set();
    let prev = null;
    for (const r of rows) {
      keep.add(r.key);
      const acted = x.acted === r.key;
      const sig = r.sig + (acted ? '|a' + cdashChatStore.canRewind(x.state, r) : '') + (x.opened.has(r.key) ? '|o' : '');
      let n = x.nodes.get(r.key);
      if (!n) {
        n = { el: decorate(r, acted), sig };
        x.nodes.set(r.key, n);
      } else if (n.sig !== sig) {
        const fresh = decorate(r, acted);
        const focused = document.activeElement === n.el;
        n.el.replaceWith(fresh);
        if (focused) fresh.focus({ preventScroll: true });  // Enter or Space on a message keeps the keyboard there
        Object.assign(n, { el: fresh, sig });
      }
      const want = prev ? prev.nextSibling : log.firstChild;
      if (n.el !== want) log.insertBefore(n.el, want);
      prev = n.el;
    }
    for (const [k, n] of x.nodes) if (!keep.has(k)) { n.el.remove(); x.nodes.delete(k); }
    // A cut reuses seqs, so a key that left must not come back opened.
    if (!keep.has(x.acted)) x.acted = null;
    for (const k of x.opened) if (!keep.has(k)) x.opened.delete(k);
    renderStatus();
    renderBanner();
    renderTodo();
    cdashSheets.refresh(x);
    const hint = $('#chat-hint');
    hint.textContent = x.hint;
    hint.hidden = !x.hint;
    cdashCards.render($('#chat-cards'), x.state, sendFrame);
    cdashMenu.render(x.state);
    renderHeld();
    renderComposer();
    // Last: the held list and composer above change how tall the log is.
    if (x.paged) {
      x.paged = false;
      log.scrollTop = top + (log.scrollHeight - height); // older rows went on top; keep the view still
    } else if (x.follow) {
      log.scrollTop = log.scrollHeight;
    }
    $('#chat-jump').hidden = x.follow;
    askPage(log);
  }

  function toggleActions(key) {
    c.acted = c.acted === key ? null : key;
    schedule();
  }

  function decorate(r, acted) {
    const n = build(r);
    if (!['user', 'text', 'thinking'].includes(r.kind)) return n;
    n.dataset.row = r.key;
    n.tabIndex = 0;
    if (acted) n.append(actions(r));
    return n;
  }

  const MARK = { completed: '\u2713', in_progress: '\u2022' };
  function renderTodo() {
    const box = $('#chat-todo');
    const t = cdashChatStore.todo(c.state);
    const sig = t ? JSON.stringify(t.items) + c.todoOpen : '';
    if (c.todoSig === sig) return;
    c.todoSig = sig;
    box.textContent = '';
    box.hidden = !t;
    if (!t) return;
    const b = el('button', 'chat-todo-sum', t.done + ' of ' + t.total + ' done');
    b.type = 'button';
    b.setAttribute('aria-expanded', c.todoOpen);
    b.onclick = () => { c.todoOpen = !c.todoOpen; renderTodo(); };
    box.append(b);
    if (!c.todoOpen) return;
    const list = el('ul', 'chat-todo-list');
    for (const i of t.items) {
      const li = el('li', i.status === 'completed' ? 'done' : '');
      li.append(el('span', 'chat-todo-mark', Object.hasOwn(MARK, i.status) ? MARK[i.status] : '\u25cb'), el('span', '', i.text));
      list.append(li);
    }
    box.append(list);
  }

  function renderHeld() {
    const box = $('#chat-held');
    const off = c.done || composerMode(c.state, false) === 'off';
    box.textContent = '';
    if (c.state.held.length) box.append(el('div', 'chat-held-title', 'Held'));
    for (const h of c.state.held) {
      const item = el('div', 'chat-held-item');
      item.append(el('span', 'chat-held-text', h.text));
      for (const [op, label] of [['now', 'Send now'], ['edit', 'Edit'], ['remove', 'Remove']]) {
        const b = el('button', '', label);
        b.type = 'button';
        b.dataset.op = op;
        b.dataset.id = h.id;
        b.disabled = op === 'now' && off; // never typed into a dialog or a session chat cannot read
        item.append(b);
      }
      box.append(item);
    }
  }

  function renderComposer() {
    const s = c.state;
    const input = $('#chat-input');
    const go = $('#chat-go');
    const mode = c.done ? 'off' : composerMode(s, pending(s));
    const u = c.uploads;
    const stop = stopOffered(mode, input.value.trim(), u);
    input.disabled = $('#chat-attach').disabled = mode === 'off';
    go.disabled = mode === 'off' || (!stop && u.pending());
    input.placeholder = s.ended || c.done ? 'Session ended'
      : s.unreadable ? 'Use Terminal for this session'
      : s.waiting ? (s.asks.length ? 'Answer the card first' : 'Answer in Terminal first')
      : s.working ? 'Queue a message…' : 'Message…';
    go.textContent = stop ? 'Stop'
      : u.items.some(i => i.state === 'uploading' || i.state === 'queued') ? 'Uploading…'
      : u.items.some(i => i.error) ? 'Remove failed files' : 'Send';
    go.classList.toggle('stop', stop);
    cdashAttach.renderChips(u);
  }

  function renderStatus() {
    const s = c.state;
    const state = $('#chat-state');
    state.textContent = s.ended || c.done ? 'Ended'
      : s.waiting ? 'Needs an answer'
      : s.working ? 'Working… ' + Math.max(0, Math.round((Date.now() - c.since) / 1000)) + 's'
      : '';
    state.classList.toggle('working', s.working && !s.waiting && !s.ended && !c.done);
  }

  function renderBanner() {
    const b = $('#chat-banner');
    const s = c.state;
    const [text, term] = c.link ? [c.link, false]
      : s.ended || c.done ? ['Session ended.', false]
      : s.unreadable ? ['Chat can’t read this session.', true]
      : s.waiting && cdashChatStore.needsTerminal(s, Date.now() - (c.waitSince || 0))
        ? ['Answer in Terminal to continue.' + (s.hooks ? '' : ' Restart the session to answer from chat.'), true]
      : ['', false];
    const sig = text + term;
    if (b.dataset.sig === sig) return;
    b.dataset.sig = sig;
    b.textContent = '';
    b.hidden = !text;
    if (!text) return;
    b.append(el('span', '', text));
    if (term) {
      const open = el('button', 'chat-bar-btn', 'Open Terminal');
      open.type = 'button';
      open.dataset.openTerm = '';
      b.append(open);
    }
  }

  function showTools(key) {
    const row = (c.rows || []).find(r => r.key === key);
    if (!row) return;
    $('#chat-sheet').dataset.kind = 'tools';
    const body = $('#chat-sheet-body');
    body.textContent = '';
    const title = $('#chat-sheet-title');
    title.textContent = '';
    labelled(title, row.label);
    for (const call of row.calls) {
      const sec = el('section', 'chat-call');
      const input = call.input || {};
      // The command or the path when that is all there is to see; JSON otherwise.
      const shown = typeof input.command === 'string' ? input.command
        : typeof input.file_path === 'string' && Object.keys(input).length === 1 ? input.file_path
        : JSON.stringify(input, null, 2);
      sec.append(labelled(el('h3', 'chat-call-name'), cdashChatStore.toolLabel(call.name, input, call.done)));
      const d = cdashChatStore.editDiff(call);
      if (d) sec.append(editLines(d));
      else sec.append(el('pre', 'chat-call-input', shown));
      if (call.done) sec.append(el('pre', 'chat-call-result' + (call.error ? ' error' : ''), call.result || '(no output)'));
      else sec.append(el('p', 'chat-call-wait', 'Running…'));
      body.append(sec);
    }
    $('#chat-sheet').showModal();
  }

  // An edit as a diff: per file "path +A −D", then its lines as text.
  const DIFF_CLS = { '+': 'diff-add', '-': 'diff-del' };
  function editLines(d) {
    const box = el('div', 'chat-edit');
    for (const f of d.files) {
      box.append(el('div', 'chat-diff-head', (f.path ?? '') + ' +' + (f.add ?? 0) + ' \u2212' + (f.del ?? 0)));
      const lines = el('div', 'chat-diff');
      for (const l of f.lines) lines.append(el('div', DIFF_CLS[l.t] ?? 'diff-ctx', l.t + l.s));
      box.append(lines);
    }
    return box;
  }

  // Focus: a page counts only while visible, focused and used in the last two minutes.
  const FOCUS_IDLE = 120000;
  const focusActive = ({ visible, focused, lastInput, now }) => visible && focused && now - lastInput < FOCUS_IDLE;
  const fw = { subs: new Set(), last: 0, on: false, timer: 0, started: false };
  const focusNow = () => focusActive({
    visible: !document.hidden, focused: document.hasFocus(), lastInput: fw.last, now: Date.now(),
  });
  function focusCheck() {
    clearTimeout(fw.timer);
    const on = focusNow();
    if (on) fw.timer = setTimeout(focusCheck, fw.last + FOCUS_IDLE - Date.now() + 5);
    if (on !== fw.on) { fw.on = on; for (const fn of fw.subs) fn(on); }
  }
  function watchFocus(fn) {
    if (!fw.started) {
      fw.started = true;
      fw.last = Date.now();
      document.addEventListener('visibilitychange', focusCheck);
      addEventListener('focus', focusCheck);
      addEventListener('blur', focusCheck);
      for (const t of ['keydown', 'pointerdown', 'touchstart'])
        document.addEventListener(t, () => { fw.last = Date.now(); focusCheck(); }, { capture: true, passive: true });
    }
    focusCheck(); // arms the idle timer and settles fw.on before fn subscribes
    fw.subs.add(fn);
    return () => fw.subs.delete(fn);
  }

  g.cdashChat = {
    focusActive,
    watchFocus,
    focusNow,
    composerMode,
    needPage,
    resend,
    httpBase,
    open,
    close,
    isOpen: () => !!c,
    name: () => c?.opts.name ?? null,
  };
})(typeof globalThis === 'undefined' ? this : globalThis);
