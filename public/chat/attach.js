/* Attachment uploads for the chat: a queue that sends one file at a time over the
   chat socket, the composer's chips and the image viewer. The DOM is touched only
   inside wire/renderChips/view, so node --test can drive Uploads. Names reach the
   page as textContent only. Same IIFE shape as store.js. */
(function (g) {
  const LIMIT = 20 * 1024 * 1024, MAX = 10, CHUNK = 1024 * 1024;
  const sleep = ms => new Promise(r => setTimeout(r, ms));

  function fmtSize(n) {
    const f = x => +x.toFixed(1);
    return n < 1024 ? n + ' B' : n < 1024 * 1024 ? f(n / 1024) + ' KB' : f(n / 1024 / 1024) + ' MB';
  }

  class Uploads {
    // send(data) -> false when the socket is not open; buffered() reads the socket's bufferedAmount.
    // Both are read per call: the socket is replaced on reconnect.
    constructor({ send, buffered, onChange }) {
      Object.assign(this, { _send: send, _buffered: buffered, _change: onChange || (() => {}) });
      this.items = [];
      this._n = 0;
      this._gen = 0;       // bumped by clear(): an upload in flight is abandoned
      this._running = false;
      this._wait = null;   // {item, done}: the upload frame the server has not answered yet
    }

    _item(o) { return { key: 'u' + ++this._n, progress: 0, ...o }; }

    add(file) {
      const item = this._item({ file, name: file.name, size: file.size, mime: file.type, state: 'queued' });
      const error = file.size > LIMIT ? 'Over 20 MB' : file.size === 0 ? 'Empty file'
        : this.items.filter(i => i.state !== 'refused').length >= MAX ? '10 attachments at most' : null;
      if (error) Object.assign(item, { state: 'refused', error });
      this.items.push(item);
      this._change();
      this._pump();
      return item;
    }

    // An item the server is still answering for stays out of sight but keeps the queue held until it replies.
    remove(key) {
      const i = this.items.find(x => x.key === key);
      if (!i) return;
      this.items = this.items.filter(x => x !== i);
      if (i.url) URL.revokeObjectURL(i.url);
      this._change();
    }

    retry(key) {
      const i = this.items.find(x => x.key === key);
      if (!i?.file || i.state !== 'failed') return;
      Object.assign(i, { state: 'queued', progress: 0, error: undefined });
      this._change();
      this._pump();
    }

    // Edit puts a message's attachments back, up to the per-message cap.
    restore(files) {
      const room = MAX - this.items.filter(i => i.state !== 'refused').length;
      for (const f of (files || []).slice(0, Math.max(0, room))) this.items.push(this._item({ ...f, state: 'done', progress: 1 }));
      this._change();
    }

    onFrame(f) {
      const w = this._wait;
      if (!w || f?.t !== 'uploaded' || f.ref !== w.item.key) return;
      const i = w.item;
      if (f.error) Object.assign(i, { state: 'failed', error: f.error });
      else Object.assign(i, { state: 'done', progress: 1, id: f.id, name: f.name ?? i.name, size: f.size ?? i.size, kind: f.kind, mime: f.mime ?? i.mime });
      this._wait = null;
      w.done();
      this._change();
    }

    lost() {
      for (const i of this.items) if (i.state === 'uploading' || i.state === 'queued') Object.assign(i, { state: 'failed', error: 'Connection lost' });
      const w = this._wait;
      this._wait = null;
      w?.done();
      this._change();
    }

    ready() {
      return this.items.filter(i => i.state === 'done').map(({ id, name, kind, size, mime }) => ({ id, name, kind, size, mime }));
    }

    pending() { return this.items.some(i => i.state !== 'done'); }

    // Call only when idle or when the socket was replaced: on the same socket the server's open
    // upload stays, and the next `upload` is refused with `one upload at a time`.
    clear() {
      for (const i of this.items) if (i.url) URL.revokeObjectURL(i.url);
      this.items = [];
      this._gen++;
      const w = this._wait;
      this._wait = null;
      w?.done();
      this._change();
    }

    async _pump() {
      if (this._running) return;
      this._running = true;
      try {
        for (let i; (i = this.items.find(x => x.state === 'queued')); ) await this._upload(i);
      } finally { this._running = false; }
    }

    async _upload(item) {
      const gen = this._gen;
      const live = () => gen === this._gen && item.state === 'uploading';
      const fail = error => {
        if (this._wait?.item === item) this._wait = null; // a stale loop must not drop a newer upload's wait
        if (live()) { Object.assign(item, { state: 'failed', error }); this._change(); }
      };
      item.state = 'uploading';
      item.progress = 0;
      const total = Math.ceil(item.size / CHUNK);
      this._change();
      // The server may answer at any point after the upload frame (a refusal, a failed write), so listen from here.
      const answered = new Promise(done => { this._wait = { item, done }; });
      if (!this._send(JSON.stringify({ t: 'upload', ref: item.key, name: item.name, size: item.size }))) return fail('Offline');
      for (let o = 0, sent = 1; o < item.size; o += CHUNK, sent++) {
        while (live() && this._buffered() >= CHUNK) await sleep(50);
        if (!live()) return;
        let data;
        try { data = await item.file.slice(o, o + CHUNK).arrayBuffer(); } catch { return fail('Could not read file'); }
        if (!live()) return;
        if (!this._send(data)) return fail('Offline');
        item.progress = sent / total;
        this._change();
      }
      // Binary frames carry no ref, so nothing else goes out until the server answers this one.
      // ponytail: no timeout; a silent server holds the queue until the socket drops and lost() runs.
      await answered;
    }
  }

  const $ = s => document.querySelector(s);
  function el(tag, cls, text) {
    const n = document.createElement(tag);
    if (cls) n.className = cls;
    if (text != null) n.textContent = text;
    return n;
  }
  const holdsFiles = dt => !!dt && [...(dt.types || [])].includes('Files');

  // current() is the open chat's Uploads, or null; changed() asks for a render.
  function wire(current, changed) {
    const add = files => {
      const u = current();
      if (!u || !files.length || $('#chat-attach').disabled) return; // the composer is off
      for (const f of files) u.add(f);
      changed();
    };
    const file = $('#chat-file');
    $('#chat-attach').addEventListener('click', () => file.click());
    file.addEventListener('change', () => { add([...file.files]); file.value = ''; });
    $('#chat-input').addEventListener('paste', e => {
      const files = [...(e.clipboardData?.files || [])];
      if (!files.length) return; // plain text pastes as usual
      e.preventDefault();
      add(files);
    });
    const chat = $('#chat');
    chat.addEventListener('dragover', e => {
      if (!holdsFiles(e.dataTransfer)) return;
      e.preventDefault();
      chat.classList.add('chat-drop');
    });
    chat.addEventListener('dragleave', e => { if (!chat.contains(e.relatedTarget)) chat.classList.remove('chat-drop'); });
    chat.addEventListener('drop', e => {
      chat.classList.remove('chat-drop');
      if (!holdsFiles(e.dataTransfer)) return;
      e.preventDefault();
      add([...e.dataTransfer.files]);
    });
    $('#chat-atts').addEventListener('click', e => {
      const b = e.target.closest('[data-remove],[data-retry]');
      const u = current();
      if (!b || !u) return;
      if (b.dataset.remove !== undefined) u.remove(b.dataset.remove); else u.retry(b.dataset.retry);
      changed();
    });
    // Every way out (Escape, a click, back) ends in one history entry less, so
    // Android back closes the image and not the chat.
    const d = $('#chat-viewer');
    d.addEventListener('click', () => d.close());
    d.addEventListener('close', () => {
      $('#chat-viewer-img').removeAttribute('src');
      if (history.state?.viewer) history.back();
    });
    addEventListener('popstate', () => { if (!history.state?.viewer && d.open) d.close(); });
  }

  function renderChips(u) {
    const box = $('#chat-atts');
    const items = u?.items ?? [];
    const sig = items.map(i => [i.key, i.state, Math.round(i.progress * 100), i.error].join(':')).join('|');
    if (box.dataset.sig === sig) return; // render runs on every frame; the thumbnails stay put
    box.dataset.sig = sig;
    box.textContent = '';
    box.hidden = !items.length;
    for (const i of items) {
      const chip = el('div', 'chat-att' + (i.error ? ' failed' : ''));
      if (i.file && i.mime?.startsWith('image/')) {
        i.url ||= URL.createObjectURL(i.file);
        const img = el('img', 'chat-att-thumb');
        img.src = i.url;
        img.alt = i.name;
        chip.append(img);
      } else chip.append(el('span', 'chat-att-name', i.name));
      chip.append(el('span', 'chat-att-size', fmtSize(i.size)));
      if (i.state === 'uploading') {
        const bar = el('progress', 'chat-att-bar');
        bar.max = 1;
        bar.value = i.progress;
        chip.append(bar);
      }
      if (i.error) chip.append(el('span', 'chat-att-error', i.error));
      if (i.state === 'failed' && i.file) {
        const retry = el('button', 'chat-att-retry', 'Retry');
        retry.type = 'button';
        retry.dataset.retry = i.key;
        chip.append(retry);
      }
      const rm = el('button', 'chat-att-remove', '×');
      rm.type = 'button';
      rm.dataset.remove = i.key;
      rm.setAttribute('aria-label', 'Remove ' + i.name);
      chip.append(rm);
      box.append(chip);
    }
  }

  function view(src) {
    const d = $('#chat-viewer');
    $('#chat-viewer-img').src = src;
    if (d.open) return;
    history.pushState({ ...history.state, viewer: true }, '');
    d.showModal();
  }

  g.cdashAttach = { LIMIT, MAX, CHUNK, Uploads, fmtSize, wire, renderChips, view };
})(typeof globalThis === 'undefined' ? this : globalThis);
