import test from 'node:test';
import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { mkdtemp, mkdir, writeFile, readFile, rm, stat, rename } from 'node:fs/promises';
import { readFileSync, existsSync } from 'node:fs';
import { createConnection } from 'node:net';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import { createHash } from 'node:crypto';

// Bounded: a supervisor that ignores SIGTERM gets SIGKILL, so teardown cannot hang.
function stopped(child) {
  if (child.exitCode !== null || child.signalCode !== null) return Promise.resolve();
  const exit = new Promise(resolve => child.once('exit', resolve));
  child.kill('SIGTERM');
  const force = setTimeout(() => child.kill('SIGKILL'), 5000);
  return exit.finally(() => clearTimeout(force));
}

async function fixture(t, failure = '', platform = 'linux', resuming = false, { history } = {}) {
  const dir = await mkdtemp(join(tmpdir(), 'cdash-native-test-'));
  const state = join(dir, 'state.json'), wire = join(dir, 'wire.jsonl');
  await mkdir(join(dir,'controls','cdash-test'),{recursive:true});
  await writeFile(state, JSON.stringify({ dir, native_terminal: true, ...(resuming || failure.startsWith('remote-once') ? { sid: '01234567-1234-1234-1234-0123456789ab', rc_unavailable: true } : {}), model: 'gpt-check', effort: 'low', display_name: '[Codex] project · elephant' }));
  await writeFile(wire, '');
  const header = `#!${process.execPath}
const fs=require('node:fs');
const log=m=>fs.appendFileSync(process.env.CDASH_CHECK_WIRE,JSON.stringify(m)+'\\n');
const credential=!!process.env.CLAUDE_CODE_SESSION_ACCESS_TOKEN;
`;
  await writeFile(join(dir, 'codex'), header + `
const args=process.argv.slice(2);
if(args[0]==='app-server'){
 log({kind:'backend',pid:process.pid,args,credential,token:process.env.CDASH_CODEX_TOKEN});
 if(process.env.CDASH_CHECK_FAILURE==='backend')process.exit(1);
 const listen=args[args.indexOf('--listen')+1], path=listen.replace('unix://','');
 if(listen.startsWith('ws://'))require('node:net').createServer().listen(0,'127.0.0.1',function(){console.error('codex app-server (WebSockets)\\n  listening on: ws://127.0.0.1:'+this.address().port)});
 else require('node:net').createServer().listen(path,()=>fs.chmodSync(path,0o600));
}else{
 log({kind:'tui',pid:process.pid,args,credential,token:process.env.CDASH_CODEX_TOKEN});
 if(process.env.CDASH_CHECK_FAILURE==='tui')process.exit(1);
 console.log('NATIVE_TUI_READY');
 process.stdin.on('data',b=>{log({kind:'native-input',text:String(b)});if(String(b).includes('quit'))process.exit(0)});
 setInterval(()=>{},1000);
}
`, { mode: 0o700 });
  await writeFile(join(dir, 'agent'), header + `
log({kind:'proxy',pid:process.pid,args:process.argv.slice(2),credential,token:process.env.CDASH_CODEX_TOKEN});
setInterval(()=>{
 const marker=process.env.CDASH_CHECK_ROOT+'/native-event';
 if(fs.existsSync(marker)) {
   const events=JSON.parse(fs.readFileSync(marker,'utf8'));fs.unlinkSync(marker);
   for(const event of events)process.stdout.write(JSON.stringify(event)+'\\n');
 }
},30);
require('node:readline').createInterface({input:process.stdin}).on('line',line=>{
 const m=JSON.parse(line);log({rpc:m});
 const send=result=>process.stdout.write(JSON.stringify({id:m.id,result})+'\\n');
 if(m.method==='initialize')send({});
 if(m.method==='account/read')send({account:{type:'apiKey'},requiresOpenaiAuth:false});
 if(['thread/start','thread/resume'].includes(m.method))send({thread:{id:'01234567-1234-1234-1234-0123456789ab',cwd:process.env.CDASH_CHECK_ROOT},model:'gpt-check',reasoningEffort:'low'});
 if(m.method==='thread/read')send({thread:{id:m.params.threadId,parentThreadId:'01234567-1234-1234-1234-0123456789ab',agentNickname:'Scout'}});
 if(m.method==='thread/name/set')send({});
 if(m.method==='thread/revert')send({thread:{id:'01234567-1234-1234-1234-0123456789ab',turns:[]}});
 if(m.method==='thread/turns/list'&&process.env.CDASH_CHECK_HISTORY)send(JSON.parse(process.env.CDASH_CHECK_HISTORY));
 else if(m.method==='thread/turns/list')send({data:[{id:'t1',items:[
   {id:'u1',type:'userMessage',content:[{type:'text',text:'hello'},{type:'image',url:'data:image/png;base64,'+Buffer.concat([Buffer.from('89504e470d0a1a0a','hex'),Buffer.from('hist')]).toString('base64')}]},
   {id:'a1',type:'agentMessage',text:'hi there',phase:'final'}]}],nextCursor:null});
 if(['turn/steer','turn/interrupt','thread/settings/update','thread/compact/start','review/start'].includes(m.method))send({});
 if(m.method==='model/list')send({data:[{model:'gpt-check',displayName:'GPT Check',hidden:false,supportedReasoningEfforts:[{reasoningEffort:'low'},{reasoningEffort:'high'}],defaultReasoningEffort:'low'},{model:'gpt-mini',displayName:'GPT Mini',hidden:false,supportedReasoningEfforts:[{reasoningEffort:'low'}],defaultReasoningEffort:'low'}],nextCursor:null});
 if(m.method==='skills/list')send({data:[{cwd:'/p',errors:[],skills:[{name:'lint',description:'Run lint',enabled:true,path:'/p/.codex/skills/lint/SKILL.md',scope:'repo'}]}]});
 if(m.method==='turn/start')send({turn:{id:m.params.threadId==='side-1'?'st1':'t3',status:'inProgress'}});
 if(m.method==='thread/fork'){const go=()=>fs.existsSync(process.env.CDASH_CHECK_ROOT+'/hold-fork')?setTimeout(go,30):send({thread:{id:'side-1'}});go();}
 if(['thread/inject_items','thread/unsubscribe'].includes(m.method))send({});
});
`, { mode: 0o700 });
  await writeFile(join(dir, 'claude'), header + `
log({kind:'remote',pid:process.pid,args:process.argv.slice(2)});
if(process.env.CDASH_CHECK_FAILURE.startsWith('remote-once') && !fs.existsSync(process.env.CDASH_CHECK_ROOT+'/attempted')) {
 fs.writeFileSync(process.env.CDASH_CHECK_ROOT+'/attempted','1');process.exit(process.env.CDASH_CHECK_FAILURE==='remote-once-zero'?0:1);
}
if(process.env.CDASH_CHECK_FAILURE==='remote-always')process.exit(1);
fs.writeFileSync(process.env.CDASH_CODEX_STATE.replace(/\\.json$/,'' )+'.link','https://claude.ai/code/session_test');
console.log('REMOTE_LOG_ONLY');
setInterval(()=>{const marker=process.env.CDASH_CHECK_ROOT+'/stop-remote';if(fs.existsSync(marker)){const code=fs.readFileSync(marker,'utf8')==='0'?0:1;fs.unlinkSync(marker);process.exit(code)}},30);
`, { mode: 0o700 });
  // The transport depends on the platform, so pin it: this suite also runs on Termux.
  const pin = `data:text/javascript,${encodeURIComponent(`Object.defineProperty(process,'platform',{value:'${platform}'})`)}`;
  const child = spawn(process.execPath, ['--import', pin, resolve('crates/agent/src/collect/codex-remote.mjs'), '--state', state, '--name', 'cdash-test'], {
    env: { ...process.env, PATH: `${dir}:${process.env.PATH}`, CDASH_CHECK_ROOT: dir, CDASH_CHECK_WIRE: wire,
      CDASH_CHECK_FAILURE: failure, ...(history ? { CDASH_CHECK_HISTORY: JSON.stringify(history) } : {}), CDASH_MEDIA_DIR: join(dir, 'media'), CDASH_CODEX_AGENT: join(dir, 'agent'), CLAUDE_CODE_SESSION_ACCESS_TOKEN: 'test-must-not-leak' },
    stdio: ['pipe','pipe','pipe'],
  });
  let output = '';
  child.stdout.on('data',b=>{output+=b;});
  child.stderr.on('data',b=>{output+=b;});
  const messages = async () => (await readFile(wire,'utf8')).trim().split('\n').filter(Boolean).map(JSON.parse);
  const alive = pid => { try { process.kill(pid,0); return true; } catch { return false; } };
  t.after(async () => {
    // The supervisor writes into `dir` while it shuts down: rm only once it has exited.
    if (child.exitCode === null && child.signalCode === null) { child.stdin.end(); await stopped(child); }
    for (const m of await messages()) if (m.pid && alive(m.pid)) process.kill(m.pid,'SIGKILL');
    await rm(dir,{recursive:true,force:true});
  });
  const wait = async check => {
    for(let i=0;i<1000;i++) { if(await check())return; await new Promise(r=>setTimeout(r,30)); }
    throw Error(`Native launcher condition timed out: ${output}`);
  };
  const native = () => { try { return JSON.parse(readFileSync(state.replace(/\.json$/, '.native'), 'utf8')); } catch { return undefined; } };
  const marker = join(dir, 'native-event');
  // One notification per marker file, consumed before the next is written.
  const inject = async notification => {
    await wait(() => !existsSync(marker));
    await writeFile(marker + '.tmp', JSON.stringify([notification]));
    await rename(marker + '.tmp', marker);
    await wait(() => !existsSync(marker));
  };
  const rpc = method => readFileSync(wire, 'utf8').trim().split('\n').filter(Boolean).map(JSON.parse)
    .filter(m => m.rpc?.method === method).at(-1)?.rpc;
  return { dir, state, messages, child, wait, alive, output:()=>output, native, inject, rpc };
}

