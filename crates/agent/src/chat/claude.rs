//! The Claude engine: tails the session transcript and the session state
//! file, and types through tmux.

use super::asks::Cards;
use super::hub::{Answer, Engine, EngineMsg, Sent};
use super::picker;
use super::tap::Tap;
use super::tmux_io;
use crate::collect::ctx::Ctx;
use crate::collect::lookup::session_file_for;
use crate::parse::paths::project_dir_name;
use serde_json::{json, Value};
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncSeekExt};
use tokio::sync::{mpsc, Notify};
use tokio::time::Instant;

/// Most bytes one poll reads, so a multi-megabyte record cannot stall a tick.
const READ_MAX: u64 = 4 * 1024 * 1024;
const TICK: Duration = Duration::from_millis(150);
/// How often an idle session nobody watches has its status file read and its
/// transcript's length checked.
const SLOW_TICK: Duration = Duration::from_secs(1);
/// How often `has-session` checks the pane is still there.
const ALIVE_EVERY: Duration = Duration::from_secs(2);
/// How long after the status file says idle a turn the tap still holds open
/// keeps the fast poll. The turn end lands about 0.2 s after `idle`; a local
/// command such as `/model` opens a turn that never ends.
// ponytail: a turn end written later than this is read at the next slow pass, up to `SLOW_TICK` late.
const TURN_END_GRACE: Duration = Duration::from_millis(2500);

/// Complete lines appended to a file since the last poll.
pub struct Tail {
    path: PathBuf,
    offset: u64,
    carry: Vec<u8>,
    at_end: bool,
}

impl Tail {
    pub fn new(path: PathBuf) -> Self {
        Self { path, offset: 0, carry: Vec::new(), at_end: false }
    }

    /// True when the last poll read to the file's length, or found no file.
    pub fn at_end(&self) -> bool {
        self.at_end
    }

    pub async fn poll(&mut self) -> std::io::Result<Vec<String>> {
        let len = match tokio::fs::metadata(&self.path).await {
            Ok(m) => m.len(),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                self.at_end = true;
                return Ok(Vec::new());
            }
            Err(e) => return Err(e),
        };
        if len < self.offset {
            self.offset = 0;
            self.carry.clear();
        }
        self.at_end = len == self.offset;
        if self.at_end {
            return Ok(Vec::new());
        }
        let mut f = tokio::fs::File::open(&self.path).await?;
        f.seek(std::io::SeekFrom::Start(self.offset)).await?;
        let mut buf = Vec::new();
        f.take(READ_MAX.min(len - self.offset)).read_to_end(&mut buf).await?;
        self.offset += buf.len() as u64;
        self.at_end = self.offset == len;
        self.carry.extend_from_slice(&buf);
        let Some(last) = self.carry.iter().rposition(|&b| b == b'\n') else { return Ok(Vec::new()) };
        let rest = self.carry.split_off(last + 1);
        let done = std::mem::replace(&mut self.carry, rest);
        Ok(done[..last].split(|&b| b == b'\n').map(|l| String::from_utf8_lossy(l).into_owned()).collect())
    }
}

pub struct ClaudeEngine {
    ctx: Arc<Ctx>,
    pid: i32,
    name: String,
    /// The pane was launched with the permission hook and the listener runs.
    pub(crate) hooked: bool,
    live: LiveRef,
    /// The transcript of the conversation the loop follows.
    transcript: Arc<Mutex<Option<PathBuf>>>,
    shared: Arc<Shared>,
}

/// The hook cards and the session's settings, shared by the engine, its loop
/// and the hook listener.
#[derive(Default)]
pub struct Live {
    pub cards: Cards,
    pub plan: Option<bool>,
    pub model: Option<String>,
    pub effort: Option<String>,
    /// The loop's wake-up, so a card shows without waiting out a parked tick.
    wake: Arc<Notify>,
}

pub type LiveRef = Arc<Mutex<Live>>;

impl Live {
    /// Holds a hook request as a card; the receiver yields the decision.
    pub fn hold(&mut self, req: &Value, now: std::time::Instant) -> Option<tokio::sync::oneshot::Receiver<Value>> {
        self.plan = Some(req["permission_mode"] == "plan");
        let rx = self.cards.add(req, now)?;
        self.wake.notify_one();
        Some(rx)
    }
}

fn lock(live: &LiveRef) -> std::sync::MutexGuard<'_, Live> {
    live.lock().unwrap_or_else(|e| e.into_inner())
}

/// The model and effort of one assistant record; `None` for Claude's own
/// `<synthetic>` replies and for lines without a model.
pub fn model_effort(line: &str) -> Option<(String, Option<String>)> {
    let v: Value = serde_json::from_str(line).ok()?;
    let model = v["message"]["model"].as_str().filter(|m| *m != "<synthetic>")?;
    Some((model.to_string(), v["effort"].as_str().map(String::from)))
}

/// What the engine and its loop share.
#[derive(Default)]
struct Shared {
    /// Clients attached to the hub; the loop parks while none are.
    watchers: AtomicUsize,
    wake: Arc<Notify>,
    /// A picker command left in the input box behind a dialog.
    leftover: picker::Leftover,
    /// Transcript polls the loop made.
    #[cfg(test)]
    polls: AtomicUsize,
}

