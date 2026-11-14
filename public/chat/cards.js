/* Question and plan cards. Every string comes from the model or a tool, so it
   is shown with textContent, or through cdashMd.render for the plan, which
   escapes first. Same IIFE shape as store.js; document is touched only inside
   functions so node --test can import the pure helpers. */
(function (g) {
  const ui = new Map(); // ask id -> {picks, note, sent, stale}; pruned when the ask goes
  const stamps = new WeakMap(); let stamp = 0; // answered entries are compared by identity, never by text
  const idOf = e => e ? (stamps.get(e) || (stamps.set(e, ++stamp), stamp)) : 0;
  let shown = null;     // signature of what the box currently holds

  const pick = (picks, id) => picks[id] || { choices: [], text: '' };
  const complete = (ask, picks) => ask.questions.every(q => { const p = pick(picks, q.id); return p.choices.length > 0 || p.text.trim() !== ''; });
  function frame(ask, picks) {
    const answers = {};
    for (const q of ask.questions) { const p = pick(picks, q.id); answers[q.id] = { choices: [...p.choices], text: p.text.trim() }; }
    return { t: 'answer', id: ask.id, answers };
  }

  function el(tag, cls, text) {
    const n = document.createElement(tag);
    if (cls) n.className = cls;
    if (text != null) n.textContent = text;
    return n;
  }
  function btn(label, onclick, disabled) {
    const b = el('button', '', label);
    b.type = 'button';
    b.disabled = !!disabled;
    b.onclick = onclick;
    return b;
  }

  function question(q, u, refresh) {
    const box = el('div', 'chat-card-q');
    if (q.header) box.append(el('div', 'chat-card-head', q.header));
    box.append(el('div', 'chat-card-text', q.question));
    const p = u.picks[q.id] ||= { choices: [], text: '' };
    const opts = el('div', 'chat-card-opts');
    for (const o of q.options) {
      const on = p.choices.includes(o.label);
      let b;
      if (q.multi) {
        b = el('label', 'chat-card-opt');
        const cb = el('input');
        cb.type = 'checkbox';
        cb.checked = on;
        cb.disabled = u.locked;
        cb.onchange = () => { p.choices = on ? p.choices.filter(l => l !== o.label) : [...p.choices, o.label]; refresh(true); };
        b.append(cb, el('span', '', o.label));
      } else {
        b = el('button', 'chat-card-opt', o.label);
        b.type = 'button';
        b.disabled = u.locked;
        b.setAttribute('aria-pressed', on ? 'true' : 'false');
        b.onclick = () => { p.choices = on ? [] : [o.label]; refresh(true); };
      }
      if (o.description) b.append(el('span', 'chat-card-desc', o.description));
      opts.append(b);
    }
    box.append(opts);
    if (q.other) {
      const t = el('input', 'chat-card-other');
      t.type = q.secret ? 'password' : 'text';
      t.placeholder = 'Other answer';
      t.value = p.text;
      t.disabled = u.locked;
      if (q.secret) t.autocomplete = 'off';
      t.oninput = () => { p.text = t.value; refresh(false); };
      box.append(t);
    }
    return box;
  }

  function card(ask, u, answered, send, refresh) {
    const root = el('div', 'chat-card');
    const a = answered.get(ask.id);
    const live = a && a !== u.stale ? a : null;
    const gone = live && !live.ok && live.error === 'already answered';
    if (live && !live.ok && !gone) u.sent = false;
    const busy = u.sent && !(live && !live.ok);
    u.locked = busy || gone;
    const fire = f => { if (send(f)) { u.sent = true; u.stale = answered.get(ask.id); refresh(true); } };
    if (ask.kind === 'plan') {
      const codex = ask.tool === 'codex';
      if (codex) root.append(el('div', 'chat-card-head', 'Implement this plan?'));
      const plan = el('div', 'chat-card-plan');
      plan.innerHTML = g.cdashMd.render(ask.plan || ''); // render() escapes everything first
      root.append(plan);
      if (!gone) {
        const row = el('div', 'chat-card-actions');
        if (codex) {
          row.append(btn('Yes', () => fire({ t: 'answer', id: ask.id, choice: 'yes' }), busy),
            btn('No', () => fire({ t: 'answer', id: ask.id, choice: 'no' }), busy));
        } else {
          const t = el('input', 'chat-card-other');
          t.type = 'text';
          t.placeholder = 'What to change';
          t.value = u.note || '';
          t.oninput = () => { u.note = t.value; };
          row.append(btn('Approve', () => fire({ t: 'answer', id: ask.id, choice: 'yes' }), busy), t,
            btn('Keep planning', () => fire({ t: 'answer', id: ask.id, choice: 'no', text: (u.note || '').trim() }), busy));
        }
        root.append(row);
      }
    } else {
      for (const q of ask.questions) root.append(question(q, u, refresh));
      if (!gone) {
        const go = btn('Submit', () => fire(frame(ask, u.picks)), busy || !complete(ask, u.picks));
        u.submit = go;
        root.append(el('div', 'chat-card-actions')).append(go);
      }
    }
    const note = gone ? 'Already answered' : busy ? 'Sending…' : live && !live.ok ? live.error || 'Could not answer' : '';
    if (note) root.append(el('div', 'chat-card-note', note));
    return root;
  }

  function render(box, state, send) {
    const asks = state.asks || [];
    for (const id of ui.keys()) if (!asks.some(a => a.id === id)) ui.delete(id);
    const sig = JSON.stringify(asks.map(a => [a.id, idOf(state.answered.get(a.id)), ui.get(a.id)?.sent || false]));
    if (sig === shown && box.firstChild) return;
    if (!asks.length && !box.firstChild) { shown = sig; return; }
    shown = sig;
    // A pick toggle rebuilds; typing only re-checks Submit, so the field keeps focus.
    const refresh = full => {
      if (full) { shown = null; render(box, state, send); return; }
      for (const a of asks) {
        const u = ui.get(a.id);
        if (a.kind !== 'plan' && u.submit) u.submit.disabled = u.locked || !complete(a, u.picks);
      }
    };
    box.textContent = '';
    for (const a of asks) {
      if (!ui.has(a.id)) ui.set(a.id, { picks: {}, note: '', sent: false, stale: null });
      box.append(card(a, ui.get(a.id), state.answered, send, refresh));
    }
    shown = JSON.stringify(asks.map(a => [a.id, idOf(state.answered.get(a.id)), ui.get(a.id).sent]));
  }

  g.cdashCards = { render, complete, frame };
})(typeof globalThis === 'undefined' ? this : globalThis);