async function client(path, first) {
  const socket = createConnection(path); const lines = [], sizes = []; let buf = '';
  socket.on('data', d => { buf += d; let i; while ((i = buf.indexOf('\n')) >= 0) { sizes.push(i); lines.push(JSON.parse(buf.slice(0, i))); buf = buf.slice(i + 1); } });
  await new Promise(r => socket.once('connect', r));
  socket.write(JSON.stringify(first) + '\n');
  return { lines, sizes, write: o => socket.write(JSON.stringify(o) + '\n'), close: () => socket.destroy() };
}
const SID = '01234567-1234-1234-1234-0123456789ab';
const res = async (c, rid) => { await until(() => c.lines.some(l => l.t === 'res' && l.rid === rid)); return c.lines.find(l => l.t === 'res' && l.rid === rid); };
const asksNow = c => c.lines.findLast(l => l.t === 'asks')?.asks ?? [];
const until = async (f, ms = 30000) => { const t = Date.now(); while (!f()) { if (Date.now() - t > ms) throw Error('timed out'); await new Promise(r => setTimeout(r, 10)); } };

test('native launcher owns one backend, isolates credentials, and preserves TUI after RC exit', {timeout:30000}, async t => {
  const f=await fixture(t);
  await f.wait(()=>f.output().includes('NATIVE_TUI_READY'));
  const messages=await f.messages();
  for(const kind of ['backend','proxy','tui']) {
    assert.equal(messages.filter(m=>m.kind===kind).length,1,kind);
    assert.equal(messages.find(m=>m.kind===kind).credential,false,`${kind} receives no ingress credential`);
  }
  const backend=messages.find(m=>m.kind==='backend'), tui=messages.find(m=>m.kind==='tui');
  assert.equal(tui.args[0],'resume');
  assert.ok(tui.args.includes('tui.fullscreen_transcript=true'),'the TUI must own transcript scrolling');
  assert.ok(tui.args.includes('01234567-1234-1234-1234-0123456789ab'));
  const endpoint=backend.args[backend.args.indexOf('--listen')+1];
  assert.equal(tui.args[tui.args.indexOf('--remote')+1],endpoint);
  const path=endpoint.replace('unix://','');
  assert.equal((await stat(join(path,'..'))).mode & 0o777,0o700);
  assert.ok(messages.some(m=>m.rpc?.method==='thread/name/set' && m.rpc.params.name==='[Codex] project · elephant'));
  assert.equal(f.output().includes('REMOTE_LOG_ONLY'),false,'RC logs cannot overwrite the TUI');
  await f.wait(async()=> (await f.messages()).some(m=>m.kind==='remote'));
  await writeFile(join(f.dir,'stop-remote'),'stop');
  const remote=(await f.messages()).find(m=>m.kind==='remote');
  await f.wait(()=>!f.alive(remote.pid));
  assert.equal(f.alive(tui.pid),true);
  assert.equal(f.alive(backend.pid),true);
  await f.wait(async()=>{try{await stat(f.state.replace(/\.json$/,'')+'.link');return false;}catch{return true;}});
  await writeFile(join(f.dir,'native-event.tmp'),JSON.stringify([
    {method:'thread/settings/updated',params:{threadSettings:{model:'gpt-new',effort:'high'}}},
    {method:'turn/started',params:{turn:{id:'offline'}}},
    {method:'item/completed',params:{item:{type:'agentMessage',text:'Offline native answer'}}},
    {method:'turn/completed',params:{turn:{id:'offline',status:'completed'}}},
  ]));
  await rename(join(f.dir,'native-event.tmp'),join(f.dir,'native-event'));
  await f.wait(async()=>{
    const native=JSON.parse(await readFile(f.state.replace(/\.json$/,'.native'),'utf8'));
    return native.model==='gpt-new' && native.effort==='high' && !native.working && native.last_message==='Offline native answer';
  });
  f.child.stdin.write('quit\n');
  await f.wait(()=>f.child.exitCode!==null);
  assert.equal(f.child.exitCode,0);
  await f.wait(async()=> (await f.messages()).filter(m=>m.pid).every(m=>!f.alive(m.pid)));
  await assert.rejects(stat(path),{code:'ENOENT'});
});

test('Android native launcher reaches Codex over token-guarded loopback', {timeout:30000}, async t => {
  const f=await fixture(t,'','android');
  await f.wait(()=>f.output().includes('NATIVE_TUI_READY'));
  const messages=await f.messages();
  const [backend,tui,proxy]=['backend','tui','proxy'].map(kind=>messages.find(m=>m.kind===kind));
  const at=(m,flag)=>m.args[m.args.indexOf(flag)+1];
  assert.equal(at(backend,'--listen'),'ws://127.0.0.1:0');
  assert.equal(at(backend,'--ws-auth'),'capability-token');
  const tokenFile=at(backend,'--ws-token-file'), token=await readFile(tokenFile,'utf8');
  assert.match(token,/^[0-9a-f]{64}$/);
  assert.equal((await stat(tokenFile)).mode & 0o777,0o600);
  assert.equal(backend.token,undefined,'Codex tools never see the token');
  const endpoint=at(tui,'--remote');
  assert.match(endpoint,/^ws:\/\/127\.0\.0\.1:\d+$/);
  assert.notEqual(endpoint,'ws://127.0.0.1:0');
  assert.equal(at(tui,'--remote-auth-token-env'),'CDASH_CODEX_TOKEN');
  assert.equal(tui.token,token);
  assert.deepEqual(proxy.args,['codex-proxy',endpoint]);
  assert.equal(proxy.token,token);
  assert.match(await readFile(join(f.dir,'controls','cdash-test','native.log'),'utf8'),/listening on: ws:/);
  f.child.stdin.write('quit\n');
  await f.wait(()=>f.child.exitCode!==null);
  assert.equal(f.child.exitCode,0);
  await assert.rejects(stat(tokenFile),{code:'ENOENT'});
});

for(const failure of ['backend','tui']) test(`native ${failure} failure cleans up its session processes`,{timeout:30000},async t=>{
  const f=await fixture(t,failure);
  await f.wait(()=>f.child.exitCode!==null);
  assert.notEqual(f.child.exitCode,0);
  await f.wait(async()=> (await f.messages()).filter(m=>m.pid).every(m=>!f.alive(m.pid)));
});

