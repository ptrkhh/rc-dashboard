//! The Claude engine: tails the session transcript and the session state
//! file, and types through tmux.

use super::asks::Cards;
use super::hub::{place_cut, Answer, AsideChunk, Attachment, Engine, EngineMsg, Kind, Sent, WINDOW};
use super::media;
use super::picker;
use super::rewind::{self, Tui};
use super::tap::{is_cut, Listed, Tap};
use super::tmux_io;
use crate::collect::ctx::Ctx;
use crate::collect::lookup::session_file_for;
use crate::host::log::LogBuffer;
use crate::parse::paths::project_dir_name;
use serde_json::{json, Value};
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncSeekExt, BufReader};
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
/// How long a pasted image path has to turn into a chip.
pub const CHIP_WAIT: Duration = Duration::from_secs(5);
/// The longest a side question may run.
pub const ASIDE_MAX: Duration = Duration::from_secs(300);
/// How long a side question that answered has to exit on its own, so it can
/// remove its `~/.claude/sessions` file, before it is killed.
const ASIDE_EXIT: Duration = Duration::from_secs(5);

/// `claude -p` arguments for a side question: a fork of the session that is
/// never saved, with no tools, MCP servers or hooks. The question comes last,
/// after `--`, as one argument.
pub fn aside_args(sid: &str, model: Option<&str>, question: &str) -> Vec<String> {
    let mut a = vec!["-p", "--resume", sid, "--fork-session", "--no-session-persistence", "--tools", "", "--strict-mcp-config"];
    a.extend(["--mcp-config", r#"{"mcpServers":{}}"#, "--settings", r#"{"disableAllHooks":true}"#]);
    if let Some(m) = model {
        a.extend(["--model", m]);
    }
    a.extend(["--output-format", "stream-json", "--verbose", "--", question]);
    a.into_iter().map(String::from).collect()
}

/// One `stream-json` line as a piece of the answer: an assistant message's
/// text, or the end on a `result`. Anything else is `None`.
pub fn aside_chunk(line: &str) -> Option<AsideChunk> {
    let v: Value = serde_json::from_str(line).ok()?;
    match v["type"].as_str()? {
        "assistant" => {
            let blocks = v["message"]["content"].as_array()?;
            let text = blocks.iter().filter(|b| b["type"] == "text").filter_map(|b| b["text"].as_str()).collect::<Vec<_>>().join("\n\n");
            (!text.is_empty()).then_some(AsideChunk { text, done: false, error: None })
        }
        "result" => {
            let error = (v["is_error"] == true)
                .then(|| v["result"].as_str().filter(|r| !r.is_empty()).unwrap_or("side question failed").to_string());
            Some(AsideChunk { text: String::new(), done: true, error })
        }
        _ => None,
    }
}

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
    chip_wait: Duration,
    /// `ASIDE_MAX`, shorter in tests.
    aside_max: Duration,
}

/// The hook cards and the session's settings, shared by the engine, its loop
/// and the hook listener.
#[derive(Default)]
pub struct Live {
    pub cards: Cards,
    pub plan: Option<bool>,
    pub model: Option<String>,
    pub effort: Option<String>,
    /// The prompts Claude's rewind selector lists, oldest first.
    pub branch: Vec<Listed>,
    /// A chat rewind cut `branch` and Claude has written no prompt since: the
    /// transcript's last leaf is still the abandoned branch.
    pub rewound: bool,
    /// The session file's `sessionId` and `cwd`, as last read.
    pub sid: Option<String>,
    pub cwd: Option<String>,
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
    /// Whether the loop's last pass parked it in the slow wait.
    #[cfg(test)]
    parked: std::sync::atomic::AtomicBool,
}

impl Engine for ClaudeEngine {
    async fn send(&self, text: &str, _steer: bool, _cid: &str, files: &[Attachment]) -> Result<Sent, String> {
        // ponytail: attachments on a command are ignored; refuse them if owners hit it.
        match picker::route(text) {
            // A paste cannot be confirmed by the TUI; the transcript confirms it.
            picker::Route::Paste => {
                picker::clear_leftover(&self.ctx, &self.name, self.pid, &self.shared.leftover).await;
                let (runner, name) = (&self.ctx.runner, self.name.as_str());
                let (images, others): (Vec<_>, Vec<_>) = files.iter().partition(|f| f.kind == Kind::Image);
                let body = media::with_attached(text, &others.iter().map(|f| f.path.as_path()).collect::<Vec<_>>());
                if images.is_empty() {
                    return tmux_io::paste(runner, name, &body).await.map(|()| Sent::Unconfirmed);
                }
                let paths = |n: usize| images[..n].iter().map(|i| i.path.to_string_lossy()).collect::<Vec<_>>().join("\n");
                // Each path alone becomes a chip; text on the same line would stay text.
                for (i, img) in images.iter().enumerate() {
                    // A dialog that came up since the last image takes no more keys.
                    if i > 0 && !picker::input_open(&self.ctx, self.pid).await {
                        self.leave(paths(i)).await;
                        return Err("image did not attach".into());
                    }
                    if tmux_io::attach_image(runner, name, &img.path, self.chip_wait).await.is_err() {
                        self.leave(paths(i + 1)).await;
                        return Err("image did not attach".into());
                    }
                }
                let text = format!(" {body}");
                let sent = tmux_io::paste(runner, name, &text).await;
                if sent.is_err() {
                    self.leave(paths(images.len()) + &text).await;
                }
                sent.map(|()| Sent::Unconfirmed)
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

    async fn rewind(&self, uuid: &str, files: bool) -> Result<String, String> {
        let branch = lock(&self.live).branch.clone();
        let target = branch.iter().position(|p| p.uuid == uuid).ok_or("this prompt cannot be rewound from chat; use Terminal")?;
        let leftover = &self.shared.leftover;
        // Text left in the box would make Esc Esc clear it instead of opening the dialog.
        picker::clear_leftover(&self.ctx, &self.name, self.pid, leftover).await;
        let tui = TmuxTui { ctx: &self.ctx, name: &self.name, pid: self.pid, leftover };
        rewind::drive(&tui, &branch, target, files).await?;
        // Claude writes nothing until the next prompt; until then the selector lists only the prompts before the target.
        {
            let mut l = lock(&self.live);
            l.branch.truncate(target);
            l.rewound = true;
        }
        Ok(branch[target].text.clone())
    }

    async fn clear_input(&self, text: &str) {
        if let Err(e) = tmux_io::clear_input(&self.ctx.runner, &self.name, text).await {
            self.ctx.host.log.push(format!("chat clear failed {}: {e}", self.name));
        }
    }

    /// Replays the whole transcript through a fresh tap and places its cuts
    /// as the hub does, so positions match the hub's and an abandoned branch
    /// never comes back; a later cut can still remove events before `to`.
    async fn history(&self, from: u64, to: u64) -> Result<Vec<Value>, String> {
        let path = self.transcript.lock().unwrap_or_else(|e| e.into_inner()).clone().ok_or("no conversation yet")?;
        let (mut tail, mut tap) = (Tail::new(path), Tap::default());
        // Events `base + 1..` mirror the hub's memory: a cut whose anchor left it removes nothing.
        // ponytail: the hub's window also counts its own retracts, so ≤R older rows (R = hub retracts in the window) can shift by the cut's length; have the hub count retract extras in its window so both windows hold the same engine events.
        let (mut all, mut base, span) = (Vec::new(), 0, from as usize..=to as usize);
        loop {
            for line in tail.poll().await.map_err(|e| e.to_string())? {
                for ev in tap.feed(&line) {
                    if is_cut(&ev) {
                        if let Some(len) = place_cut(&all[base..], base, &ev) {
                            all.truncate(base + len);
                        }
                        continue;
                    }
                    // Outside the page only the uuid matters, to place cuts.
                    all.push(if span.contains(&(all.len() + 1)) { ev } else { json!({"uuid":ev["uuid"]}) });
                    if all.len() - base > WINDOW {
                        base += 1;
                    }
                }
            }
            if tail.at_end() {
                break;
            }
        }
        let page = all.into_iter().skip(span.start().saturating_sub(1)).take(span.count());
        Ok(page.map(|ev| stash_images(ev, &self.name, &self.ctx.host.log)).collect())
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

    /// Runs `claude -p` on a never-saved fork of the session, in the session's
    /// folder. Dropping the receiver kills it; there is no other cancel.
    async fn aside(&self, _id: &str, text: &str) -> Result<mpsc::Receiver<AsideChunk>, String> {
        let (sid, cwd, model) = {
            let l = lock(&self.live);
            if l.rewound {
                // `--resume` would fork the abandoned branch and answer from it.
                return Err("after a rewind, side questions work again once you send a message".into());
            }
            (l.sid.clone(), l.cwd.clone(), l.model.clone())
        };
        let (Some(sid), Some(cwd)) = (sid, cwd) else { return Err("no session yet".into()) };
        let args = aside_args(&sid, model.as_deref(), text);
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        let mut child = self.ctx.runner.spawn_piped("claude", &args, std::path::Path::new(&cwd)).map_err(|e| e.to_string())?;
        // An open, silent stdin costs a 3 s wait; closed, it reads as /dev/null.
        drop(child.stdin.take());
        let own = picker::OwnPid::new(&self.ctx, child.id().map(|p| p as i32));
        let stdout = child.stdout.take().ok_or("no stdout")?;
        let (tx, rx) = mpsc::channel(16);
        let max = self.aside_max;
        tokio::spawn(async move {
            let mut lines = BufReader::new(stdout).lines();
            // `Some(answered)`, `false` when the receiver went away; `None` when the stream ended first.
            let read = async {
                loop {
                    let line = tokio::select! {
                        l = lines.next_line() => l,
                        () = tx.closed() => return Some(false),
                    };
                    let Ok(Some(line)) = line else { return None };
                    if let Some(c) = aside_chunk(&line) {
                        let done = c.done;
                        if tx.send(c).await.is_err() {
                            return Some(false);
                        }
                        if done {
                            return Some(true);
                        }
                    }
                }
            };
            // ponytail: a killed run can leave its `~/.claude/sessions/<pid>.json`; the collector skips dead pids.
            // ponytail: an answered run may spend up to `ASIDE_EXIT` exiting while the next question starts a second
            // one; harmless, it has already answered.
            let error = match tokio::time::timeout(max, read).await {
                Ok(Some(answered)) => {
                    if answered {
                        let _ = tokio::time::timeout(ASIDE_EXIT, child.wait()).await;
                    }
                    None
                }
                // stderr stays discarded: the exit code says why without echoing anything.
                Ok(None) => Some(match tokio::time::timeout(ASIDE_EXIT, child.wait()).await.ok().and_then(Result::ok).and_then(|s| s.code()) {
                    Some(code) => format!("no answer (claude exited {code})"),
                    None => "no answer".to_string(),
                }),
                Err(_) => Some("no answer in 5 minutes".to_string()),
            };
            if let Some(e) = error {
                let _ = tx.send(AsideChunk { text: String::new(), done: true, error: Some(e) }).await;
            }
            drop(child); // `kill_on_drop` ends it if it still runs
            drop(own);
        });
        Ok(rx)
    }
}

impl ClaudeEngine {
    /// Text chat typed but did not submit: cleared now if no dialog is up,
    /// else by the next send.
    async fn leave(&self, typed: String) {
        *self.shared.leftover.lock().unwrap_or_else(|e| e.into_inner()) = Some(typed);
        picker::clear_leftover(&self.ctx, &self.name, self.pid, &self.shared.leftover).await;
    }

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

/// The pane, for driving the rewind dialog.
struct TmuxTui<'a> {
    ctx: &'a Ctx,
    name: &'a str,
    pid: i32,
    leftover: &'a picker::Leftover,
}

impl Tui for TmuxTui<'_> {
    async fn screen(&self) -> String {
        tmux_io::capture(&self.ctx.runner, self.name).await.unwrap_or_default()
    }
    async fn press(&self, key: &str) -> Result<(), String> {
        tmux_io::key(&self.ctx.runner, self.name, key).await
    }
    async fn input(&self) -> Option<String> {
        tmux_io::input_text(&tmux_io::capture_styled(&self.ctx.runner, self.name).await.ok()?)
    }
    /// Cleared now when the box takes keys, else before the next send.
    async fn clear(&self, text: &str) {
        self.leave(text);
        picker::clear_leftover(self.ctx, self.name, self.pid, self.leftover).await;
    }
    fn leave(&self, text: &str) {
        *self.leftover.lock().unwrap_or_else(|e| e.into_inner()) = Some(text.to_string());
    }
    async fn status(&self) -> (Option<String>, Option<String>) {
        session_file_for(&self.ctx.claude_dir, self.pid).await.map_or((None, None), |f| (f.status, f.waiting_for))
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
        chip_wait: CHIP_WAIT,
        aside_max: ASIDE_MAX,
    };
    tokio::spawn(async move {
        run(ctx, name, pid, tx, transcript, shared, &live).await;
        // Held hooks get no decision once nothing follows the session.
        lock(&live).cards.clear();
    });
    (engine, rx)
}

/// Image entries `{kind:"image",mime,data}` become `{kind:"image",ref,mime}`
/// through the media cache, so no bytes travel on to the hub. Transcript
/// content is untrusted: only allowed image types within the upload limit
/// are cached, under their hash; anything else becomes `{kind:"file",name:"image"}`.
// ponytail: decodes, hashes and writes on the engine task; move to spawn_blocking if large transcripts stall the tick.
pub fn stash_images(mut ev: Value, session: &str, log: &LogBuffer) -> Value {
    use base64::Engine as _;
    let Some(files) = ev.get_mut("files").and_then(Value::as_array_mut) else { return ev };
    for f in files.iter_mut().filter(|f| f.get("data").is_some()) {
        let mime = f["mime"].as_str().filter(|m| media::image_ext(m).is_some());
        let data = f["data"].as_str().filter(|d| d.len() as u64 / 4 * 3 <= media::MAX_UPLOAD);
        let stashed = mime.zip(data).and_then(|(mime, data)| {
            let bytes = base64::engine::general_purpose::STANDARD.decode(data).ok()?;
            media::stash(&bytes, mime).ok().map(|r| json!({"kind":"image","ref":r,"mime":mime}))
        });
        *f = stashed.unwrap_or_else(|| {
            log.push(format!("chat image not cached {session}"));
            json!({"kind":"file","name":"image"})
        });
    }
    ev
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
    let (mut last_usage, mut last_tasks): (Option<Value>, Option<Vec<Value>>) = (None, None);
    // Set only when it changes, so a rewind's shortened branch outlives lines that are no prompt.
    let mut last_branch: Vec<Listed> = Vec::new();
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
        if let Some(f) = &file {
            let mut l = lock(live);
            (l.sid, l.cwd) = (f.session_id.clone(), f.cwd.clone());
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
                (last_usage, last_tasks) = (None, None);
                last_branch.clear();
                {
                    let mut l = lock(live);
                    l.branch.clear();
                    l.rewound = false;
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
            #[cfg(test)]
            shared.parked.store(slow, Ordering::Relaxed);
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
                    if tx.send(EngineMsg::Ev(stash_images(ev, &name, &ctx.host.log))).await.is_err() {
                        return;
                    }
                }
            }
            if !idle_read {
                let branch = c.tap.listed();
                if branch != last_branch {
                    let mut l = lock(live);
                    l.branch.clone_from(&branch);
                    l.rewound = false;
                    drop(l);
                    last_branch = branch;
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
            if c.ready {
                let mut usage = c.tap.usage();
                if usage["cwd"].is_null() {
                    usage["cwd"] = json!(file.as_ref().and_then(|f| f.cwd.clone()));
                }
                if last_usage.as_ref() != Some(&usage) {
                    last_usage = Some(usage.clone());
                    if tx.send(EngineMsg::Usage(usage)).await.is_err() {
                        return;
                    }
                }
                let tasks = c.tap.tasks();
                // An empty list is never sent first: the hub starts with none.
                let first_empty = last_tasks.is_none() && tasks.is_empty();
                if last_tasks.as_ref() != Some(&tasks) {
                    last_tasks = Some(tasks.clone());
                    if !first_empty && tx.send(EngineMsg::Tasks(tasks)).await.is_err() {
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
    use crate::chat::hub::Kind;
    use crate::chat::hub::{ClientFrame, Hub, WINDOW};
    use crate::chat::tmux_io::tests::{chip_stub, eventually, stub_tmux, tempfile_path};
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
        engine.send("hello", false, "c1", &[]).await.unwrap();
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

    pub(crate) async fn collect_until_ready(rx: &mut mpsc::Receiver<EngineMsg>) -> Vec<EngineMsg> {
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

    /// Waits for the loop to park in the slow wait.
    async fn parks(engine: &ClaudeEngine) {
        eventually("the loop parks", || engine.shared.parked.load(Ordering::Relaxed)).await;
    }

    /// Waits for `n` more transcript polls. A parked loop with an unchanged
    /// transcript makes none, so this passing means the loop is not parked.
    async fn polled(engine: &ClaudeEngine, n: usize) {
        let before = engine.shared.polls.load(Ordering::Relaxed);
        eventually("transcript polls", || engine.shared.polls.load(Ordering::Relaxed) >= before + n).await;
    }

    /// Generous: a positive wait under CPU load.
    const SLOW_OK: Duration = Duration::from_secs(10);

    #[tokio::test]
    async fn an_idle_session_nobody_watches_is_read_through_its_status_file_only() {
        let (ctx, dir) = fixture_ctx("idle");
        write_session(&dir, r#"{"sessionId":"sid-1","cwd":"/proj","status":"idle"}"#);
        let (engine, mut rx) = start(ctx, "cdash-a".into(), 4242);
        collect_until_ready(&mut rx).await;
        next_status(&mut rx).await; // idle
        parks(&engine).await;
        // Must not happen: checked over a bounded window of two slow waits.
        assert_eq!(polls_in(&engine, 2000).await, 0, "parked: the transcript is not polled");
        write_session(&dir, r#"{"sessionId":"sid-1","cwd":"/proj","status":"busy"}"#);
        assert!(matches!(tokio::time::timeout(SLOW_OK, next_status(&mut rx)).await, Ok(EngineMsg::Status { working: true, .. })), "busy is seen while parked");
        // The transcript has not changed: only the fast poll reads it now.
        polled(&engine, 2).await;
        append(&dir.join("projects/-proj/sid-1.jsonl"), &fixture_lines(13..14));
        assert!(tokio::time::timeout(SLOW_OK, next_ev(&mut rx)).await.is_ok(), "busy: the prompt is read at the fast rate");
        append(&dir.join("projects/-proj/sid-1.jsonl"), &fixture_lines(9..10)); // the turn ends
        write_session(&dir, r#"{"sessionId":"sid-1","cwd":"/proj","status":"idle"}"#);
        next_ev(&mut rx).await; // its result: slow again
        parks(&engine).await;
        engine.watchers(1).await;
        polled(&engine, 10).await; // someone watches: polled as today
        assert!(!engine.shared.parked.load(Ordering::Relaxed), "someone watches: not parked");
        append(&dir.join("projects/-proj/sid-1.jsonl"), &fixture_lines(13..14));
        assert!(tokio::time::timeout(SLOW_OK, next_ev(&mut rx)).await.is_ok(), "and what is appended is read");
    }

    #[tokio::test]
    async fn a_turn_shorter_than_a_slow_wait_is_read_from_the_transcript_growing() {
        let (ctx, dir) = fixture_ctx("idle-grow");
        write_session(&dir, r#"{"sessionId":"sid-1","cwd":"/proj","status":"idle"}"#);
        let (engine, mut rx) = start(ctx, "cdash-a".into(), 4242);
        collect_until_ready(&mut rx).await;
        parks(&engine).await;
        // Must not happen: checked over a bounded window of two slow waits.
        assert_eq!(polls_in(&engine, 2000).await, 0, "parked");
        append(&dir.join("projects/-proj/sid-1.jsonl"), &format!("{}{}", fixture_lines(13..14), fixture_lines(9..10)));
        assert!(tokio::time::timeout(SLOW_OK, next_ev(&mut rx)).await.is_ok(), "the prompt, with no status change");
        let end = tokio::time::timeout(SLOW_OK, next_ev(&mut rx)).await;
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
        parks(&engine).await;
        // Must not happen: checked over a bounded window of two slow waits.
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
        parks(&engine).await;
        // Must not happen: checked over a bounded window longer than one slow wait.
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
    fn att(path: &str, kind: Kind) -> Attachment {
        let mime = if kind == Kind::Image { "image/png" } else { "application/pdf" };
        let name = path.rsplit('/').next().unwrap().to_string();
        Attachment { path: path.into(), kind, mime: mime.into(), name, size: 8 }
    }

    fn engine(ctx: &Arc<Ctx>, wait: Duration) -> ClaudeEngine {
        let shared = Arc::new(Shared::default());
        let live = Arc::new(Mutex::new(Live { wake: Arc::clone(&shared.wake), ..Live::default() }));
        ClaudeEngine {
            ctx: Arc::clone(ctx),
            pid: 4242,
            name: "cdash-a".into(),
            hooked: false,
            live,
            transcript: Arc::new(Mutex::new(None)),
            shared,
            chip_wait: wait,
            aside_max: ASIDE_MAX,
        }
    }

    #[tokio::test]
    async fn images_go_first_then_the_text_with_attached_lines_then_enter() {
        let (ctx, dir) = fixture_with("send-img", "");
        chip_stub(&dir, Some("❯ [Image #1]"));
        let files = [att("/u/up/a.png", Kind::Image), att("/u/up/r.pdf", Kind::File)];
        assert_eq!(engine(&ctx, Duration::from_secs(2)).send("what is this", false, "c1", &files).await, Ok(Sent::Unconfirmed), "the transcript confirms it");
        assert_eq!(std::fs::read_to_string(dir.join("pastes")).unwrap(), "/u/up/a.png\n--\n what is this\nAttached: /u/up/r.pdf\n--\n");
        assert_eq!(std::fs::read_to_string(dir.join("tmux-args")).unwrap().lines().last(), Some("send-keys -t =cdash-a: Enter"));
    }
    #[tokio::test]
    async fn an_image_that_does_not_attach_clears_the_box_and_nothing_is_sent() {
        let (ctx, dir) = fixture_with("send-noimg", "");
        write_session(&dir, r#"{"sessionId":"sid-1","cwd":"/proj","status":"idle"}"#);
        chip_stub(&dir, None);
        let engine = engine(&ctx, Duration::from_millis(300));
        assert_eq!(engine.send("x", false, "c1", &[att("/u/up/a.png", Kind::Image)]).await, Err("image did not attach".to_string()));
        let args = std::fs::read_to_string(dir.join("tmux-args")).unwrap();
        assert!(args.lines().last().unwrap().starts_with("send-keys -t =cdash-a: C-u") && !args.contains("Enter"));
        assert_eq!(*engine.shared.leftover.lock().unwrap(), None, "cleared");
    }
    #[tokio::test]
    async fn behind_a_dialog_no_more_images_are_pasted_and_nothing_is_cleared() {
        let (ctx, dir) = fixture_with("send-dialog", "");
        write_session(&dir, r#"{"sessionId":"sid-1","cwd":"/proj","status":"waiting"}"#);
        chip_stub(&dir, Some("❯ [Image #1]"));
        let engine = engine(&ctx, Duration::from_millis(300));
        let files = [att("/u/up/a.png", Kind::Image), att("/u/up/b.png", Kind::Image)];
        assert_eq!(engine.send("x", false, "c1", &files).await, Err("image did not attach".to_string()));
        assert_eq!(std::fs::read_to_string(dir.join("pastes")).unwrap(), "/u/up/a.png\n--\n", "the second image is not pasted");
        assert!(!std::fs::read_to_string(dir.join("tmux-args")).unwrap().contains("send-keys"), "no key into the dialog");
        assert_eq!(engine.shared.leftover.lock().unwrap().as_deref(), Some("/u/up/a.png"), "the next send clears it");
    }
    #[tokio::test]
    async fn image_blocks_reach_the_hub_as_media_refs() {
        use base64::Engine as _;
        let (ctx, dir) = fixture_with("stash", "");
        let png = [crate::chat::media::tests::PNG, b"engine"].concat();
        let b64 = |b: &[u8]| base64::engine::general_purpose::STANDARD.encode(b);
        let line = json!({"type":"user","uuid":"g9","parentUuid":"s2","promptSource":"typed","message":{"role":"user","content":[
            {"type":"text","text":"[Image #1] [Image #2] see"},
            {"type":"image","source":{"type":"base64","media_type":"image/png","data":b64(&png)}},
            {"type":"image","source":{"type":"base64","media_type":"image/png","data":b64(b"not an image")}}]}});
        append(&dir.join("projects/-proj/sid-1.jsonl"), &format!("{line}\n"));
        let (engine, mut rx) = start(ctx, "cdash-a".into(), 4242);
        let got = collect_until_ready(&mut rx).await;
        let ev = got.iter().find_map(|m| match m { EngineMsg::Ev(e) if e["uuid"] == "g9" => Some(e), _ => None }).unwrap();
        let r = format!("{:x}", <sha2::Sha256 as sha2::Digest>::digest(&png));
        assert_eq!(ev["files"], json!([{"kind":"image","ref":r,"mime":"image/png"}, {"kind":"file","name":"image"}]));
        assert!(crate::chat::media::media_dir().join(format!("{r}.png")).exists());
        let page = serde_json::to_string(&engine.history(1, 1000).await.unwrap()).unwrap();
        assert!(page.contains(&r) && !page.contains("\"data\""), "an older page carries refs too");
    }
    #[test]
    fn only_allowed_images_within_the_upload_limit_are_cached() {
        use base64::Engine as _;
        let b64 = |b: &[u8]| base64::engine::general_purpose::STANDARD.encode(b);
        let svg = b64(b"<svg xmlns='http://www.w3.org/2000/svg'/>");
        let huge = b64(&[crate::chat::media::tests::PNG, &vec![0; crate::chat::media::MAX_UPLOAD as usize]].concat());
        let ev = json!({"type":"user","files":[
            {"kind":"image","mime":"image/svg+xml","data":svg},
            {"kind":"image","mime":"image/png","data":huge},
            {"kind":"file","name":"r.pdf"}]});
        let log = LogBuffer::new();
        let out = stash_images(ev, "cdash-a", &log);
        assert_eq!(out["files"], json!([{"kind":"file","name":"image"}, {"kind":"file","name":"image"}, {"kind":"file","name":"r.pdf"}]));
        let lines = log.lines();
        assert!(lines.len() == 2 && lines.iter().all(|l| l.ends_with("chat image not cached cdash-a")), "{lines:?}");
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

    /// A hub over a Claude engine whose transcript holds `lines`, and the transcript.
    async fn hub_over(tag: &str, lines: &str) -> (Hub, PathBuf) {
        let (ctx, dir) = fixture_ctx(tag);
        write_transcript(&dir, "sid-1", lines);
        let (engine, rx) = start(ctx, "cdash-a".into(), 4242);
        (Hub::spawn("claude", engine, rx, Arc::new(LogBuffer::new()), "cdash-a".into()), dir.join("projects/-proj/sid-1.jsonl"))
    }
    async fn hub_head(hub: &Hub) -> Value {
        serde_json::from_str::<Value>(&hub.subscribe(None, None).await.unwrap().backlog[0]).unwrap()["head"].clone()
    }
    async fn page(hub: &Hub, before: u64, limit: usize) -> Value {
        serde_json::from_str(&hub.command(ClientFrame::Page { before, limit: Some(limit) }).await.unwrap()).unwrap()
    }
    /// `n` replies, each the child of the one before, the first of `parent`.
    fn replies(parent: &str, n: usize) -> String {
        (0..n).map(|i| {
            let p = if i == 0 { parent.to_string() } else { format!("f{}", i - 1) };
            format!("{}\n", json!({"type":"assistant","uuid":format!("f{i}"),"parentUuid":p,"message":{"id":format!("m{i}"),"role":"assistant","content":[{"type":"text","text":"x"}]}}))
        }).collect()
    }
    #[tokio::test]
    async fn an_older_page_numbers_events_as_the_hub_did_and_skips_an_abandoned_branch() {
        let (hub, t) = hub_over("history-cut", include_str!("testdata/rewind.jsonl")).await;
        let shown: Vec<Value> = hub.subscribe(None, None).await.unwrap().backlog.iter()
            .map(|f| serde_json::from_str::<Value>(f).unwrap()).filter(|f| f["t"] == "ev")
            .map(|f| json!({"seq":f["seq"],"ev":f["ev"]})).collect();
        let abandoned = ["dd5d85d6", "8a67cba9", "ea31531f", "44ede06f"];
        assert!(shown.len() == 6 && !shown.iter().any(|e| abandoned.iter().any(|a| e["ev"]["uuid"].as_str().unwrap_or("").starts_with(a))), "{shown:?}");
        append(&t, &replies("db5a7c16-64a2-4664-8f9a-652a769dbbce", WINDOW)); // the six leave the hub's memory
        for _ in 0..300 {
            if hub_head(&hub).await == 6 + WINDOW { break; }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        assert_eq!(page(&hub, 7, 6).await["events"], json!(shown), "re-read from the transcript, numbered as the hub did");
    }
    #[tokio::test]
    async fn a_cut_whose_anchor_left_the_hubs_window_removes_nothing_in_an_older_page_either() {
        let turn: String = include_str!("testdata/rewind.jsonl").lines().take(5).map(|l| format!("{l}\n")).collect(); // prompt, reply, turn end
        let end = "37929adb-63f0-4e64-b002-3f3c2f2a84b4";
        // Back to the turn end (a cut after it), or to the record before any event (a cut after null).
        for (tag, parent) in [("history-far", end), ("history-far-root", ROOT)] {
            let back = json!({"type":"user","uuid":"b1","parentUuid":parent,"promptSource":"typed","message":{"role":"user","content":"back"}});
            let (hub, _) = hub_over(tag, &format!("{turn}{}{back}\n", replies(end, WINDOW))).await;
            assert_eq!(hub_head(&hub).await, 3 + WINDOW + 1, "{tag}: the anchor had left memory, so nothing was cut");
            let p = page(&hub, 5, 4).await;
            assert_eq!((p["events"][3]["seq"].as_u64(), p["events"][3]["ev"]["uuid"].as_str()), (Some(4), Some("f0")), "{tag}: the source keeps what the hub kept");
        }
    }
    #[tokio::test]
    async fn rewind_targets_a_listed_prompt_and_keys_go_only_to_an_idle_session() {
        let (ctx, dir) = fixture_ctx("rewind");
        write_transcript(&dir, "sid-1", &fixture_lines(1..37));
        let (engine, mut rx) = start(ctx, "cdash-a".into(), 4242);
        collect_until_ready(&mut rx).await;
        {
            let l = engine.live.lock().unwrap();
            assert_eq!(l.branch.iter().map(|p| p.uuid.as_str()).collect::<Vec<_>>(), ["u1", "b1", "u2", "u5", "q1", "g1"]);
            assert_eq!((l.sid.as_deref(), l.cwd.as_deref()), (Some("sid-1"), Some("/proj")));
        }
        assert_eq!(engine.rewind("u4", false).await, Err("this prompt cannot be rewound from chat; use Terminal".into()), "handed back, off the branch");
        assert_eq!(engine.rewind("u2", false).await, Err("Stop the turn or wait for it to finish".into()));
        assert!(!std::fs::read_to_string(dir.join("tmux-args")).unwrap_or_default().contains("send-keys"));
    }

    /// The fixture's first record, which shows no event.
    const ROOT: &str = "b2d9530c-cfa3-452a-8335-e1f1ef9c5d5a";
    #[tokio::test]
    async fn the_engine_reports_usage_once_ready_with_the_session_folder_as_fallback() {
        // The tag selects no transcript: this is the default fixture, whose records carry no `cwd`.
        let (ctx, dir) = fixture_ctx("facts");
        assert!(!std::fs::read_to_string(dir.join("projects/-proj/sid-1.jsonl")).unwrap().contains("\"cwd\""));
        let (_engine, mut rx) = start(ctx, "cdash-a".into(), 4242);
        collect_until_ready(&mut rx).await;
        let EngineMsg::Usage(u) = next_of(&mut rx, |m| matches!(m, EngineMsg::Usage(_))).await else { unreachable!() };
        assert_eq!(u["cwd"], "/proj", "no transcript record carries a cwd: the session file's");
    }

    #[tokio::test]
    async fn a_root_cut_in_memory_leaves_the_replay_only_what_follows_it() {
        let turn: String = include_str!("testdata/rewind.jsonl").lines().take(5).map(|l| format!("{l}\n")).collect();
        let back = json!({"type":"user","uuid":"b1","parentUuid":ROOT,"promptSource":"typed","message":{"role":"user","content":"back"}});
        let (ctx, dir) = fixture_ctx("history-root");
        write_transcript(&dir, "sid-1", &format!("{turn}{back}\n"));
        let (engine, mut rx) = start(ctx, "cdash-a".into(), 4242);
        collect_until_ready(&mut rx).await;
        let page = engine.history(1, 4).await.unwrap();
        assert_eq!(page.iter().map(|e| e["uuid"].as_str().unwrap()).collect::<Vec<_>>(), ["b1"]);
    }
}

#[cfg(test)]
pub(crate) mod aside_tests {
    use super::tests::{collect_until_ready, fixture_with, write_session};
    use super::*;
    use crate::chat::tmux_io::tests::{eventually, install_stub};
    use std::path::Path;

    /// A stub `claude` that appends `$PWD|$*` to `<dir>/claude-args`, then runs `script`.
    pub(crate) fn stub_claude(dir: &Path, script: &str) {
        install_stub(dir, "claude", &format!("#!/bin/sh\nPATH=/usr/bin:/bin\nprintf '%s|%s\\n' \"$PWD\" \"$*\" >> '{}/claude-args'\n{script}\n", dir.display()));
    }
    pub(crate) fn print_lines(text: &str) -> String {
        text.lines().map(|l| format!("printf '%s\\n' '{l}'\n")).collect()
    }
    pub(crate) const ANSWER: &str = r#"{"type":"system","subtype":"init"}
{"type":"assistant","message":{"content":[{"type":"text","text":"You asked about the tests."}]}}
{"type":"result","subtype":"success","is_error":false,"result":"You asked about the tests."}"#;

    /// A session whose file's `cwd` is the test folder, with that folder's command list cached so no warm-up runs the stub.
    pub(crate) fn aside_fixture(tag: &str) -> (Arc<Ctx>, PathBuf) {
        let (ctx, dir) = fixture_with(tag, "");
        write_session(&dir, &format!(r#"{{"sessionId":"sid-1","cwd":"{}","status":"busy"}}"#, dir.display()));
        picker::seed(&dir, json!({}));
        (ctx, dir)
    }

    async fn ready(ctx: &Arc<Ctx>) -> (ClaudeEngine, mpsc::Receiver<EngineMsg>) {
        let (engine, mut rx) = start(Arc::clone(ctx), "cdash-a".into(), 4242);
        collect_until_ready(&mut rx).await;
        (engine, rx)
    }

    #[test]
    fn side_question_arguments_follow_the_spec_and_put_the_question_last() {
        let a = aside_args("sid-1", Some("claude-opus-5-5"), "--what did I ask?");
        assert_eq!(a, ["-p", "--resume", "sid-1", "--fork-session", "--no-session-persistence", "--tools", "", "--strict-mcp-config",
            "--mcp-config", r#"{"mcpServers":{}}"#, "--settings", r#"{"disableAllHooks":true}"#, "--model", "claude-opus-5-5",
            "--output-format", "stream-json", "--verbose", "--", "--what did I ask?"]);
        assert!(!aside_args("sid-1", None, "q").iter().any(|x| x == "--model"));
    }
    #[test]
    fn stream_json_lines_become_chunks() {
        let c = aside_chunk(r#"{"type":"assistant","message":{"content":[{"type":"text","text":"Hi"}]}}"#).unwrap();
        assert_eq!((c.text.as_str(), c.done, c.error), ("Hi", false, None));
        let two = r#"{"type":"assistant","message":{"content":[{"type":"text","text":"One"},{"type":"tool_use"},{"type":"text","text":"Two"}]}}"#;
        assert_eq!(aside_chunk(two).unwrap().text, "One\n\nTwo");
        assert!(aside_chunk(r#"{"type":"result","subtype":"success","is_error":false,"result":"Hi"}"#).is_some_and(|c| c.done && c.text.is_empty() && c.error.is_none()));
        assert!(aside_chunk(r#"{"type":"result","subtype":"error_during_execution","is_error":true,"result":"overloaded"}"#).is_some_and(|c| c.done && c.error.as_deref() == Some("overloaded")));
        assert!(aside_chunk(r#"{"type":"result","subtype":"error_during_execution","is_error":true}"#).is_some_and(|c| c.error.as_deref() == Some("side question failed")));
        assert!(aside_chunk(r#"{"type":"system","subtype":"init"}"#).is_none() && aside_chunk("garbage").is_none());
    }
    #[tokio::test]
    async fn a_side_question_runs_claude_in_the_session_folder_and_streams_its_answer() {
        let (ctx, dir) = aside_fixture("aside");
        stub_claude(&dir, &print_lines(ANSWER));
        let (engine, _rx) = ready(&ctx).await;
        let mut answer = engine.aside("q1", "what did I ask first?").await.unwrap();
        let first = answer.recv().await.unwrap();
        assert_eq!((first.text.as_str(), first.done), ("You asked about the tests.", false));
        assert!(answer.recv().await.unwrap().done);
        let args = std::fs::read_to_string(dir.join("claude-args")).unwrap();
        assert!(args.starts_with(&format!("{}|-p --resume sid-1 ", dir.display())) && args.trim_end().ends_with("-- what did I ask first?"), "{args}");
        assert_eq!(args.lines().count(), 1, "one run");
        eventually("the pid is forgotten once it exits", || ctx.own_pids.lock().unwrap().is_empty()).await;
    }
    #[tokio::test]
    async fn a_stream_without_a_result_ends_with_no_answer_and_no_session_refuses() {
        let (ctx, dir) = aside_fixture("aside-cut");
        stub_claude(&dir, &format!("{}exit 3", print_lines(r#"{"type":"assistant","message":{"content":[{"type":"text","text":"Half"}]}}"#)));
        let (engine, _rx) = ready(&ctx).await;
        let mut answer = engine.aside("q1", "why?").await.unwrap();
        assert_eq!(answer.recv().await.unwrap().text, "Half");
        let end = answer.recv().await.unwrap();
        assert_eq!((end.done, end.error.as_deref()), (true, Some("no answer (claude exited 3)")));
        lock(&engine.live).sid = None;
        assert_eq!(engine.aside("q2", "why?").await.err().as_deref(), Some("no session yet"));
        {
            let mut l = lock(&engine.live);
            l.sid = Some("sid-1".into());
            l.rewound = true;
        }
        assert!(engine.aside("q3", "why?").await.err().is_some_and(|e| e.starts_with("after a rewind")), "the fork would see the abandoned branch");
    }
    #[tokio::test]
    async fn a_side_question_that_outlives_its_limit_is_killed_and_says_so() {
        let (ctx, dir) = aside_fixture("aside-slow");
        stub_claude(&dir, "exec sleep 30");
        let (mut engine, _rx) = ready(&ctx).await;
        engine.aside_max = Duration::from_millis(300);
        let mut answer = engine.aside("q1", "why?").await.unwrap();
        let end = tokio::time::timeout(Duration::from_secs(10), answer.recv()).await.expect("ends at the limit").unwrap();
        assert_eq!((end.done, end.error.as_deref()), (true, Some("no answer in 5 minutes")));
        eventually("the pid is forgotten", || ctx.own_pids.lock().unwrap().is_empty()).await;
    }
    #[tokio::test]
    async fn closing_the_answer_kills_the_process_and_forgets_its_pid() {
        let (ctx, dir) = aside_fixture("aside-kill");
        let tick = dir.join("tick");
        stub_claude(&dir, &format!("while :; do echo x >> '{}'; sleep 0.05; done", tick.display()));
        let (engine, _rx) = ready(&ctx).await;
        let answer = engine.aside("q1", "why?").await.unwrap();
        let size = || std::fs::metadata(&tick).map_or(0, |m| m.len());
        eventually("the process runs", || size() > 0).await;
        assert_eq!(ctx.own_pids.lock().unwrap().len(), 1, "registered while it runs");
        drop(answer);
        eventually("the pid is forgotten", || ctx.own_pids.lock().unwrap().is_empty()).await;
        tokio::time::sleep(Duration::from_millis(300)).await;
        let after = size();
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert_eq!(size(), after, "the process is dead");
    }
}
