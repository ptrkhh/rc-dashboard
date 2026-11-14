// The supervisor's side of dashboard chat: a numbered log of conversation
// messages (Agent SDK shapes) served as NDJSON on a private unix socket, plus
// send and stop routed back to the supervisor. The agent is the only client.
// Never writes to stdout or stderr: stdout is the native TUI's terminal.
import { createServer } from 'node:net';
import { chmod, mkdir, open, rename, rm, utimes } from 'node:fs/promises';
import { isAbsolute, join } from 'node:path';
import { createHash, randomBytes } from 'node:crypto';

const LINE_MAX = 64 * 1024;

// Twins of crates/agent/src/chat/media.rs: with_attached, split_attached, sniff, stash.
const IMAGES = { 'image/png': 'png', 'image/jpeg': 'jpg', 'image/gif': 'gif', 'image/webp': 'webp' };

export function withAttached(text, paths) {
  return [...(text ? [text] : []), ...paths.map(p => `Attached: ${p}`)].join('\n');
}

export function splitAttached(text) {
  let rest = text;
  const files = [];
  for (;;) {
    const line = rest.slice(rest.lastIndexOf('\n') + 1);
    if (!line.startsWith('Attached: /') || !line.includes('/cdash/uploads/')) break;
    files.push({ kind: 'file', name: line.slice(line.lastIndexOf('/') + 1) });
    rest = rest.length > line.length ? rest.slice(0, rest.length - line.length - 1) : '';
  }
  return { text: rest.trimEnd(), files: files.reverse() };
}

export function sniffImage(b) {
  const at = (hex, i = 0) => b.length >= i + hex.length / 2 && Buffer.from(b.subarray(i, i + hex.length / 2)).toString('hex') === hex;
  if (at('89504e470d0a1a0a')) return 'image/png';
  if (at('ffd8ff')) return 'image/jpeg';
  if (at('474946383761') || at('474946383961')) return 'image/gif';
  if (at('52494646') && at('57454250', 8)) return 'image/webp';
  return null;
}

// Store an image once under its hash in dir (CDASH_MEDIA_DIR); null when there is no
// absolute dir or the bytes are not an image. A rewrite only refreshes the age.
export async function stashMedia(dir, bytes) {
  const mime = sniffImage(bytes);
  if (!mime || !dir || !isAbsolute(dir)) return null;
  const ref = createHash('sha256').update(bytes).digest('hex');
  const target = join(dir, `${ref}.${IMAGES[mime]}`);
  const now = new Date();
  try { await utimes(target, now, now); return { ref, mime }; } catch (e) { if (e?.code !== 'ENOENT') throw e; }
  await mkdir(dir, { recursive: true, mode: 0o700 });
  await chmod(dir, 0o700);
  const tmp = `${target}.${process.pid}-${randomBytes(4).toString('hex')}.tmp`;
  try {
    const f = await open(tmp, 'wx', 0o600);
    try { await f.writeFile(bytes); await f.chmod(0o600); } finally { await f.close(); }
    await rename(tmp, target);
  } catch (e) { await rm(tmp, { force: true }); throw e; }
  return { ref, mime };
}

export class ChatFeed {
  #send; #stop; #ops; #opTimeout;
  #asks = new Map();
  #settings = null;
  #settingsJson = '';
  #log = [];
  #uuids = new Set();
  #approvals = new Set();
  #state = { working: false, waiting: false };
  #ready = false;
  #clients = new Set();
  #server = null;
  epoch = randomBytes(8).toString('hex');

  constructor({ send, stop, ops = {}, opTimeout = 10000 }) {
    this.#opTimeout = opTimeout;
    this.#send = send;
    this.#stop = stop;
    this.#ops = ops;
  }