for(const exitCode of [0,1]) test(`resumed native session retries Remote Control startup exit ${exitCode} without replacing its thread or TUI`, {timeout:30000}, async t=>{
  const f=await fixture(t,exitCode===0?'remote-once-zero':'remote-once');
  await f.wait(async()=> (await f.messages()).some(m=>m.kind==='remote'));
  assert.equal(JSON.parse(await readFile(f.state.replace(/\.json$/,'.native'),'utf8')).rc_unavailable,false,'resume clears the previous outage while connecting');
  await f.wait(async()=> (await f.messages()).filter(m=>m.kind==='remote').length===2);
  await f.wait(async()=>{try{await stat(f.state.replace(/\.json$/,'')+'.link');return true;}catch{return false;}});
  const messages=await f.messages();
  assert.equal(messages.filter(m=>m.kind==='backend').length,1);
  assert.equal(messages.filter(m=>m.kind==='tui').length,1);
  assert.equal(messages.filter(m=>m.rpc?.method==='thread/start').length,0);
  assert.equal(messages.find(m=>m.rpc?.method==='thread/resume').rpc.params.threadId,'01234567-1234-1234-1234-0123456789ab');
  assert.equal(messages.some(m=>m.rpc?.method==='turn/start'),false,'reconnection must not submit a prompt');
  const log=await readFile(join(f.dir,'controls','cdash-test','native.log'),'utf8');
  assert.match(log,new RegExp(`exit ${exitCode}.*retry`,'i'));
  f.child.stdin.write('quit\n');
  await f.wait(()=>f.child.exitCode!==null);
  await f.wait(async()=> (await f.messages()).filter(m=>m.pid).every(m=>!f.alive(m.pid)));
});

test('Remote Control keeps retrying after the Terminal fallback and shutdown cancels pending retries', {timeout:30000}, async t=>{
  const f=await fixture(t,'remote-always');
  const logPath=join(f.dir,'controls','cdash-test','native.log');
  await f.wait(async()=> (await f.messages()).some(m=>m.kind==='remote'));
  await f.wait(async()=> (await readFile(logPath,'utf8')).includes('failure 1 '));
  assert.equal(JSON.parse(await readFile(f.state.replace(/\.json$/,'.native'),'utf8')).rc_unavailable,false,'a pending retry still waits for a link');
  await f.wait(async()=> (await readFile(logPath,'utf8')).includes('failure 4 '));
  assert.equal(JSON.parse(await readFile(f.state.replace(/\.json$/,'.native'),'utf8')).rc_unavailable,true,'repeated failures publish the Terminal fallback');
  const messages=await f.messages();
  assert.equal(messages.filter(m=>m.kind==='remote').length,4,'initial launch plus three retries');
  const failures=(await readFile(logPath,'utf8')).split('\n').filter(line=>line.includes('Remote Control failure'));
  assert.equal(failures.length,4,'every failed attempt is logged exactly once');
  failures.forEach((line,i)=>{
    assert.match(line,new RegExp(`^\\d{4}-\\d{2}-\\d{2}T\\S+ Remote Control failure ${i+1} \\(exit 1, signal none\\); retrying in ${1000*2**i}ms;`));
  });
  assert.ok(messages.filter(m=>m.kind==='backend'||m.kind==='tui').every(m=>f.alive(m.pid)));
  assert.equal((await stat(join(f.dir,'controls','cdash-test','remote-debug.log'))).mode & 0o777,0o600);
  f.child.stdin.write('quit\n');
  await f.wait(()=>f.child.exitCode!==null);

  const closing=await fixture(t,'remote-always');
  await closing.wait(async()=> (await closing.messages()).some(m=>m.kind==='remote'));
  await closing.wait(async()=> (await readFile(join(closing.dir,'controls','cdash-test','native.log'),'utf8')).includes('retrying'));
  closing.child.stdin.write('quit\n');
  await closing.wait(()=>closing.child.exitCode!==null);
  await new Promise(r=>setTimeout(r,1200));
  assert.equal((await closing.messages()).filter(m=>m.kind==='remote').length,1);
});

test('an established Remote Control connection resets the retry backoff', {timeout:30000}, async t=>{
  const f=await fixture(t);
  const link=f.state.replace(/\.json$/,'')+'.link';
  const logPath=join(f.dir,'controls','cdash-test','native.log');
  for(let i=1;i<=5;i++){
    await f.wait(async()=> (await f.messages()).filter(m=>m.kind==='remote').length===i);
    await f.wait(async()=>{try{await stat(link);return true;}catch{return false;}});
    await writeFile(join(f.dir,'stop-remote'),'1');
    await f.wait(async()=> (await readFile(logPath,'utf8')).split('\n').filter(line=>line.includes('Remote Control failure')).length===i);
  }
  const failures=(await readFile(logPath,'utf8')).split('\n').filter(line=>line.includes('Remote Control failure'));
  assert.ok(failures.every(line=>line.includes('failure 1 ') && line.includes('retrying in 1000ms')),'each drop after a connection starts the backoff over');
  assert.equal(JSON.parse(await readFile(f.state.replace(/\.json$/,'.native'),'utf8')).rc_unavailable,false,'drops spread over time never exhaust a budget');
  await f.wait(async()=> (await f.messages()).filter(m=>m.kind==='remote').length===6);
  f.child.stdin.write('quit\n');
  await f.wait(()=>f.child.exitCode!==null);
  await f.wait(async()=> (await f.messages()).filter(m=>m.pid).every(m=>!f.alive(m.pid)));
});

for(const exitCode of [0,1]) test(`Remote Control reconnect after exit ${exitCode} continues its existing session without incompatible spawn flags`, {timeout:30000}, async t=>{
  const f=await fixture(t);
  const link=f.state.replace(/\.json$/,'')+'.link';
  await f.wait(async()=>{try{await stat(link);return true;}catch{return false;}});
  await writeFile(join(f.dir,'stop-remote'),String(exitCode));
  await f.wait(async()=> (await f.messages()).filter(m=>m.kind==='remote').length===2);
  const reconnect=(await f.messages()).filter(m=>m.kind==='remote')[1];
  assert.ok(reconnect.args.includes('--continue'));
  assert.equal(reconnect.args.includes('--spawn=session'),false);
  f.child.stdin.write('quit\n');
  await f.wait(()=>f.child.exitCode!==null);
  await f.wait(async()=> (await f.messages()).filter(m=>m.pid).every(m=>!f.alive(m.pid)));
});

for (const platform of ['linux', 'android']) for (const resuming of [false, true]) {
  test('native Codex ' + platform + ' ' + (resuming ? 'resume' : 'start') + ' sets full access on the thread without saved config', { timeout: 30000 }, async t => {
    const f = await fixture(t, '', platform, resuming);
    await f.wait(() => f.output().includes('NATIVE_TUI_READY'));
    const messages = await f.messages();
    const method = resuming ? 'thread/resume' : 'thread/start';
    const params = messages.find(message => message.rpc?.method === method).rpc.params;
    assert.equal(params.approvalPolicy, 'never');
    assert.equal(params.sandbox, 'danger-full-access');
    // `codex resume --remote` exits with "Permission overrides are not supported" if given a bypass flag.
    assert.ok(!messages.find(message => message.kind === 'tui').args.includes('--dangerously-bypass-approvals-and-sandbox'));
    f.child.stdin.write('quit\n');
    await f.wait(() => f.child.exitCode !== null);
    assert.equal(f.child.exitCode, 0);
  });
}

