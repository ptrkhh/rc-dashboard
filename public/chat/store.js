/* Chat frames to view rows. Pure: no DOM, no socket, so node --test can drive
   it frame by frame. Events are kept by position and rows are rebuilt on
   demand; the view diffs rows by key. Same IIFE shape as term/keys.js. */
(function (g) {
  const initial = () => ({
    epoch: null, tool: null, head: 0, first: 1, more: false,
    working: false, waiting: false, ended: false, unreadable: false, held: [],
    events: new Map(),   // seq -> event
    streams: new Map(),  // message id -> text so far (deltas, never stored server-side)
    mine: [],            // {cid, text, status, error, seq}; seq set once an event showed it
    rev: 0,              // bumped by whatever rows() reads; rows() is reused while it holds still
    forgetHeld: false,   // after a reset, the next state frame says which held ones survived
    asks: [], settings: {}, hooks: true, waiting_for: null,
    menu: null,          // {commands, models, efforts, agents} once the menu frame arrived
    answered: new Map(), // ask id -> {ok, error}; forgotten once the ask leaves `asks`
    setReply: null,      // {ok, error} of the last settings change
    usage: null, tasks: [], stateTodo: null,  // from the state frame; stateTodo is the hub's checklist
  });

  const isPrompt = ev => ev.type === 'user' && !(Array.isArray(ev.message?.content) && ev.message.content.some(b => b?.type === 'tool_result'));
  const promptText = ev => {
    const c = ev.message?.content;
    return typeof c === 'string' ? c : Array.isArray(c) ? c.filter(b => b?.type === 'text').map(b => b.text).join('') : '';
  };
  const HEX64 = /^[0-9a-f]{64}$/;
  const keepFiles = fs => (Array.isArray(fs) ? fs : []).filter(f => f && (f.kind === 'image' ? typeof f.ref === 'string' && HEX64.test(f.ref) : f.kind === 'file' && typeof f.name === 'string'));
  const flatten = c => typeof c === 'string' ? c : Array.isArray(c) ? c.map(b => b?.text ?? '').join('\n') : '';

  function clearConversation(s) {
    s.events.clear();
    s.streams.clear();
    s.head = 0;
    s.first = 1;
    s.more = false;
    // Messages the new conversation never showed stay, so their text is not lost.
    s.mine = s.mine.filter(m => m.seq == null);
    s.forgetHeld = true;
  }

  function store(s, seq, ev) {
    if (s.events.has(seq)) return;
    s.events.set(seq, ev);
    if (isPrompt(ev)) {
      const text = promptText(ev);
      const m = ev.cid ? s.mine.find(x => x.cid === ev.cid)
        : s.mine.find(x => x.seq == null && x.text === text);  // the ack may have been lost across a reconnect
      if (m) { m.seq = seq; m.status = 'sent'; }
    }
    // Claude queued my message: the queued row stands in for my own until it is taken.
    if (ev.type === 'cdash' && ev.subtype === 'queued' && ev.cid) {
      const m = s.mine.find(x => x.cid === ev.cid);
      if (m) m.seq = seq;
    }
    if (ev.type === 'assistant' && ev.message?.id) s.streams.delete(ev.message.id);
    if (ev.type === 'result') s.streams.clear();
  }

  function apply(s, f) {
    if (f?.t !== 'state') s.rev++;  // a state frame bumps it below, only when it changes `mine`
    switch (f?.t) {
      case 'hello':
        if (f.reset || f.epoch !== s.epoch) { clearConversation(s); s.first = f.first; }
        s.epoch = f.epoch;
        s.tool = f.tool;
        s.more = s.first > 1;
        break;
      case 'reset':
        clearConversation(s);
        s.forgetHeld = false;  // the agent keeps its queue across a conversation change
        s.epoch = f.epoch;
        break;
      case 'ev':
        if (f.seq <= s.head) break;
        store(s, f.seq, f.ev);
        s.head = f.seq;
        break;
      case 'page':
        for (const e of f.events || []) { store(s, e.seq, e.ev); s.first = Math.min(s.first, e.seq); }
        s.more = !!f.more;
        break;
      case 'delta':
        s.streams.set(f.id, (s.streams.get(f.id) || '') + f.text);
        break;
      case 'state':
        Object.assign(s, { usage: f.usage ?? null, tasks: f.tasks ?? [], stateTodo: f.todo ?? null });
        Object.assign(s, { working: !!f.working, waiting: !!f.waiting, ended: !!f.ended, unreadable: !!f.unreadable, held: f.held || [] });
        Object.assign(s, { asks: f.asks ?? [], settings: f.settings ?? {}, hooks: f.hooks !== false, waiting_for: f.waiting_for ?? null });
        for (const id of s.answered.keys()) if (!s.asks.some(a => a.id === id)) s.answered.delete(id);
        {
          // Held rows the agent no longer lists: the session ended, the agent lost its queue, or they left it elsewhere.
          const ids = new Set(s.held.map(h => h.id));
          const gone = m => m.status === 'held' && !ids.has(m.heldId);
          for (const m of s.mine) if (gone(m) && (s.ended || s.forgetHeld)) { m.status = 'refused'; m.error = s.ended ? 'session ended' : 'not sent'; s.rev++; }
          const n = s.mine.length;
          s.mine = s.mine.filter(m => m.status !== 'held' || ids.has(m.heldId));
          if (s.mine.length !== n) s.rev++;
          s.forgetHeld = false;
        }
        break;
      case 'ack': {
        const m = s.mine.find(x => x.cid === f.cid);
        if (!m) break;
        // `done`: the engine ran the command itself, so no prompt event will ever show this row.
        if (f.status === 'done') { s.mine = s.mine.filter(x => x !== m); break; }
        m.status = f.status;
        if (f.id) m.heldId = f.id;
        if (f.error) m.error = f.error;
        // Handed back or never shown: it is the owner's text again.
        if (['returned', 'refused', 'unconfirmed'].includes(f.status)) m.seq = null;
        break;
      }
      case 'answered':
        s.answered.set(f.id, { ok: f.ok, error: f.error });
        break;
      case 'menu': {
        const l = k => f[k] || [];
        s.menu = { commands: l('commands'), models: l('models'), efforts: l('efforts'), agents: l('agents') };
        break;
      }
      case 'set':
        s.setReply = { ok: f.ok, error: f.error };
        break;
    }
    return s;
  }

  function addMine(s, cid, text, files = []) {
    s.mine.push({ cid, text, status: 'sending', seq: null, files });
    s.rev++;
    return s;
  }

  // The view removes own rows here, so rows() never serves a stale memo.
  function dropMine(s, cid) {
    s.mine = s.mine.filter(x => x.cid !== cid);
    s.rev++;
    return s;
  }

  // The Terminal banner waits 500 ms for a card, unless none can come.
  const needsTerminal = (s, waitedMs) => s.waiting && s.asks.length === 0 && (s.waiting_for === 'dialog open' || !s.hooks || waitedMs >= 500);

  function chipText(s) {
    const { model, effort, variant, plan, agent } = s.settings || {};
    const label = !model ? '' : s.tool === 'claude' ? model.replace(/^claude-/, '').split('-')[0].replace(/^./, c => c.toUpperCase())
      : s.tool === 'opencode' ? model.split('/').pop() : model;
    return [label, effort ?? variant, plan === true && 'Plan', s.tool === 'opencode' && agent && agent !== 'build' && agent].filter(Boolean).join(' · ');
  }

  const base = p => String(p || '').split('/').filter(Boolean).pop() || '';
  const VERBS = { Bash: ['Running', 'Ran'], Read: ['Reading', 'Read'], Edit: ['Editing', 'Edited'], Write: ['Editing', 'Edited'], Grep: ['Searching', 'Searched'], Glob: ['Searching', 'Searched'] };
  function toolLabel(name, input, done) {
    const v = VERBS[name];
    if (!v) return name;
    const verb = v[done ? 1 : 0];
    if (name === 'Bash') {
      const cmd = String(input?.command ?? '');
      return verb + ' `' + (cmd.length > 60 ? cmd.slice(0, 59) + '…' : cmd) + '`';
    }
    return verb + ' ' + base(input?.file_path ?? input?.path ?? input?.pattern);
  }
  const groupLabel = (name, calls, done) => calls.length === 1 ? toolLabel(name, calls[0].input, done)
    : VERBS[name] && name !== 'Bash' ? VERBS[name][done ? 1 : 0] + ' ' + calls.length + ' files'
    : name + ' ×' + calls.length;

  const lines = x => x === '' ? [] : String(x).replace(/\n$/, '').split('\n');
  const mark = (t, ls) => ls.map(s => ({ t, s }));
  function lineDiff(a, b) {
    const x = lines(a), y = lines(b);
    // ponytail: the table is x*y cells; past 250,000 the whole of a goes, then the whole of b. Myers diff if that shows.
    if (x.length * y.length > 250000) return [...mark('-', x), ...mark('+', y)];
    const L = Array.from({ length: x.length + 1 }, () => new Array(y.length + 1).fill(0));
    for (let i = x.length; i--;) for (let j = y.length; j--;) L[i][j] = x[i] === y[j] ? L[i + 1][j + 1] + 1 : Math.max(L[i + 1][j], L[i][j + 1]);
    const out = [];
    let i = 0, j = 0;
    while (i < x.length || j < y.length) {
      if (i < x.length && j < y.length && x[i] === y[j]) { out.push({ t: ' ', s: x[i++] }); j++; }
      else if (j >= y.length || (i < x.length && L[i + 1][j] >= L[i][j + 1])) out.push({ t: '-', s: x[i++] });
      else out.push({ t: '+', s: y[j++] });
    }
    return out;
  }
  // A unified patch's body: no headers, so counts skip +++ and ---.
  const patchLines = p => {
    let hunk = false;  // ---/+++ are headers only before the first @@
    const out = [];
    for (const l of lines(p)) {
      if (l.startsWith('@@')) hunk = true;
      else if (hunk || !/^(\+\+\+|---)/.test(l)) out.push({ t: l[0] === '+' || l[0] === '-' ? l[0] : ' ', s: l.slice(1) });
    }
    return out;
  };
  const diffs = new WeakMap();  // tool input (or OpenCode diff array) -> its editDiff; rows rebuild on every delta
  function editDiff(call) {
    const key = Array.isArray(call?.diff) ? call.diff : call?.input;
    if (key && typeof key === 'object' && !diffs.has(key)) diffs.set(key, diffOf(call));
    return key && typeof key === 'object' ? diffs.get(key) : diffOf(call);
  }
  function diffOf(call) {
    const i = call?.input || {};
    let files;
    if (Array.isArray(call?.diff)) files = call.diff.filter(d => d && d.file != null).map(d => typeof d.patch === 'string' ? { path: d.file, lines: patchLines(d.patch) } : { path: d.file, lines: [], add: +d.additions || 0, del: +d.deletions || 0 });
    else if (Array.isArray(i.changes)) {
      files = i.changes.map(c => ({ path: c.path, lines: c.kind?.type === 'update' ? patchLines(c.diff ?? '') : mark(c.kind?.type === 'delete' ? '-' : '+', lines(c.diff ?? '')) }));
    } else if (call?.name === 'Edit' && typeof i.old_string === 'string' && typeof i.new_string === 'string') files = [{ path: i.file_path, lines: lineDiff(i.old_string, i.new_string) }];
    else if (call?.name === 'MultiEdit' && Array.isArray(i.edits)) files = [{ path: i.file_path, lines: i.edits.flatMap(e => lineDiff(e.old_string ?? '', e.new_string ?? '')) }];
    else if (call?.name === 'Write' && typeof i.content === 'string') files = [{ path: i.file_path, lines: mark('+', lines(i.content)) }];
    if (!files?.length) return null;
    for (const f of files) if (f.lines.length) { f.add = f.lines.filter(l => l.t === '+').length; f.del = f.lines.filter(l => l.t === '-').length; }
    const count = k => files.reduce((n, f) => n + (f[k] ?? 0), 0);
    return { files, add: count('add'), del: count('del') };
  }

  // The hub's checklist outlives the backlog; without it, the latest todo event.
  function todo(s) {
    let items = s.stateTodo;
    if (!items) for (const q of [...s.events.keys()].sort((a, b) => b - a)) { const e = s.events.get(q); if (e.type === 'cdash' && e.subtype === 'todo') { items = e.items; break; } }
    return Array.isArray(items) ? { done: items.filter(i => i.status === 'completed').length, total: items.length, items } : null;
  }

  const canRewind = (s, row) => row?.kind === 'user' && !!row.uuid && row.src !== 'steer' && !(s.working || s.waiting || s.ended || s.unreadable || s.mine?.some(m => m.status === 'sending'));

  function rows(s) {
    // An idle session hides stale queued events, so a change of `working` rebuilds when the events hold one.
    const queued = [...s.events.values()].some(e => e.subtype === 'queued');
    if (s.memo?.rev !== s.rev || (queued && s.memo.idle !== (!s.working && !s.waiting))) s.memo = { rev: s.rev, idle: !s.working && !s.waiting, rows: build(s) };
    return s.memo.rows;
  }

  function build(s) {
    const seqs = [...s.events.keys()].sort((a, b) => a - b);
    const gone = new Set();
    for (const q of seqs) { const e = s.events.get(q); if (e.type === 'cdash' && e.subtype === 'retract') gone.add(e.uuid); }
    const out = [];
    const calls = new Map();  // tool_use id -> call
    let lastResult = -1;
    for (const q of seqs) {
      const e = s.events.get(q);
      const last = out[out.length - 1];
      if (e.type === 'user' && isPrompt(e)) {
        if (gone.has(e.uuid)) continue;
        const text = promptText(e);
        const queued = out.findIndex(r => r.kind === 'user' && r.status === 'queued' && r.text === text);
        if (queued >= 0) out.splice(queued, 1);
        const row = { kind: 'user', key: 'e' + q, text, status: 'sent', uuid: e.uuid, src: e.src, ts: e.ts };
        const files = keepFiles(e.files);
        if (files.length) row.files = files;
        out.push(row);
      } else if (e.type === 'user') {
        for (const b of e.message.content) {
          const c = calls.get(b?.tool_use_id);
          if (c) Object.assign(c, { result: flatten(b.content), error: !!b.is_error, done: true });
          if (c && b.diff) c.diff = b.diff;
        }
      } else if (e.type === 'assistant') {
        for (const b of e.message?.content || []) {
          if (b.type === 'thinking') out.push({ kind: 'thinking', key: 'e' + q + 'k', text: b.thinking ?? '', ts: e.ts });
          if (b.type === 'text') out.push({ kind: 'text', key: 'e' + q, text: b.text, streaming: false, ts: e.ts });
          if (b.type === 'tool_use') {
            const c = { id: b.id, name: b.name, input: b.input, result: null, error: false, done: false, at: q };
            calls.set(b.id, c);
            if (last?.kind === 'tools' && last.name === b.name) last.calls.push(c);
            else out.push({ kind: 'tools', key: 'e' + q, name: b.name, calls: [c], ts: e.ts });
          }
        }
      } else if (e.type === 'result') {
        lastResult = q;
        if (e.subtype === 'interrupted') out.push({ kind: 'note', key: 'e' + q, text: 'Stopped' });
        if (e.subtype === 'error_during_execution') out.push({ kind: 'note', key: 'e' + q, text: 'Turn ended with an error' });
      } else if (e.type === 'cdash' && e.subtype === 'shell') {
        out.push({ kind: 'shell', key: 'e' + q, command: e.command, stdout: '', stderr: '' });
      } else if (e.type === 'cdash' && e.subtype === 'shell_output') {
        const sh = [...out].reverse().find(r => r.kind === 'shell');
        if (sh) Object.assign(sh, { stdout: e.stdout ?? '', stderr: e.stderr ?? '' });
      } else if (e.type === 'cdash' && e.subtype === 'staged') {
        out.push({ kind: 'staged', key: 'e' + q, count: e.messages });
      } else if (e.type === 'cdash' && e.subtype === 'queued') {
        out.push({ kind: 'user', key: 'e' + q, text: e.text, status: 'queued' });
      }
    }
    for (const r of out) {
      if (r.kind !== 'tools') continue;
      r.running = r.calls.some(c => !c.done && c.at > lastResult);
      r.label = groupLabel(r.name, r.calls, !r.running);
      const d = r.calls.length === 1 && editDiff(r.calls[0]);
      if (d) r.label += ' +' + d.add + ' \u2212' + d.del;
    }
    // A queued row nobody took (a rewind dropped it) is stale once the session is idle.
    // ponytail: a status poll can catch idle just before the dequeued prompt lands, so the row may flash for one tick.
    if (!s.working && !s.waiting) for (let i = out.length; i--;) if (out[i].kind === 'user' && out[i].status === 'queued') out.splice(i, 1);
    for (const [id, text] of s.streams) out.push({ kind: 'text', key: 's' + id, text, streaming: true });
    for (const m of s.mine) {
      if (m.seq != null) continue;
      const r = { kind: 'user', key: 'm' + m.cid, cid: m.cid, text: m.text, status: m.status };
      if (m.error) r.error = m.error;
      if (m.files?.length) r.files = m.files;
      out.push(r);
    }
    // What build() shows, as a short string; the view compares it instead of serializing the row.
    // A row's text only grows under its key, so lengths stand in for the text.
    // A day row goes before the first row of each later local day.
    let day = null;
    for (let i = 0; i < out.length; i++) {
      const ts = out[i].ts;
      if (!ts) continue;
      const d = new Date(ts).toDateString();
      if (day && d !== day) { out.splice(i, 0, { kind: 'day', key: 'd' + out[i].key, ts }); i++; }
      day = d;
    }
    for (const r of out) {
      r.sig = r.kind === 'user' ? [r.status, r.error, r.text.length, r.files?.length, r.uuid, r.src].join('|')
        : r.kind === 'thinking' ? r.text.length
        : r.kind === 'day' ? new Date(r.ts).toDateString()
        : r.kind === 'staged' ? r.count
        : r.kind === 'text' ? r.text.length + '|' + r.streaming
        : r.kind === 'tools' ? [r.label, r.running, r.calls.map(c => c.error ? 'e' : c.done ? 'd' : 'r').join('')].join('|')
        : r.kind === 'shell' ? [r.command, r.stdout.length, r.stderr.length].join('|')
        : r.text;
    }
    return out;
  }

  g.cdashChatStore = { initial, apply, rows, addMine, dropMine, toolLabel, needsTerminal, chipText, todo, lineDiff, editDiff, canRewind };
})(typeof globalThis === 'undefined' ? this : globalThis);