  get asks() { return [...this.#asks.values()]; }

  putAsk(ask) {
    this.#asks.set(ask.id, ask);
    this.#publishAsks();
  }

  dropAsk(id) {
    if (this.#asks.delete(id)) this.#publishAsks();
  }

  setSettings(settings) {
    const json = JSON.stringify(settings);
    if (json === this.#settingsJson) return;
    this.#settingsJson = json;
    this.#settings = settings;
    this.#broadcast({ t: 'settings', settings });
  }

  get head() { return this.#log.length; }
  get log() { return this.#log; }
  get state() { return { working: this.#state.working, waiting: this.#state.waiting || this.#approvals.size > 0 }; }

  accept(m) {
    if (!m || typeof m !== 'object') return;
    if (m.type === 'control_request' && m.request?.subtype === 'can_use_tool') {
      this.#approvals.add(m.request_id);
      return this.#publishState();
    }
    if (m.type === 'control_cancel_request' || m.type === 'control_response') {
      if (this.#approvals.delete(m.request_id ?? m.response?.request_id)) this.#publishState();
      return;
    }
    if (!['user', 'assistant', 'result'].includes(m.type)) return;
    if (m.uuid) {
      if (this.#uuids.has(m.uuid)) return;
      this.#uuids.add(m.uuid);
    }
    const ev = m.type === 'result' ? { type: 'result', subtype: m.subtype } : m;
    const seq = this.#log.length + 1;
    this.#log.push({ seq, ev });
    this.#broadcast({ t: 'ev', seq, ev });
  }

  delta(id, text) {
    if (id && text) this.#broadcast({ t: 'delta', id, text });
  }

  setState(next) {
    const before = JSON.stringify(this.state);
    Object.assign(this.#state, next);
    if (JSON.stringify(this.state) !== before) this.#publishState();
  }

  ready() {
    if (this.#ready) return;
    this.#ready = true;
    this.#broadcast({ t: 'ready' });
  }

  async listen(dir) {
    const path = join(dir, 'chat.sock');
    await rm(path, { force: true });
    this.#server = createServer(socket => this.#serve(socket));
    await new Promise((resolve, reject) => {
      this.#server.once('error', reject);
      this.#server.listen(path, resolve);
    });
    this.#server.on('error', () => {});
    await chmod(path, 0o600);
    return path;
  }

  close() {
    for (const s of this.#clients) s.destroy();
    this.#clients.clear();
    this.#server?.close();
  }

  #publishState() {
    this.#broadcast({ t: 'state', ...this.state });
  }

  #asksLine() {
    const asks = this.asks;
    const line = JSON.stringify({ t: 'asks', asks });
    if (Buffer.byteLength(line) < LINE_MAX) return line;
    // ponytail: only plans are cut; many huge questions would still overflow.
    const cut = asks.map(a => typeof a.plan === 'string' && a.plan.length > 8000 ? { ...a, plan: a.plan.slice(0, 8000) + '\n\n…' } : a);
    return JSON.stringify({ t: 'asks', asks: cut });
  }

  #publishAsks() {
    const line = this.#asksLine() + '\n';
    for (const s of this.#clients) s.write(line);
  }

  #broadcast(frame) {
    const line = JSON.stringify(frame) + '\n';
    for (const s of this.#clients) s.write(line);
  }

  #serve(socket) {
    socket.on('error', () => {});
    socket.on('close', () => this.#clients.delete(socket));
    let buf = '';
    let hello = false;
    let chain = Promise.resolve(); // one request at a time, acks in order
    socket.on('data', chunk => {
      buf += chunk;
      let i;
      while ((i = buf.indexOf('\n')) >= 0) {
        const line = buf.slice(0, i);
        buf = buf.slice(i + 1);
        let m;
        try { m = JSON.parse(line); } catch { m = null; }
        if (!hello) {
          hello = true;
          this.#attach(socket, m && typeof m === 'object' ? m : {});
        } else if (m && typeof m === 'object') {
          if (m.t === 'req') { this.#op(socket, m); continue; } // matched by rid; never waits behind sends
          chain = chain.then(() => this.#request(socket, m));
        }
      }
      if (buf.length > LINE_MAX) socket.destroy();
    });
  }

  #attach(socket, { after, epoch }) {
    const from = epoch === this.epoch && Number.isInteger(after) && after >= 0 ? after : 0;
    const lines = [{ t: 'hello', epoch: this.epoch, head: this.head }];
    for (const e of this.#log.slice(from)) lines.push({ t: 'ev', seq: e.seq, ev: e.ev });
    lines.push({ t: 'state', ...this.state });
    const out = lines.map(l => JSON.stringify(l) + '\n');
    out.push(this.#asksLine() + '\n'); // also when empty: a reconnecting agent drops asks settled meanwhile
    if (this.#settings) out.push(JSON.stringify({ t: 'settings', settings: this.#settings }) + '\n');
    if (this.#ready) out.push('{"t":"ready"}\n');
    socket.write(out.join(''));
    this.#clients.add(socket);
  }

  async #op(socket, m) {
    let res;
    try {
      if (!Object.hasOwn(this.#ops, m.op) || typeof this.#ops[m.op] !== 'function') throw Error('not supported');
      let timer;
      const timeout = new Promise((_, rej) => { timer = setTimeout(() => rej(Error('timed out')), this.#opTimeout); });
      try { res = { t: 'res', rid: m.rid, ok: true, data: (await Promise.race([this.#ops[m.op](m), timeout])) ?? null }; } finally { clearTimeout(timer); }
    } catch (e) {
      res = { t: 'res', rid: m.rid, ok: false, error: String(e?.message ?? e) };
    }
    let line = JSON.stringify(res);
    if (Buffer.byteLength(line) >= LINE_MAX) line = JSON.stringify({ t: 'res', rid: m.rid, ok: false, error: 'reply too large' });
    if (!socket.destroyed) socket.write(line + '\n');
  }

  async #request(socket, m) {
    if (m.t === 'stop') {
      try { await this.#stop(); } catch { /* nothing to stop */ }
      return;
    }
    if (m.t !== 'send' || typeof m.text !== 'string') return;
    let ack;
    try {
      const r = await this.#send(m.text, { steer: !!m.steer, cid: m.cid, files: Array.isArray(m.files) ? m.files.filter(f => typeof f?.path === 'string') : [] });
      ack = { t: 'ack', cid: m.cid, ok: true, ...(r?.done ? { done: true } : {}) };
    } catch (e) {
      ack = { t: 'ack', cid: m.cid, ok: false, error: String(e?.message ?? e) };
    }
    if (!socket.destroyed) socket.write(JSON.stringify(ack) + '\n');
  }
}