test('the native supervisor serves chat: history, live events, send, steer and stop', { timeout: 30000 }, async t => {
  const f = await fixture(t, undefined, process.platform, true);       // resuming, so history is replayed
  await f.wait(() => f.native()?.chat_sock);                      // read <name>.native, as the other tests read it
  const c = await client(f.native().chat_sock, { after: 0, epoch: null });
  await until(() => c.lines.some(l => l.t === 'ready'));
  // The fake's thread/turns/list returns one user message and one agent message.
  assert.deepEqual(c.lines.filter(l => l.t === 'ev').map(l => l.ev.type), ['user', 'assistant']);
  await f.inject({ method: 'turn/started', params: { turn: { id: 't2' } } });
  await f.inject({ method: 'item/started', params: { turnId: 't2', item: { id: 'i1', type: 'commandExecution', command: 'ls', cwd: '/p', status: 'inProgress' } } });
  await f.inject({ method: 'item/agentMessage/delta', params: { turnId: 't2', itemId: 'i2', delta: 'Do' } });
  await f.inject({ method: 'item/completed', params: { turnId: 't2', item: { id: 'i1', type: 'commandExecution', command: 'ls', cwd: '/p', status: 'completed', exitCode: 0, aggregatedOutput: 'a\nb' } } });
  await f.inject({ method: 'item/completed', params: { turnId: 't2', item: { id: 'i2', type: 'agentMessage', text: 'Done', phase: 'final' } } });
  await until(() => c.lines.filter(l => l.t === 'ev').length >= 5);
  const evs = c.lines.filter(l => l.t === 'ev').slice(2).map(l => l.ev);
  assert.deepEqual(evs.map(e => e.message.content[0].type), ['tool_use', 'tool_result', 'text']);
  assert.equal(evs[2].message.id, 'i2');
  assert.deepEqual(c.lines.find(l => l.t === 'delta'), { t: 'delta', id: 'i2', text: 'Do' });
  assert.ok(c.lines.some(l => l.t === 'state' && l.working === true));
  c.write({ t: 'send', cid: 'c1', text: 'steer me', steer: true });
  await until(() => f.rpc('turn/steer'));
  assert.equal(f.rpc('turn/steer').params.expectedTurnId, 't2');
  assert.equal(f.rpc('turn/steer').params.input[0].text, 'steer me');
  await f.inject({ method: 'turn/completed', params: { turn: { id: 't2', status: 'interrupted' } } });
  await until(() => c.lines.some(l => l.t === 'ev' && l.ev.type === 'result'));
  assert.equal(c.lines.findLast(l => l.t === 'ev').ev.subtype, 'interrupted');
  c.write({ t: 'send', cid: 'c2', text: 'fresh', steer: false });
  await until(() => f.rpc('turn/start'));
  assert.equal(f.rpc('turn/start').params.input[0].text, 'fresh');
  c.close();
  assert.ok(!String(f.output()).includes('"t":"'), 'no feed frame leaked onto the TUI');
});

test('a subagent thread never reaches the conversation, the working state or the turn id', { timeout: 30000 }, async t => {
  const f = await fixture(t, undefined, process.platform, true);
  await f.wait(() => f.native()?.chat_sock);
  const c = await client(f.native().chat_sock, { after: 0, epoch: null });
  await until(() => c.lines.some(l => l.t === 'ready'));
  const main = '01234567-1234-1234-1234-0123456789ab', child = '89abcdef-0000-4000-8000-000000000000';
  await f.inject({ method: 'turn/started', params: { threadId: main, turn: { id: 't2' } } });
  await f.inject({ method: 'turn/started', params: { threadId: child, turn: { id: 'c1' } } });
  await f.inject({ method: 'item/completed', params: { threadId: child, turnId: 'c1', item: { id: 'ci', type: 'agentMessage', text: 'child says', phase: 'final' } } });
  await f.inject({ method: 'turn/completed', params: { threadId: child, turn: { id: 'c1', status: 'completed' } } });
  await f.inject({ id: 77, method: 'item/tool/requestUserInput', params: { threadId: child, turnId: 'c1', itemId: 'q', questions: [] } });
  await until(() => c.lines.some(l => l.t === 'state' && l.waiting === true));   // a subagent's question still needs an answer
  assert.ok(!c.lines.some(l => l.t === 'ev' && JSON.stringify(l.ev).includes('child says')));
  assert.ok(!c.lines.some(l => l.t === 'ev' && l.ev.type === 'result'), "the child's turn end is not the main turn's");
  assert.equal(c.lines.findLast(l => l.t === 'state').working, true);
  c.write({ t: 'send', cid: 'c1', text: 'steer me', steer: true });
  await until(() => f.rpc('turn/steer'));
  assert.equal(f.rpc('turn/steer').params.expectedTurnId, 't2');
  c.close();
});
test('questions from any thread become cards; an answer is the server-request response; resolution drops them', { timeout: 20000 }, async t => {
  const f = await fixture(t);
  await f.wait(() => f.native()?.chat_sock);
  const c = await client(f.native().chat_sock, { after: 0, epoch: null });
  await until(() => c.lines.some(l => l.t === 'ready'));
  const q = { id: 'q_file', header: 'File', question: 'Which file?', isOther: true, isSecret: false, options: [{ label: 'a.txt', description: 'A' }, { label: 'b.txt', description: 'B' }] };
  await f.inject({ id: 41, method: 'item/tool/requestUserInput', params: { threadId: 'child-thread', turnId: 't9', itemId: 'i9', isBlocking: true, questions: [q] } });
  await until(() => asksNow(c).length === 1);
  assert.deepEqual(asksNow(c)[0], { id: '41', kind: 'question', tool: 'codex', questions: [{ id: 'q_file', header: 'File', question: 'Which file?', multi: false, other: true, secret: false, options: [{ label: 'a.txt', description: 'A' }, { label: 'b.txt', description: 'B' }] }] });
  c.write({ t: 'req', rid: 1, op: 'answer', id: '41', answer: { answers: { q_file: { choices: ['b.txt'], text: 'keep it short' } } } });
  assert.equal((await res(c, 1)).ok, true);
  const reply = (await f.messages()).find(m => m.rpc?.id === 41 && m.rpc.result);
  assert.deepEqual(reply.rpc.result, { answers: { q_file: { answers: ['b.txt', 'user_note: keep it short'] } } });
  await until(() => asksNow(c).length === 0);
  await f.inject({ id: 42, method: 'item/tool/requestUserInput', params: { threadId: SID, turnId: 't9', itemId: 'i10', isBlocking: true, questions: [{ ...q, isOther: false }] } });
  await until(() => asksNow(c).length === 1);
  await f.inject({ method: 'serverRequest/resolved', params: { threadId: SID, requestId: 42 } });
  await until(() => asksNow(c).length === 0);
  c.write({ t: 'req', rid: 2, op: 'answer', id: '42', answer: { answers: { q_file: { choices: ['a.txt'] } } } });
  assert.deepEqual(await res(c, 2), { t: 'res', rid: 2, ok: false, error: 'already answered' });
  c.close();
});

const asyncQuestionId = index => JSON.stringify(['request_user_input_async', 'call_question', index]);
const asyncReply = (index, question, answer) => ({
  type: 'text',
  text: '<send_user_message_question_reply>\n' + JSON.stringify([{ answer, question, questionItemId: asyncQuestionId(index) }]) + '\n</send_user_message_question_reply>',
});

test('Codex async question cards steer an active turn and accept a follow-up after it finishes', { timeout: 20000 }, async t => {
  const f = await fixture(t);
  await f.wait(() => f.native()?.chat_sock);
  const c = await client(f.native().chat_sock, { after: 0, epoch: null });
  t.after(c.close);
  const event = (method, params) => f.inject({ method, params: { threadId: SID, turnId: 't2', ...params } });
  const title = 'Which environment?';
  await event('turn/started', { turn: { id: 't2' } });
  await event('item/completed', { item: { type: 'agentMessage', id: 'call_question', delivery: 'async', phase: 'final', text: title, questions: [
    { title, options: ['Staging', 'Production'] }, { title: 'Which audience?', options: null }, { title: 'Which date?', options: null },
  ] } });
  await until(() => asksNow(c).length === 3, 2000);
  assert.deepEqual(asksNow(c)[0], { id: asyncQuestionId(0), kind: 'question', tool: 'codex', questions: [{
    id: 'answer', question: title, other: true, options: [{ label: 'Staging' }, { label: 'Production' }],
  }] });
  assert.equal(c.lines.findLast(l => l.t === 'state').waiting, false);

  const answer = async (index, picks) => {
    c.write({ t: 'req', rid: index + 1, op: 'answer', id: asyncQuestionId(index), answer: { answers: { answer: picks } } });
    return res(c, index + 1);
  };
  assert.equal((await answer(0, { choices: ['Staging'] })).ok, true);
  assert.equal(f.rpc('turn/steer').params.expectedTurnId, 't2');
  assert.equal(f.rpc('turn/steer').params.input[0].text, asyncReply(0, title, 'Staging').text);

  await event('item/completed', { item: { type: 'userMessage', id: 'terminal-answer', content: [asyncReply(1, 'Which audience?', 'Customers')] } });
  await until(() => asksNow(c).length === 1);
  assert.equal(asksNow(c)[0].id, asyncQuestionId(2));
  assert.deepEqual(await answer(1, { text: 'Someone else' }), { t: 'res', rid: 2, ok: false, error: 'already answered' });
  const message = () => c.lines.find(l => l.t === 'ev' && l.ev.message?.id === 'terminal-answer');
  await until(message);
  assert.equal(message().ev.message.content[0].text, 'Which audience?\n\nCustomers');

  await event('turn/completed', { turn: { id: 't2', status: 'completed' } });
  await until(() => c.lines.findLast(l => l.t === 'state').working === false);
  const reopened = await client(f.native().chat_sock, { after: 0, epoch: null });
  t.after(reopened.close);
  await until(() => asksNow(reopened).length === 1);
  assert.equal((await answer(2, { choices: [], text: 'Tomorrow' })).ok, true);
  assert.equal(f.rpc('turn/start').params.input[0].text, asyncReply(2, 'Which date?', 'Tomorrow').text);
  await until(() => asksNow(reopened).length === 0);
});

