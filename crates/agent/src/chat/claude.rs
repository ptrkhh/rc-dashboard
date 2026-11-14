//! The Claude engine: tails the session transcript and the session state
//! file, and types through tmux.

use super::hub::{Engine, EngineMsg};
use super::tap::Tap;
use super::tmux_io;
use crate::collect::ctx::Ctx;
use crate::collect::lookup::session_file_for;
use crate::host::cmd::Runner;
use crate::host::log::LogBuffer;
use crate::parse::paths::project_dir_name;
use serde_json::Value;
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
    runner: Arc<Runner>,
    log: Arc<LogBuffer>,
    name: String,
    /// The transcript of the conversation the loop follows.
    transcript: Arc<Mutex<Option<PathBuf>>>,
    shared: Arc<Shared>,
}

/// What the engine and its loop share.
#[derive(Default)]
struct Shared {
    /// Clients attached to the hub; the loop parks while none are.
    watchers: AtomicUsize,
    wake: Notify,
    /// Transcript polls the loop made.
    #[cfg(test)]
    polls: AtomicUsize,
}

impl Engine for ClaudeEngine {
    async fn send(&self, text: &str, _steer: bool, _cid: &str) -> Result<bool, String> {
        // A paste cannot be confirmed by the TUI; the transcript confirms it.
        tmux_io::paste(&self.runner, &self.name, text).await.map(|()| false)
    }

    async fn stop(&self) -> Result<(), String> {
        tmux_io::escape(&self.runner, &self.name).await
    }

    async fn clear_input(&self, text: &str) {
        if let Err(e) = tmux_io::clear_input(&self.runner, &self.name, text).await {
            self.log.push(format!("chat clear failed {}: {e}", self.name));
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

    async fn watchers(&self, n: usize) {
        self.shared.watchers.store(n, Ordering::Relaxed);
        self.shared.wake.notify_one();
    }
}

pub fn start(ctx: Arc<Ctx>, name: String, pid: i32) -> (ClaudeEngine, mpsc::Receiver<EngineMsg>) {
    let (tx, rx) = mpsc::channel(256);
    let transcript = Arc::new(Mutex::new(None));
    let shared = Arc::new(Shared::default());
    let engine = ClaudeEngine {
        runner: Arc::clone(&ctx.runner),
        log: Arc::clone(&ctx.host.log),
        name: name.clone(),
        transcript: Arc::clone(&transcript),
        shared: Arc::clone(&shared),
    };
    tokio::spawn(run(ctx, name, pid, tx, transcript, shared));
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
) {
    let mut conv: Option<Conv> = None;
    let mut last: Option<(bool, bool, Option<String>)> = None;
    let mut blind_sent = false;
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
    use crate::chat::tmux_io::tests::{stub_tmux, tempfile_path};
    use crate::host::cmd::Runner;
    use crate::host::log::LogBuffer;
    use serde_json::Value;
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
    async fn history_replays_the_transcript_from_its_start() {
        let (ctx, _dir) = fixture_ctx("history");
        let (engine, mut rx) = start(ctx, "cdash-a".into(), 4242);
        let replayed: Vec<Value> = collect_until_ready(&mut rx).await.into_iter()
            .filter_map(|m| if let EngineMsg::Ev(v) = m { Some(v) } else { None }).collect();
        assert_eq!(engine.history(1, 3).await.unwrap(), replayed[..3]);
        assert_eq!(engine.history(2, 2).await.unwrap(), replayed[1..2]);
    }
}