impl Engine for ClaudeEngine {
    async fn send(&self, text: &str, _steer: bool, _cid: &str) -> Result<Sent, String> {
        match picker::route(text) {
            // A paste cannot be confirmed by the TUI; the transcript confirms it.
            picker::Route::Paste => {
                picker::clear_leftover(&self.ctx, &self.name, self.pid, &self.shared.leftover).await;
                tmux_io::paste(&self.ctx.runner, &self.name, text).await.map(|()| Sent::Unconfirmed)
            }
            // The engine ran it itself: no prompt will ever show it.
            picker::Route::Set(what, v) => self.set(what, &v).await.map(|()| Sent::Done),
            picker::Route::Refuse(why) => Err(why),
        }
    }

    async fn menu(&self) -> Value {
        if let Ok(screen) = tmux_io::capture(&self.ctx.runner, &self.name).await {
            if let Some(mode) = picker::footer_mode(&screen) {
                lock(&self.live).plan = Some(mode == "plan");
                self.shared.wake.notify_one();
            }
        }
        self.listed().await
    }

    /// Only values from the session's own lists reach the pane.
    async fn set(&self, what: &str, value: &Value) -> Result<(), String> {
        let (ctx, name, pid) = (&self.ctx, self.name.as_str(), self.pid);
        match what {
            "plan" => {
                let on = value.as_bool().ok_or("plan takes true or false")?;
                picker::switch_plan(ctx, name, pid, on).await?;
                lock(&self.live).plan = Some(on);
            }
            "model" => {
                const NOT_LISTED: &str = "not a model this session offers";
                let alias = value.as_str().ok_or(NOT_LISTED)?;
                let models = self.listed().await["models"].take();
                let entry = models.as_array().and_then(|m| m.iter().find(|e| e["value"] == alias)).ok_or(NOT_LISTED)?;
                picker::type_setting(ctx, name, pid, "model", alias, &self.shared.leftover).await?;
                lock(&self.live).model = entry["model"].as_str().map(String::from);
            }
            "effort" => {
                let effort = value.as_str().filter(|e| picker::EFFORTS.contains(e)).ok_or("not an effort level")?;
                picker::type_setting(ctx, name, pid, "effort", effort, &self.shared.leftover).await?;
                lock(&self.live).effort = Some(effort.to_string());
            }
            _ => return Err("not supported".into()),
        }
        self.shared.wake.notify_one();
        Ok(())
    }

    async fn stop(&self) -> Result<(), String> {
        tmux_io::escape(&self.ctx.runner, &self.name).await
    }

    async fn clear_input(&self, text: &str) {
        if let Err(e) = tmux_io::clear_input(&self.ctx.runner, &self.name, text).await {
            self.ctx.host.log.push(format!("chat clear failed {}: {e}", self.name));
        }
    }

    /// Replays the transcript from its start through a fresh tap, so cuts the
    /// tap makes apply again; reading from an offset would bring back
    /// abandoned branches.
    async fn history(&self, from: u64, to: u64) -> Result<Vec<Value>, String> {
        let path = self.transcript.lock().unwrap_or_else(|e| e.into_inner()).clone().ok_or("no conversation yet")?;
        let (mut tail, mut tap, mut n, mut out) = (Tail::new(path), Tap::default(), 0u64, Vec::new());
        loop {
            for line in tail.poll().await.map_err(|e| e.to_string())? {
                for ev in tap.feed(&line) {
                    n += 1;
                    if n >= from {
                        out.push(ev);
                    }
                    if n >= to {
                        return Ok(out);
                    }
                }
            }
            if tail.at_end() {
                return Ok(out);
            }
        }
    }

    async fn answer(&self, id: &str, a: &Answer) -> Result<(), String> {
        let mut l = lock(&self.live);
        let plan = l.cards.list().iter().any(|c| c["id"] == id && c["kind"] == "plan");
        l.cards.answer(id, a)?;
        if plan && a.choice.as_deref() == Some("yes") {
            // Approving the plan leaves plan mode (the decision sets bypassPermissions).
            l.plan = Some(false);
        }
        drop(l);
        // The card is gone: publish the shorter list now, not at the next tick.
        self.shared.wake.notify_one();
        Ok(())
    }

    fn hooks(&self) -> bool {
        self.hooked
    }

    async fn watchers(&self, n: usize) {
        self.shared.watchers.store(n, Ordering::Relaxed);
        self.shared.wake.notify_one();
    }
}

impl ClaudeEngine {
    pub fn live(&self) -> LiveRef {
        Arc::clone(&self.live)
    }

    /// The menu built from the cached `initialize` of the session's folder.
    async fn listed(&self) -> Value {
        let cwd = session_file_for(&self.ctx.claude_dir, self.pid).await.and_then(|f| f.cwd);
        let init = match cwd {
            Some(cwd) => picker::initialize(&self.ctx, std::path::Path::new(&cwd)).await.ok(),
            None => None,
        };
        init.map_or_else(|| picker::menu_from(&Value::Null), |i| picker::menu_from(&i))
    }
}

