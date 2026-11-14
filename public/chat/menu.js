/* The slash menu, the header chip and the session pickers. Command names,
   descriptions, hints, model labels and agent names come from tools, so they
   reach the page only through textContent (cdashCards.el). document is touched
   only inside functions so node --test can import filter. */
(function (g) {
  const $ = s => document.querySelector(s);
  const el = (...a) => g.cdashCards.el(...a);
  const MAX = 50;
  const ui = { send: null, get: null, asked: false, sel: 0, closed: false, list: [], saving: false, sentReply: null, sheetSig: '' };

  // "/co" lists commands starting with "co"; once arguments start the menu closes.
  function filter(commands, text) {
    const m = /^\/(\S*)$/.exec(text);
    if (!m) return [];
    const p = m[1].toLowerCase();
    return commands.filter(c => c.name.toLowerCase().startsWith(p)).slice(0, MAX);
  }

  const askMenu = () => { ui.asked = true; ui.send({ t: 'menu' }); };

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
    ui.list = typing && s.menu ? filter(s.menu.commands, text) : [];
    ui.sel = Math.min(ui.sel, Math.max(0, ui.list.length - 1));
    box.textContent = '';
    box.hidden = !(ui.list.length || (typing && !s.menu));
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
    for (const [value, text] of options) {
      const o = el('option', '', text);
      o.value = value;
      o.selected = value === current;
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
      const efforts = claude ? m.efforts : model?.efforts || [];
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
    if ($('#chat-settings').open) $('#chat-settings').close();
  }

  function wire(send, getState) {
    ui.send = send;
    ui.get = getState;
    const input = $('#chat-input');
    input.addEventListener('input', () => {
      ui.closed = false;
      const s = getState();
      if (s && !s.menu && !ui.asked && input.value === '/') askMenu();
      renderList(s);
    });
    input.addEventListener('keydown', e => {
      if (!ui.list.length) return;
      const k = e.key;
      if (!['ArrowUp', 'ArrowDown', 'Enter', 'Tab', 'Escape'].includes(k)) return;
      e.preventDefault();
      e.stopImmediatePropagation();
      if (k === 'Escape') { ui.closed = true; renderList(getState()); }
      else if (k === 'Enter' || k === 'Tab') complete(ui.list[ui.sel]);
      else { ui.sel = (ui.sel + (k === 'ArrowDown' ? 1 : -1) + ui.list.length) % ui.list.length; renderList(getState()); }
    }, true);
    $('#chat-chip').addEventListener('click', () => {
      ui.sheetSig = '';
      $('#chat-settings').showModal();
      askMenu();
      render(getState());
    });
    $('#chat-settings-close').addEventListener('click', () => $('#chat-settings').close());
  }

  g.cdashMenu = { wire, render, reset, filter };
})(typeof globalThis === 'undefined' ? this : globalThis);