test('Codex history displays the answer from a reply envelope and preserves a prompt quoting it', { timeout: 20000 }, async t => {
  const reply = asyncReply(0, 'Which environment?', 'Staging');
  const quoted = 'Please inspect this reply:\n' + reply.text;
  const history = { data: [{ id: 't1', items: [
    { id: 'u1', type: 'userMessage', content: [reply] },
    { id: 'u2', type: 'userMessage', content: [{ type: 'text', text: quoted }] },
  ] }], nextCursor: null };
  const f = await fixture(t, '', 'linux', true, { history });
  await f.wait(() => f.native()?.chat_sock);
  const c = await client(f.native().chat_sock, { after: 0, epoch: null });
  t.after(c.close);
  await until(() => c.lines.some(l => l.t === 'ready'));
  const prompts = c.lines.filter(l => l.t === 'ev' && l.ev.type === 'user');
  assert.deepEqual(prompts.map(l => l.ev.message.content[0].text), ['Which environment?\n\nStaging', quoted]);
});

test('the plan switch, pickers, menu, slash commands and Implement this plan? drive the app server', { timeout: 20000 }, async t => {
  const f = await fixture(t);
  await f.wait(() => f.native()?.chat_sock);
  const c = await client(f.native().chat_sock, { after: 0, epoch: null });
  await until(() => c.lines.some(l => l.t === 'ready'));
  c.write({ t: 'req', rid: 1, op: 'set', what: 'plan', value: true });
  assert.equal((await res(c, 1)).ok, true);
  assert.deepEqual(f.rpc('thread/settings/update').params, { threadId: SID, collaborationMode: { mode: 'plan', settings: { model: 'gpt-check', reasoning_effort: 'low', developer_instructions: null } } });
  await until(() => c.lines.some(l => l.t === 'settings' && l.settings.plan === true));
  c.write({ t: 'req', rid: 2, op: 'set', what: 'effort', value: 'high' });
  await res(c, 2);
  assert.deepEqual(f.rpc('thread/settings/update').params, { threadId: SID, effort: 'high' });
  c.write({ t: 'req', rid: 3, op: 'set', what: 'effort', value: 'ultra' });
  assert.deepEqual(await res(c, 3), { t: 'res', rid: 3, ok: false, error: 'unknown effort' });
  c.write({ t: 'req', rid: 4, op: 'menu' });
  const menu = (await res(c, 4)).data;
  assert.deepEqual(menu.commands.map(x => x.name), ['compact', 'review', 'plan', 'model', 'effort', 'lint']);
  assert.deepEqual([menu.models, menu.efforts, menu.agents], [[{ value: 'gpt-check', label: 'GPT Check', efforts: ['low', 'high'] }, { value: 'gpt-mini', label: 'GPT Mini', efforts: ['low'] }], ['low', 'high'], []]);
  await f.inject({ method: 'turn/started', params: { threadId: SID, turn: { id: 't5' } } });
  await f.inject({ method: 'item/completed', params: { threadId: SID, turnId: 't5', item: { id: 'p1', type: 'plan', text: '# Plan\n1. x' } } });
  await f.inject({ method: 'turn/completed', params: { threadId: SID, turn: { id: 't5', status: 'completed' } } });
  await until(() => asksNow(c).length === 1);
  assert.deepEqual(asksNow(c)[0], { id: 'implement:t5', kind: 'plan', tool: 'codex', plan: '# Plan\n1. x' });
  c.write({ t: 'req', rid: 5, op: 'answer', id: 'implement:t5', answer: { choice: 'yes' } });
  await res(c, 5);
  assert.equal(f.rpc('thread/settings/update').params.collaborationMode.mode, 'default');
  assert.equal(f.rpc('turn/start').params.input[0].text, 'Implement the plan.');
  for (const [cid, text] of [['c1', '/compact'], ['c2', '/review'], ['c3', '/lint fix it'], ['c4', '/diff']]) c.write({ t: 'send', cid, text, steer: false });
  await until(() => c.lines.filter(l => l.t === 'ack').length === 4);
  assert.ok(f.rpc('thread/compact/start'));
  assert.deepEqual(f.rpc('review/start').params, { threadId: SID, target: { type: 'uncommittedChanges' } });
  assert.deepEqual(f.rpc('turn/start').params.input, [{ type: 'skill', name: 'lint', path: '/p/.codex/skills/lint/SKILL.md' }, { type: 'text', text: 'fix it', text_elements: [] }]);
  assert.deepEqual(c.lines.find(l => l.t === 'ack' && l.cid === 'c4'), { t: 'ack', cid: 'c4', ok: false, error: 'Use Terminal for this command' });
  c.close();
});
test('a path-like prompt is a prompt; set model falls back to the model default effort; no to the plan card keeps the thread in plan mode', { timeout: 20000 }, async t => {
  const f = await fixture(t);
  await f.wait(() => f.native()?.chat_sock);
  const c = await client(f.native().chat_sock, { after: 0, epoch: null });
  await until(() => c.lines.some(l => l.t === 'ready'));
  c.write({ t: 'send', cid: 'p1', text: '/usr/bin/x broke, why?', steer: false });
  await until(() => c.lines.some(l => l.t === 'ack' && l.cid === 'p1'));
  assert.deepEqual(c.lines.find(l => l.t === 'ack' && l.cid === 'p1'), { t: 'ack', cid: 'p1', ok: true });
  assert.equal(f.rpc('turn/start').params.input[0].text, '/usr/bin/x broke, why?');
  assert.equal(f.rpc('skills/list'), undefined, 'no extra round trip');
  c.write({ t: 'req', rid: 1, op: 'set', what: 'effort', value: 'high' });
  await res(c, 1);
  c.write({ t: 'req', rid: 2, op: 'set', what: 'model', value: 'gpt-mini' });
  assert.equal((await res(c, 2)).ok, true);
  assert.deepEqual(f.rpc('thread/settings/update').params, { threadId: SID, model: 'gpt-mini', effort: 'low' });
  await until(() => c.lines.some(l => l.t === 'settings' && l.settings.model === 'gpt-mini' && l.settings.effort === 'low'));
  await f.inject({ method: 'turn/started', params: { threadId: SID, turn: { id: 't6' } } });
  await f.inject({ method: 'item/completed', params: { threadId: SID, turnId: 't6', item: { id: 'p2', type: 'plan', text: '# P' } } });
  await f.inject({ method: 'turn/completed', params: { threadId: SID, turn: { id: 't6', status: 'completed' } } });
  await until(() => asksNow(c).length === 1);
  const before = f.rpc('turn/start');
  c.write({ t: 'req', rid: 3, op: 'answer', id: 'implement:t6', answer: { choice: 'no' } });
  assert.equal((await res(c, 3)).ok, true);
  await until(() => asksNow(c).length === 0);
  assert.equal(f.rpc('turn/start').id, before.id, 'no prompt sent');
  c.close();
});