pub fn start(ctx: Arc<Ctx>, name: String, pid: i32) -> (ClaudeEngine, mpsc::Receiver<EngineMsg>) {
    let (tx, rx) = mpsc::channel(256);
    let transcript = Arc::new(Mutex::new(None));
    let shared = Arc::new(Shared::default());
    let live = Arc::new(Mutex::new(Live { wake: Arc::clone(&shared.wake), ..Live::default() }));
    let engine = ClaudeEngine {
        ctx: Arc::clone(&ctx),
        pid,
        name: name.clone(),
        hooked: false,
        live: Arc::clone(&live),
        transcript: Arc::clone(&transcript),
        shared: Arc::clone(&shared),
    };
    tokio::spawn(async move {
        run(ctx, name, pid, tx, transcript, shared, &live).await;
        // Held hooks get no decision once nothing follows the session.
        lock(&live).cards.clear();
    });
    (engine, rx)
}

/// The conversation currently followed.
struct Conv {
    sid: String,
    tail: Tail,
    tap: Tap,
    ready: bool,
}

async fn run(
    ctx: Arc<Ctx>,
    name: String,
    pid: i32,
    tx: mpsc::Sender<EngineMsg>,
    transcript: Arc<Mutex<Option<PathBuf>>>,
    shared: Arc<Shared>,
    live: &LiveRef,
) {
    let mut conv: Option<Conv> = None;
    let mut last: Option<(bool, bool, Option<String>)> = None;
    let (mut last_asks, mut last_settings) = (None, None);
    let mut blind_sent = false;
    let mut warmed = false;
    let mut slow = false;
    let mut idle_since: Option<Instant> = None;
    let mut alive_at = Instant::now();
    let target = format!("={name}");
    loop {
        // A client attaching wakes a slow wait at once.
        let _ = tokio::time::timeout(if slow { SLOW_TICK } else { TICK }, shared.wake.notified()).await;
        let file = session_file_for(&ctx.claude_dir, pid).await;
        let status = file.as_ref().and_then(|f| f.status.clone());
        if !matches!(status.as_deref(), Some("idle" | "shell")) {
            idle_since = None;
        } else if idle_since.is_none() {
            idle_since = Some(Instant::now());
        }
        if let Some((sid, cwd)) = file.as_ref().and_then(|f| Some((f.session_id.clone()?, f.cwd.clone()?))) {
            if !warmed {
                // The first menu should find the command list cached, not wait for `claude -p`.
                warmed = true;
                let (ctx, cwd) = (Arc::clone(&ctx), PathBuf::from(&cwd));
                tokio::spawn(async move {
                    let _ = picker::initialize(&ctx, &cwd).await;
                });
            }
            if conv.as_ref().is_none_or(|c| c.sid != sid) {
                // `/clear` or `/resume` gave the pane a new conversation.
                if conv.is_some() && tx.send(EngineMsg::Reset).await.is_err() {
                    return;
                }
                let path = ctx.claude_dir.join("projects").join(project_dir_name(&cwd)).join(format!("{sid}.jsonl"));
                *transcript.lock().unwrap_or_else(|e| e.into_inner()) = Some(path.clone());
                conv = Some(Conv { sid, tail: Tail::new(path), tap: Tap::default(), ready: false });
                last = None;
            }
        }
        if let Some(c) = conv.as_mut() {
            // Nobody watches and the session is idle: the status file is read
            // and the transcript only measured. An open turn keeps the fast
            // poll until its end is read (or `TURN_END_GRACE` passes), so a
            // turn end still notifies at once; a transcript that changed length
            // is read now, so a turn shorter than one slow wait is not missed.
            slow = shared.watchers.load(Ordering::Relaxed) == 0
                && c.ready
                && idle_since.is_some_and(|t| !c.tap.turn_open() || t.elapsed() >= TURN_END_GRACE)
                // No transcript yet (no prompt so far) counts as unchanged.
                && tokio::fs::metadata(&c.tail.path).await.map_or(true, |m| m.len() == c.tail.offset);
            let lines = if slow {
                Vec::new()
            } else {
                #[cfg(test)]
                shared.polls.fetch_add(1, Ordering::Relaxed);
                match c.tail.poll().await {
                    Ok(l) => l,
                    Err(e) => {
                        ctx.host.log.push(format!("chat read failed {name}: {e}"));
                        Vec::new()
                    }
                }
            };
            let idle_read = lines.is_empty();
            {
                let mut l = lock(live);
                lines.iter().for_each(|line| l.cards.seen(line));
                let reply = lines.iter().rev().find(|l| l.contains(r#""type":"assistant""#) && !l.contains(r#""isSidechain":true"#));
                if let Some((model, effort)) = reply.and_then(|r| model_effort(r)) {
                    (l.model, l.effort) = (Some(model), effort);
                }
            }
            for line in lines {
                for ev in c.tap.feed(&line) {
                    if tx.send(EngineMsg::Ev(ev)).await.is_err() {
                        return;
                    }
                }
            }
            if idle_read && c.tail.at_end() && !c.ready {
                c.ready = true;
                if tx.send(EngineMsg::Ready).await.is_err() {
                    return;
                }
            }
            if c.ready {
                let working = match status.as_deref() {
                    Some(s) => s == "busy",
                    None => c.tap.turn_open(),
                };
                let waiting = status.as_deref() == Some("waiting");
                let waiting_for = file.as_ref().and_then(|f| f.waiting_for.clone()).filter(|_| waiting);
                let now = (working, waiting, waiting_for);
                if last.as_ref() != Some(&now) {
                    let (working, waiting, waiting_for) = now.clone();
                    last = Some(now);
                    if tx.send(EngineMsg::Status { working, waiting, waiting_for }).await.is_err() {
                        return;
                    }
                }
                let (asks, settings) = {
                    let mut l = lock(live);
                    l.cards.tick(waiting, std::time::Instant::now());
                    (l.cards.list(), json!({"model":l.model,"effort":l.effort,"plan":l.plan,"agent":null,"variant":null}))
                };
                if last_asks.as_ref() != Some(&asks) {
                    last_asks = Some(asks.clone());
                    if tx.send(EngineMsg::Asks(asks)).await.is_err() {
                        return;
                    }
                }
                if last_settings.as_ref() != Some(&settings) {
                    last_settings = Some(settings.clone());
                    if tx.send(EngineMsg::Settings(settings)).await.is_err() {
                        return;
                    }
                }
            }
            if c.tap.blind() && !blind_sent {
                blind_sent = true;
                if tx.send(EngineMsg::Unreadable).await.is_err() {
                    return;
                }
            }
        }
        if alive_at.elapsed() >= ALIVE_EVERY {
            alive_at = Instant::now();
            if ctx.runner.run_checked("tmux", &["has-session", "-t", &target], "tmux has-session").await.is_err() {
                let _ = tx.send(EngineMsg::Ended).await;
                return;
            }
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::chat::asks::tests::{ask_request, plan_request, result_line, use_line};
    use crate::chat::hub::Answer;
    use crate::chat::tmux_io::tests::{stub_tmux, tempfile_path};
    use crate::host::cmd::Runner;
    use crate::host::log::LogBuffer;
    use serde_json::{json, Value};
    use std::path::Path;
    use std::time::Duration;

    pub(crate) fn append(f: &Path, s: &str) {
        use std::io::Write;
        std::fs::OpenOptions::new().append(true).open(f).unwrap().write_all(s.as_bytes()).unwrap();
    }

    /// Lines `r.start` to `r.end - 1` of the fixture, counting from 1.
    pub(crate) fn fixture_lines(r: std::ops::Range<usize>) -> String {
        let all: Vec<&str> = include_str!("testdata/claude.jsonl").lines().collect();
        all[r.start - 1..r.end - 1].iter().map(|l| format!("{l}\n")).collect()
    }

    pub(crate) fn write_session(dir: &Path, json: &str) {
        let tmp = dir.join("sessions/.4242.json.tmp");
        std::fs::write(&tmp, json).unwrap();
        std::fs::rename(tmp, dir.join("sessions/4242.json")).unwrap();
    }

    pub(crate) fn write_transcript(dir: &Path, sid: &str, lines: &str) {
        let proj = dir.join("projects/-proj");
        std::fs::create_dir_all(&proj).unwrap();
        std::fs::write(proj.join(format!("{sid}.jsonl")), lines).unwrap();
    }

    pub(crate) fn make_tmux_fail(dir: &Path) {
        stub_tmux(dir, "", 1);
    }

    pub(crate) fn ctx_with_path(dir: PathBuf, path: String) -> Arc<Ctx> {
        let log = Arc::new(LogBuffer::new());
        let host = crate::host::init::Host {
            runner: Runner::new(path.clone(), Arc::clone(&log)),
            log,
            path,
            sampler: std::sync::Mutex::new(crate::host::sample::Sampler::new()),
        };
        Arc::new(Ctx::new(host, dir, None))
    }

    /// A claude_dir with `sessions/4242.json` for `sid-1` in `/proj` and its
    /// transcript, and a stub tmux on PATH that answers `answer` and exits 0.
    /// `"fallback"` writes no `status` and only lines 1 to 7 (a turn still open).
    pub(crate) fn fixture_with(tag: &str, answer: &str) -> (Arc<Ctx>, PathBuf) {
        let dir = tempfile_path(tag);
        std::fs::create_dir_all(dir.join("sessions")).unwrap();
        std::fs::write(dir.join("history.jsonl"), "").unwrap();
        if tag == "fallback" {
            write_session(&dir, r#"{"sessionId":"sid-1","cwd":"/proj"}"#);
            write_transcript(&dir, "sid-1", &fixture_lines(1..8));
        } else {
            write_session(&dir, r#"{"sessionId":"sid-1","cwd":"/proj","status":"busy"}"#);
            write_transcript(&dir, "sid-1", &fixture_lines(1..10));
        }
        let path = stub_tmux(&dir, answer, 0);
        (ctx_with_path(dir.clone(), path), dir)
    }

    fn fixture_ctx(tag: &str) -> (Arc<Ctx>, PathBuf) {
        fixture_with(tag, "")
    }

    #[tokio::test]
    async fn without_a_command_list_the_menu_still_offers_the_effort_levels() {
        let (ctx, dir) = fixture_with("menu", "⏸ plan mode on (shift+tab to cycle)");
        write_session(&dir, r#"{"sessionId":"sid-1","cwd":"/no-such-dir-menu","status":"idle"}"#);
        let (engine, mut rx) = start(ctx, "cdash-a".into(), 4242);
        collect_until_ready(&mut rx).await;
        assert_eq!(engine.menu().await, json!({"commands":[],"models":[],"efforts":["low","medium","high","xhigh","max","auto"],"agents":[]}));
        assert_eq!(engine.live().lock().unwrap().plan, Some(true), "the menu reads the footer");
    }

    #[tokio::test]
    async fn set_takes_only_listed_values_and_refuses_before_typing() {
        let (ctx, dir) = fixture_ctx("set");
        write_session(&dir, r#"{"sessionId":"sid-1","cwd":"/proj-set","status":"busy"}"#); // a cwd no other test caches
        picker::seed(std::path::Path::new("/proj-set"), serde_json::from_str(include_str!("testdata/initialize.json")).unwrap());
        let (engine, mut rx) = start(ctx, "cdash-a".into(), 4242);
        collect_until_ready(&mut rx).await;
        assert_eq!(engine.set("model", &json!("gpt-9")).await, Err("not a model this session offers".into()));
        assert_eq!(engine.set("effort", &json!("ultracode")).await, Err("not an effort level".into()));
        assert_eq!(engine.set("plan", &json!("yes")).await, Err("plan takes true or false".into()));
        assert_eq!(engine.set("agent", &json!("build")).await, Err("not supported".into()));
        assert_eq!(engine.set("model", &json!("haiku")).await, Err("Stop the turn or wait for it to finish".into()));
        assert!(!std::fs::read_to_string(dir.join("tmux-args")).unwrap_or_default().contains("send-keys"));
    }

    #[tokio::test]
    async fn a_picker_command_left_behind_a_dialog_is_cleared_before_the_next_send() {
        let (ctx, dir) = fixture_ctx("leftover");
        let idle = r#"{"sessionId":"sid-1","cwd":"/proj-left","status":"idle"}"#;
        write_session(&dir, idle);
        picker::seed(std::path::Path::new("/proj-left"), serde_json::from_str(include_str!("testdata/initialize.json")).unwrap());
        // The first paste opens a dialog (shell builtins only: PATH is the stub's folder).
        let script = format!(
            "#!/bin/sh\nD='{}'\nprintf '%s\\n' \"$*\" >> \"$D/tmux-args\"\ncase \"$*\" in paste-buffer*) if [ ! -e \"$D/opened\" ]; then : > \"$D/opened\"; printf '%s' '{}' > \"$D/sessions/4242.json\"; fi ;; esac\n",
            dir.display(),
            idle.replace(r#""status":"idle""#, r#""status":"waiting","waitingFor":"dialog open""#)
        );
        crate::chat::tmux_io::tests::install_stub(&dir, "tmux", &script);
        let (engine, mut rx) = start(ctx, "cdash-a".into(), 4242);
        collect_until_ready(&mut rx).await;
        assert_eq!(engine.set("model", &json!("haiku")).await, Err("Answer the open question first".into()));
        write_session(&dir, idle); // the dialog was answered in Terminal
        engine.send("hello", false, "c1").await.unwrap();
        let args = std::fs::read_to_string(dir.join("tmux-args")).unwrap();
        let keys: Vec<&str> = args.lines().filter(|l| l.starts_with("send-keys") || l.starts_with("paste-buffer")).collect();
        assert_eq!(keys.len(), 4, "{keys:?}");
        assert!(keys[0].starts_with("paste-buffer"), "the picker's paste; no key into the dialog");
        assert!(keys[1].starts_with("send-keys -t =cdash-a: C-u"), "the leftover is cleared first");
        assert_eq!(keys[2..], ["paste-buffer -p -d -b cdash-chat-cdash-a -t =cdash-a:", "send-keys -t =cdash-a: Enter"]);
    }

    async fn next_msg(rx: &mut mpsc::Receiver<EngineMsg>) -> EngineMsg {
        tokio::time::timeout(Duration::from_secs(30), rx.recv()).await.expect("no engine message within 30 s").expect("engine ended")
    }

    async fn collect_until_ready(rx: &mut mpsc::Receiver<EngineMsg>) -> Vec<EngineMsg> {
        let mut got = Vec::new();
        loop {
            let m = next_msg(rx).await;
            let ready = matches!(m, EngineMsg::Ready);
            got.push(m);
            if ready {
                return got;
            }
        }
    }

    async fn next_of(rx: &mut mpsc::Receiver<EngineMsg>, f: impl Fn(&EngineMsg) -> bool) -> EngineMsg {
        loop {
            let m = next_msg(rx).await;
            if f(&m) {
                return m;
            }
        }
    }

    async fn next_status(rx: &mut mpsc::Receiver<EngineMsg>) -> EngineMsg {
        next_of(rx, |m| matches!(m, EngineMsg::Status { .. })).await
    }

    async fn next_ev(rx: &mut mpsc::Receiver<EngineMsg>) -> EngineMsg {
        next_of(rx, |m| matches!(m, EngineMsg::Ev(_))).await
    }

    /// Transcript polls the loop makes over `ms`: none parked, one per 150 ms otherwise.
    async fn polls_in(engine: &ClaudeEngine, ms: u64) -> usize {
        let before = engine.shared.polls.load(Ordering::Relaxed);
        tokio::time::sleep(Duration::from_millis(ms)).await;
        engine.shared.polls.load(Ordering::Relaxed) - before
    }

    #[tokio::test]
    async fn an_idle_session_nobody_watches_is_read_through_its_status_file_only() {
        let (ctx, dir) = fixture_ctx("idle");
        write_session(&dir, r#"{"sessionId":"sid-1","cwd":"/proj","status":"idle"}"#);
        let (engine, mut rx) = start(ctx, "cdash-a".into(), 4242);
        collect_until_ready(&mut rx).await;
        next_status(&mut rx).await; // idle
        tokio::time::sleep(Duration::from_millis(500)).await; // the pass after Ready parks
        assert_eq!(polls_in(&engine, 2000).await, 0, "parked: the transcript is not polled");
        let wait = |ms| Duration::from_millis(ms);
        write_session(&dir, r#"{"sessionId":"sid-1","cwd":"/proj","status":"busy"}"#);
        assert!(matches!(tokio::time::timeout(wait(1500), next_status(&mut rx)).await, Ok(EngineMsg::Status { working: true, .. })), "busy is seen within a slow wait");
        append(&dir.join("projects/-proj/sid-1.jsonl"), &fixture_lines(13..14));
        assert!(tokio::time::timeout(wait(300), next_ev(&mut rx)).await.is_ok(), "busy: the prompt is read at the fast rate");
        append(&dir.join("projects/-proj/sid-1.jsonl"), &fixture_lines(9..10)); // the turn ends
        write_session(&dir, r#"{"sessionId":"sid-1","cwd":"/proj","status":"idle"}"#);
        next_ev(&mut rx).await; // its result: slow again
        engine.watchers(1).await;
        assert!(polls_in(&engine, 2000).await > 10, "someone watches: polled as today");
        append(&dir.join("projects/-proj/sid-1.jsonl"), &fixture_lines(13..14));
        assert!(tokio::time::timeout(wait(1000), next_ev(&mut rx)).await.is_ok(), "and what is appended is read");
    }

    #[tokio::test]
    async fn a_turn_shorter_than_a_slow_wait_is_read_from_the_transcript_growing() {
        let (ctx, dir) = fixture_ctx("idle-grow");
        write_session(&dir, r#"{"sessionId":"sid-1","cwd":"/proj","status":"idle"}"#);
        let (engine, mut rx) = start(ctx, "cdash-a".into(), 4242);
        collect_until_ready(&mut rx).await;
        tokio::time::sleep(Duration::from_millis(500)).await;
        assert_eq!(polls_in(&engine, 2000).await, 0, "parked");
        append(&dir.join("projects/-proj/sid-1.jsonl"), &format!("{}{}", fixture_lines(13..14), fixture_lines(9..10)));
        let wait = |ms| Duration::from_millis(ms);
        assert!(tokio::time::timeout(wait(1500), next_ev(&mut rx)).await.is_ok(), "the prompt, with no status change");
        let end = tokio::time::timeout(wait(300), next_ev(&mut rx)).await;
        assert!(matches!(end, Ok(EngineMsg::Ev(ref v)) if v["type"] == "result"), "and its turn end");
    }

    #[tokio::test]
    async fn a_turn_that_never_ends_stops_the_fast_poll_soon_after_idle() {
        let (ctx, dir) = fixture_ctx("idle-open");
        write_transcript(&dir, "sid-1", &fixture_lines(1..14)); // ends on a prompt, as `/model` would
        write_session(&dir, r#"{"sessionId":"sid-1","cwd":"/proj","status":"idle"}"#);
        let (engine, mut rx) = start(ctx, "cdash-a".into(), 4242);
        collect_until_ready(&mut rx).await;
        assert!(polls_in(&engine, 1000).await >= 4, "inside the grace the open turn is polled fast");
        tokio::time::sleep(TURN_END_GRACE).await;
        assert_eq!(polls_in(&engine, 2000).await, 0, "past the grace it parks");
    }

    #[tokio::test]
    async fn a_line_split_across_two_writes_is_delivered_once_whole() {
        let f = tempfile_path("tail");
        std::fs::write(&f, "{\"a\":1}\n{\"b\":").unwrap();
        let mut t = Tail::new(f.clone());
        assert_eq!(t.poll().await.unwrap(), ["{\"a\":1}"]);
        assert!(t.poll().await.unwrap().is_empty());
        append(&f, "2}\n");
        assert_eq!(t.poll().await.unwrap(), ["{\"b\":2}"]);
    }
    #[tokio::test]
    async fn a_huge_line_and_a_missing_or_shrunken_file_do_not_break_the_tail() {
        let f = tempfile_path("tail-big");
        let mut t = Tail::new(f.clone());
        assert!(t.poll().await.unwrap().is_empty(), "not there yet is not an error");
        std::fs::write(&f, format!("{}\nsmall\n", "x".repeat(9 * 1024 * 1024))).unwrap();
        let mut got = Vec::new();
        for _ in 0..8 { got.extend(t.poll().await.unwrap()); }
        assert_eq!((got.len(), got[1].as_str()), (2, "small"));
        std::fs::write(&f, "fresh\n").unwrap();
        assert_eq!(t.poll().await.unwrap(), ["fresh"], "a file that got shorter is read from the start");
    }
    #[tokio::test]
    async fn the_engine_replays_the_transcript_reports_state_and_follows_a_new_session_id() {
        // claude_dir with sessions/4242.json {"sessionId":"sid-1","cwd":"/proj","status":"busy"}
        // and projects/-proj/sid-1.jsonl holding fixture lines 1 to 9; PATH = stub tmux that exits 0.
        // fixture_lines(a..b) returns lines a to b-1 of testdata/claude.jsonl, counting from 1.
        let (ctx, dir) = fixture_ctx("engine");
        let (_engine, mut rx) = start(ctx, "cdash-a".into(), 4242);
        let got = collect_until_ready(&mut rx).await;              // everything up to and including Ready
        assert_eq!(got.iter().filter(|m| matches!(m, EngineMsg::Ev(_))).count(), 5);
        assert!(matches!(next_status(&mut rx).await, EngineMsg::Status { working: true, waiting: false, waiting_for: None }));
        write_session(&dir, r#"{"sessionId":"sid-1","cwd":"/proj","status":"waiting","waitingFor":"dialog open"}"#);
        assert!(matches!(next_status(&mut rx).await, EngineMsg::Status { working: false, waiting: true, waiting_for: Some(ref w) } if w == "dialog open"));
        write_session(&dir, r#"{"sessionId":"sid-1","cwd":"/proj","status":"busy","waitingFor":"dialog open"}"#);
        assert!(matches!(next_status(&mut rx).await, EngineMsg::Status { working: true, waiting_for: None, .. }), "a stale waitingFor outside waiting is ignored");
        write_transcript(&dir, "sid-2", &fixture_lines(2..3));
        write_session(&dir, r#"{"sessionId":"sid-2","cwd":"/proj","status":"idle"}"#);
        assert!(matches!(next_of(&mut rx, |m| matches!(m, EngineMsg::Reset)).await, EngineMsg::Reset));
        assert!(matches!(next_of(&mut rx, |m| matches!(m, EngineMsg::Ev(_))).await, EngineMsg::Ev(_)));
    }
    #[tokio::test]
    async fn without_a_status_field_the_transcript_decides_and_a_dead_pane_ends_the_engine() {
        // sessions/4242.json without "status"; transcript = fixture lines 1 to 7 (a prompt and a reply, no turn end yet).
        let (ctx, dir) = fixture_ctx("fallback");
        let (_engine, mut rx) = start(ctx, "cdash-a".into(), 4242);
        collect_until_ready(&mut rx).await;
        assert!(matches!(next_status(&mut rx).await, EngineMsg::Status { working: true, .. }));
        make_tmux_fail(&dir);                                      // rewrite the stub so has-session exits 1
        assert!(matches!(next_of(&mut rx, |m| matches!(m, EngineMsg::Ended)).await, EngineMsg::Ended));
    }

    #[tokio::test]
    async fn a_background_shell_counts_as_idle() {
        let (ctx, dir) = fixture_ctx("shell");
        let (_engine, mut rx) = start(ctx, "cdash-a".into(), 4242);
        collect_until_ready(&mut rx).await;
        assert!(matches!(next_status(&mut rx).await, EngineMsg::Status { working: true, .. }));
        write_session(&dir, r#"{"sessionId":"sid-1","cwd":"/proj","status":"shell"}"#);
        assert!(matches!(next_status(&mut rx).await, EngineMsg::Status { working: false, waiting: false, .. }), "\"· 1 shell\": idle with background work");
    }

    #[tokio::test]
    async fn a_poll_that_stops_inside_a_huge_line_is_not_the_end_of_the_file() {
        let f = tempfile_path("tail-end");
        std::fs::write(&f, format!("{}\nsmall\n", "x".repeat(9 * 1024 * 1024))).unwrap();
        let mut t = Tail::new(f.clone());
        assert!(t.poll().await.unwrap().is_empty());
        assert!(!t.at_end(), "4 MB read, 5 MB to go");
        let mut got = Vec::new();
        for _ in 0..8 { if t.at_end() { break; } got.extend(t.poll().await.unwrap()); }
        assert!(t.at_end());
        assert_eq!(got.last().map(String::as_str), Some("small"));
    }

    #[tokio::test]
    async fn ready_waits_for_the_end_of_a_transcript_with_a_huge_line() {
        let (ctx, dir) = fixture_ctx("huge");
        let blob = format!(r#"{{"type":"api-request-blob","body":"{}"}}"#, "x".repeat(9 * 1024 * 1024));
        write_transcript(&dir, "sid-1", &format!("{}{blob}\n{}", fixture_lines(1..10), fixture_lines(13..14)));
        let (_engine, mut rx) = start(ctx, "cdash-a".into(), 4242);
        let got = collect_until_ready(&mut rx).await;
        assert_eq!(got.iter().filter(|m| matches!(m, EngineMsg::Ev(_))).count(), 6, "the prompt after the blob is history");
    }

    #[tokio::test]
    async fn hook_cards_publish_settle_from_the_transcript_and_settings_follow_the_last_reply() {
        let (ctx, dir) = fixture_ctx("cards");
        write_session(&dir, r#"{"sessionId":"sid-1","cwd":"/proj","status":"waiting","waitingFor":"input needed"}"#); // keeps the 2 s net out of it
        let (engine, mut rx) = start(ctx, "cdash-a".into(), 4242);
        collect_until_ready(&mut rx).await;
        let hook = engine.live().lock().unwrap().cards.add(&ask_request("p1"), std::time::Instant::now()).unwrap();
        next_of(&mut rx, |m| matches!(m, EngineMsg::Asks(a) if a.len() == 1 && a[0]["kind"] == "question")).await;
        let t = dir.join("projects/-proj/sid-1.jsonl");
        append(&t, &format!("{}\n{}\n", use_line("p1", "toolu_9", "AskUserQuestion", &ask_request("p1")["tool_input"]), result_line("toolu_9")));
        next_of(&mut rx, |m| matches!(m, EngineMsg::Asks(a) if a.is_empty())).await;
        assert!(hook.await.is_err());
        append(&t, concat!(
            r#"{"type":"assistant","uuid":"k8","effort":"low","message":{"id":"m8","model":"claude-haiku-5-5","role":"assistant","content":[{"type":"text","text":"ok"}]}}"#, "\n",
            r#"{"type":"assistant","uuid":"k9","isSidechain":true,"effort":"max","message":{"id":"s9","model":"claude-fable-5-2","role":"assistant","content":[{"type":"text","text":"sub"}]}}"#, "\n"));
        let EngineMsg::Settings(s) = next_of(&mut rx, |m| matches!(m, EngineMsg::Settings(s) if s["model"] == "claude-haiku-5-5")).await else { unreachable!() };
        assert_eq!(s, json!({"model":"claude-haiku-5-5","effort":"low","plan":null,"agent":null,"variant":null}), "a subagent's reply is not the session's model");
    }
    #[tokio::test]
    async fn a_card_held_while_the_loop_is_parked_is_published_at_once() {
        let (ctx, dir) = fixture_ctx("park-card");
        write_session(&dir, r#"{"sessionId":"sid-1","cwd":"/proj","status":"idle"}"#);
        let (engine, mut rx) = start(ctx, "cdash-a".into(), 4242);
        collect_until_ready(&mut rx).await;
        tokio::time::sleep(Duration::from_millis(500)).await; // the pass after Ready parks
        assert_eq!(polls_in(&engine, 1200).await, 0, "parked");
        let _hook = engine.live().lock().unwrap().hold(&plan_request("p1", "# P"), std::time::Instant::now()).unwrap();
        let shown = tokio::time::timeout(Duration::from_millis(150), next_of(&mut rx, |m| matches!(m, EngineMsg::Asks(a) if a.len() == 1))).await;
        assert!(shown.is_ok(), "not up to a slow wait later");
        let EngineMsg::Settings(s) = next_of(&mut rx, |m| matches!(m, EngineMsg::Settings(_))).await else { unreachable!() };
        assert_eq!(s["plan"], true, "the request's permission mode");
    }
    #[tokio::test]
    async fn a_chat_answer_resolves_the_held_hook_and_an_approved_plan_leaves_plan_mode() {
        let (ctx, _dir) = fixture_ctx("answer");
        let (engine, mut rx) = start(ctx, "cdash-a".into(), 4242);
        collect_until_ready(&mut rx).await;
        let live = engine.live();
        let hook = { let mut l = live.lock().unwrap(); l.plan = Some(true); l.cards.add(&plan_request("p1", "# P"), std::time::Instant::now()).unwrap() };
        let id = live.lock().unwrap().cards.list()[0]["id"].as_str().unwrap().to_string();
        engine.answer(&id, &Answer { choice: Some("yes".into()), ..Default::default() }).await.unwrap();
        assert_eq!(hook.await.unwrap()["behavior"], "allow");
        assert_eq!(live.lock().unwrap().plan, Some(false));
        assert_eq!(engine.answer(&id, &Answer::default()).await, Err("already answered".into()));
        assert!(!engine.hooks(), "start leaves hooked false; hub_for sets it");
    }
    #[test]
    fn model_and_effort_come_from_one_reply_and_a_synthetic_one_is_skipped() {
        let r = |m: &str| format!(r#"{{"type":"assistant","effort":"xhigh","message":{{"model":"{m}"}}}}"#);
        assert_eq!(model_effort(&r("claude-opus-5-5")), Some(("claude-opus-5-5".into(), Some("xhigh".into()))));
        assert_eq!(model_effort(r#"{"type":"assistant","message":{"model":"m"}}"#), Some(("m".into(), None)));
        assert_eq!(model_effort(&r("<synthetic>")), None);
        assert_eq!(model_effort("not json"), None);
    }
    #[tokio::test]
    async fn history_replays_the_transcript_from_its_start() {
        let (ctx, _dir) = fixture_ctx("history");
        let (engine, mut rx) = start(ctx, "cdash-a".into(), 4242);
        let replayed: Vec<Value> = collect_until_ready(&mut rx).await.into_iter()
            .filter_map(|m| if let EngineMsg::Ev(v) = m { Some(v) } else { None }).collect();
        assert_eq!(engine.history(1, 3).await.unwrap(), replayed[..3]);
        assert_eq!(engine.history(2, 2).await.unwrap(), replayed[1..2]);
    }
}
