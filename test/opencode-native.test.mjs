import { test } from 'node:test';
import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { mkdtemp, writeFile, readFile, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import { createHash } from 'node:crypto';

test('project model query uses a private metadata lease and strips Claude credentials', { timeout: 30000 }, async t => {
  const dir = await mkdtemp(join(tmpdir(), 'cdash-oc-native-'));
  t.after(() => rm(dir, { recursive: true, force: true }));
  const log = join(dir, 'server.json');
  await writeFile(join(dir, 'opencode'), `#!${process.execPath}
const http = require('node:http'), fs = require('node:fs');
fs.writeFileSync(process.env.CDASH_TEST_LOG, JSON.stringify({args:process.argv.slice(2), leaked:!!process.env.CLAUDE_CODE_SESSION_ACCESS_TOKEN, config:JSON.parse(process.env.OPENCODE_CONFIG_CONTENT)}));
const server = http.createServer((req,res) => { if (req.headers.authorization !== 'Basic '+Buffer.from('opencode:'+process.env.OPENCODE_PASSWORD).toString('base64')) { res.writeHead(401); return res.end(); } res.end(JSON.stringify({ data:[{providerID:'fixture',id:'model/one',name:'Project Model',variants:[{id:'deep'}]}] })); });
server.listen(0, '127.0.0.1', () => console.log(JSON.stringify({url:'http://127.0.0.1:'+server.address().port})));
process.stdin.resume(); process.stdin.on('end',()=>server.close());
`, { mode: 0o700 });
  const result = await new Promise((done, reject) => {
    const child = spawn(process.execPath, [resolve('crates/agent/src/collect/opencode-remote.mjs'), '--models', '--dir', dir], {
      cwd: dir, env: { ...process.env, PATH: `${dir}:${process.env.PATH}`, CDASH_TEST_LOG: log, CLAUDE_CODE_SESSION_ACCESS_TOKEN: 'secret' }, stdio: ['ignore', 'pipe', 'pipe'],
    });
    let stdout = '', stderr = '';
    child.stdout.on('data', value => stdout += value);
    child.stderr.on('data', value => stderr += value);
    child.once('error', reject);
    child.once('exit', code => code ? reject(Error(stderr)) : done(JSON.parse(stdout)));
  });
  assert.equal(result[0].value, 'opencode:fixture/model/one');
  assert.deepEqual(result[0].variants, ['deep']);
  const server = JSON.parse(await readFile(log, 'utf8'));
  assert.equal(server.leaked, false);
  assert.ok(server.config.plugins.includes('-opencode.config.mcp'));
  assert.equal(server.args.includes('--service'), false);
});

test('history includes only owned active top-level sessions and writes text-only snapshots', { timeout: 30000 }, async t => {
  const dir = await mkdtemp(join(tmpdir(), 'cdash-oc-history-'));
  t.after(() => rm(dir, { recursive: true, force: true }));
  for (const sid of ['ses_owned', 'ses_archived', 'ses_child']) await writeFile(join(dir, `${sid}.owned.json`), JSON.stringify({ sid, dir, display_name: sid }));
  await writeFile(join(dir, 'opencode'), `#!${process.execPath}
const http = require('node:http');
const server = http.createServer((req,res) => {
 const parts=req.url.split('?')[0].split('/'), sid=parts[3];
 const data=parts[4]==='message' ? {data:[{id:'msg_a',type:'user',text:'Fix project'}, {id:'msg_b',type:'assistant',content:[{type:'reasoning',text:'private reasoning'},{type:'text',text:'Fixed'}]}],cursor:{}} : {data:{id:sid,title:sid,metadata:{cdash_owner:true},location:{directory:process.env.CDASH_TEST_DIR},time:{updated:1000,...(sid==='ses_archived'?{archived:1}:{})},...(sid==='ses_child'?{parentID:'ses_parent'}:{}),model:{providerID:'fixture',id:'model',variant:'deep'}}};
 res.end(JSON.stringify(data));
});
server.listen(0,'127.0.0.1',()=>console.log(JSON.stringify({url:'http://127.0.0.1:'+server.address().port})));
process.stdin.resume();process.stdin.on('end',()=>server.close());
`, { mode: 0o700 });
  const rows = await new Promise((done, reject) => {
    const child = spawn(process.execPath, [resolve('crates/agent/src/collect/opencode-remote.mjs'), '--threads', '--root', dir, '--limit', '100'], {
      cwd: dir, env: { ...process.env, PATH: `${dir}:${process.env.PATH}`, CDASH_TEST_DIR: dir }, stdio: ['ignore', 'pipe', 'pipe'],
    });
    let stdout = '', stderr = '';
    child.stdout.on('data', value => stdout += value); child.stderr.on('data', value => stderr += value);
    child.once('error', reject); child.once('exit', code => { if (code) reject(Error(stderr)); else { try { done(JSON.parse(stdout)); } catch (error) { reject(error); } } });
  });
  assert.deepEqual(rows.map(row => row.sid), ['ses_owned']);
  assert.deepEqual(rows[0].prompts, ['Fix project']);
  assert.equal(rows[0].variant, 'deep');
  const transcript = await readFile(rows[0].source_file, 'utf8');
  assert.match(transcript, /Fix project/);
  assert.match(transcript, /Fixed/);
  assert.doesNotMatch(transcript, /private reasoning/);
});

test('native supervisor attaches one TUI to its owned session and cleans up', { timeout: 30000 }, async t => {
  const dir = await mkdtemp(join(tmpdir(), 'cdash-oc-supervisor-'));
  t.after(() => rm(dir, { recursive: true, force: true }));
  const state = join(dir, 'pane.json'), log = join(dir, 'wire.jsonl');
  await writeFile(state, JSON.stringify({ dir, display_name: 'Fixture Session', sid: null, model: 'fixture/model', model_label: 'Old Model', variant: 'deep' }));
  await writeFile(join(dir, 'opencode'), `#!${process.execPath}
const fs=require('node:fs'), http=require('node:http');
const log=x=>fs.appendFileSync(process.env.CDASH_TEST_LOG,JSON.stringify(x)+'\\n');
if(process.argv.includes('--server')) {log({kind:'tui',pid:process.pid,args:process.argv.slice(2),hasPassword:!!process.env.OPENCODE_PASSWORD,leaked:!!process.env.CLAUDE_CODE_SESSION_ACCESS_TOKEN}); process.stdin.resume();setInterval(()=>{},1000);}
else {
 log({kind:'server',pid:process.pid,leaked:!!process.env.CLAUDE_CODE_SESSION_ACCESS_TOKEN});
 let eventConnections=0;
 const server=http.createServer(async(req,res)=>{let raw='';for await(const c of req)raw+=c;log({kind:'api',method:req.method,path:req.url,body:raw?JSON.parse(raw):null});
 if(req.url==='/api/event'){res.writeHead(200,{'content-type':'text/event-stream'});res.write('data: {"type":"server.connected","data":{}}\\n\\n');if(++eventConnections===1)setTimeout(()=>res.end(),50);else setTimeout(()=>res.write('data: '+JSON.stringify({type:'session.model.selected',data:{sessionID:'ses_fixture',model:{providerID:'fixture',id:'new-model'}}})+'\\n\\n'),500);return;}
 if(req.url==='/api/session/active')return res.end(JSON.stringify({data:eventConnections>1?{ses_fixture:{type:'running'}}:{}}));
 if(req.url==='/api/session'&&req.method==='POST')return res.end(JSON.stringify({data:{id:'ses_fixture'}}));
 if(req.url==='/api/session/ses_fixture/model'){res.writeHead(204);return res.end();}
 return res.end(JSON.stringify({data:{id:'ses_fixture',metadata:{cdash_owner:true},time:{},model:{id:'model',providerID:'fixture',variant:'deep'}}}));});
 server.listen(0,'127.0.0.1',()=>console.log(JSON.stringify({url:'http://127.0.0.1:'+server.address().port})));
 process.stdin.resume();process.stdin.on('end',()=>server.closeAllConnections()||server.close());
}
`, { mode: 0o700 });
  await writeFile(join(dir, 'claude'), `#!${process.execPath}\nconst child=require('node:child_process').spawn(process.execPath,['-e','setInterval(()=>{},1000)'],{stdio:'ignore'});require('node:fs').appendFileSync(process.env.CDASH_TEST_LOG,JSON.stringify({kind:'claude',descendant:child.pid})+'\\n');process.exit(1);`, { mode: 0o700 });
  let child = spawn(process.execPath, [resolve('crates/agent/src/collect/opencode-remote.mjs'), '--state', state, '--name', 'cdash-fixture'], {
    cwd: dir, env: { ...process.env, PATH: `${dir}:${process.env.PATH}`, CDASH_TEST_LOG: log, CLAUDE_CODE_SESSION_ACCESS_TOKEN: 'do-not-leak' }, stdio: ['pipe', 'pipe', 'pipe'],
  });
  let stderr = ''; child.stderr.on('data', data => stderr += data);
  let rows = [];
  try {
    for (let i=0;i<100;i++) {
      rows = (await readFile(log, 'utf8').catch(() => '')).trim().split('\n').filter(Boolean).map(JSON.parse);
      if (rows.some(row => row.kind === 'tui')) break;
      if (child.exitCode !== null) throw Error(`supervisor exited: ${stderr}`);
      await new Promise(resolve => setTimeout(resolve, 50));
    }
    const tui = rows.find(row => row.kind === 'tui');
    assert.ok(tui, stderr);
    assert.equal(rows.filter(row => row.kind === 'server').length, 1);
    assert.equal(tui.args[tui.args.indexOf('--session')+1], 'ses_fixture');
    assert.ok(tui.args.includes('--auto'), 'fresh coding sessions must auto-approve without saved config');
    assert.equal(tui.hasPassword, true);
    assert.equal(tui.leaked, false);
    assert.equal(rows.find(row => row.kind === 'server').leaked, false);
    assert.equal(JSON.parse(await readFile(join(dir, 'ses_fixture.owned.json'), 'utf8')).sid, 'ses_fixture');
    assert.equal(JSON.parse(await readFile(join(dir, 'pane.native'), 'utf8')).sid, 'ses_fixture');
    for (let i = 0; i < 60; i++) {
      rows = (await readFile(log, 'utf8')).trim().split('\n').filter(Boolean).map(JSON.parse);
      if (rows.filter(row => row.kind === 'claude').length >= 2) break;
      await new Promise(resolve => setTimeout(resolve, 50));
    }
    assert.ok(rows.filter(row => row.kind === 'claude').length >= 2, 'Remote Control should retry');
    assert.equal(child.exitCode, null, 'native supervisor remains alive during Remote Control failures');
    let nativeState;
    for (let i = 0; i < 100; i++) {
      nativeState = JSON.parse(await readFile(join(dir, 'pane.native'), 'utf8'));
      if (nativeState.working) break;
      await new Promise(resolve => setTimeout(resolve, 50));
    }
    assert.equal(nativeState.working, true, 'native status must recover after event stream disconnect');
    for (let i = 0; i < 100 && nativeState.model !== 'fixture/new-model'; i++) {
      await new Promise(resolve => setTimeout(resolve, 50));
      nativeState = JSON.parse(await readFile(join(dir, 'pane.native'), 'utf8'));
    }
    assert.equal(nativeState.model, 'fixture/new-model');
    assert.equal(nativeState.model_label, null, 'a native model switch must not keep the old label');
    const orphan = rows.find(row => row.kind === 'claude').descendant;
    try { assert.throws(() => process.kill(orphan, 0), { code: 'ESRCH' }, 'a failed runner must not leave children'); }
    finally { try { process.kill(orphan, 'SIGKILL'); } catch {} }
    const firstExit = new Promise(resolve => child.once('exit', resolve));
    child.kill('SIGTERM'); await firstExit;
    child = spawn(process.execPath, [resolve('crates/agent/src/collect/opencode-remote.mjs'), '--state', state, '--name', 'cdash-fixture'], {
      cwd: dir, env: { ...process.env, PATH: `${dir}:${process.env.PATH}`, CDASH_TEST_LOG: log, CLAUDE_CODE_SESSION_ACCESS_TOKEN: 'do-not-leak' }, stdio: ['pipe', 'pipe', 'pipe'],
    });
    child.stderr.on('data', data => stderr += data);
    for (let i = 0; i < 100; i++) {
      rows = (await readFile(log, 'utf8')).trim().split('\n').filter(Boolean).map(JSON.parse);
      if (rows.filter(row => row.kind === 'tui').length === 2) break;
      if (child.exitCode !== null) throw Error(`resume exited: ${stderr}`);
      await new Promise(resolve => setTimeout(resolve, 50));
    }
    const resumed = rows.filter(row => row.kind === 'tui').at(-1);
    assert.equal(rows.filter(row => row.kind === 'tui').length, 2);
    assert.equal(resumed.args[resumed.args.indexOf('--session') + 1], 'ses_fixture');
    assert.ok(resumed.args.includes('--auto'), 'resumed coding sessions must also auto-approve');
    assert.equal(rows.filter(row => row.kind === 'api' && row.path === '/api/session' && row.method === 'POST').length, 1, 'resume must not create another session');
    assert.equal(rows.filter(row => row.kind === 'api' && row.path === '/api/session/ses_fixture/model').length, 0, 'unchanged model must not be reselected');
  } finally {
    if (child.exitCode === null) { const exit = new Promise(resolve => child.once('exit', resolve)); child.kill('SIGTERM'); await exit; }
    for (const row of rows.filter(item => item.descendant)) { try { process.kill(row.descendant, 'SIGKILL'); } catch {} }
    for (const row of rows.filter(item => item.pid)) {
      let alive = true;
      for (let i = 0; i < 50 && alive; i++) {
        try { process.kill(row.pid, 0); await new Promise(resolve => setTimeout(resolve, 20)); }
        catch (error) { if (error.code === 'ESRCH') alive = false; else throw error; }
      }
      assert.equal(alive, false, `${row.kind} process leaked`);
    }
  }
});

// A supervisor over a fake OpenCode server that also serves the chat feed's
// reads (messages, forms, permissions) and records prompts and interrupts.
async function fixture(t, { flakyMessages = false, withFiles = '', mediaDir = null, rows: testRows = null } = {}) {
  const { readFileSync, existsSync } = await import('node:fs');
  const { rename } = await import('node:fs/promises');
  const dir = await mkdtemp(join(tmpdir(), 'cdash-oc-chat-'));
  const state = join(dir, 'pane.json'), log = join(dir, 'wire.jsonl'), marker = join(dir, 'emit-event');
  await writeFile(log, '');
  await writeFile(state, JSON.stringify({ dir, display_name: 'Chat Session', sid: null, model: 'fixture/model', variant: null }));
  await writeFile(join(dir, 'opencode'), `#!${process.execPath}
const fs=require('node:fs'), http=require('node:http');
const log=x=>fs.appendFileSync(process.env.CDASH_TEST_LOG,JSON.stringify(x)+'\\n');
if(process.argv.includes('--server')) {log({kind:'tui',pid:process.pid}); process.stdin.resume();setInterval(()=>{},1000);}
else {
 log({kind:'server',pid:process.pid});
 const streams=new Set();let revert=null;
 setInterval(()=>{const m=process.env.CDASH_TEST_MARKER;if(fs.existsSync(m)){const e=fs.readFileSync(m,'utf8');fs.unlinkSync(m);for(const s of streams)s.write('data: '+e+'\\n\\n');}},30);
 const send=(res,data)=>res.end(JSON.stringify(data));
 const server=http.createServer(async(req,res)=>{let raw='';for await(const c of req)raw+=c;const body=raw?JSON.parse(raw):null;const path=req.url.split('?')[0];log({kind:'api',method:req.method,path,body});
 if(path==='/api/event'){res.writeHead(200,{'content-type':'text/event-stream'});res.write('data: {"type":"server.connected","data":{}}\\n\\n');streams.add(res);req.on('close',()=>streams.delete(res));return;}
 if(path==='/api/session/active')return send(res,{data:{}});
 if(path==='/api/session'&&req.method==='POST')return send(res,{data:{id:'ses_fixture'}});
 if(path==='/api/session/ses_fixture/message'&&process.env.CDASH_TEST_FLAKY&&!fs.existsSync(process.env.CDASH_TEST_FLAKY)){fs.writeFileSync(process.env.CDASH_TEST_FLAKY,'1');return send(res,{data:'broken'});}
 if(path==='/api/session/ses_fixture/message'&&fs.existsSync(process.env.CDASH_TEST_FAILMSG)){fs.unlinkSync(process.env.CDASH_TEST_FAILMSG);return send(res,{data:'broken'});}
 if(path.endsWith('/revert/stage')){revert={messageID:body.messageID};return send(res,{data:revert});}
 if(path.endsWith('/revert')&&req.method==='DELETE'){revert=null;res.writeHead(204);return res.end();}
 const rowFiles=process.env.CDASH_TEST_FILES==='bad'?[{data:'AAAA',mime:'image/png',name:'x.png'},{mime:'image/png'},'junk',null,{data:5,mime:'application/pdf'}]:[{data:'${PNG.toString('base64')}',mime:'image/png',name:'a.png',source:{type:'inline'}},{data:'${PDF.toString('base64')}',mime:'application/pdf',name:'r.pdf',source:{type:'uri',uri:'file:///x/r.pdf'}}];
 const extra=process.env.CDASH_TEST_FILES?[{id:'msg_f',type:'user',text:'see\\nAttached: /h/.cache/cdash/uploads/cdash-fixture/0123456789abcdef/n.txt',files:rowFiles}]:[];
 if(path==='/api/session/ses_fixture/message'&&process.env.CDASH_TEST_ROWS)return send(res,{data:JSON.parse(process.env.CDASH_TEST_ROWS),cursor:{}});
 if(path==='/api/session/ses_fixture/message')return send(res,{data:[{id:'msg_u',type:'user',text:'hello'},{id:'msg_a',type:'assistant',time:{completed:1},content:[{type:'text',text:'hi there'}]},...extra],cursor:{}});
 if(path.includes('/form/')&&path.endsWith('/reply')){res.writeHead(path.includes('/frm_done/')?409:204);return res.end();}
 if(path==='/api/command')return send(res,{data:[{name:'review',description:'Review changes'}]});
 if(path==='/api/agent')return send(res,{data:[{id:'build',name:'build',mode:'primary',hidden:false},{id:'plan',name:'plan',mode:'primary',hidden:false},{id:'explore',name:'explore',mode:'subagent',hidden:false},{id:'title',name:'title',mode:'primary',hidden:true}]});
 if(path==='/api/model')return send(res,{data:[{providerID:'fixture',id:'model',name:'Model',variants:[{id:'high'}]},{providerID:'fixture',id:'other',name:'Other',variants:[]}]});
 if(req.method==='POST'&&/\\/(agent|model|command)$/.test(path)){res.writeHead(204);return res.end();}
 if(path.endsWith('/form')||path.endsWith('/permission'))return send(res,{data:[]});
 if(path.endsWith('/prompt'))return send(res,{data:{id:body.id,sessionID:'ses_fixture',type:'user'}});
 if(path.endsWith('/interrupt'))return send(res,{data:true});
 return send(res,{data:{id:'ses_fixture',metadata:{cdash_owner:true},time:{},model:{id:'model',providerID:'fixture'},...(revert&&{revert})}});});
 server.listen(0,'127.0.0.1',()=>console.log(JSON.stringify({url:'http://127.0.0.1:'+server.address().port})));
 process.stdin.resume();process.stdin.on('end',()=>server.closeAllConnections()||server.close());
}
`, { mode: 0o700 });
  await writeFile(join(dir, 'claude'), `#!${process.execPath}\nprocess.exit(1);`, { mode: 0o700 });
  const child = spawn(process.execPath, [resolve('crates/agent/src/collect/opencode-remote.mjs'), '--state', state, '--name', 'cdash-fixture'], {
    cwd: dir, env: { ...process.env, PATH: `${dir}:${process.env.PATH}`, CDASH_TEST_LOG: log, CDASH_TEST_MARKER: marker, CDASH_MEDIA_DIR: mediaDir ?? join(dir, 'media'), ...(testRows && { CDASH_TEST_ROWS: JSON.stringify(testRows) }), ...(withFiles && { CDASH_TEST_FILES: withFiles === true ? '1' : withFiles }), CDASH_TEST_FAILMSG: join(dir, 'fail-msg'), ...(flakyMessages && { CDASH_TEST_FLAKY: join(dir, 'failed-once') }) }, stdio: ['pipe', 'pipe', 'pipe'],
  });
  let output = '';
  child.stdout.on('data', b => { output += b; });
  child.stderr.on('data', b => { output += b; });
  const rows = () => readFileSync(log, 'utf8').trim().split('\n').filter(Boolean).map(JSON.parse);
  t.after(async () => {
    if (child.exitCode === null) { const exit = new Promise(r => child.once('exit', r)); child.kill('SIGTERM'); await exit; }
    for (const row of rows().filter(r => r.pid)) { try { process.kill(row.pid, 'SIGKILL'); } catch {} }
    await rm(dir, { recursive: true, force: true });
  });
  const wait = async check => {
    for (let i = 0; i < 1000; i++) { if (await check()) return; if (child.exitCode !== null) throw Error(`supervisor exited: ${output}`); await new Promise(r => setTimeout(r, 30)); }
    throw Error(`condition timed out: ${output}`);
  };
  return {
    dir, wait, output: () => output,
    native: () => { try { return JSON.parse(readFileSync(join(dir, 'pane.native'), 'utf8')); } catch { return undefined; } },
    posted: suffix => rows().filter(r => r.kind === 'api' && r.method === 'POST' && r.path.endsWith(suffix)).at(-1)?.body,
    deleted: suffix => rows().some(r => r.kind === 'api' && r.method === 'DELETE' && r.path.endsWith(suffix)),
    emit: async event => {
      await wait(() => !existsSync(marker));
      await writeFile(marker + '.tmp', JSON.stringify(event));
      await rename(marker + '.tmp', marker);
    },
  };
}

async function client(path, first) {
  const { createConnection } = await import('node:net');
  const socket = createConnection(path); const lines = []; let buf = '';
  socket.on('data', d => { buf += d; let i; while ((i = buf.indexOf('\n')) >= 0) { lines.push(JSON.parse(buf.slice(0, i))); buf = buf.slice(i + 1); } });
  await new Promise(r => socket.once('connect', r));
  socket.write(JSON.stringify(first) + '\n');
  return { lines, write: o => socket.write(JSON.stringify(o) + '\n'), close: () => socket.destroy() };
}
const until = async (f, ms = 30000) => { const t = Date.now(); while (!f()) { if (Date.now() - t > ms) throw Error('timed out'); await new Promise(r => setTimeout(r, 10)); } };
test('the supervisor serves chat: history, send as steering, and stop', { timeout: 30000 }, async t => {
  const f = await fixture(t);                                     // the file's existing supervisor fixture
  await f.wait(() => f.native()?.chat_sock);
  const c = await client(f.native().chat_sock, { after: 0, epoch: null });
  await until(() => c.lines.some(l => l.t === 'ready'));
  const evs = c.lines.filter(l => l.t === 'ev').map(l => l.ev);
  assert.deepEqual(evs.map(e => [e.type, e.message.content[0].text]), [['user', 'hello'], ['assistant', 'hi there']]);
  c.write({ t: 'send', cid: 'c1', text: 'next', steer: true });
  await until(() => f.posted('/prompt'));
  assert.deepEqual([f.posted('/prompt').text, f.posted('/prompt').delivery], ['next', 'steer']);
  await until(() => c.lines.some(l => l.t === 'ack'));            // the ack follows the POST's response
  assert.deepEqual(c.lines.find(l => l.t === 'ack'), { t: 'ack', cid: 'c1', ok: true });
  c.write({ t: 'stop' });
  await until(() => f.posted('/interrupt'));
  await f.emit({ type: 'session.execution.started', data: { sessionID: f.native().sid } });
  await until(() => c.lines.some(l => l.t === 'state' && l.working === true));
  c.close();
  assert.ok(!String(f.output()).includes('"t":"'), 'no feed frame leaked onto the TUI');
});

test('a stop from any door reaches chat as interrupted, and a failed turn as an error', { timeout: 30000 }, async t => {
  const f = await fixture(t);
  await f.wait(() => f.native()?.chat_sock);
  const c = await client(f.native().chat_sock, { after: 0, epoch: null });
  await until(() => c.lines.some(l => l.t === 'ready'));
  const sid = f.native().sid;
  const results = () => c.lines.filter(l => l.t === 'ev' && l.ev.type === 'result').map(l => l.ev.subtype);
  await f.emit({ type: 'session.execution.started', data: { sessionID: sid } });
  await f.emit({ type: 'session.execution.interrupted', data: { sessionID: sid } });
  await until(() => results().length === 1);
  await f.emit({ type: 'session.execution.started', data: { sessionID: sid } });
  await f.emit({ type: 'session.execution.failed', data: { sessionID: sid } });
  await until(() => results().length === 2);
  assert.deepEqual(results(), ['interrupted', 'error_during_execution']);
  c.close();
});

test('one failed history read neither leaves chat connecting forever nor freezes it',{ timeout: 30000 }, async t => {
  const f = await fixture(t, { flakyMessages: true });
  await f.wait(() => f.native()?.chat_sock);
  const c = await client(f.native().chat_sock, { after: 0, epoch: null });
  await until(() => c.lines.some(l => l.t === 'ready'));
  await until(() => c.lines.filter(l => l.t === 'ev').length >= 2);
  assert.deepEqual(c.lines.filter(l => l.t === 'ev').map(l => l.ev.message.content[0].text), ['hello', 'hi there']);
  c.close();
});

const res = async (c, rid) => { await until(() => c.lines.some(l => l.t === 'res' && l.rid === rid)); return c.lines.find(l => l.t === 'res' && l.rid === rid); };
const asksNow = c => c.lines.findLast(l => l.t === 'asks')?.asks ?? [];
const form = id => ({ id, sessionID: 'ses_fixture', title: 'Questions', metadata: { kind: 'question' }, fields: [
  { key: 'q0', type: 'string', title: 'Color', description: 'Pick a color', options: [{ value: 'Red', label: 'Red' }, { value: 'Blue', label: 'Blue' }], custom: true },
  { key: 'q1', type: 'multiselect', title: 'Fruits', description: 'Pick fruits', options: [{ value: 'Apple', label: 'Apple' }, { value: 'Pear', label: 'Pear' }], custom: true }] });
test('question forms become cards, answers post the form reply, and settled forms drop', { timeout: 30000 }, async t => {
  const f = await fixture(t);
  await f.wait(() => f.native()?.chat_sock);
  const c = await client(f.native().chat_sock, { after: 0, epoch: null });
  await until(() => c.lines.some(l => l.t === 'ready'));
  await f.emit({ type: 'form.created', data: { form: form('frm_1') } });
  await until(() => asksNow(c).length === 1);
  assert.deepEqual(asksNow(c)[0], { id: 'frm_1', kind: 'question', tool: 'opencode', questions: [
    { id: 'q0', header: 'Color', question: 'Pick a color', multi: false, other: true, secret: false, options: [{ label: 'Red', description: '' }, { label: 'Blue', description: '' }] },
    { id: 'q1', header: 'Fruits', question: 'Pick fruits', multi: true, other: true, secret: false, options: [{ label: 'Apple', description: '' }, { label: 'Pear', description: '' }] }] });
  await until(() => c.lines.some(l => l.t === 'state' && l.waiting === true));
  c.write({ t: 'req', rid: 1, op: 'answer', id: 'frm_1', answer: { answers: { q0: { choices: ['Blue'] }, q1: { choices: ['Apple'], text: 'Kiwi' } } } });
  assert.equal((await res(c, 1)).ok, true);
  assert.deepEqual(f.posted('/form/frm_1/reply'), { answer: { q0: 'Blue', q1: ['Apple', 'Kiwi'] } });
  await f.emit({ type: 'form.replied', data: { id: 'frm_1', sessionID: f.native().sid, answer: {} } });
  await until(() => asksNow(c).length === 0);
  await f.emit({ type: 'form.created', data: { form: form('frm_done') } });
  await until(() => asksNow(c).length === 1);
  c.write({ t: 'req', rid: 2, op: 'answer', id: 'frm_done', answer: { answers: { q0: { choices: ['Red'] }, q1: { choices: ['Pear'] } } } });
  assert.deepEqual(await res(c, 2), { t: 'res', rid: 2, ok: false, error: 'already answered' });
  await until(() => asksNow(c).length === 0);
  c.close();
});
test('pickers and the menu use the session API, and listed slash commands run as commands', { timeout: 30000 }, async t => {
  const f = await fixture(t);
  await f.wait(() => f.native()?.chat_sock);
  const c = await client(f.native().chat_sock, { after: 0, epoch: null });
  await until(() => c.lines.some(l => l.t === 'ready'));
  c.write({ t: 'req', rid: 1, op: 'menu' });
  assert.deepEqual((await res(c, 1)).data, { commands: [{ name: 'review', description: 'Review changes' }],
    models: [{ value: 'fixture/model', label: 'Model', variants: ['high'] }, { value: 'fixture/other', label: 'Other', variants: [] }],
    efforts: [], agents: [{ value: 'build', label: 'build' }, { value: 'plan', label: 'plan' }] });
  c.write({ t: 'req', rid: 2, op: 'set', what: 'agent', value: 'plan' }); await res(c, 2);
  assert.deepEqual(f.posted('/agent'), { agent: 'plan' });
  c.write({ t: 'req', rid: 3, op: 'set', what: 'agent', value: 'explore' });
  assert.deepEqual(await res(c, 3), { t: 'res', rid: 3, ok: false, error: 'unknown agent' });
  c.write({ t: 'req', rid: 4, op: 'set', what: 'variant', value: 'high' }); await res(c, 4);
  assert.deepEqual(f.posted('/model'), { model: { providerID: 'fixture', id: 'model', variant: 'high' } });
  c.write({ t: 'req', rid: 5, op: 'set', what: 'model', value: 'fixture/other' }); await res(c, 5);
  assert.deepEqual(f.posted('/model'), { model: { providerID: 'fixture', id: 'other' } });
  c.write({ t: 'send', cid: 'c1', text: '/review src', steer: false });
  await until(() => f.posted('/command'));
  assert.deepEqual(f.posted('/command'), { name: 'review', text: 'src' });
  await until(() => c.lines.some(l => l.t === 'ack' && l.cid === 'c1'));
  assert.deepEqual(c.lines.find(l => l.t === 'ack' && l.cid === 'c1'), { t: 'ack', cid: 'c1', ok: true, done: true });
  c.write({ t: 'send', cid: 'c2', text: '/unknown x', steer: false });
  await until(() => f.posted('/prompt')?.text === '/unknown x');
  await f.emit({ type: 'session.agent.selected', data: { sessionID: f.native().sid, agent: 'plan' } });
  await until(() => c.lines.some(l => l.t === 'settings' && l.settings.agent === 'plan'));
  c.close();
});
test('a card for a form settled while the bridge restarts is dropped by the new bridge', { timeout: 30000 }, async t => {
  const f = await fixture(t);
  await f.wait(() => f.native()?.chat_sock);
  const c = await client(f.native().chat_sock, { after: 0, epoch: null });
  await until(() => c.lines.some(l => l.t === 'ready'));
  await f.emit({ type: 'form.created', data: { form: form('frm_1') } });
  await until(() => asksNow(c).length === 1);
  await writeFile(join(f.dir, 'fail-msg'), '1');                // the next history read fails: the bridge stops and is replaced
  await f.emit({ type: 'session.execution.succeeded', data: { sessionID: f.native().sid } });
  await until(() => asksNow(c).length === 0);                    // GET form lists nothing: settled in the TUI meanwhile
  c.close();
});

const PNG = Buffer.concat([Buffer.from('89504e470d0a1a0a', 'hex'), Buffer.from('row')]), PDF = Buffer.from('%PDF-1.7 tiny');
const sha = b => createHash('sha256').update(b).digest('hex');
test('chat sends OpenCode images and PDFs as file parts and other files as paths', { timeout: 20000 }, async t => {
  const f = await fixture(t);
  await f.wait(() => f.native()?.chat_sock);
  const c = await client(f.native().chat_sock, { after: 0, epoch: null });
  await until(() => c.lines.some(l => l.t === 'ready'));
  const file = (path, kind, mime) => ({ path, kind, mime, name: path.split('/').pop(), size: 8 });
  c.write({ t: 'send', cid: 'c1', text: 'look', steer: false, files: [file('/u/up/a b.png', 'image', 'image/png'),
    file('/u/up/r.pdf', 'file', 'application/pdf'), file('/u/up/n.txt', 'file', 'application/octet-stream')] });
  await until(() => f.posted('/prompt'));
  assert.equal(f.posted('/prompt').text, 'look\nAttached: /u/up/n.txt');
  assert.deepEqual(f.posted('/prompt').files, [{ uri: 'file:///u/up/a%20b.png', name: 'a b.png' }, { uri: 'file:///u/up/r.pdf', name: 'r.pdf' }]);
  c.close();
});
test('a user row with files reaches chat as refs and chips, without its base64', { timeout: 20000 }, async t => {
  const f = await fixture(t, { withFiles: true });
  await f.wait(() => f.native()?.chat_sock);
  const c = await client(f.native().chat_sock, { after: 0, epoch: null });
  await until(() => c.lines.some(l => l.t === 'ev' && l.ev.message?.id === 'msg_f'));
  const ev = c.lines.find(l => l.t === 'ev' && l.ev.message?.id === 'msg_f').ev;
  assert.equal(ev.message.content[0].text, 'see');
  assert.deepEqual(ev.files, [{ kind: 'image', ref: sha(PNG), mime: 'image/png' }, { kind: 'file', name: 'r.pdf', size: PDF.length }, { kind: 'file', name: 'n.txt' }]);
  assert.ok(!JSON.stringify(c.lines).includes(PNG.toString('base64')));
  c.close();
});
test('an unwritable media folder or a malformed data part gives chips, and the session lives', { timeout: 20000 }, async t => {
  const { mkdtemp: mk } = await import('node:fs/promises');
  const blocker = join(await mk(join(tmpdir(), 'cdash-oc-blk-')), 'media');
  await writeFile(blocker, 'a file where the folder should be');
  t.after(() => rm(join(blocker, '..'), { recursive: true, force: true }));
  for (const [withFiles, mediaDir, expected] of [
    [true, blocker, [{ kind: 'file', name: 'a.png', size: PNG.length }, { kind: 'file', name: 'r.pdf', size: PDF.length }, { kind: 'file', name: 'n.txt' }]],
    ['bad', null, [{ kind: 'file', name: 'x.png', size: 3 }, { kind: 'file', name: 'file', size: 0 }, { kind: 'file', name: 'file', size: 0 }, { kind: 'file', name: 'file', size: 0 }, { kind: 'file', name: 'file', size: 0 }, { kind: 'file', name: 'n.txt' }]],
  ]) {
    const f = await fixture(t, { withFiles, mediaDir });
    await f.wait(() => f.native()?.chat_sock);
    const c = await client(f.native().chat_sock, { after: 0, epoch: null });
    await until(() => c.lines.some(l => l.t === 'ev' && l.ev.message?.id === 'msg_f'));
    assert.deepEqual(c.lines.find(l => l.t === 'ev' && l.ev.message?.id === 'msg_f').ev.files, expected);
    await until(() => c.lines.some(l => l.t === 'ready'));
    c.close();
  }
});

const four = [{ id: 'msg_01', type: 'user', text: 'hello' }, { id: 'msg_02', type: 'assistant', time: { completed: 1 }, content: [{ type: 'text', text: 'hi there' }] },
  { id: 'msg_03', type: 'user', text: 'second' }, { id: 'msg_04', type: 'assistant', time: { completed: 2 }, content: [{ type: 'text', text: 'ok' }] }];
test('OpenCode rewinds: stage from chat, show it as staged with Undo, and follow reverts made anywhere', { timeout: 20000 }, async t => {
  const f = await fixture(t, { rows: four });
  await f.wait(() => f.native()?.chat_sock);
  const c = await client(f.native().chat_sock, { after: 0, epoch: null });
  await until(() => c.lines.some(l => l.t === 'ready'));
  const u2 = c.lines.find(l => l.t === 'ev' && l.ev.message?.id === 'msg_03').ev.uuid;
  c.write({ t: 'req', rid: 1, op: 'rewind', uuid: u2, files: true });
  await until(() => c.lines.some(l => l.rid === 1));
  assert.deepEqual(f.posted('/revert/stage'), { messageID: 'msg_03', files: true });
  assert.deepEqual(c.lines.find(l => l.rid === 1).data, { text: 'second' });
  let mark = c.lines.length;
  await f.emit({ type: 'session.revert.staged', data: { sessionID: 'ses_fixture', revert: { messageID: 'msg_03' } } });
  await until(() => c.lines.slice(mark).some(l => l.t === 'ready'));
  const staged = c.lines.slice(mark);
  assert.equal(staged[0].t, 'hello');
  assert.deepEqual(staged.filter(l => l.t === 'ev').map(l => l.ev.subtype ?? l.ev.message.content[0].text), ['hello', 'hi there', 'staged']);
  assert.equal(staged.find(l => l.ev?.subtype === 'staged').ev.messages, 2);
  c.write({ t: 'req', rid: 2, op: 'rewind_undo' });
  await until(() => f.deleted('/revert'));
  mark = c.lines.length;
  await f.emit({ type: 'session.revert.cleared', data: { sessionID: 'ses_fixture' } });
  await until(() => c.lines.slice(mark).some(l => l.t === 'ready'));
  assert.equal(c.lines.slice(mark).filter(l => l.t === 'ev').length, 4, 'everything is back; no staged row');
  c.close();
});