const PNG = Buffer.concat([Buffer.from('89504e470d0a1a0a', 'hex'), Buffer.from('hist')]);  // also the fixture's u1 image
const sha = b => createHash('sha256').update(b).digest('hex');
test('chat sends Codex images as data URLs and other files as paths, within its image budget', { timeout: 20000 }, async t => {
  const f = await fixture(t);
  await f.wait(() => f.native()?.chat_sock);
  const c = await client(f.native().chat_sock, { after: 0, epoch: null });
  await until(() => c.lines.some(l => l.t === 'ready'));
  const png = join(f.dir, 'a.png'); await writeFile(png, PNG);
  const image = { path: png, kind: 'image', mime: 'image/png', name: 'a.png', size: PNG.length };
  c.write({ t: 'send', cid: 'c1', text: 'look', steer: false, files: [image, { path: '/u/up/r.pdf', kind: 'file', mime: 'application/pdf', name: 'r.pdf', size: 9 }] });
  await until(() => f.rpc('turn/start'));
  assert.deepEqual(f.rpc('turn/start').params.input, [{ type: 'text', text: 'look\nAttached: /u/up/r.pdf', text_elements: [] },
    { type: 'image', url: 'data:image/png;base64,' + PNG.toString('base64') }]);
  const big = { ...image, size: 13 * 1024 * 1024 };
  c.write({ t: 'send', cid: 'c2', text: 'two big ones', steer: false, files: [big, big] });
  await until(() => c.lines.some(l => l.t === 'ack' && l.cid === 'c2'));
  assert.deepEqual(c.lines.find(l => l.t === 'ack' && l.cid === 'c2'), { t: 'ack', cid: 'c2', ok: false, error: 'too many images for one Codex message' });
  c.close();
});
test('image prompts from the native TUI and from history reach chat as cache refs, never as bytes', { timeout: 20000 }, async t => {
  const f = await fixture(t, undefined, process.platform, true);
  await f.wait(() => f.native()?.chat_sock);
  const c = await client(f.native().chat_sock, { after: 0, epoch: null });
  await until(() => c.lines.some(l => l.t === 'ready'));
  const history = c.lines.find(l => l.t === 'ev' && l.ev.type === 'user').ev;
  assert.deepEqual([history.message.content[0].text, history.files], ['hello', [{ kind: 'image', ref: sha(PNG), mime: 'image/png' }]]);
  const huge = Buffer.concat([PNG, Buffer.alloc(300 * 1024)]);
  const up = '/home/u/.cache/cdash/uploads/cdash-test/0123456789abcdef';
  await f.inject({ method: 'item/completed', params: { turnId: 't2', item: { id: 'u2', type: 'userMessage', content: [
    { type: 'text', text: `see\nAttached: ${up}/r.pdf` }, { type: 'image', url: 'data:image/png;base64,' + huge.toString('base64') },
    { type: 'localImage', path: join(f.dir, 'gone.png') }] } } });
  await until(() => c.lines.some(l => l.t === 'ev' && l.ev.message?.id === 'u2'));
  const ev = c.lines.find(l => l.t === 'ev' && l.ev.message?.id === 'u2').ev;
  assert.equal(ev.message.content[0].text, 'see');
  assert.deepEqual(ev.files, [{ kind: 'image', ref: sha(huge), mime: 'image/png' }, { kind: 'file', name: 'gone.png' }, { kind: 'file', name: 'r.pdf' }]);
  assert.ok(existsSync(join(f.dir, 'media', sha(huge) + '.png')));
  assert.ok(c.sizes.every(n => n < 64 * 1024), 'no line on the agent socket carries image bytes');
  c.close();
});
test('a media folder that cannot be written, or a bad data URL, still shows the message and keeps the supervisor up', { timeout: 20000 }, async t => {
  const f = await fixture(t);
  await f.wait(() => f.native()?.chat_sock);
  const c = await client(f.native().chat_sock, { after: 0, epoch: null });
  await until(() => c.lines.some(l => l.t === 'ready'));
  await writeFile(join(f.dir, 'media'), 'a file, so mkdir under it fails');   // CDASH_MEDIA_DIR is this path
  await f.inject({ method: 'item/completed', params: { turnId: 't2', item: { id: 'u3', type: 'userMessage', content: [
    { type: 'text', text: 'see' }, { type: 'image', url: 'data:image/png;base64,' + PNG.toString('base64') },
    { type: 'image', url: 'data:image/png;base64' }, { type: 'image', url: 'data:text/plain;base64,aGk=' }] } } });
  await until(() => c.lines.some(l => l.t === 'ev' && l.ev.message?.id === 'u3'));
  assert.deepEqual(c.lines.find(l => l.t === 'ev' && l.ev.message?.id === 'u3').ev.files, [{ kind: 'file', name: 'image' }, { kind: 'file', name: 'image' }, { kind: 'file', name: 'image' }]);
  assert.ok(f.alive(f.child.pid), 'the supervisor is still running');
  c.close();
});

const steered = { data: [
  { id: 't1', startedAt: 1789000000, items: [{ id: 'u1', type: 'userMessage', content: [{ type: 'text', text: 'hello' }] }, { id: 'a1', type: 'agentMessage', text: 'hi', phase: 'final' }] },
  { id: 't2', startedAt: 1789000100, items: [{ id: 'u2', type: 'userMessage', content: [{ type: 'text', text: 'second' }] },
    { id: 'u3', type: 'userMessage', content: [{ type: 'text', text: 'steer me' }] }, { id: 'a2', type: 'agentMessage', text: 'ok', phase: 'final' }] },
], nextCursor: null };
test('Codex rewinds: steers are marked, chat reverts and restarts the TUI, any revert resets chat', { timeout: 30000 }, async t => {
  const f = await fixture(t, '', process.platform, true, { history: steered });
  await f.wait(() => f.native()?.chat_sock);
  const c = await client(f.native().chat_sock, { after: 0, epoch: null });
  await until(() => c.lines.some(l => l.t === 'ready'));
  const ev = id => c.lines.find(l => l.t === 'ev' && l.ev.message?.id === id).ev;
  assert.deepEqual([ev('u2').src, ev('u3').src], [undefined, 'steer']);
  await f.wait(async () => (await f.messages()).some(m => m.kind === 'tui')); // a TUI killed before it starts never logs
  c.write({ t: 'req', rid: 1, op: 'rewind', uuid: ev('u3').uuid, files: false });
  c.write({ t: 'req', rid: 2, op: 'rewind', uuid: ev('u2').uuid, files: true });
  await until(() => c.lines.some(l => l.rid === 2));
  assert.equal(c.lines.find(l => l.rid === 1).ok, false, 'a steer cannot be rewound');
  assert.equal(f.rpc('thread/revert').params.beforeTurnId, 't2');
  assert.deepEqual(c.lines.find(l => l.rid === 2).data, { text: 'second' });
  await f.wait(async () => (await f.messages()).filter(m => m.kind === 'tui').length === 2);
  const [old] = (await f.messages()).filter(m => m.kind === 'tui');
  assert.equal(f.alive(old.pid), false);
  assert.equal(f.child.exitCode, null, 'the supervisor keeps running');
  const before = c.lines.length;
  await f.inject({ method: 'thread/reverted', params: { threadId: '01234567-1234-1234-1234-0123456789ab' } });
  await until(() => c.lines.slice(before).some(l => l.t === 'ready'));
  const after = c.lines.slice(before);
  assert.equal(after[0].t, 'hello');
  assert.notEqual(after[0].epoch, c.lines[0].epoch);
  assert.equal(after.find(l => l.t === 'ev').seq, 1);
  assert.equal(after.find(l => l.t === 'ev' && l.ev.message?.id === 'u3').ev.src, 'steer', 'a reset keeps steers marked');
  const [first, second] = (await f.messages()).filter(m => m.kind === 'tui');
  assert.deepEqual(second.args, first.args, 'the restarted TUI connects the same way');
  f.child.stdin.write('quit\n');
  await f.wait(() => f.child.exitCode !== null);
});

