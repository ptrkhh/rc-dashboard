/* The chat's sheets: Changes, Usage, the running list and side questions.
   They render into #chat-sheet with textContent only; a side answer is the one
   thing that goes through cdashMd.render. Diff lines, paths, task titles and
   model names come from the session, so they stay text. document is touched
   only inside functions so node --test can import the pure helpers. */
(function (g) {
  const $ = s => document.querySelector(s);
  const el = (...a) => g.cdashCards.el(...a);
  const BIG = 100000;  // Claude side questions above this many context tokens ask first
  const TOO_BIG = 'Over 2 MB of changes: the rest is listed without its lines. Use Terminal to see them.';
  const S = { wired: new WeakSet(), seq: 0, working: false, usageSig: '', aside: null };

  // [cls, line] per patch line. Headers are meta only before a file's first hunk.
  function diffLines(patch) {
    if (typeof patch !== 'string' || !patch) return [];
    let hunk = false;
    return patch.replace(/\n$/, '').split('\n').map(l => {
      if (l.startsWith('diff ')) hunk = false;
      if (l.startsWith('@@')) { hunk = true; return ['hunk', l]; }
      return [!hunk ? 'meta' : l[0] === '+' ? 'add' : l[0] === '-' ? 'del' : 'ctx', l];
    });
  }

  // "45s", "3m 05s", "1h 02m"; '' when the start is missing or unreadable.
  function elapsed(started, now) {
    const t = typeof started === 'number' || typeof started === 'string' ? new Date(started).getTime() : NaN;
    if (!Number.isFinite(t)) return '';
    const s = Math.max(0, Math.floor((now - t) / 1000));
    const p = n => String(n).padStart(2, '0');
    return s < 60 ? s + 's' : s < 3600 ? Math.floor(s / 60) + 'm ' + p(s % 60) + 's' : Math.floor(s / 3600) + 'h ' + p(Math.floor(s % 3600 / 60)) + 'm';
  }

  // Claude re-reads the whole conversation: its size when over BIG, -1 when unknown, else 0.
  function asideConfirm(state) {
    if (state?.tool !== 'claude') return 0;
    const used = state.usage?.context?.used;
    if (typeof used !== 'number') return -1;
    return used > BIG ? used : 0;
  }

  const btw = text => { const m = /^\/btw(?:\s+([\s\S]*))?$/.exec(text); return m ? (m[1] ?? '').trim() : null; };

  const showing = kind => $('#chat-sheet').open && $('#chat-sheet').dataset.kind === kind;

  function sheet(c, kind, title) {
    const dlg = $('#chat-sheet');
    if (!S.wired.has(dlg)) { S.wired.add(dlg); dlg.addEventListener('close', dropAside); }
    dlg.dataset.kind = kind;
    $('#chat-sheet-title').textContent = title;
    const body = $('#chat-sheet-body');
    body.textContent = '';
    if (!dlg.open) dlg.showModal();
    return body;
  }

  const note = text => el('p', 'chat-sheet-note', text);
  const button = (label, onclick, cls) => { const b = el('button', cls || '', label); b.type = 'button'; b.onclick = onclick; return b; };

  /* ---------- Changes ---------- */

  async function openDiff(c) {
    sheet(c, 'diff', 'Changes').append(note('Loading…'));
    S.working = !!c.state.working;
    await loadDiff(c);
  }

  async function loadDiff(c) {
    const my = ++S.seq;
    let d;
    try { d = await c.opts.api('/api/chat/diff?session=' + encodeURIComponent(c.opts.name)); }
    catch (e) { d = { error: e?.message || 'Could not load the changes.' }; }
    if (my !== S.seq || !showing('diff')) return;  // closed, or a newer load is on its way
    const body = $('#chat-sheet-body');
    body.textContent = '';
    if (d?.error) { body.append(note(String(d.error))); return; }
    if (d?.folder) body.append(el('p', 'chat-sheet-folder', String(d.folder)));
    const why = d?.withheld != null ? String(d.withheld) : !d?.repo ? 'Not a git repository.' : !d.files?.length ? 'No changes.' : null;
    if (why) { body.append(note(why)); return; }
    if (d.truncated) body.append(note(TOO_BIG));
    for (const f of d.files) {
      const det = el('details', 'chat-diff-file');
      const sum = el('summary');
      // No counts: binary, or an untracked file past the cap.
      const counts = f.additions != null ? '+' + f.additions + ' −' + f.deletions : d.truncated && f.status === 'untracked' ? '' : 'binary';
      sum.append(el('span', 'chat-diff-path', String(f.path)), el('span', 'chat-diff-count', counts));
      det.append(sum);
      // Lines are built on first open: a 2 MB patch is many nodes.
      if (f.patch) det.addEventListener('toggle', () => {
        if (det.dataset.built) return;
        det.dataset.built = '1';
        const box = el('div', 'chat-diff');
        for (const [cls, text] of diffLines(f.patch)) box.append(el('div', 'diff-' + cls, text));
        det.append(box);
      });
      body.append(det);
    }
  }

  /* ---------- Usage ---------- */

  function openUsage(c) {
    sheet(c, 'usage', 'Usage');
    S.usageSig = '';
    renderUsage(c);
  }

  function renderUsage(c) {
    const u = c.state.usage;
    const tool = c.state.tool;
    const limits = (tool === 'claude' || tool === 'codex') ? (c.opts.limits?.()?.[tool] || []) : [];
    const sig = JSON.stringify([u, limits]);
    if (sig === S.usageSig) return;
    S.usageSig = sig;
    const body = $('#chat-sheet-body');
    body.textContent = '';
    const num = n => Number(n).toLocaleString();
    const section = (title, rows) => {
      const sec = el('section', 'chat-usage');
      sec.append(el('h3', 'chat-usage-title', title));
      for (const [label, value] of rows) {
        const r = el('div', 'chat-usage-row');
        if (label) r.append(el('span', 'chat-usage-label', label));
        r.append(el('span', '', value));
        sec.append(r);
      }
      body.append(sec);
    };
    const ctx = u?.context;
    section('Context', [
      ['', !ctx ? 'Not known yet' : ctx.window ? num(ctx.used) + ' of ' + num(ctx.window) + ' tokens (' + Math.round(ctx.used / ctx.window * 100) + '%)' : num(ctx.used) + ' tokens'],
      ...(u?.model ? [['Model', String(u.model)]] : []),
    ]);
    const t = u?.tokens;
    section('This session', [
      ...(t ? [['Input', num(t.input)], ['Output', num(t.output)], ['Cache read', num(t.cache_read)], ['Cache write', num(t.cache_write)]] : [['', 'Not known yet']]),
      ...(u?.cost != null ? [['Cost', '$' + Number(u.cost).toFixed(2)]] : []),
    ]);
    if (limits.length) section('Plan limits', limits.map(l => ['', l.short + ': ' + l.pct + '% used'
      + (l.resetsAt ? ' · resets ' + new Date(l.resetsAt).toLocaleString([], { month: 'short', day: 'numeric', hour: 'numeric', minute: '2-digit' }) : '')]));
  }

  /* ---------- Running list ---------- */

  function openRunning(c) {
    sheet(c, 'running', 'Running');
    renderRunning(c);
  }

  function renderRunning(c) {
    const body = $('#chat-sheet-body');
    body.textContent = '';
    const tasks = c.state.tasks || [];
    if (!tasks.length) body.append(note('Nothing is running.'));
    const now = Date.now();
    for (const t of tasks) {
      const r = el('div', 'chat-task');
      r.append(el('span', 'chat-task-kind', t.kind === 'agent' ? 'Agent' : 'Shell'), el('span', 'chat-task-title', String(t.title ?? '')), el('span', 'chat-task-time', elapsed(t.started, now)));
      body.append(r);
    }
  }

  /* ---------- Side chat ---------- */

  // Forget the answer in flight; the agent stops it unless it already finished.
  function dropAside() {
    const a = S.aside;
    S.aside = null;
    if (a && !a.done) a.c.send({ t: 'aside_cancel', id: a.id });
  }

  function openAside(c, question) {
    dropAside();
    const body = sheet(c, 'aside', 'Ask aside');
    const box = el('textarea', 'chat-aside-q');
    box.rows = 3;
    box.value = question;
    box.placeholder = 'A question about this session…';
    box.setAttribute('aria-label', 'Side question');
    const bar = el('div', 'chat-rewind-actions');
    const out = el('div', 'chat-aside-out');
    const ask = sure => {
      const q = box.value.trim();
      if (!q || S.aside) return;
      out.textContent = '';
      const n = sure ? 0 : asideConfirm(c.state);
      if (n) {
        out.append(note(n > 0 ? 'This question re-reads about ' + n.toLocaleString() + ' tokens of the conversation. Ask anyway?'
          : 'This question re-reads the whole conversation, and its size is not known yet. Ask anyway?'));
        out.append(button('Ask anyway', () => ask(true), 'chat-rewind-go'));
        return;
      }
      const id = crypto.randomUUID();
      if (!c.send({ t: 'aside', id, text: q })) return;
      box.disabled = true;
      go.hidden = true;
      const answer = el('div', 'chat-text chat-aside-answer');
      const status = el('p', 'chat-sheet-note', 'Answering…');
      out.append(answer, status);
      S.aside = { c, id, text: '', done: false, answer, status };
    };
    const go = button('Ask', () => ask(false), 'chat-rewind-go');
    bar.append(button('Ask another', () => openAside(c, '')), go);
    body.append(box, bar, out);
    if (question) ask(false); else box.focus();
  }

  function onFrame(c, f) {
    const a = S.aside;
    if (f?.t !== 'aside' || !a || a.c !== c || f.id !== a.id) return;
    if (typeof f.text === 'string') a.text += f.text;
    if (f.done) a.done = true;
    a.answer.innerHTML = g.cdashMd.render(a.text);
    a.status.textContent = f.error ? String(f.error) : a.done ? (a.text ? '' : 'No answer.') : 'Answering…';
    a.status.className = f.error ? 'chat-sheet-note error' : 'chat-sheet-note';
  }

  // The socket went: its answer will not arrive.
  function lost(c) {
    if (S.aside?.c === c && !S.aside.done) onFrame(c, { t: 'aside', id: S.aside.id, done: true, error: 'Connection lost. Ask again.' });
  }

  /* ---------- Called on every render and every second ---------- */

  function refresh(c) {
    const n = c.state.tasks?.length || 0;
    const run = $('#chat-running');
    run.hidden = !n;
    if (n && run.textContent !== n + ' running') run.textContent = n + ' running';
    const was = S.working;
    S.working = !!c.state.working;
    if (showing('diff') && was && !S.working) loadDiff(c);  // a turn ended: its edits are in
    else if (showing('usage')) renderUsage(c);
    else if (showing('running')) renderRunning(c);
  }

  g.cdashSheets = { diffLines, elapsed, asideConfirm, btw, openDiff, openUsage, openRunning, openAside, onFrame, lost, refresh };
})(typeof globalThis === 'undefined' ? this : globalThis);
