/* The slash menu, the header chip and the session pickers. Command names,
   descriptions, hints, model labels and agent names come from tools, so they
   reach the page only through textContent (cdashCards.el). document is touched
   only inside functions so node --test can import filter. */
(function (g) {
  const $ = s => document.querySelector(s);
  const el = (...a) => g.cdashCards.el(...a);
  const MAX = 50;
  // Every tool gets it: chat answers it in the side sheet (cdashSheets), never in the session.
  const BTW = { name: 'btw', description: 'Ask aside: answered in a side sheet, never added to the conversation', hint: '<question>' };
  const ui = { send: null, get: null, asked: false, sel: 0, closed: false, list: [], saving: false, sentReply: null, sheetSig: '' };

  // "/co" lists commands starting with "co"; once arguments start the menu closes.
  function filter(commands, text) {
    const m = /^\/(\S*)$/.exec(text);
    if (!m) return [];
    const p = m[1].toLowerCase();
    return commands.filter(c => c.name.toLowerCase().startsWith(p)).slice(0, MAX);
  }

  const askMenu = () => { if (ui.send({ t: 'menu' })) ui.asked = true; };

  // A value the list lacks (a fresh Claude session has no model yet) shows as a disabled dash, not as the first option.
  const withPlaceholder = (options, current) =>
    options.some(([v]) => v === current) ? options : [['', '—', true], ...options];

  function complete(c) {
    const input = $('#chat-input');
    input.value = '/' + c.name + ' ';
    input.setSelectionRange(input.value.length, input.value.length);
    input.dispatchEvent(new Event('input')); // the composer resizes and re-reads its mode
    input.focus();
  }

  function renderList(s) {
    const box = $('#chat-menu');
    const text = $('#chat-input').value;
    const typing = !!s && !ui.closed && /^\/\S*$/.test(text);
    ui.list = typing && s.menu ? filter([BTW, ...s.menu.commands], text) : [];
    ui.sel = Math.min(ui.sel, Math.max(0, ui.list.length - 1));
    box.textContent = '';
    box.hidden = !(ui.list.length || (typing && !s.menu));
    $('#chat-input').setAttribute('aria-expanded', String(!box.hidden));
    if (typing && !s.menu) box.append(el('div', 'chat-menu-note', 'Loading…'));
    ui.list.forEach((c, i) => {
      const item = el('div', 'chat-menu-item');
      item.setAttribute('role', 'option');
      item.setAttribute('aria-selected', String(i === ui.sel));
      item.append(el('span', 'chat-menu-name', '/' + c.name));
      if (c.hint) item.append(el('span', 'chat-menu-hint', c.hint));
      if (c.description) item.append(el('span', 'chat-menu-desc', c.description));
      item.onmousedown = e => e.preventDefault(); // keep the composer focused
      item.onclick = () => complete(c);
      box.append(item);
    });
    box.querySelector('[aria-selected="true"]')?.scrollIntoView?.({ block: 'nearest' });
  }

  function select(label, options, current, onchange, disabled) {
    const s = el('select');
    s.setAttribute('aria-label', label);
    for (const [value, text, ph] of withPlaceholder(options, current)) {
      const o = el('option', '', text);
      o.value = value;
      o.selected = ph || value === current;
      o.disabled = !!ph;
      s.append(o);
    }
    s.disabled = disabled;
    s.onchange = () => onchange(s.value);
    return s;
  }

  function change(what, value) {
    if (!ui.send({ t: 'set', what, value })) return;
    ui.saving = true;
    ui.sentReply = ui.get()?.setReply ?? null;
    render(ui.get());
  }

  function renderSheet(s) {
    const dlg = $('#chat-settings');
    if (!dlg.open || !s) return;
    if (ui.saving && s.setReply !== ui.sentReply) ui.saving = false;
    const failed = !ui.saving && s.setReply && s.setReply !== ui.sentReply && !s.setReply.ok ? s.setReply.error || 'Not saved' : '';
    const m = s.menu;
    const sig = JSON.stringify([m, s.settings, s.tool, s.working, s.waiting, ui.saving, failed]);
    if (sig === ui.sheetSig) return; // a rebuild would close a picker the user has open
    ui.sheetSig = sig;
    const body = $('#chat-settings-body');
    body.textContent = '';
    if (!m) { body.append(el('p', 'chat-set-note', 'Loading…')); return; }
    const claude = s.tool === 'claude';
    const busy = claude && (s.working || s.waiting);
    const cur = s.settings || {};
    const row = (label, control, note) => {
      const r = el('div', 'chat-set-row');
      r.append(el('span', 'chat-set-label', label), control);
      if (note) r.append(el('p', 'chat-set-note', note));
      body.append(r);
    };
    const dflt = claude ? 'Also becomes your default for new Claude sessions' : '';
    const model = m.models.find(x => x.value === cur.model || x.model === cur.model);
    row('Model', select('Model', m.models.map(x => [x.value, x.label || x.value]), model?.value, v => change('model', v), busy), dflt);
    if (s.tool === 'opencode') {
      const variants = model?.variants || [];
      row('Variant', select('Variant', [['', 'default'], ...variants.map(v => [v, v])], cur.variant ?? '', v => change('variant', v), busy));
    } else {
      const efforts = model?.efforts || m.efforts;
      row('Effort', select('Effort', efforts.map(e => [e, e]), cur.effort, v => change('effort', v), busy), dflt);
    }
    if (claude || s.tool === 'codex') {
      const sw = el('input');
      sw.type = 'checkbox';
      sw.setAttribute('role', 'switch');
      sw.setAttribute('aria-label', 'Plan');
      sw.checked = cur.plan === true;
      sw.disabled = busy;
      sw.onchange = () => change('plan', sw.checked);
      row('Plan', sw);
    }
    if (s.tool === 'opencode')
      row('Agent', select('Agent', m.agents.map(a => [a.value, a.label || a.value]), cur.agent, v => change('agent', v), busy));
    if (s.tool === 'codex') body.append(el('p', 'chat-set-note', 'Changes apply from the next turn.'));
    if (busy) body.append(el('p', 'chat-set-note', 'Stop the turn or wait for it to finish'));
    if (ui.saving) body.append(el('p', 'chat-set-note', 'Saving…'));
    if (failed) body.append(el('p', 'chat-set-error', failed));
  }

  function render(s) {
    const chip = $('#chat-chip');
    const text = s ? cdashChatStore.chipText(s) : '';
    chip.textContent = text;
    chip.hidden = !text;
    renderList(s);
    renderSheet(s);
  }

  function reset() {
    Object.assign(ui, { asked: false, closed: false, sel: 0, saving: false, sheetSig: '', list: [] });
    $('#chat-menu').hidden = true;
    $('#chat-menu').textContent = '';
    $('#chat-input').setAttribute('aria-expanded', 'false');
    if ($('#chat-settings').open) $('#chat-settings').close();
  }

  function wire(send, getState) {
    ui.send = send;
    ui.get = getState;
    const input = $('#chat-input');
    input.addEventListener('input', () => {
      ui.closed = false;
      ui.sel = 0;
      const s = getState();
      if (s && !s.menu && !ui.asked && /^\/\S*$/.test(input.value)) askMenu();
      renderList(s);
    });
    input.addEventListener('keydown', e => {
      if (e.isComposing || $('#chat-menu').hidden) return;
      const k = e.key;
      if (!ui.list.length && k !== 'Escape') return; // Loading…: only Escape acts
      if (!['ArrowUp', 'ArrowDown', 'Enter', 'Tab', 'Escape'].includes(k)) return;
      e.preventDefault();
      e.stopImmediatePropagation();
      if (k === 'Escape') { ui.closed = true; renderList(getState()); }
      else if (k === 'Enter' || k === 'Tab') complete(ui.list[ui.sel]);
      else { ui.sel = (ui.sel + (k === 'ArrowDown' ? 1 : -1) + ui.list.length) % ui.list.length; renderList(getState()); }
    }, true);
    input.addEventListener('blur', () => { ui.closed = true; renderList(getState()); });
    $('#chat-chip').addEventListener('click', () => {
      ui.sheetSig = '';
      ui.sentReply = getState()?.setReply ?? null;
      $('#chat-settings').showModal();
      askMenu();
      render(getState());
    });
    $('#chat-settings-close').addEventListener('click', () => $('#chat-settings').close());
  }

  g.cdashMenu = { wire, render, reset, filter, withPlaceholder };
})(typeof globalThis === 'undefined' ? this : globalThis);