test('Codex refuses to rewind while a turn is running', { timeout: 30000 }, async t => {
  const f = await fixture(t, '', process.platform, true, { history: steered });
  await f.wait(() => f.native()?.chat_sock);
  const c = await client(f.native().chat_sock, { after: 0, epoch: null });
  await until(() => c.lines.some(l => l.t === 'ready'));
  await f.inject({ method: 'turn/started', params: { turn: { id: 't9' } } });
  const uuid = c.lines.find(l => l.t === 'ev' && l.ev.message?.id === 'u2').ev.uuid;
  c.write({ t: 'req', rid: 1, op: 'rewind', uuid, files: false });
  const r = await res(c, 1);
  assert.deepEqual([r.ok, r.error], [false, 'Stop the turn or wait for it to finish']);
  assert.equal(f.rpc('thread/revert'), undefined);
});

test('Codex facts: usage, running shells and agents, thinking, checklist and times', { timeout: 20000 }, async t => {
  const sid = '01234567-1234-1234-1234-0123456789ab';
  const f = await fixture(t, '', process.platform, true, { history: { data: [{ id: 't1', startedAt: 1789000000, items: [
    { id: 'u1', type: 'userMessage', content: [{ type: 'text', text: 'hello' }] }, { id: 'r1', type: 'reasoning', summary: ['Thought about it'], content: [] }] }], nextCursor: null } });
  await f.wait(() => f.native()?.chat_sock);
  const c = await client(f.native().chat_sock, { after: 0, epoch: null });
  await until(() => c.lines.some(l => l.t === 'ready'));
  const evs = () => c.lines.filter(l => l.t === 'ev').map(l => l.ev);
  assert.equal(evs()[0].ts, new Date(1789000000 * 1000).toISOString());
  assert.deepEqual(evs()[1].message.content[0], { type: 'thinking', thinking: 'Thought about it' });
  await f.inject({ method: 'thread/tokenUsage/updated', params: { threadId: sid, turnId: 't1', tokenUsage: {
    total: { totalTokens: 900, inputTokens: 700, cachedInputTokens: 100, cacheWriteInputTokens: 0, outputTokens: 200, reasoningOutputTokens: 0 },
    last: { totalTokens: 300, inputTokens: 250, cachedInputTokens: 0, cacheWriteInputTokens: 0, outputTokens: 50, reasoningOutputTokens: 0 }, modelContextWindow: 272000 } } });
  await until(() => c.lines.some(l => l.t === 'usage' && l.usage.context));
  assert.deepEqual(c.lines.findLast(l => l.t === 'usage').usage, { cwd: f.dir, model: 'gpt-check', context: { used: 300, window: 272000 },
    tokens: { input: 700, output: 200, cache_read: 100, cache_write: 0 }, cost: null });
  await f.inject({ method: 'item/started', params: { threadId: sid, turnId: 't2', item: { id: 'x1', type: 'commandExecution', source: 'unifiedExecStartup', command: 'npm run dev', status: 'inProgress' } } });
  await f.inject({ method: 'thread/status/changed', params: { threadId: 'child-1', status: { type: 'active', activeFlags: [] } } });
  await until(() => c.lines.findLast(l => l.t === 'tasks')?.tasks.length === 2);
  assert.deepEqual(c.lines.findLast(l => l.t === 'tasks').tasks.map(x => [x.id, x.kind, x.title]), [['x1', 'shell', 'npm run dev'], ['child-1', 'agent', 'Scout']]);
  for (const x of c.lines.findLast(l => l.t === 'tasks').tasks) assert.ok(typeof x.started === 'string' && !isNaN(Date.parse(x.started)), x.started);
  await until(() => f.native()?.tasks === 2); // the dashboard card reads this
  await f.inject({ method: 'item/completed', params: { threadId: sid, turnId: 't2', item: { id: 'x1', type: 'commandExecution', source: 'unifiedExecStartup', command: 'npm run dev', status: 'completed', exitCode: 0 } } });
  await f.inject({ method: 'thread/status/changed', params: { threadId: 'child-1', status: { type: 'idle' } } });
  await until(() => c.lines.findLast(l => l.t === 'tasks').tasks.length === 0);
  await until(() => f.native()?.tasks === 0);
  await f.inject({ method: 'turn/plan/updated', params: { threadId: sid, turnId: 't2', explanation: null, plan: [{ step: 'Read', status: 'completed' }, { step: 'Fix', status: 'inProgress' }] } });
  await until(() => evs().some(e => e.subtype === 'todo'));
  assert.deepEqual(evs().find(e => e.subtype === 'todo').items, [{ text: 'Read', status: 'completed' }, { text: 'Fix', status: 'in_progress' }]);
});

test('a reread drops shell tasks and keeps agent tasks', { timeout: 20000 }, async t => {
  const sid = '01234567-1234-1234-1234-0123456789ab';
  const f = await fixture(t, '', process.platform, true, { history: steered });
  await f.wait(() => f.native()?.chat_sock);
  const c = await client(f.native().chat_sock, { after: 0, epoch: null });
  await until(() => c.lines.some(l => l.t === 'ready'));
  await f.inject({ method: 'item/started', params: { threadId: sid, turnId: 't2', item: { id: 'x1', type: 'commandExecution', source: 'unifiedExecStartup', command: 'npm run dev', status: 'inProgress' } } });
  await f.inject({ method: 'thread/status/changed', params: { threadId: 'child-1', status: { type: 'active', activeFlags: [] } } });
  await until(() => c.lines.findLast(l => l.t === 'tasks')?.tasks.length === 2);
  await f.inject({ method: 'thread/reverted', params: { threadId: sid } });
  await until(() => c.lines.findLast(l => l.t === 'tasks').tasks.length === 1);
  assert.deepEqual(c.lines.findLast(l => l.t === 'tasks').tasks.map(x => [x.id, x.kind]), [['child-1', 'agent']]);
});

test('a revert drops the plan card, a long task title is clipped, and one answered question of two keeps waiting', { timeout: 20000 }, async t => {
  const sid = '01234567-1234-1234-1234-0123456789ab';
  const f = await fixture(t, '', process.platform, true, { history: steered });
  await f.wait(() => f.native()?.chat_sock);
  const c = await client(f.native().chat_sock, { after: 0, epoch: null });
  await until(() => c.lines.some(l => l.t === 'ready'));
  await f.inject({ method: 'turn/started', params: { threadId: sid, turn: { id: 't5' } } });
  await f.inject({ method: 'item/completed', params: { threadId: sid, turnId: 't5', item: { id: 'p1', type: 'plan', text: '# Plan' } } });
  await f.inject({ method: 'turn/completed', params: { threadId: sid, turn: { id: 't5', status: 'completed' } } });
  await until(() => asksNow(c).length === 1);
  await f.inject({ method: 'thread/tokenUsage/updated', params: { threadId: sid, tokenUsage: { total: { inputTokens: 9, outputTokens: 1, cachedInputTokens: 0 }, last: { totalTokens: 5000 }, modelContextWindow: 100000 } } });
  await until(() => c.lines.findLast(l => l.t === 'usage')?.usage.context?.used === 5000);
  await f.inject({ method: 'thread/reverted', params: { threadId: sid } });
  await until(() => asksNow(c).length === 0);
  await until(() => c.lines.findLast(l => l.t === 'usage').usage.context === null);
  assert.equal(c.lines.findLast(l => l.t === 'usage').usage.tokens.input, 9, 'tokens spent stay spent');
  await f.inject({ method: 'item/started', params: { threadId: sid, turnId: 't6', item: { id: 'x1', type: 'commandExecution', source: 'unifiedExecStartup', command: 'cat <<EOF\n' + 'y'.repeat(500), status: 'inProgress' } } });
  await until(() => c.lines.findLast(l => l.t === 'tasks')?.tasks.length === 1);
  assert.equal(c.lines.findLast(l => l.t === 'tasks').tasks[0].title.length, 80);
  await f.inject({ id: 81, method: 'item/tool/requestUserInput', params: { threadId: sid, turnId: 't6', itemId: 'q1', questions: [] } });
  await f.inject({ id: 82, method: 'item/tool/requestUserInput', params: { threadId: sid, turnId: 't6', itemId: 'q2', questions: [] } });
  await until(() => c.lines.some(l => l.t === 'state' && l.waiting === true));
  await f.inject({ method: 'serverRequest/resolved', params: { threadId: sid, requestId: 81 } });
  await until(() => asksNow(c).length === 1);
  assert.equal(c.lines.findLast(l => l.t === 'state').waiting, true, 'the other question is still open');
  await f.inject({ method: 'serverRequest/resolved', params: { threadId: sid, requestId: 82 } });
  await until(() => c.lines.findLast(l => l.t === 'state').waiting === false);
});

