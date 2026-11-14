/* Attachment uploads for the chat: a queue that sends one file at a time over the
   chat socket. No DOM here beyond URL.revokeObjectURL. Same IIFE shape as store.js. */
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

    restore(files) {
      for (const f of files || []) this.items.push(this._item({ ...f, state: 'done', progress: 1 }));
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
      const fail = error => { if (live()) Object.assign(item, { state: 'failed', error }); this._change(); };
      item.state = 'uploading';
      item.progress = 0;
      const total = Math.ceil(item.size / CHUNK);
      this._change();
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
      await new Promise(done => { this._wait = { item, done }; });
    }
  }

  g.cdashAttach = { LIMIT, MAX, CHUNK, Uploads, fmtSize };
})(typeof globalThis === 'undefined' ? this : globalThis);