test('a Codex side question forks read-only, streams the answer aside, and cleans up', { timeout: 20000 }, async t => {
  const f = await fixture(t);
  await f.wait(() => f.native()?.chat_sock);
  const c = await client(f.native().chat_sock, { after: 0, epoch: null });
  await until(() => c.lines.some(l => l.t === 'ready'));
  c.write({ t: 'req', rid: 1, op: 'aside', id: 'q1', text: 'what did I ask?' });
  assert.equal((await res(c, 1)).ok, true);
  // The answer can come before the side turn is started; the fake logs requests in order.
  await until(() => f.rpc('turn/start'));
  const fork = f.rpc('thread/fork').params;
  assert.deepEqual([fork.threadId, fork.ephemeral, fork.excludeTurns, fork.sandbox, fork.approvalPolicy, fork.model, fork.config], [SID, true, true, 'read-only', 'never', 'gpt-check', { model_reasoning_effort: 'low' }]);
  const inject = f.rpc('thread/inject_items').params;
  assert.equal(inject.threadId, 'side-1');
  assert.deepEqual([inject.items[0].type, inject.items[0].role, inject.items[0].content[0].type], ['message', 'user', 'input_text']);
  assert.ok(inject.items[0].content[0].text.startsWith('Side conversation boundary.'));
  assert.deepEqual(f.rpc('turn/start').params, { threadId: 'side-1', input: [{ type: 'text', text: 'what did I ask?', text_elements: [] }] });
  // the fake's thread/read names main as every thread's parent: a side thread still never runs as an agent
  await f.inject({ method: 'thread/status/changed', params: { threadId: 'side-1', status: { type: 'active', activeFlags: [] } } });
  await f.inject({ method: 'turn/started', params: { threadId: 'side-1', turn: { id: 'st1' } } });
  await f.inject({ method: 'item/agentMessage/delta', params: { threadId: 'side-1', turnId: 'st1', itemId: 's', delta: 'You asked' } });
  await f.inject({ method: 'item/completed', params: { threadId: 'side-1', turnId: 'st1', item: { id: 's', type: 'agentMessage', text: 'You asked', phase: 'final' } } });
  await f.inject({ method: 'item/agentMessage/delta', params: { threadId: 'side-1', turnId: 'st1', itemId: 's2', delta: 'Also this' } });
  await f.inject({ method: 'turn/completed', params: { threadId: 'side-1', turn: { id: 'st1', status: 'completed' } } });
  await until(() => c.lines.some(l => l.t === 'aside' && l.done));
  assert.deepEqual(c.lines.filter(l => l.t === 'aside'), [{ t: 'aside', id: 'q1', text: 'You asked', done: false },
    { t: 'aside', id: 'q1', text: '\n\nAlso this', done: false }, { t: 'aside', id: 'q1', text: '', done: true }], 'a new message is a new paragraph, as Claude joins text blocks');
  await until(() => f.rpc('thread/unsubscribe'));
  assert.deepEqual(f.rpc('thread/unsubscribe').params, { threadId: 'side-1' });
  // a finished side's late notifications are still the side's: dropped, never read as a subagent
  await f.inject({ method: 'thread/status/changed', params: { threadId: 'side-1', status: { type: 'active', activeFlags: [] } } });
  await f.inject({ method: 'thread/tokenUsage/updated', params: { threadId: SID, tokenUsage: { total: { inputTokens: 1, outputTokens: 1, cachedInputTokens: 0 }, last: { totalTokens: 7 }, modelContextWindow: 100 } } });
  await until(() => c.lines.some(l => l.t === 'usage' && l.usage.context?.used === 7)); // handled in order, so the status above was too
  assert.equal(f.rpc('thread/read'), undefined, 'a side thread is never read as a subagent');
  assert.ok(!c.lines.some(l => l.t === 'ev' || l.t === 'delta'), 'the side answer never reaches the conversation');
  assert.ok(!c.lines.some(l => l.t === 'state' && l.working), 'a side turn is not the main turn');
  assert.ok(!c.lines.some(l => l.t === 'tasks' && l.tasks.length), 'a side question is not a running agent');
  // the side turn did not become the main turn: a steer starts a main turn
  c.write({ t: 'send', cid: 'c1', text: 'main prompt', steer: true });
  await until(() => f.rpc('turn/start')?.params.threadId === SID || f.rpc('turn/steer'));
  assert.equal(f.rpc('turn/steer'), undefined, 'the steer went to the side turn');
  // a second question cancelled mid-turn
  c.write({ t: 'req', rid: 2, op: 'aside', id: 'q2', text: 'and then?' });
  assert.equal((await res(c, 2)).ok, true);
  await until(() => f.rpc('turn/start')?.params.input[0].text === 'and then?');
  // a side thread's server request is refused: no main waiting state, no card
  await f.inject({ id: 88, method: 'item/tool/requestUserInput', params: { threadId: 'side-1', turnId: 'st1', itemId: 'q', questions: [] } });
  await until(() => readFileSync(join(f.dir, 'wire.jsonl'), 'utf8').includes('"id":88,"error"'), 3000); // refused, not left hanging
  assert.ok(!c.lines.some(l => l.t === 'state' && l.waiting), 'a side request never makes the main session wait');
  assert.equal(asksNow(c).length, 0);
  c.write({ t: 'req', rid: 3, op: 'aside_cancel', id: 'q2' });
  assert.equal((await res(c, 3)).ok, true);
  await until(() => f.rpc('turn/interrupt'));
  assert.deepEqual(f.rpc('turn/interrupt').params, { threadId: 'side-1', turnId: 'st1' });
  await until(() => readFileSync(join(f.dir, 'wire.jsonl'), 'utf8').split('"thread/unsubscribe"').length === 3);
  await f.inject({ method: 'turn/completed', params: { threadId: 'side-1', turn: { id: 'st1', status: 'interrupted' } } });
  // notifications are handled in order, so once this usage shows, the line above was handled
  await f.inject({ method: 'thread/tokenUsage/updated', params: { threadId: SID, tokenUsage: { total: { inputTokens: 1, outputTokens: 1, cachedInputTokens: 0 }, last: { totalTokens: 9 }, modelContextWindow: 100 } } });
  await until(() => c.lines.some(l => l.t === 'usage' && l.usage.context?.used === 9));
  assert.ok(!c.lines.some(l => l.t === 'aside' && l.id === 'q2'), 'a cancelled question sends nothing more');
  c.close();
});

test('a Codex side question answers at once, and a cancel during a slow fork closes the fork before any turn', { timeout: 20000 }, async t => {
  const f = await fixture(t);
  await f.wait(() => f.native()?.chat_sock);
  const c = await client(f.native().chat_sock, { after: 0, epoch: null });
  await until(() => c.lines.some(l => l.t === 'ready'));
  await writeFile(join(f.dir, 'hold-fork'), '');
  c.write({ t: 'req', rid: 1, op: 'aside', id: 'q1', text: 'slow?' });
  await until(() => c.lines.some(l => l.t === 'res' && l.rid === 1), 3000); // not after the feed's 10 s op timeout
  assert.equal((await res(c, 1)).ok, true);
  await until(() => f.rpc('thread/fork'));
  c.write({ t: 'req', rid: 2, op: 'aside_cancel', id: 'q1' });
  assert.equal((await res(c, 2)).ok, true);
  await rm(join(f.dir, 'hold-fork'));
  await until(() => f.rpc('thread/unsubscribe'));
  assert.deepEqual(f.rpc('thread/unsubscribe').params, { threadId: 'side-1' });
  assert.equal(f.rpc('thread/inject_items'), undefined);
  assert.equal(f.rpc('turn/start'), undefined, 'a cancelled side question never starts a turn');
  assert.ok(!c.lines.some(l => l.t === 'aside'), 'a cancelled question sends nothing');
  c.close();
});
