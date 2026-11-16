//! One actor per chatted session: numbers events, fans them out, and owns the
//! held-message queue and the delivery rules.

pub use super::wire::{ClientFrame, HeldOp};
use super::asks::check_answer;
use super::media::{self, MAX_FILES};
use super::native::ACK_WAIT;
use super::notify::{NoteKind, Sink};
use super::queue::Queue;
use super::tap::is_cut;
use super::wire::sanitize;
use super::Frame;
use crate::host::log::LogBuffer;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{HashMap, VecDeque};
use std::future::Future;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{broadcast, mpsc, oneshot};
use tokio::time::Instant;

/// Events a fresh client gets before it has to page.
pub const BACKLOG: usize = 200;
pub const PAGE_MAX: usize = 200;
/// Events a hub keeps in memory; older pages are re-read from the source.
pub const WINDOW: usize = 2000;
/// A sent message the session's record never showed is reported after this.
pub const CONFIRM_AFTER: Duration = Duration::from_secs(10);
/// A Stop settles once the record has been quiet this long.
pub const STOP_QUIET: Duration = Duration::from_millis(500);
/// The ceiling on how long after a Stop the hub looks for a prompt Claude
/// handed back. Transcript writes were seen up to 2.2 s late; an interrupt
/// marker that lands inside this window proves the prompt was taken.
pub const STOP_SETTLE: Duration = Duration::from_millis(2500);
/// How long a session waits for an answer before it is worth a notification.
pub const ASK_AFTER: Duration = Duration::from_secs(5);
const TICK: Duration = Duration::from_millis(250);

/// What an engine reports about its session.
#[derive(Debug)]
pub enum EngineMsg {
    Ev(Value),
    Delta { id: String, text: String },
    Status { working: bool, waiting: bool, waiting_for: Option<String> },
    /// The open questions and plans, replacing the last list.
    Asks(Vec<Value>),
    /// The session's model, effort, plan switch and so on, replacing the last.
    Settings(Value),
    /// The session's folder, model, context and token use, replacing the last.
    Usage(Value),
    /// The background shells and subagents running now, replacing the last list.
    Tasks(Vec<Value>),
    /// History has been replayed; subscribers may be answered.
    Ready,
    /// The conversation in the pane changed; positions restart at 1.
    Reset,
    /// The record's format is not understood; nothing is typed from now on.
    Unreadable,
    Ended,
}

/// How far the tool took a message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sent {
    /// Typed in; only the record will show whether it was taken.
    Unconfirmed,
    /// The tool itself accepted it.
    Confirmed,
    /// The engine handled it itself (a command): no prompt will ever show it.
    Done,
}

/// What the user chose on an ask card.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Answer {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub choice: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub answers: Option<Value>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Image,
    File,
}

/// A file the browser uploaded, stored under the cache.
#[derive(Debug, Clone, Serialize)]
pub struct Attachment {
    pub path: PathBuf,
    pub kind: Kind,
    pub mime: String,
    pub name: String,
    pub size: u64,
}

/// A piece of a side question's answer; `done` ends it.
#[derive(Debug)]
pub struct AsideChunk {
    pub text: String,
    pub done: bool,
    pub error: Option<String>,
}

/// The input side of a session.
pub trait Engine: Send + Sync + 'static {
    fn send(&self, text: &str, steer: bool, cid: &str, files: &[Attachment]) -> impl Future<Output = Result<Sent, String>> + Send;
    fn stop(&self) -> impl Future<Output = Result<(), String>> + Send;
    /// Empty an input box a stopped prompt was handed back into.
    fn clear_input(&self, _text: &str) -> impl Future<Output = ()> + Send {
        async {}
    }
    /// The engine's own events numbered `from..=to` since the last `Reset`,
    /// re-read from the source.
    fn history(&self, _from: u64, _to: u64) -> impl Future<Output = Result<Vec<Value>, String>> + Send {
        async { Err("not supported".to_string()) }
    }
    fn answer(&self, _id: &str, _a: &Answer) -> impl Future<Output = Result<(), String>> + Send {
        async { Err("not supported".to_string()) }
    }
    fn set(&self, _what: &str, _value: &Value) -> impl Future<Output = Result<(), String>> + Send {
        async { Err("not supported".to_string()) }
    }
    /// Commands, models, efforts and agents for the composer's menus.
    fn menu(&self) -> impl Future<Output = Value> + Send {
        async { json!({}) }
    }
    /// Whether answers arrive through hooks; read once when the hub starts.
    fn hooks(&self) -> bool {
        true
    }
    /// Rewinds the conversation to before the prompt `uuid`, and the code too
    /// when `files`; answers the prompt's text for the composer.
    fn rewind(&self, _uuid: &str, _files: bool) -> impl Future<Output = Result<String, String>> + Send {
        async { Err("not supported".to_string()) }
    }
    fn rewind_undo(&self) -> impl Future<Output = Result<(), String>> + Send {
        async { Err("not supported".to_string()) }
    }
    /// Asks a side question that never enters the conversation.
    fn aside(&self, _id: &str, _text: &str) -> impl Future<Output = Result<mpsc::Receiver<AsideChunk>, String>> + Send {
        async { Err("not supported".to_string()) }
    }
    fn aside_cancel(&self, _id: &str) -> impl Future<Output = ()> + Send {
        async {}
    }
    /// How many clients are attached now; the hub reports each change.
    fn watchers(&self, _n: usize) -> impl Future<Output = ()> + Send {
        async {}
    }
}

pub struct Subscription {
    pub backlog: Vec<Frame>,
    pub live: broadcast::Receiver<Frame>,
}

enum Cmd {
    Subscribe { after: Option<u64>, epoch: Option<String>, reply: oneshot::Sender<Subscription> },
    Client { frame: ClientFrame, reply: oneshot::Sender<Option<Frame>> },
    Upload { id: String, a: Attachment },
    Folder { reply: oneshot::Sender<Option<String>> },
    Aside { id: String, text: String, reply: oneshot::Sender<Result<mpsc::Receiver<AsideChunk>, String>> },
    AsideCancel { id: String },
}

#[derive(Clone)]
pub struct Hub {
    cmds: mpsc::Sender<Cmd>,
    /// A Claude session's hook cards and settings, for the hook listener.
    pub(crate) claude: Option<super::claude::LiveRef>,
}

impl Hub {
    pub fn spawn<E: Engine>(
        tool: &'static str,
        engine: E,
        events: mpsc::Receiver<EngineMsg>,
        log: Arc<LogBuffer>,
        name: String,
    ) -> Hub {
        Self::spawn_with(tool, engine, events, log, name, None)
    }

    pub fn spawn_with<E: Engine>(
        tool: &'static str,
        engine: E,
        events: mpsc::Receiver<EngineMsg>,
        log: Arc<LogBuffer>,
        name: String,
        notes: Option<Sink>,
    ) -> Hub {
        let (cmds, rx) = mpsc::channel(64);
        let hooks = engine.hooks();
        let (out, _) = broadcast::channel(1024);
        let actor = Actor {
            tool,
            engine,
            log,
            name,
            out,
            epoch: new_epoch(),
            log_events: VecDeque::new(),
            base: 0,
            extra: Vec::new(),
            pending: Vec::new(),
            queue: Queue::default(),
            uploads: HashMap::new(),
            working: false,
            waiting: false,
            waiting_for: None,
            asks: Vec::new(),
            settings: json!({}),
            usage: Value::Null,
            tasks: Vec::new(),
            todo: None,
            hooks,
            ended: false,
            unreadable: false,
            ready: false,
            turn_ok: false,
            stopped_at: None,
            last_ev: Instant::now(),
            waiters: Vec::new(),
            last_state: String::new(),
            notes,
            live: false,
            waiting_since: None,
            asked: false,
            watchers: 0,
        };
        tokio::spawn(actor.run(events, rx));
        Hub { cmds, claude: None }
    }

    /// `None` once the hub has stopped.
    pub async fn subscribe(&self, after: Option<u64>, epoch: Option<String>) -> Option<Subscription> {
        let (reply, rx) = oneshot::channel();
        self.cmds.send(Cmd::Subscribe { after, epoch, reply }).await.ok()?;
        rx.await.ok()
    }

    /// The reply for `send`, `page`, `answer`, `set`, `menu`, `rewind` and
    /// `rewind_undo`; `None` for the rest (`ws.rs` routes `aside` through
    /// [`Hub::aside`]).
    pub async fn command(&self, frame: ClientFrame) -> Option<Frame> {
        let (reply, rx) = oneshot::channel();
        self.cmds.send(Cmd::Client { frame, reply }).await.ok()?;
        rx.await.ok().flatten()
    }

    /// Registers a finished upload. It goes through the command channel, so a
    /// `send` that follows on the same socket always finds it.
    pub async fn add_upload(&self, id: String, a: Attachment) {
        let _ = self.cmds.send(Cmd::Upload { id, a }).await;
    }

    /// The session's current folder: the latest usage snapshot's `cwd`.
    pub async fn folder(&self) -> Option<String> {
        let (reply, rx) = oneshot::channel();
        self.cmds.send(Cmd::Folder { reply }).await.ok()?;
        rx.await.ok().flatten()
    }

    /// Asks a side question; its answer goes only to the caller.
    pub async fn aside(&self, id: String, text: String) -> Result<mpsc::Receiver<AsideChunk>, String> {
        let (reply, rx) = oneshot::channel();
        self.cmds.send(Cmd::Aside { id, text, reply }).await.map_err(|_| "session ended")?;
        rx.await.unwrap_or_else(|_| Err("session ended".into()))
    }

    pub async fn aside_cancel(&self, id: String) {
        let _ = self.cmds.send(Cmd::AsideCancel { id }).await;
    }

    pub fn is_closed(&self) -> bool {
        self.cmds.is_closed()
    }
}

pub(crate) fn new_epoch() -> String {
    let mut b = [0u8; 8];
    getrandom::fill(&mut b).expect("the OS CSPRNG must be available");
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn frame(v: Value) -> Frame {
    Frame::from(v.to_string())
}

fn ack(cid: &str, status: &str, error: Option<&str>) -> Value {
    let mut a = json!({"t":"ack","cid":cid,"status":status});
    if let Some(e) = error {
        a["error"] = json!(e);
    }
    a
}

/// The `rewound` reply: the `Ok` object's fields on success, `error` otherwise.
fn rewound(uuid: Value, r: Result<Value, String>) -> Value {
    let mut v = json!({"t":"rewound","uuid":uuid,"ok":r.is_ok()});
    match r {
        Ok(Value::Object(extra)) => v.as_object_mut().unwrap().extend(extra),
        Ok(_) => {}
        Err(e) => v["error"] = json!(e),
    }
    v
}

/// A prompt is a `user` event whose content is not a `tool_result` list.
fn prompt_text(ev: &Value) -> Option<String> {
    if ev["type"] != "user" {
        return None;
    }
    match &ev["message"]["content"] {
        Value::String(s) => Some(s.clone()),
        Value::Array(blocks) if !blocks.iter().any(|b| b["type"] == "tool_result") => Some(
            blocks.iter().filter(|b| b["type"] == "text").filter_map(|b| b["text"].as_str()).collect::<Vec<_>>().join(""),
        ),
        _ => None,
    }
}

fn is_todo(ev: &Value) -> bool {
    ev["type"] == "cdash" && ev["subtype"] == "todo"
}

fn is_retract(ev: &Value) -> bool {
    ev["type"] == "cdash" && ev["subtype"] == "retract"
}

/// Where the cut event `cut` leaves `log`, the part of a log still in memory
/// after `base` events left it: the length to keep, or `None` when it removes
/// nothing or its anchor is not there (then nothing is removed, so chat never
/// hides what the tool continues from). A `null` anchor is the log's start,
/// which is not there once `base > 0`. The hub and Claude's `history` replay
/// both place cuts here, so their positions agree.
pub(crate) fn place_cut<'a, I>(log: I, base: usize, cut: &Value) -> Option<usize>
where
    I: IntoIterator<Item = &'a Value>,
    I::IntoIter: DoubleEndedIterator + ExactSizeIterator,
{
    let mut log = log.into_iter();
    let len = log.len();
    let keep = match &cut["after"] {
        Value::Null if base == 0 => 0,
        // A retract's uuid names the prompt it withdraws, not itself.
        Value::String(u) => 1 + log.rposition(|e| e["uuid"] == u.as_str() && !is_retract(e))?,
        _ => return None,
    };
    (keep < len).then_some(keep)
}

/// A message handed to the engine and not yet answered.
struct Pending {
    cid: String,
    text: String,
    at: Instant,
    /// The event that showed it, once one did.
    uuid: Option<String>,
    /// Already in the tool's hands: never times out, never blocks the queue.
    exempt: bool,
}

struct Actor<E> {
    tool: &'static str,
    engine: E,
    log: Arc<LogBuffer>,
    name: String,
    out: broadcast::Sender<Frame>,
    epoch: String,
    /// Positions `base + 1..=head`, at most `WINDOW` of them.
    log_events: VecDeque<Value>,
    base: usize,
    /// Events the hub appended itself, by position: the engine never numbered
    /// them, so an engine number is a position less the extras before it.
    extra: Vec<(usize, Value)>,
    pending: Vec<Pending>,
    queue: Queue,
    /// Finished uploads by id, for the sends that name them.
    // ponytail: one entry per upload for the hub's life; prune on send or on the sweep if it grows.
    uploads: HashMap<String, Attachment>,
    working: bool,
    waiting: bool,
    waiting_for: Option<String>,
    asks: Vec<Value>,
    settings: Value,
    usage: Value,
    tasks: Vec<Value>,
    /// The latest `todo` event's position and items: the pinned checklist,
    /// kept here so it outlives the backlog.
    todo: Option<(usize, Value)>,
    hooks: bool,
    ended: bool,
    unreadable: bool,
    ready: bool,
    /// The last turn ended on its own, so the next held message may go.
    turn_ok: bool,
    stopped_at: Option<Instant>,
    /// When the engine last reported a record event.
    last_ev: Instant,
    waiters: Vec<(Option<u64>, Option<String>, oneshot::Sender<Subscription>)>,
    last_state: String,
    notes: Option<Sink>,
    /// Past the replayed history: only live turn ends notify.
    live: bool,
    waiting_since: Option<Instant>,
    asked: bool,
    /// Attached clients the engine was last told about.
    watchers: usize,
}

impl<E: Engine> Actor<E> {
    fn note(&self, kind: NoteKind) {
        if let (true, Some(s)) = (self.live, &self.notes) {
            s.note(&self.name, kind);
        }
    }

    async fn run(mut self, mut events: mpsc::Receiver<EngineMsg>, mut cmds: mpsc::Receiver<Cmd>) {
        let mut tick = tokio::time::interval(TICK);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tokio::select! {
                msg = events.recv() => match msg {
                    Some(m) => self.engine_msg(m).await,
                    None => {
                        self.end();
                        return;
                    }
                },
                cmd = cmds.recv() => match cmd {
                    Some(Cmd::Subscribe { after, epoch, reply }) => {
                        if self.ready {
                            let sub = self.subscription(after, epoch);
                            self.watch().await;
                            let _ = reply.send(sub);
                        } else {
                            self.waiters.push((after, epoch, reply));
                        }
                    }
                    Some(Cmd::Client { frame, reply }) => {
                        let r = self.client(frame).await;
                        let _ = reply.send(r);
                    }
                    Some(Cmd::Upload { id, a }) => {
                        self.uploads.insert(id, a);
                    }
                    Some(Cmd::Folder { reply }) => {
                        let _ = reply.send(self.usage["cwd"].as_str().map(String::from));
                    }
                    Some(Cmd::Aside { id, text, reply }) => {
                        let _ = reply.send(self.aside(&id, &text).await);
                    }
                    Some(Cmd::AsideCancel { id }) => self.engine.aside_cancel(&id).await,
                    // Every handle was dropped; nobody can reach this hub.
                    None => return,
                },
                _ = tick.tick() => self.tick().await,
            }
            self.release().await;
            self.publish_state();
        }
    }

    fn broadcast(&self, v: Value) {
        let _ = self.out.send(frame(v));
    }

    fn state(&self) -> Value {
        let held: Vec<Value> = self
            .queue
            .list()
            .iter()
            .map(|h| {
                let files: Vec<Value> = h
                    .files
                    .iter()
                    .filter_map(|id| self.uploads.get(id).map(|a| json!({"id":id,"name":a.name,"kind":a.kind,"size":a.size,"mime":a.mime})))
                    .collect();
                json!({"id":h.id,"text":h.text,"files":files})
            })
            .collect();
        json!({"t":"state","working":self.working,"waiting":self.waiting,"waiting_for":self.waiting_for,"asks":self.asks,"settings":self.settings,"usage":self.usage,"tasks":self.tasks,"todo":self.todo.as_ref().map(|(_, v)| v),"hooks":self.hooks,"ended":self.ended,"unreadable":self.unreadable,"held":held})
    }

    fn publish_state(&mut self) {
        let s = self.state().to_string();
        if s != self.last_state {
            let _ = self.out.send(Frame::from(s.as_str()));
            self.last_state = s;
        }
    }

    fn head(&self) -> usize {
        self.base + self.log_events.len()
    }

    fn ev_frame(&self, seq: usize) -> Value {
        json!({"t":"ev","seq":seq,"ev":self.log_events[seq - self.base - 1]})
    }

    /// Built inside the actor, so nothing is missed or repeated between the
    /// backlog and the live stream.
    fn subscription(&self, after: Option<u64>, epoch: Option<String>) -> Subscription {
        Subscription { live: self.out.subscribe(), backlog: self.backlog(after, epoch) }
    }

    /// `hello`, the events a client lacks, then `state`.
    fn backlog(&self, after: Option<u64>, epoch: Option<String>) -> Vec<Frame> {
        let head = self.head();
        // A position older than the window starts over, as a new conversation would.
        let reset = epoch.as_deref() != Some(self.epoch.as_str()) || after.is_some_and(|a| (a as usize) < self.base);
        let first = match (reset, after) {
            (false, Some(a)) => (a as usize).min(head) + 1,
            _ => head.saturating_sub(BACKLOG) + 1,
        };
        let mut backlog = vec![frame(json!({
            "t":"hello","epoch":self.epoch,"tool":self.tool,"first":first,"head":head,"reset":reset
        }))];
        backlog.extend((first..=head).map(|seq| frame(self.ev_frame(seq))));
        backlog.push(frame(self.state()));
        backlog
    }

    /// Keeps the first `len` events in memory and reloads every client as a
    /// new one would load. Pending messages and the queue stay.
    fn cut_to(&mut self, len: usize) {
        if len >= self.log_events.len() {
            return;
        }
        self.log_events.truncate(len);
        let head = self.head();
        self.extra.retain(|(p, _)| *p <= head);
        if self.todo.as_ref().is_some_and(|(p, _)| *p > head) {
            // ponytail: a checklist older than the window is not found again; keep a list per position if that shows.
            self.todo = self.log_events.iter().rposition(is_todo).map(|i| (self.base + i + 1, self.log_events[i]["items"].clone()));
        }
        self.epoch = new_epoch();
        if self.out.receiver_count() == 0 {
            return;
        }
        for f in self.backlog(None, None) {
            let _ = self.out.send(f);
        }
    }

    fn append(&mut self, ev: Value) {
        self.log_events.push_back(ev);
        let seq = self.head();
        self.broadcast(self.ev_frame(seq));
        if self.log_events.len() > WINDOW {
            self.log_events.pop_front();
            self.base += 1;
        }
    }

    async fn engine_msg(&mut self, m: EngineMsg) {
        match m {
            EngineMsg::Ev(ev) => {
                self.last_ev = Instant::now();
                self.event(ev)
            }
            EngineMsg::Delta { id, text } => self.broadcast(json!({"t":"delta","id":id,"text":text})),
            EngineMsg::Status { working, waiting, waiting_for } => {
                if waiting && !self.waiting {
                    self.waiting_since = Some(Instant::now());
                    self.asked = false;
                } else if !waiting {
                    self.waiting_since = None;
                }
                self.working = working;
                self.waiting = waiting;
                self.waiting_for = waiting_for;
            }
            EngineMsg::Asks(a) => self.asks = a,
            EngineMsg::Settings(v) => self.settings = v,
            EngineMsg::Usage(v) => self.usage = v,
            EngineMsg::Tasks(t) => self.tasks = t,
            EngineMsg::Ready => {
                self.ready = true;
                self.live = true;
                for (after, epoch, reply) in std::mem::take(&mut self.waiters) {
                    let _ = reply.send(self.subscription(after, epoch));
                }
            }
            EngineMsg::Reset => {
                self.live = false;
                self.epoch = new_epoch();
                self.log_events.clear();
                self.base = 0;
                self.extra.clear();
                (self.usage, self.tasks, self.todo) = (Value::Null, Vec::new(), None);
                self.broadcast(json!({"t":"reset","epoch":self.epoch}));
                // As CONFIRM_AFTER would have: what no event showed (and the tool never accepted).
                for p in std::mem::take(&mut self.pending) {
                    if !p.exempt && p.uuid.is_none() {
                        self.broadcast(ack(&p.cid, "unconfirmed", None));
                    }
                }
            }
            EngineMsg::Unreadable => self.unreadable = true,
            EngineMsg::Ended => self.end(),
        }
    }

    fn event(&mut self, mut ev: Value) {
        if is_cut(&ev) {
            if let Some(len) = place_cut(&self.log_events, self.base, &ev) {
                self.cut_to(len);
            }
            return;
        }
        if is_todo(&ev) {
            self.todo = Some((self.head() + 1, ev["items"].clone()));
        }
        if let Some(text) = prompt_text(&ev) {
            if let Some(p) = self.pending.iter_mut().find(|p| p.uuid.is_none() && p.text == text) {
                ev["cid"] = json!(p.cid);
                p.uuid = Some(ev["uuid"].as_str().unwrap_or_default().to_string());
            }
            self.turn_ok = false;
        } else if ev["type"] == "assistant" || ev["type"] == "user" || ev["type"] == "result" {
            // A finished turn also settles what the tool itself confirmed.
            let done = ev["type"] == "result";
            self.pending.retain(|p| p.uuid.is_none() && !(done && p.exempt));
            if done {
                if ev["subtype"] == "interrupted" {
                    self.queue.stop_all();
                } else {
                    self.turn_ok = true;
                }
                match ev["subtype"].as_str() {
                    // A held message about to leave on its own is not the end.
                    Some("success") if !self.queue.list().first().is_some_and(|h| h.auto) => self.note(NoteKind::Done),
                    Some("error_during_execution") => self.note(NoteKind::Error),
                    _ => {}
                }
            }
        } else if ev["type"] == "cdash" && ev["subtype"] == "queued" {
            // In the tool's own queue now: out of our hands, so it must not
            // time out or block the next held message.
            let text = ev["text"].as_str().unwrap_or_default();
            if let Some(p) = self.pending.iter_mut().find(|p| p.uuid.is_none() && !p.exempt && p.text == text) {
                p.exempt = true;
                ev["cid"] = json!(p.cid);
            }
        }
        self.append(ev);
    }

    async fn client(&mut self, f: ClientFrame) -> Option<Frame> {
        match f {
            ClientFrame::Send { cid, text, files } => Some(frame(self.send(cid, &text, files).await)),
            ClientFrame::Upload { .. } => None,
            ClientFrame::Stop => {
                self.queue.stop_all();
                if self.working || !self.pending.is_empty() {
                    if let Err(e) = self.engine.stop().await {
                        self.log.push(format!("chat stop failed {}: {e}", self.name));
                    }
                    self.stopped_at = Some(Instant::now());
                }
                None
            }
            ClientFrame::Held { op: HeldOp::Remove, id } => {
                self.queue.take(&id);
                None
            }
            ClientFrame::Held { op: HeldOp::Now, id } => {
                if let Some(e) = self.refusal() {
                    // Kept in the queue: Enter into a dialog would answer it.
                    if let Some(h) = self.queue.list().iter().find(|h| h.id == id) {
                        self.broadcast(ack(&h.cid, "refused", Some(e)));
                    }
                } else if let Some(h) = self.queue.take(&id) {
                    let steer = self.working;
                    let a = self.deliver(&h.cid, &h.text, &h.files, steer).await;
                    self.broadcast(a);
                }
                None
            }
            ClientFrame::Page { before, limit } => Some(frame(self.page(before, limit).await)),
            // The socket pump consumes focus frames itself.
            ClientFrame::Focus { .. } => None,
            ClientFrame::Answer { id, choice, text, answers } => {
                let a = Answer { choice, text, answers };
                let r = match self.asks.iter().find(|k| k["id"] == id.as_str()) {
                    None => Err("already answered".to_string()),
                    Some(ask) => match check_answer(ask, &a) {
                        Ok(()) => self.engine.answer(&id, &a).await,
                        Err(e) => Err(e.to_string()),
                    },
                };
                self.log.push(format!("chat answer {}", self.name));
                let mut v = json!({"t":"answered","id":id,"ok":r.is_ok()});
                if let Err(e) = r {
                    v["error"] = json!(e);
                }
                Some(frame(v))
            }
            ClientFrame::Set { what, value } => {
                // Claude's picker types `/model x` into the box: same rule as a send.
                // Codex and OpenCode change settings over their API, mid-turn too.
                let r = match self.rewind_refusal().filter(|_| self.tool == "claude") {
                    Some(e) => Err(e.to_string()),
                    None => self.engine.set(&what, &value).await,
                };
                self.log.push(format!("chat set {} {what:?}", self.name));
                let mut v = json!({"t":"set","ok":r.is_ok()});
                if let Err(e) = r {
                    v["error"] = json!(e);
                }
                Some(frame(v))
            }
            ClientFrame::Menu => {
                let m = self.engine.menu().await;
                let list = |k: &str| m.get(k).cloned().unwrap_or_else(|| json!([]));
                Some(frame(json!({"t":"menu","commands":list("commands"),"models":list("models"),"efforts":list("efforts"),"agents":list("agents")})))
            }
            ClientFrame::Rewind { uuid, files } => Some(frame(self.rewind(uuid, files).await)),
            ClientFrame::RewindUndo => {
                let r = match self.rewind_refusal() {
                    Some(e) => Err(e.to_string()),
                    None => self.engine.rewind_undo().await,
                };
                match &r {
                    Ok(()) => self.log.push(format!("chat rewind undo {}", self.name)),
                    Err(e) => self.log.push(format!("chat rewind undo failed {}: {e}", self.name)),
                }
                Some(frame(rewound(Value::Null, r.map(|()| json!({})))))
            }
            ClientFrame::Aside { .. } | ClientFrame::AsideCancel { .. } => None,
        }
    }

    /// A side question reaches the engine and nothing else: no log, no queue.
    async fn aside(&self, id: &str, text: &str) -> Result<mpsc::Receiver<AsideChunk>, String> {
        let text = sanitize(text)?;
        if self.ended {
            return Err("session ended".into());
        }
        let r = self.engine.aside(id, &text).await;
        match &r {
            Ok(_) => self.log.push(format!("chat aside {}", self.name)),
            Err(e) => self.log.push(format!("chat aside failed {}: {e}", self.name)),
        }
        r
    }

    /// Why the conversation may not be rewound now: only an idle session that
    /// waits for nothing is, so no key lands in a turn or a dialog. A prompt
    /// just typed counts as a turn: the engine's `working` lags it.
    fn rewind_refusal(&self) -> Option<&'static str> {
        use super::picker::BUSY;
        if self.working {
            Some(BUSY)
        } else if let Some(e) = self.refusal() {
            Some(e)
        } else {
            self.pending.iter().any(|p| !p.exempt).then_some(BUSY)
        }
    }

    /// Runs inside the actor, so no paste, release or other key interleaves.
    async fn rewind(&mut self, uuid: String, files: bool) -> Value {
        if let Some(e) = self.rewind_refusal() {
            return rewound(json!(uuid), Err(e.into()));
        }
        // A prompt older than the window is not found: it is not in memory to cut.
        let Some(i) = self.log_events.iter().rposition(|e| e["uuid"] == uuid.as_str() && prompt_text(e).is_some()) else {
            return rewound(json!(uuid), Err("not in this conversation".into()));
        };
        let target = &self.log_events[i];
        if target["src"] == "steer" {
            return rewound(json!(uuid), Err("this prompt cannot be rewound".into()));
        }
        let attachments = target["files"].as_array().map_or(0, Vec::len);
        // A uuid-less row just before it (a `cdash/queued`) announced this prompt.
        // ponytail: a queued row further back (the turn it waited on wrote more)
        // stays and shows as "waiting" again. Dropping it from the middle would
        // shift every later position off the engine's numbering that paging and
        // Claude's history replay use; hide it in the store if that shows.
        let len = i - self.log_events.range(..i).rev().take_while(|e| !e["uuid"].is_string()).count();
        match self.engine.rewind(&uuid, files).await {
            Ok(text) => {
                self.cut_to(len);
                self.log.push(format!("chat rewind {}", self.name));
                rewound(json!(uuid), Ok(json!({"text":text,"attachments":attachments})))
            }
            Err(e) => {
                self.log.push(format!("chat rewind failed {}: {e}", self.name));
                rewound(json!(uuid), Err(e))
            }
        }
    }

    fn resolve(&self, ids: &[String]) -> Result<Vec<Attachment>, &'static str> {
        if ids.len() > MAX_FILES {
            return Err("too many attachments");
        }
        ids.iter()
            .map(|id| self.uploads.get(id).filter(|a| a.path.exists()).cloned().ok_or("attachment expired"))
            .collect()
    }

    async fn send(&mut self, cid: String, text: &str, files: Vec<String>) -> Value {
        let text = match sanitize(text) {
            Ok(t) => t,
            // Attachments alone are a message.
            Err("empty message") if !files.is_empty() => String::new(),
            Err(e) => return ack(&cid, "refused", Some(e)),
        };
        if let Err(e) = self.resolve(&files) {
            return ack(&cid, "refused", Some(e));
        }
        if self.working && !self.ended && !self.unreadable && !self.waiting {
            let id = self.queue.hold(cid.clone(), text, files);
            let mut a = ack(&cid, "held", None);
            a["id"] = json!(id);
            return a;
        }
        if let Some(e) = self.refusal() {
            return ack(&cid, "refused", Some(e));
        }
        self.deliver(&cid, &text, &files, false).await
    }

    /// Why nothing may be typed into the session right now, if anything.
    fn refusal(&self) -> Option<&'static str> {
        if self.ended {
            Some("session ended")
        } else if self.unreadable {
            Some("cannot read this session; use Terminal")
        } else if self.waiting {
            Some(if self.asks.is_empty() { "answer in Terminal first" } else { "answer the card first" })
        } else if self.stopped_at.is_some() && self.pending.iter().any(|p| !p.exempt) {
            // A paste now could join a prompt Claude is handing back.
            Some("stopping; send again in a moment")
        } else {
            None
        }
    }

    async fn deliver(&mut self, cid: &str, text: &str, ids: &[String], steer: bool) -> Value {
        // Resolved again: an upload may have expired while the message waited.
        let files = match self.resolve(ids) {
            Ok(f) => f,
            Err(e) => return ack(cid, "refused", Some(e)),
        };
        match self.engine.send(text, steer, cid, &files).await {
            Err(e) => ack(cid, "refused", Some(&e)),
            Ok(Sent::Done) => {
                self.log.push(format!("chat send {}", self.name));
                ack(cid, "done", None)
            }
            Ok(confirmed) => {
                self.pending.push(Pending {
                    cid: cid.to_string(),
                    text: text.to_string(),
                    at: Instant::now(),
                    uuid: None,
                    exempt: confirmed == Sent::Confirmed,
                });
                self.log.push(format!("chat send {}", self.name));
                ack(cid, "sent", None)
            }
        }
    }

    async fn release(&mut self) {
        let idle = !self.working && !self.waiting && !self.unreadable && !self.ended;
        if !idle || !self.turn_ok || !self.pending.iter().all(|p| p.exempt) {
            return;
        }
        if let Some(h) = self.queue.next_auto() {
            self.turn_ok = false;
            let a = self.deliver(&h.cid, &h.text, &h.files, false).await;
            self.broadcast(a);
        }
    }

    // ponytail: an older page blocks this hub while the source is re-read, O(transcript) per page for Claude; move it off the actor if it shows.
    async fn page(&self, before: u64, limit: Option<usize>) -> Value {
        let limit = limit.unwrap_or(PAGE_MAX).min(PAGE_MAX);
        let end = (before as usize).saturating_sub(1).min(self.head());
        let start = end.saturating_sub(limit);
        let own = |p: usize| self.extra.iter().find(|(q, _)| *q == p).map(|(_, v)| v);
        let number = |p: usize| (p - self.extra.iter().filter(|(q, _)| *q < p).count()) as u64;
        // Positions that left memory and are the engine's: one contiguous run of its numbers.
        let gone: Vec<usize> = (start + 1..=end.min(self.base)).filter(|&p| own(p).is_none()).collect();
        let (mut source, mut ok) = (Vec::new(), true);
        if let (Some(&a), Some(&b)) = (gone.first(), gone.last()) {
            let read = tokio::time::timeout(ACK_WAIT, self.engine.history(number(a), number(b))).await;
            match read.unwrap_or_else(|_| Err("no answer from the source".into())) {
                Ok(v) if v.len() == gone.len() => source = v,
                // Short: a transcript cut or replaced since; its numbers no longer line up.
                r => {
                    ok = false;
                    let e = r.err().unwrap_or_else(|| "the source came up short".into());
                    self.log.push(format!("chat page failed {}: {e}", self.name));
                }
            }
        }
        let first = gone.first().map_or(0, |&p| number(p));
        let events: Vec<Value> = (start + 1..=end)
            .filter_map(|p| {
                let ev = if p > self.base {
                    Some(&self.log_events[p - self.base - 1])
                } else {
                    own(p).or_else(|| source.get((number(p) - first) as usize))
                };
                ev.map(|ev| json!({"seq":p,"ev":ev}))
            })
            .collect();
        // Without the source, asking again would loop on the same empty page.
        json!({"t":"page","events":events,"more":start > 0 && ok})
    }

    /// Tells the engine how many clients are attached, when that changed.
    async fn watch(&mut self) {
        let n = self.out.receiver_count();
        if n != self.watchers {
            self.watchers = n;
            self.engine.watchers(n).await;
        }
    }

    async fn tick(&mut self) {
        self.watch().await;
        let now = Instant::now();
        if let (true, false, true, Some(since)) = (self.live, self.asked, self.waiting, self.waiting_since) {
            if now.duration_since(since) >= ASK_AFTER && self.waiting_for.as_deref() != Some("dialog open") {
                self.asked = true;
                self.note(NoteKind::Ask);
            }
        }
        let (late, keep): (Vec<Pending>, Vec<Pending>) = std::mem::take(&mut self.pending)
            .into_iter()
            .partition(|p| !p.exempt && p.uuid.is_none() && now.duration_since(p.at) > CONFIRM_AFTER);
        self.pending = keep;
        for p in late {
            self.broadcast(ack(&p.cid, "unconfirmed", None));
        }
        let Some(at) = self.stopped_at else { return };
        let settled = now.duration_since(at) >= STOP_SETTLE || now.duration_since(at.max(self.last_ev)) >= STOP_QUIET;
        if !settled || self.working {
            if now.duration_since(at) > CONFIRM_AFTER {
                self.stopped_at = None;
            }
            return;
        }
        self.stopped_at = None;
        // Stopped before any reply: Claude put the prompt back in its input box.
        let Some(i) = self.pending.iter().rposition(|p| !p.exempt) else { return };
        let p = self.pending.remove(i);
        self.engine.clear_input(&p.text).await;
        if let Some(uuid) = &p.uuid {
            let ev = json!({"type":"cdash","subtype":"retract","uuid":uuid});
            self.extra.push((self.head() + 1, ev.clone()));
            self.append(ev);
        }
        self.broadcast(ack(&p.cid, "returned", None));
    }

    fn end(&mut self) {
        if self.ended {
            return;
        }
        self.ended = true;
        self.asks.clear();
        self.tasks.clear();
        let held: Vec<String> = self.queue.clear().into_iter().map(|h| h.cid).collect();
        let pending: Vec<String> = self.pending.drain(..).map(|p| p.cid).collect();
        for cid in held.iter().chain(&pending) {
            self.broadcast(ack(cid, "refused", Some("session ended")));
        }
        self.publish_state();
        if let Some(dir) = media::uploads_dir(&self.name) {
            tokio::spawn(tokio::fs::remove_dir_all(dir));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chat::notify::Note;
    use crate::collect::ctx::{Ctx, FocusGuard};
    use serde_json::{json, Value};
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    #[derive(Clone)]
    struct FakeEngine {
        calls: Arc<Mutex<Vec<String>>>,
        /// What every send answers.
        sent: Sent,
        slow: Option<Duration>,
        /// The last event number the source still has.
        has: u64,
        fail_rewind: bool,
    }
    impl Engine for FakeEngine {
        async fn send(&self, text: &str, steer: bool, _cid: &str, files: &[Attachment]) -> Result<Sent, String> {
            match self.slow {
                Some(d) => {
                    self.calls.lock().unwrap().push(format!("begin:{text}"));
                    tokio::time::sleep(d).await;
                    self.calls.lock().unwrap().push(format!("end:{text}"));
                }
                None => {
                    let names: Vec<&str> = files.iter().map(|a| a.name.as_str()).collect();
                    let tail = if names.is_empty() { String::new() } else { format!(":{}", names.join(",")) };
                    self.calls.lock().unwrap().push(format!("send:{text}:{steer}{tail}"));
                }
            }
            Ok(self.sent)
        }
        async fn stop(&self) -> Result<(), String> {
            self.calls.lock().unwrap().push("stop".into());
            Ok(())
        }
        async fn clear_input(&self, text: &str) {
            self.calls.lock().unwrap().push(format!("clear:{text}"));
        }
        async fn answer(&self, id: &str, a: &Answer) -> Result<(), String> {
            self.calls.lock().unwrap().push(format!("answer:{id}:{}", a.choice.as_deref().unwrap_or("-")));
            Ok(())
        }
        async fn set(&self, what: &str, value: &Value) -> Result<(), String> {
            self.calls.lock().unwrap().push(format!("set:{what}:{value}"));
            if what == "nope" { Err("not here".into()) } else { Ok(()) }
        }
        async fn menu(&self) -> Value {
            json!({"commands":[{"name":"compact"}]})
        }
        async fn history(&self, from: u64, to: u64) -> Result<Vec<Value>, String> {
            Ok((from..=to.min(self.has)).map(|k| json!({"type":"user","n":k})).collect())
        }
        async fn rewind(&self, uuid: &str, files: bool) -> Result<String, String> {
            self.calls.lock().unwrap().push(format!("rewind:{uuid}:{files}"));
            if self.fail_rewind { Err("rewind failed; use Terminal".into()) } else { Ok(format!("text of {uuid}")) }
        }
        async fn rewind_undo(&self) -> Result<(), String> {
            self.calls.lock().unwrap().push("undo".into());
            Ok(())
        }
        /// Answers `re: <text>`, then `done`.
        async fn aside(&self, id: &str, text: &str) -> Result<mpsc::Receiver<AsideChunk>, String> {
            self.calls.lock().unwrap().push(format!("aside:{id}:{text}"));
            let (tx, rx) = mpsc::channel(2);
            tx.try_send(AsideChunk { text: format!("re: {text}"), done: false, error: None }).unwrap();
            tx.try_send(AsideChunk { text: String::new(), done: true, error: None }).unwrap();
            Ok(rx)
        }
        async fn aside_cancel(&self, id: &str) {
            self.calls.lock().unwrap().push(format!("aside_cancel:{id}"));
        }
    }

    type Calls = Arc<Mutex<Vec<String>>>;

    fn build(
        name: &str,
        slow: Option<Duration>,
        sent: Sent,
        notes: Option<Sink>,
        ready: bool,
        has: u64,
        fail_rewind: bool,
    ) -> (Hub, tokio::sync::mpsc::Sender<EngineMsg>, Calls) {
        let calls = Calls::default();
        let (tx, rx) = tokio::sync::mpsc::channel(64);
        let engine = FakeEngine { calls: calls.clone(), sent, slow, has, fail_rewind };
        let log = Arc::new(crate::host::log::LogBuffer::new());
        let hub = Hub::spawn_with("claude", engine, rx, log, name.into(), notes);
        if ready {
            tx.try_send(EngineMsg::Ready).unwrap();
        }
        (hub, tx, calls)
    }
    fn rig_with(slow: Option<Duration>) -> (Hub, tokio::sync::mpsc::Sender<EngineMsg>, Calls) {
        build("cdash-a", slow, Sent::Unconfirmed, None, true, u64::MAX, false)
    }
    /// A rig whose engine confirms what it accepts, as Codex and OpenCode do.
    fn rig_accepting() -> (Hub, tokio::sync::mpsc::Sender<EngineMsg>, Calls) {
        build("cdash-a", None, Sent::Confirmed, None, true, u64::MAX, false)
    }
    fn rig_failing_rewind() -> (Hub, tokio::sync::mpsc::Sender<EngineMsg>, Calls) {
        build("cdash-a", None, Sent::Unconfirmed, None, true, u64::MAX, true)
    }
    fn rig_sending(sent: Sent) -> (Hub, tokio::sync::mpsc::Sender<EngineMsg>, Calls) {
        build("cdash-a", None, sent, None, true, u64::MAX, false)
    }
    fn rig_noting(ready: bool) -> (Hub, tokio::sync::mpsc::Sender<EngineMsg>, Arc<Ctx>, broadcast::Receiver<Note>) {
        let ctx = crate::chat::claude::tests::ctx_with_path(
            crate::chat::tmux_io::tests::tempfile_path("noting"),
            String::new(),
        );
        let notes = ctx.notes.subscribe();
        let sink = Sink { ctx: Arc::clone(&ctx), project: "proj".into() };
        let (hub, tx, _) = build("cdash-a", None, Sent::Unconfirmed, Some(sink), ready, u64::MAX, false);
        (hub, tx, ctx, notes)
    }
    fn note_kinds(rx: &mut broadcast::Receiver<Note>) -> Vec<&'static str> {
        std::iter::from_fn(|| rx.try_recv().ok()).map(|n| n.kind.as_str()).collect()
    }
    fn turn_end(subtype: &str) -> EngineMsg {
        EngineMsg::Ev(json!({"type":"result","subtype":subtype}))
    }
    fn engine_status(working: bool, waiting: bool) -> EngineMsg {
        EngineMsg::Status { working, waiting, waiting_for: None }
    }
    /// The fake re-reads history as `{"type":"user","n":k}`, up to number `has`.
    fn rig_history_to(has: u64) -> (Hub, tokio::sync::mpsc::Sender<EngineMsg>, Calls) {
        build("cdash-a", None, Sent::Unconfirmed, None, true, has, false)
    }
    fn rig_history() -> (Hub, tokio::sync::mpsc::Sender<EngineMsg>, Calls) {
        rig_history_to(u64::MAX)
    }
    fn rig_named(name: &str) -> (Hub, tokio::sync::mpsc::Sender<EngineMsg>, Calls) {
        build(name, None, Sent::Unconfirmed, None, true, u64::MAX, false)
    }
    fn rig() -> (Hub, tokio::sync::mpsc::Sender<EngineMsg>, Calls) {
        rig_named("cdash-a")
    }
    fn rig_slow(d: Duration) -> (Hub, tokio::sync::mpsc::Sender<EngineMsg>, Calls) {
        rig_with(Some(d))
    }
    fn frames(sub: &mut Subscription) -> Vec<Value> {
        let mut out = Vec::new();
        while let Ok(f) = sub.live.try_recv() {
            out.push(serde_json::from_str(&f).unwrap());
        }
        out
    }
    fn prompt(uuid: &str, text: &str) -> Value {
        json!({"type":"user","uuid":uuid,"message":{"role":"user","content":text},"src":"typed"})
    }
    fn answer(uuid: &str) -> Value {
        json!({"type":"assistant","uuid":uuid,"message":{"id":"m","role":"assistant","content":[{"type":"text","text":"ok"}]}})
    }
    fn result(uuid: &str) -> Value {
        json!({"type":"result","subtype":"success","uuid":uuid})
    }
    fn cut(after: Option<&str>) -> Value {
        json!({"type":"cdash","subtype":"cut","after":after})
    }
    async fn log6(hub: &Hub, tx: &mpsc::Sender<EngineMsg>) {
        for e in [prompt("u1", "a"), answer("k1"), result("s1"), prompt("u2", "b"), answer("k2"), result("s2")] { tx.send(EngineMsg::Ev(e)).await.unwrap(); }
        settle(hub, tx).await;
    }
    fn last_state(s: &Subscription) -> Value {
        serde_json::from_str(s.backlog.last().unwrap()).unwrap()
    }
    fn send_with(cid: &str, text: &str, ids: &[&str]) -> ClientFrame {
        ClientFrame::Send { cid: cid.into(), text: text.into(), files: ids.iter().map(|s| s.to_string()).collect() }
    }
    fn send(cid: &str, text: &str) -> ClientFrame {
        send_with(cid, text, &[])
    }
    async fn upload(hub: &Hub, id: &str, kind: Kind, name: &str) -> PathBuf {
        // One folder per call: two tests uploading the same id must not share one.
        static N: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let path = crate::chat::tmux_io::tests::tempfile_path(&format!("hub-up-{id}-{n}")).join(name);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, b"12345678").unwrap();
        let mime = if kind == Kind::Image { "image/png" } else { "text/plain" };
        hub.add_upload(id.into(), Attachment { path: path.clone(), kind, mime: mime.into(), name: name.into(), size: 8 }).await;
        path
    }
    fn reply(reply: Option<Frame>) -> Value {
        serde_json::from_str(&reply.expect("a reply")).unwrap()
    }
    fn plan_ask(id: &str) -> Value {
        json!({"id":id,"kind":"plan","tool":"claude","plan":"# P"})
    }
    /// Waits until the actor has handled everything sent to it so far. The
    /// 20 ms sleep is kept so paused-clock tests advance as they always did;
    /// alone it was only a guess under load. The engine channel drained means
    /// the actor took the last message, and since it handles one message at a
    /// time, a command sent after that is answered only once that one is done.
    async fn settle(hub: &Hub, tx: &mpsc::Sender<EngineMsg>) {
        tokio::time::sleep(Duration::from_millis(20)).await;
        let end = std::time::Instant::now() + Duration::from_secs(30);
        while tx.capacity() < tx.max_capacity() {
            assert!(std::time::Instant::now() < end, "the hub took no engine message for 30 s");
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        hub.folder().await;
    }

    #[tokio::test]
    async fn a_reset_reports_what_the_old_conversation_never_showed_as_unconfirmed() {
        let (hub, tx, _) = rig();
        let mut sub = hub.subscribe(None, None).await.unwrap();
        hub.command(send("c1", "never shown")).await;
        hub.command(send("c2", "shown")).await;
        tx.send(EngineMsg::Ev(prompt("u2", "shown"))).await.unwrap();
        tx.send(EngineMsg::Reset).await.unwrap();
        settle(&hub, &tx).await;
        let acks: Vec<(String, String)> = frames(&mut sub).iter().filter(|f| f["t"] == "ack")
            .map(|f| (f["cid"].as_str().unwrap().into(), f["status"].as_str().unwrap().into())).collect();
        assert_eq!(acks, [("c1".to_string(), "unconfirmed".to_string())], "c2 was shown; it leaves with the old conversation");
    }
    #[tokio::test]
    async fn sends_carry_their_uploads_and_unknown_or_vanished_ones_are_refused() {
        let (hub, _tx, calls) = rig();
        upload(&hub, "aaaa", Kind::Image, "shot.png").await;
        std::fs::remove_file(upload(&hub, "bbbb", Kind::File, "gone.txt").await).unwrap();
        assert_eq!(reply(hub.command(send_with("c1", "look", &["aaaa"])).await)["status"], "sent");
        assert_eq!(reply(hub.command(send_with("c2", " ", &["aaaa"])).await)["status"], "sent", "attachments alone are a message");
        for (cid, id) in [("c3", "nope"), ("c4", "bbbb")] {
            let a = reply(hub.command(send_with(cid, "x", &[id])).await);
            assert_eq!((a["status"].as_str(), a["error"].as_str()), (Some("refused"), Some("attachment expired")), "{cid}");
        }
        assert_eq!(reply(hub.command(send_with("c5", " ", &[])).await)["error"], "empty message");
        assert_eq!(reply(hub.command(send_with("c6", "x", &["nope"; 11])).await)["error"], "too many attachments");
        assert_eq!(*calls.lock().unwrap(), ["send:look:false:shot.png", "send::false:shot.png"]);
    }
    #[tokio::test]
    async fn a_held_message_keeps_its_attachments_lists_them_and_sends_them() {
        let (hub, tx, calls) = rig();
        upload(&hub, "aaaa", Kind::Image, "shot.png").await;
        let mut sub = hub.subscribe(None, None).await.unwrap();
        tx.send(engine_status(true, false)).await.unwrap();
        settle(&hub, &tx).await;
        assert_eq!(reply(hub.command(send_with("c1", "later", &["aaaa"])).await)["status"], "held");
        settle(&hub, &tx).await;
        let state = frames(&mut sub).into_iter().rev().find(|f| f["t"] == "state").unwrap();
        assert_eq!(state["held"][0]["files"], json!([{"id":"aaaa","name":"shot.png","kind":"image","size":8,"mime":"image/png"}]));
        hub.command(ClientFrame::Held { op: HeldOp::Now, id: "h1".into() }).await;
        assert_eq!(*calls.lock().unwrap(), ["send:later:true:shot.png"]);
    }
    #[tokio::test]
    async fn an_ended_session_deletes_its_upload_folder() {
        let (_hub, tx, _) = rig_named("cdash-hub-end");
        let dir = media::uploads_dir("cdash-hub-end").unwrap();
        let f = dir.join("0123456789abcdef/a.txt");
        std::fs::create_dir_all(f.parent().unwrap()).unwrap();
        std::fs::write(&f, "x").unwrap();
        tx.send(EngineMsg::Ended).await.unwrap();
        crate::chat::tmux_io::tests::eventually("the session's uploads are gone", || !dir.exists()).await;
    }
    #[tokio::test(start_paused = true)]
    async fn a_message_with_a_pdf_is_confirmed_by_the_prompt_the_tap_reads_back() {
        let (hub, tx, _) = rig_named("cdash-hub-pdf");
        let pdf = media::uploads_dir("cdash-hub-pdf").unwrap().join("0123456789abcdef/r.pdf"); // contains /cdash/uploads/
        std::fs::create_dir_all(pdf.parent().unwrap()).unwrap();
        std::fs::write(&pdf, b"%PDF-1.7").unwrap();
        hub.add_upload("pppp".into(), Attachment { path: pdf.clone(), kind: Kind::File, mime: "application/pdf".into(), name: "r.pdf".into(), size: 8 }).await;
        let mut sub = hub.subscribe(None, None).await.unwrap();
        assert_eq!(reply(hub.command(send_with("c1", "read this", &["pppp"])).await)["status"], "sent");
        let typed = json!({"type":"user","uuid":"u1","parentUuid":"p","promptSource":"typed","message":{"role":"user","content":format!("read this\nAttached: {}", pdf.display())}});
        for ev in crate::chat::tap::Tap::default().feed(&typed.to_string()) { tx.send(EngineMsg::Ev(ev)).await.unwrap(); }
        tokio::time::sleep(CONFIRM_AFTER + Duration::from_secs(1)).await;
        let f = frames(&mut sub);
        let _ = std::fs::remove_dir_all(pdf.parent().unwrap());
        assert!(f.iter().any(|x| x["ev"]["cid"] == "c1" && x["ev"]["files"] == json!([{"kind":"file","name":"r.pdf"}])));
        assert!(!f.iter().any(|x| x["t"] == "ack" && x["status"] == "unconfirmed"), "confirmed, not unconfirmed");
    }
    #[tokio::test]
    async fn a_message_the_tool_confirmed_is_forgotten_at_the_next_turn_end() {
        let (hub, tx, calls) = rig_accepting();
        hub.command(send("c1", "the tool reworded this")).await;
        tx.send(EngineMsg::Ev(json!({"type":"result","subtype":"success"}))).await.unwrap();
        settle(&hub, &tx).await;
        hub.command(ClientFrame::Stop).await;
        assert_eq!(*calls.lock().unwrap(), ["send:the tool reworded this:false"], "idle with nothing pending: Stop interrupts nothing");
    }
    #[tokio::test]
    async fn a_fresh_subscriber_gets_hello_backlog_and_state_then_live_events() {
        let (hub, tx, _) = rig();
        for i in 0..3 { tx.send(EngineMsg::Ev(prompt(&format!("u{i}"), "x"))).await.unwrap(); }
        settle(&hub, &tx).await;
        let mut sub = hub.subscribe(None, None).await.unwrap();
        let b: Vec<Value> = sub.backlog.iter().map(|f| serde_json::from_str(f).unwrap()).collect();
        assert_eq!((b[0]["t"].as_str(), b[0]["reset"].as_bool(), b[0]["first"].as_u64(), b[0]["head"].as_u64()), (Some("hello"), Some(true), Some(1), Some(3)));
        assert_eq!(b[1..4].iter().map(|f| f["seq"].as_u64().unwrap()).collect::<Vec<_>>(), [1, 2, 3]);
        assert_eq!(b[4]["t"], "state");
        tx.send(EngineMsg::Ev(answer("k"))).await.unwrap();
        settle(&hub, &tx).await;
        assert_eq!(frames(&mut sub)[0]["seq"], 4);
    }
    #[tokio::test]
    async fn a_known_position_resumes_and_a_stale_epoch_gets_a_reset_and_a_fresh_backlog() {
        let (hub, tx, _) = rig();
        for i in 0..5 { tx.send(EngineMsg::Ev(prompt(&format!("u{i}"), "x"))).await.unwrap(); }
        settle(&hub, &tx).await;
        let first = hub.subscribe(None, None).await.unwrap();
        let epoch = serde_json::from_str::<Value>(&first.backlog[0]).unwrap()["epoch"].as_str().unwrap().to_string();
        let resumed = hub.subscribe(Some(3), Some(epoch)).await.unwrap();
        let r: Vec<Value> = resumed.backlog.iter().map(|f| serde_json::from_str(f).unwrap()).collect();
        assert_eq!(r[0]["reset"], false);
        assert_eq!(r.iter().filter_map(|f| f["seq"].as_u64()).collect::<Vec<_>>(), [4, 5]);
        let stale = hub.subscribe(Some(3), Some("an-older-agent".into())).await.unwrap();
        let s: Vec<Value> = stale.backlog.iter().map(|f| serde_json::from_str(f).unwrap()).collect();
        assert_eq!(s[0]["reset"], true);
        assert_eq!(s.iter().filter_map(|f| f["seq"].as_u64()).collect::<Vec<_>>(), [1, 2, 3, 4, 5]);
    }
    #[tokio::test]
    async fn idle_sends_go_out_busy_sends_are_held_and_a_turn_that_ends_on_its_own_releases_one() {
        let (hub, tx, calls) = rig();
        assert_eq!(reply(hub.command(send("c1", "first")).await)["status"], "sent");
        tx.send(engine_status(true, false)).await.unwrap();
        tx.send(EngineMsg::Ev(prompt("u1", "first"))).await.unwrap();
        settle(&hub, &tx).await;
        let held = reply(hub.command(send("c2", "second")).await);
        assert_eq!((held["status"].as_str(), held["id"].as_str()), (Some("held"), Some("h1")));
        hub.command(send("c3", "third")).await;
        tx.send(EngineMsg::Ev(answer("k1"))).await.unwrap();
        tx.send(EngineMsg::Ev(json!({"type":"result","subtype":"success"}))).await.unwrap();
        tx.send(EngineMsg::Status { working: false, waiting: false, waiting_for: None }).await.unwrap();
        settle(&hub, &tx).await;
        assert_eq!(*calls.lock().unwrap(), ["send:first:false", "send:second:false"], "one per turn");
    }
    #[tokio::test(start_paused = true)]
    async fn stop_keeps_held_messages_and_a_prompt_stopped_before_any_reply_is_handed_back() {
        let (hub, tx, calls) = rig();
        let mut sub = hub.subscribe(None, None).await.unwrap();
        hub.command(send("c1", "first")).await;
        tx.send(EngineMsg::Status { working: true, waiting: false, waiting_for: None }).await.unwrap();
        tx.send(EngineMsg::Ev(prompt("u1", "first"))).await.unwrap();
        settle(&hub, &tx).await;
        hub.command(send("c2", "second")).await;
        hub.command(ClientFrame::Stop).await;
        tx.send(EngineMsg::Status { working: false, waiting: false, waiting_for: None }).await.unwrap();
        tokio::time::sleep(STOP_SETTLE + Duration::from_millis(300)).await;
        assert_eq!(*calls.lock().unwrap(), ["send:first:false", "stop", "clear:first"]);
        let f = frames(&mut sub);
        assert!(f.iter().any(|x| x["ev"]["subtype"] == "retract" && x["ev"]["uuid"] == "u1"));
        assert!(f.iter().any(|x| x["t"] == "ack" && x["cid"] == "c1" && x["status"] == "returned"));
        let state = f.iter().rev().find(|x| x["t"] == "state").unwrap();
        assert_eq!(state["held"][0]["text"], "second", "still listed, not sent");
        assert_eq!(reply(hub.command(send("c4", "typed while idle")).await)["status"], "sent");
        assert!(!calls.lock().unwrap().iter().any(|c| c.starts_with("send:second")));
    }
    #[tokio::test]
    async fn an_interrupt_from_any_door_stops_the_queue_and_send_now_steers() {
        let (hub, tx, calls) = rig();
        tx.send(EngineMsg::Status { working: true, waiting: false, waiting_for: None }).await.unwrap();
        settle(&hub, &tx).await;
        hub.command(send("c1", "held one")).await;
        hub.command(send("c2", "held two")).await;
        hub.command(ClientFrame::Held { op: HeldOp::Now, id: "h2".into() }).await;
        tx.send(EngineMsg::Ev(json!({"type":"result","subtype":"interrupted"}))).await.unwrap();
        tx.send(EngineMsg::Status { working: false, waiting: false, waiting_for: None }).await.unwrap();
        settle(&hub, &tx).await;
        assert_eq!(*calls.lock().unwrap(), ["send:held two:true"], "steered at once; the other one stays held");
        hub.command(ClientFrame::Held { op: HeldOp::Remove, id: "h1".into() }).await;
        let sub = hub.subscribe(None, None).await.unwrap();
        assert_eq!(serde_json::from_str::<Value>(sub.backlog.last().unwrap()).unwrap()["held"], json!([]));
    }
    #[tokio::test(start_paused = true)]
    async fn two_identical_messages_are_confirmed_separately_in_order_and_silence_is_reported() {
        let (hub, tx, _) = rig();
        let mut sub = hub.subscribe(None, None).await.unwrap();
        hub.command(send("c1", "same")).await;
        hub.command(send("c2", "same")).await;
        tx.send(EngineMsg::Ev(prompt("u1", "same"))).await.unwrap();
        tx.send(EngineMsg::Ev(prompt("u2", "same"))).await.unwrap();
        settle(&hub, &tx).await;
        let cids: Vec<String> = frames(&mut sub).iter().filter(|f| f["ev"]["type"] == "user").map(|f| f["ev"]["cid"].as_str().unwrap().to_string()).collect();
        assert_eq!(cids, ["c1", "c2"]);
        hub.command(send("c3", "never shows up")).await;
        tokio::time::sleep(CONFIRM_AFTER + Duration::from_millis(500)).await;
        assert!(frames(&mut sub).iter().any(|f| f["t"] == "ack" && f["cid"] == "c3" && f["status"] == "unconfirmed"));
    }
    #[tokio::test]
    async fn the_state_frame_says_what_the_session_waits_for() {
        let (hub, tx, _) = rig();
        let mut sub = hub.subscribe(None, None).await.unwrap();
        assert_eq!(serde_json::from_str::<Value>(sub.backlog.last().unwrap()).unwrap()["waiting_for"], Value::Null);
        tx.send(EngineMsg::Status { working: false, waiting: true, waiting_for: Some("dialog open".into()) }).await.unwrap();
        settle(&hub, &tx).await;
        assert_eq!(frames(&mut sub).last().unwrap()["waiting_for"], "dialog open");
    }
    #[tokio::test]
    async fn waiting_and_ended_sessions_refuse_sends_and_ended_reports_held_messages_as_not_sent() {
        let (hub, tx, calls) = rig();
        let mut sub = hub.subscribe(None, None).await.unwrap();
        tx.send(EngineMsg::Status { working: true, waiting: false, waiting_for: None }).await.unwrap();
        settle(&hub, &tx).await;
        hub.command(send("c1", "held")).await;
        tx.send(EngineMsg::Status { working: false, waiting: true, waiting_for: None }).await.unwrap();
        settle(&hub, &tx).await;
        assert_eq!(reply(hub.command(send("c2", "x")).await)["status"], "refused");
        assert!(calls.lock().unwrap().is_empty(), "nothing is typed into a dialog");
        tx.send(EngineMsg::Ended).await.unwrap();
        settle(&hub, &tx).await;
        let f = frames(&mut sub);
        assert!(f.iter().any(|x| x["t"] == "ack" && x["cid"] == "c1" && x["status"] == "refused" && x["error"] == "session ended"));
        assert!(f.iter().any(|x| x["t"] == "state" && x["ended"] == true && x["held"] == json!([])));
        assert_eq!(reply(hub.command(send("c3", "x")).await)["status"], "refused");
    }
    #[tokio::test]
    async fn sends_from_two_clients_reach_the_engine_one_at_a_time() {
        let (hub, _tx, calls) = rig_slow(Duration::from_millis(50));   // FakeEngine::send sleeps, and records "begin:"/"end:"
        let (a, b) = (hub.clone(), hub.clone());
        tokio::join!(a.command(send("c1", "one")), b.command(send("c2", "two")));
        let c = calls.lock().unwrap().clone();
        assert!(c == ["begin:one", "end:one", "begin:two", "end:two"] || c == ["begin:two", "end:two", "begin:one", "end:one"]);
    }
    #[tokio::test]
    async fn paging_returns_older_events_and_reset_starts_a_new_conversation() {
        let (hub, tx, _) = rig();
        for i in 0..(BACKLOG + 50) { tx.send(EngineMsg::Ev(prompt(&format!("u{i}"), "x"))).await.unwrap(); }
        settle(&hub, &tx).await;
        let mut sub = hub.subscribe(None, None).await.unwrap();
        let hello: Value = serde_json::from_str(&sub.backlog[0]).unwrap();
        assert_eq!(hello["first"], 51);
        let page: Value = serde_json::from_str(&hub.command(ClientFrame::Page { before: 51, limit: Some(30) }).await.unwrap()).unwrap();
        assert_eq!((page["events"].as_array().unwrap().len(), page["events"][0]["seq"].as_u64(), page["more"].as_bool()), (30, Some(21), Some(true)));
        tx.send(EngineMsg::Reset).await.unwrap();
        tx.send(EngineMsg::Ev(prompt("n1", "new"))).await.unwrap();
        settle(&hub, &tx).await;
        let f = frames(&mut sub);
        assert_eq!(f[0]["t"], "reset");
        assert_eq!(f[1]["seq"], 1);
    }
    #[tokio::test]
    async fn an_unreadable_session_says_so_and_types_nothing() {
        let (hub, tx, calls) = rig();
        tx.send(EngineMsg::Unreadable).await.unwrap();
        settle(&hub, &tx).await;
        let sub = hub.subscribe(None, None).await.unwrap();
        assert_eq!(serde_json::from_str::<Value>(sub.backlog.last().unwrap()).unwrap()["unreadable"], true);
        let ack = reply(hub.command(send("c1", "x")).await);
        assert_eq!((ack["status"].as_str(), ack["error"].as_str()), (Some("refused"), Some("cannot read this session; use Terminal")));
        assert!(calls.lock().unwrap().is_empty());
    }
    #[tokio::test]
    async fn send_now_never_types_into_a_dialog_or_an_unreadable_session() {
        let (hub, tx, calls) = rig();
        let mut sub = hub.subscribe(None, None).await.unwrap();
        tx.send(EngineMsg::Status { working: true, waiting: false, waiting_for: None }).await.unwrap();
        settle(&hub, &tx).await;
        hub.command(send("c1", "held")).await;
        tx.send(EngineMsg::Status { working: false, waiting: true, waiting_for: None }).await.unwrap();
        settle(&hub, &tx).await;
        hub.command(ClientFrame::Held { op: HeldOp::Now, id: "h1".into() }).await;
        tx.send(EngineMsg::Unreadable).await.unwrap();
        settle(&hub, &tx).await;
        hub.command(ClientFrame::Held { op: HeldOp::Now, id: "h1".into() }).await;
        assert!(calls.lock().unwrap().is_empty(), "Enter in a permission dialog would approve it");
        let f = frames(&mut sub);
        assert!(f.iter().any(|x| x["t"] == "ack" && x["cid"] == "c1" && x["status"] == "refused" && x["error"] == "answer in Terminal first"));
        let state = f.iter().rev().find(|x| x["t"] == "state").unwrap();
        assert_eq!(state["held"][0]["id"], "h1", "still held, not lost");
    }
    #[tokio::test]
    async fn a_message_claude_queued_carries_its_client_id() {
        let (hub, tx, _) = rig();
        let mut sub = hub.subscribe(None, None).await.unwrap();
        tx.send(EngineMsg::Status { working: true, waiting: false, waiting_for: None }).await.unwrap();
        settle(&hub, &tx).await;
        hub.command(send("c1", "later")).await;
        hub.command(ClientFrame::Held { op: HeldOp::Now, id: "h1".into() }).await;
        tx.send(EngineMsg::Ev(json!({"type":"cdash","subtype":"queued","text":"later"}))).await.unwrap();
        settle(&hub, &tx).await;
        let f = frames(&mut sub);
        assert!(f.iter().any(|x| x["ev"]["subtype"] == "queued" && x["ev"]["cid"] == "c1"));
    }
    #[tokio::test(start_paused = true)]
    async fn nothing_is_pasted_while_a_stopped_prompt_may_still_be_handed_back() {
        let (hub, tx, calls) = rig();
        hub.command(send("c1", "first")).await;
        tx.send(EngineMsg::Status { working: true, waiting: false, waiting_for: None }).await.unwrap();
        tx.send(EngineMsg::Ev(prompt("u1", "first"))).await.unwrap();
        settle(&hub, &tx).await;
        hub.command(ClientFrame::Stop).await;
        tx.send(EngineMsg::Status { working: false, waiting: false, waiting_for: None }).await.unwrap();
        settle(&hub, &tx).await;
        let early = reply(hub.command(send("c2", "second")).await);
        assert_eq!((early["status"].as_str(), early["error"].as_str()), (Some("refused"), Some("stopping; send again in a moment")));
        assert!(STOP_SETTLE >= Duration::from_millis(2500), "transcript writes were seen up to 2.2 s late");
        tokio::time::sleep(STOP_SETTLE + Duration::from_millis(300)).await;
        assert_eq!(reply(hub.command(send("c3", "third")).await)["status"], "sent");
        assert_eq!(*calls.lock().unwrap(), ["send:first:false", "stop", "clear:first", "send:third:false"]);
    }

    #[tokio::test(start_paused = true)]
    async fn an_interrupt_marker_inside_the_quiet_window_is_not_a_returned_prompt() {
        let (hub, tx, calls) = rig();
        let mut sub = hub.subscribe(None, None).await.unwrap();
        hub.command(send("c1", "first")).await;
        tx.send(engine_status(true, false)).await.unwrap();
        tx.send(EngineMsg::Ev(prompt("u1", "first"))).await.unwrap();
        settle(&hub, &tx).await;
        hub.command(ClientFrame::Stop).await;
        tx.send(engine_status(false, false)).await.unwrap();
        tokio::time::sleep(Duration::from_millis(200)).await;
        tx.send(EngineMsg::Ev(json!({"type":"result","subtype":"interrupted"}))).await.unwrap();
        tokio::time::sleep(Duration::from_secs(2)).await;
        assert_eq!(*calls.lock().unwrap(), ["send:first:false", "stop"], "the prompt was taken, nothing is cleared");
        assert!(!frames(&mut sub).iter().any(|f| f["status"] == "returned"), "no returned ack");
    }

    #[tokio::test(start_paused = true)]
    async fn a_stop_settles_half_a_second_after_the_record_goes_quiet() {
        let (hub, tx, calls) = rig();
        hub.command(send("c1", "first")).await;
        tx.send(engine_status(true, false)).await.unwrap();
        tx.send(EngineMsg::Ev(prompt("u1", "first"))).await.unwrap();
        settle(&hub, &tx).await;
        hub.command(ClientFrame::Stop).await;
        tx.send(engine_status(false, false)).await.unwrap();
        tokio::time::sleep(Duration::from_millis(400)).await;
        assert_eq!(calls.lock().unwrap().len(), 2, "not before 500 ms of quiet");
        tokio::time::sleep(Duration::from_millis(400)).await;
        assert_eq!(*calls.lock().unwrap(), ["send:first:false", "stop", "clear:first"]);
    }

    #[tokio::test(start_paused = true)]
    async fn a_record_that_keeps_growing_settles_at_the_ceiling() {
        let (hub, tx, calls) = rig();
        hub.command(send("c1", "first")).await;
        tx.send(engine_status(true, false)).await.unwrap();
        settle(&hub, &tx).await;
        hub.command(ClientFrame::Stop).await;
        tx.send(engine_status(false, false)).await.unwrap();
        for i in 1..=14 {
            tokio::time::sleep(Duration::from_millis(200)).await;
            tx.send(EngineMsg::Ev(json!({"type":"cdash","subtype":"queued","text":"other"}))).await.unwrap();
            if i == 12 { assert_eq!(calls.lock().unwrap().len(), 2, "2.4 s and still growing"); }
        }
        assert_eq!(calls.lock().unwrap().last().map(String::as_str), Some("clear:first"), "settled at the 2.5 s ceiling");
    }

    #[tokio::test]
    async fn history_never_notifies_only_live_turn_ends_do() {
        let (hub, tx, _ctx, mut notes) = rig_noting(false);
        tx.send(turn_end("success")).await.unwrap(); // replayed history
        tx.send(EngineMsg::Ready).await.unwrap();
        for s in ["success", "error_during_execution", "interrupted"] {
            tx.send(turn_end(s)).await.unwrap();
        }
        tx.send(EngineMsg::Reset).await.unwrap();
        tx.send(turn_end("success")).await.unwrap(); // the new conversation's history
        tx.send(EngineMsg::Ready).await.unwrap();
        tx.send(turn_end("success")).await.unwrap();
        settle(&hub, &tx).await;
        assert_eq!(note_kinds(&mut notes), ["done", "error", "done"], "a Stop or Esc (interrupted) never notifies");
    }

    #[tokio::test]
    async fn finished_waits_for_the_queue_but_an_error_does_not() {
        let (hub, tx, _ctx, mut notes) = rig_noting(true);
        tx.send(engine_status(true, false)).await.unwrap();
        settle(&hub, &tx).await;
        hub.command(send("c1", "next")).await; // held; leaves on its own
        tx.send(turn_end("success")).await.unwrap();
        settle(&hub, &tx).await;
        assert!(note_kinds(&mut notes).is_empty(), "the held message is about to go");
        tx.send(turn_end("error_during_execution")).await.unwrap();
        settle(&hub, &tx).await;
        assert_eq!(note_kinds(&mut notes), ["error"]);
        hub.command(ClientFrame::Stop).await; // after a Stop nothing leaves on its own
        tx.send(turn_end("success")).await.unwrap();
        settle(&hub, &tx).await;
        assert_eq!(note_kinds(&mut notes), ["done"]);
    }

    #[tokio::test(start_paused = true)]
    async fn needs_an_answer_after_five_seconds_once_per_spell_and_never_for_a_dialog() {
        let (_hub, tx, _ctx, mut notes) = rig_noting(true);
        let waiting = |w: &str| EngineMsg::Status { working: false, waiting: true, waiting_for: Some(w.into()) };
        tx.send(waiting("permission prompt")).await.unwrap();
        tokio::time::sleep(ASK_AFTER - Duration::from_millis(500)).await;
        assert!(note_kinds(&mut notes).is_empty(), "answered quickly in Terminal: no buzz");
        tokio::time::sleep(Duration::from_secs(6)).await;
        assert_eq!(note_kinds(&mut notes), ["ask"], "once per waiting spell");
        tx.send(engine_status(true, false)).await.unwrap();
        tx.send(waiting("dialog open")).await.unwrap();
        tokio::time::sleep(Duration::from_secs(10)).await;
        assert!(note_kinds(&mut notes).is_empty(), "/exit, rewind and model-switch dialogs never notify");
    }

    #[tokio::test]
    async fn a_session_someone_is_looking_at_does_not_notify() {
        let (hub, tx, ctx, mut notes) = rig_noting(true);
        let mut looking = FocusGuard::new(&ctx, "cdash-a");
        looking.set(true);
        tx.send(turn_end("success")).await.unwrap();
        settle(&hub, &tx).await;
        assert!(note_kinds(&mut notes).is_empty());
        drop(looking);
        tx.send(turn_end("success")).await.unwrap();
        settle(&hub, &tx).await;
        assert_eq!(note_kinds(&mut notes), ["done"]);
    }

    #[tokio::test]
    async fn older_pages_come_from_the_source_and_a_position_older_than_the_window_starts_over() {
        let (hub, tx, _) = rig_history();
        for i in 0..2100 { tx.send(EngineMsg::Ev(prompt(&format!("u{i}"), "x"))).await.unwrap(); }
        settle(&hub, &tx).await;
        let page: Value = serde_json::from_str(&hub.command(ClientFrame::Page { before: 101, limit: Some(50) }).await.unwrap()).unwrap();
        assert_eq!((page["events"][0]["seq"].as_u64(), page["events"][0]["ev"]["n"].as_u64()), (Some(51), Some(51)), "re-read from the source");
        let epoch = serde_json::from_str::<Value>(&hub.subscribe(None, None).await.unwrap().backlog[0]).unwrap()["epoch"].as_str().unwrap().to_string();
        let old = hub.subscribe(Some(10), Some(epoch)).await.unwrap();
        assert_eq!(serde_json::from_str::<Value>(&old.backlog[0]).unwrap()["reset"], true);
    }

    #[tokio::test(start_paused = true)]
    async fn a_retract_the_hub_appended_keeps_its_position_in_an_older_page() {
        let (hub, tx, _) = rig_history();
        hub.command(send("c1", "first")).await;
        tx.send(engine_status(true, false)).await.unwrap();
        tx.send(EngineMsg::Ev(prompt("u1", "first"))).await.unwrap();
        settle(&hub, &tx).await;
        hub.command(ClientFrame::Stop).await;
        tx.send(engine_status(false, false)).await.unwrap();
        tokio::time::sleep(STOP_SETTLE + Duration::from_millis(300)).await; // the retract lands at position 2
        for i in 0..=WINDOW { tx.send(EngineMsg::Ev(answer(&format!("k{i}")))).await.unwrap(); } // positions 1 to 3 leave memory
        settle(&hub, &tx).await;
        let page: Value = serde_json::from_str(&hub.command(ClientFrame::Page { before: 4, limit: Some(3) }).await.unwrap()).unwrap();
        let got: Vec<(u64, Value)> = page["events"].as_array().unwrap().iter()
            .map(|e| (e["seq"].as_u64().unwrap(), if e["ev"]["subtype"] == "retract" { json!("retract") } else { e["ev"]["n"].clone() })).collect();
        assert_eq!(got, [(1, json!(1)), (2, json!("retract")), (3, json!(2))], "the engine's own numbers skip what the hub added");
    }

    #[tokio::test]
    async fn a_page_across_the_window_edge_joins_the_source_and_memory() {
        let (hub, tx, _) = rig_history();
        for i in 0..2100 { tx.send(EngineMsg::Ev(prompt(&format!("u{i}"), "x"))).await.unwrap(); }
        settle(&hub, &tx).await;
        let page: Value = serde_json::from_str(&hub.command(ClientFrame::Page { before: 120, limit: Some(50) }).await.unwrap()).unwrap();
        let ev = page["events"].as_array().unwrap();
        assert_eq!(ev.iter().map(|e| e["seq"].as_u64().unwrap()).collect::<Vec<_>>(), (70..=119).collect::<Vec<_>>());
        assert_eq!((ev[0]["ev"]["n"].as_u64(), ev[30]["ev"]["n"].as_u64(), ev[31]["ev"]["uuid"].as_str()), (Some(70), Some(100), Some("u100")));
        assert_eq!(page["more"], true);
    }

    #[tokio::test]
    async fn a_source_that_comes_up_short_ends_paging() {
        let (hub, tx, _) = rig_history_to(60); // a transcript cut short since it was read
        for i in 0..2100 { tx.send(EngineMsg::Ev(prompt(&format!("u{i}"), "x"))).await.unwrap(); }
        settle(&hub, &tx).await;
        let page: Value = serde_json::from_str(&hub.command(ClientFrame::Page { before: 101, limit: Some(50) }).await.unwrap()).unwrap();
        assert_eq!(page["more"], false, "asking again would loop on the same short page");
    }

    #[tokio::test]
    async fn a_cut_truncates_after_its_anchor_and_reloads_every_client() {
        let (hub, tx, _) = rig();
        log6(&hub, &tx).await;
        let mut sub = hub.subscribe(None, None).await.unwrap();
        let old: Value = serde_json::from_str(&sub.backlog[0]).unwrap();
        tx.send(EngineMsg::Ev(cut(Some("s1")))).await.unwrap();
        tx.send(EngineMsg::Ev(prompt("u3", "c"))).await.unwrap();
        settle(&hub, &tx).await;
        let f = frames(&mut sub);
        assert_eq!((f[0]["t"].as_str(), f[0]["reset"].as_bool(), f[0]["head"].as_u64()), (Some("hello"), Some(true), Some(3)));
        assert_ne!(f[0]["epoch"], old["epoch"]);
        assert_eq!(f[1..4].iter().map(|x| x["ev"]["uuid"].as_str().unwrap()).collect::<Vec<_>>(), ["u1", "k1", "s1"]);
        assert_eq!(f[4]["t"], "state");
        assert_eq!((f[5]["seq"].as_u64(), f[5]["ev"]["uuid"].as_str()), (Some(4), Some("u3")), "the cut itself is not stored");
    }
    #[tokio::test]
    async fn a_cut_that_removes_nothing_or_cannot_be_placed_changes_nothing() {
        let (hub, tx, _) = rig();
        log6(&hub, &tx).await;
        let mut sub = hub.subscribe(None, None).await.unwrap();
        for after in [Some("s2"), Some("not-in-this-log")] { tx.send(EngineMsg::Ev(cut(after))).await.unwrap(); }
        settle(&hub, &tx).await;
        assert!(frames(&mut sub).is_empty(), "no new epoch, no reload, nothing hidden");
    }
    #[tokio::test]
    async fn a_cut_keeps_pending_and_held_messages() {
        let (hub, tx, _) = rig();
        log6(&hub, &tx).await;
        hub.command(send("c1", "next")).await;
        tx.send(EngineMsg::Status { working: true, waiting: false, waiting_for: None }).await.unwrap();
        settle(&hub, &tx).await;
        hub.command(send("c2", "later")).await;
        let mut sub = hub.subscribe(None, None).await.unwrap();
        tx.send(EngineMsg::Ev(cut(Some("s1")))).await.unwrap();
        tx.send(EngineMsg::Ev(prompt("u3", "next"))).await.unwrap();
        settle(&hub, &tx).await;
        let f = frames(&mut sub);
        assert_eq!(f.iter().find(|x| x["ev"]["uuid"] == "u3").unwrap()["ev"]["cid"], "c1", "the prompt after a cut still confirms it");
        assert_eq!(f.iter().rev().find(|x| x["t"] == "state").unwrap()["held"][0]["text"], "later");
    }
    #[tokio::test]
    async fn usage_and_tasks_are_state_not_history_and_a_reset_clears_them() {
        let (hub, tx, _) = rig();
        let st = last_state(&hub.subscribe(None, None).await.unwrap());
        assert_eq!((st["usage"].clone(), st["tasks"].clone()), (Value::Null, json!([])), "keys always present");
        tx.send(EngineMsg::Usage(json!({"cwd":"/w","context":{"used":5,"window":null}}))).await.unwrap();
        tx.send(EngineMsg::Tasks(vec![json!({"id":"b1","kind":"shell","title":"sleep 60","started":"2026-11-13T10:00:00Z"})])).await.unwrap();
        settle(&hub, &tx).await;
        let s = hub.subscribe(None, None).await.unwrap();
        assert_eq!((last_state(&s)["usage"]["cwd"].as_str(), last_state(&s)["tasks"][0]["id"].as_str()), (Some("/w"), Some("b1")));
        assert_eq!(serde_json::from_str::<Value>(&s.backlog[0]).unwrap()["head"], 0, "snapshots are not events");
        assert_eq!(hub.folder().await.as_deref(), Some("/w"));
        tx.send(EngineMsg::Reset).await.unwrap();
        settle(&hub, &tx).await;
        let st = last_state(&hub.subscribe(None, None).await.unwrap());
        assert_eq!((st["usage"].clone(), st["tasks"].clone(), hub.folder().await), (Value::Null, json!([]), None));
        tx.send(EngineMsg::Tasks(vec![json!({"id":"b2","kind":"shell","title":"sleep 60","started":"2026-11-13T10:00:00Z"})])).await.unwrap();
        tx.send(EngineMsg::Ended).await.unwrap();
        settle(&hub, &tx).await;
        assert_eq!(last_state(&hub.subscribe(None, None).await.unwrap())["tasks"], json!([]), "an ended session runs nothing");
    }
    #[tokio::test]
    async fn a_root_cut_cuts_everything_in_memory_and_nothing_once_events_left_it() {
        let head = |s: Subscription| serde_json::from_str::<Value>(&s.backlog[0]).unwrap()["head"].clone();
        let (hub, tx, _) = rig();
        log6(&hub, &tx).await;
        tx.send(EngineMsg::Ev(cut(None))).await.unwrap();
        settle(&hub, &tx).await;
        assert_eq!(head(hub.subscribe(None, None).await.unwrap()), 0, "the whole log goes");
        let (hub, tx, _) = rig();
        for i in 0..=WINDOW { tx.send(EngineMsg::Ev(answer(&format!("k{i}")))).await.unwrap(); } // position 1 leaves memory
        tx.send(EngineMsg::Ev(cut(None))).await.unwrap();
        settle(&hub, &tx).await;
        assert_eq!(head(hub.subscribe(None, None).await.unwrap()), WINDOW + 1, "the start is not in memory: nothing goes");
    }
    #[tokio::test(start_paused = true)]
    async fn a_cut_at_a_retracted_prompt_keeps_the_prompt_not_the_retract() {
        let (hub, tx, _) = rig();
        hub.command(send("c1", "first")).await;
        tx.send(engine_status(true, false)).await.unwrap();
        tx.send(EngineMsg::Ev(prompt("u1", "first"))).await.unwrap();
        settle(&hub, &tx).await;
        hub.command(ClientFrame::Stop).await;
        tx.send(engine_status(false, false)).await.unwrap();
        tokio::time::sleep(STOP_SETTLE + Duration::from_millis(300)).await; // the retract, also uuid u1, lands at position 2
        tx.send(EngineMsg::Ev(cut(Some("u1")))).await.unwrap();
        settle(&hub, &tx).await;
        let b: Vec<Value> = hub.subscribe(None, None).await.unwrap().backlog.iter().map(|f| serde_json::from_str(f).unwrap()).collect();
        assert_eq!(b[0]["head"], 1, "a retract names the prompt it withdraws; it is not the anchor");
        assert_eq!(b[1]["ev"]["uuid"], "u1");
    }
    #[tokio::test]
    async fn the_checklist_is_the_latest_kept_todo_and_a_reset_clears_it() {
        let (hub, tx, _) = rig();
        let todo = |uuid: &str, text: &str| json!({"type":"cdash","subtype":"todo","uuid":uuid,"items":[{"text":text,"status":"pending"}]});
        let todo_now = || async { last_state(&hub.subscribe(None, None).await.unwrap())["todo"].clone() };
        assert_eq!(todo_now().await, Value::Null);
        log6(&hub, &tx).await;
        for e in [todo("t1", "first"), result("s3"), todo("t2", "second")] { tx.send(EngineMsg::Ev(e)).await.unwrap(); }
        settle(&hub, &tx).await;
        assert_eq!(todo_now().await[0]["text"], "second");
        tx.send(EngineMsg::Ev(cut(Some("s3")))).await.unwrap();
        settle(&hub, &tx).await;
        assert_eq!(todo_now().await[0]["text"], "first", "the cut list goes, the one before it is back");
        tx.send(EngineMsg::Ev(cut(Some("s1")))).await.unwrap();
        settle(&hub, &tx).await;
        assert_eq!(todo_now().await, Value::Null);
        tx.send(EngineMsg::Ev(todo("t3", "third"))).await.unwrap();
        tx.send(EngineMsg::Reset).await.unwrap();
        settle(&hub, &tx).await;
        assert_eq!(todo_now().await, Value::Null);
    }

    fn rewind(uuid: &str) -> ClientFrame { ClientFrame::Rewind { uuid: uuid.into(), files: true } }
    #[tokio::test]
    async fn rewind_runs_only_while_idle_cuts_at_the_prompt_and_reloads_clients_once() {
        let (hub, tx, calls) = rig();
        log6(&hub, &tx).await;
        tx.send(EngineMsg::Status { working: true, waiting: false, waiting_for: None }).await.unwrap();
        settle(&hub, &tx).await;
        let busy = reply(hub.command(rewind("u2")).await);
        assert_eq!((busy["ok"].as_bool(), busy["error"].as_str()), (Some(false), Some("Stop the turn or wait for it to finish")));
        tx.send(EngineMsg::Status { working: false, waiting: false, waiting_for: None }).await.unwrap();
        settle(&hub, &tx).await;
        let mut sub = hub.subscribe(None, None).await.unwrap();
        assert_eq!(reply(hub.command(rewind("u2")).await), json!({"t":"rewound","uuid":"u2","ok":true,"text":"text of u2","attachments":0}));
        assert_eq!(*calls.lock().unwrap(), ["rewind:u2:true"], "nothing reached the engine while busy");
        let f = frames(&mut sub);
        assert_eq!((f[0]["t"].as_str(), f[0]["head"].as_u64()), (Some("hello"), Some(3)), "cut before the prompt");
        tx.send(EngineMsg::Ev(cut(Some("s1")))).await.unwrap();             // the next prompt makes the tap cut at the same place
        tx.send(EngineMsg::Ev(prompt("u3", "two, edited"))).await.unwrap();
        settle(&hub, &tx).await;
        let g = frames(&mut sub);
        assert!(!g.iter().any(|x| x["t"] == "hello"), "a chat rewind followed by its prompt reloads clients once");
        assert_eq!(g.iter().find(|x| x["t"] == "ev").unwrap()["seq"], 4);
    }
    #[tokio::test]
    async fn a_failed_unknown_or_steered_rewind_leaves_the_conversation_alone() {
        let (hub, tx, calls) = rig_failing_rewind();
        log6(&hub, &tx).await;
        let failed = reply(hub.command(rewind("u2")).await);
        assert_eq!((failed["ok"].as_bool(), failed["error"].as_str()), (Some(false), Some("rewind failed; use Terminal")));
        assert_eq!(reply(hub.command(rewind("zz")).await)["error"], "not in this conversation");
        let mut steered = prompt("s9", "steered");
        steered["src"] = json!("steer");
        tx.send(EngineMsg::Ev(steered)).await.unwrap();
        settle(&hub, &tx).await;
        assert_eq!(reply(hub.command(rewind("s9")).await)["error"], "this prompt cannot be rewound");
        assert_eq!(*calls.lock().unwrap(), ["rewind:u2:true"]);
        assert_eq!(serde_json::from_str::<Value>(&hub.subscribe(None, None).await.unwrap().backlog[0]).unwrap()["head"], 7);
        let (hub, tx, _) = rig();
        let mut p = prompt("u1", "a");
        p["files"] = json!([{"kind":"file","name":"a.pdf"},{"kind":"image","ref":"ab","mime":"image/png"}]);
        tx.send(EngineMsg::Ev(p)).await.unwrap();
        settle(&hub, &tx).await;
        assert_eq!(reply(hub.command(rewind("u1")).await)["attachments"], 2);
    }
    #[tokio::test]
    async fn a_rewind_also_cuts_the_queued_row_that_announced_its_prompt() {
        let (hub, tx, _) = rig();
        for e in [prompt("u1", "a"), answer("k1"), json!({"type":"cdash","subtype":"queued","text":"b"}), prompt("u2", "b")] { tx.send(EngineMsg::Ev(e)).await.unwrap(); }
        settle(&hub, &tx).await;
        assert_eq!(reply(hub.command(rewind("u2")).await)["ok"], true);
        let mut sub = hub.subscribe(None, None).await.unwrap();
        assert_eq!(serde_json::from_str::<Value>(&sub.backlog[0]).unwrap()["head"], 2);
        tx.send(EngineMsg::Ev(cut(Some("k1")))).await.unwrap(); // the tap's cut when the edited prompt lands
        settle(&hub, &tx).await;
        assert!(!frames(&mut sub).iter().any(|x| x["t"] == "hello"), "the tap's cut removes nothing more");
        assert_eq!(serde_json::from_str::<Value>(&hub.subscribe(None, None).await.unwrap().backlog[0]).unwrap()["head"], 2);
    }
    #[tokio::test]
    async fn a_rewind_right_after_a_send_waits_for_that_prompt() {
        let (hub, tx, calls) = rig();
        log6(&hub, &tx).await;
        assert_eq!(reply(hub.command(send("c1", "next")).await)["status"], "sent");
        let r = reply(hub.command(rewind("u2")).await);
        assert_eq!((r["ok"].as_bool(), r["error"].as_str()), (Some(false), Some("Stop the turn or wait for it to finish")));
        assert_eq!(reply(hub.command(ClientFrame::RewindUndo).await)["ok"], false);
        assert_eq!(*calls.lock().unwrap(), ["send:next:false"], "neither reached the engine");
    }
    #[tokio::test]
    async fn undo_reaches_the_engine_only_while_idle() {
        let (hub, tx, calls) = rig();
        tx.send(EngineMsg::Status { working: true, waiting: false, waiting_for: None }).await.unwrap();
        settle(&hub, &tx).await;
        assert_eq!(reply(hub.command(ClientFrame::RewindUndo).await)["ok"], false);
        tx.send(EngineMsg::Status { working: false, waiting: false, waiting_for: None }).await.unwrap();
        settle(&hub, &tx).await;
        assert_eq!(reply(hub.command(ClientFrame::RewindUndo).await), json!({"t":"rewound","uuid":null,"ok":true}));
        assert_eq!(*calls.lock().unwrap(), ["undo"]);
    }

    #[tokio::test]
    async fn a_side_question_never_touches_the_log_or_the_queue() {
        let (hub, tx, calls) = rig();
        log6(&hub, &tx).await;
        tx.send(engine_status(true, false)).await.unwrap();
        settle(&hub, &tx).await;
        assert_eq!(reply(hub.command(send("c1", "later")).await)["status"], "held");
        let before = last_state(&hub.subscribe(None, None).await.unwrap());
        let mut sub = hub.subscribe(None, None).await.unwrap();
        let mut rx = hub.aside("q1".into(), "  why?\u{1b}  ".into()).await.unwrap();
        let first = rx.recv().await.unwrap();
        assert_eq!((first.text.as_str(), first.done), ("re: why?", false), "sanitised, then passed through");
        assert!(rx.recv().await.unwrap().done);
        assert_eq!(hub.aside("q2".into(), " \n ".into()).await.err().as_deref(), Some("empty message"));
        hub.aside_cancel("q1".into()).await;
        settle(&hub, &tx).await;
        assert!(frames(&mut sub).is_empty(), "no frame reaches the hub's clients");
        let after = hub.subscribe(None, None).await.unwrap();
        assert_eq!(serde_json::from_str::<Value>(&after.backlog[0]).unwrap()["head"], 6);
        assert_eq!((last_state(&after)["held"].clone(), before["held"][0]["text"].as_str()), (before["held"].clone(), Some("later")));
        assert_eq!(*calls.lock().unwrap(), ["aside:q1:why?", "aside_cancel:q1"]);
        tx.send(EngineMsg::Ended).await.unwrap();
        settle(&hub, &tx).await;
        assert_eq!(hub.aside("q3".into(), "why?".into()).await.err().as_deref(), Some("session ended"));
        assert_eq!(calls.lock().unwrap().len(), 2, "an ended session never reaches the engine");
    }

    #[tokio::test]
    async fn the_engine_hears_how_many_clients_are_attached() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        struct Watched(Arc<AtomicUsize>);
        impl Engine for Watched {
            async fn send(&self, _: &str, _: bool, _: &str, _: &[Attachment]) -> Result<Sent, String> { Ok(Sent::Unconfirmed) }
            async fn stop(&self) -> Result<(), String> { Ok(()) }
            async fn watchers(&self, n: usize) { self.0.store(n, Ordering::Relaxed); }
        }
        let n = Arc::new(AtomicUsize::new(9));
        let (tx, rx) = tokio::sync::mpsc::channel(8);
        tx.send(EngineMsg::Ready).await.unwrap();
        let hub = Hub::spawn("claude", Watched(n.clone()), rx, Arc::new(crate::host::log::LogBuffer::new()), "cdash-a".into());
        let (a, b) = (hub.subscribe(None, None).await.unwrap(), hub.subscribe(None, None).await.unwrap());
        assert_eq!(n.load(Ordering::Relaxed), 2, "told before the subscriber hears back, not at the next tick");
        drop((a, b));
        crate::chat::tmux_io::tests::eventually("none attached", || n.load(Ordering::Relaxed) == 0).await;
    }
    #[tokio::test]
    async fn asks_settings_and_hooks_are_state_and_always_present() {
        let (hub, tx, _) = rig();
        let last = |s: Subscription| serde_json::from_str::<Value>(s.backlog.last().unwrap()).unwrap();
        let st = last(hub.subscribe(None, None).await.unwrap());
        assert_eq!((&st["asks"], &st["settings"], &st["hooks"]), (&json!([]), &json!({}), &json!(true)));
        assert!(st.get("waiting_for").is_some(), "Part 4's key stays");
        tx.send(EngineMsg::Asks(vec![plan_ask("a1")])).await.unwrap();
        tx.send(EngineMsg::Settings(json!({"model":"m","effort":"high","plan":true,"agent":null,"variant":null}))).await.unwrap();
        settle(&hub, &tx).await;
        let st = last(hub.subscribe(None, None).await.unwrap());
        assert_eq!((st["asks"][0]["id"].as_str(), st["settings"]["effort"].as_str()), (Some("a1"), Some("high")));
    }
    #[tokio::test]
    async fn only_a_pending_ask_reaches_the_engine_and_a_bad_answer_does_not() {
        let (hub, tx, calls) = rig();
        let ans = |id: &str, c: &str| ClientFrame::Answer { id: id.into(), choice: Some(c.into()), text: None, answers: None };
        assert_eq!(reply(hub.command(ans("gone", "yes")).await), json!({"t":"answered","id":"gone","ok":false,"error":"already answered"}));
        tx.send(EngineMsg::Asks(vec![plan_ask("a1")])).await.unwrap();
        settle(&hub, &tx).await;
        assert_eq!(reply(hub.command(ans("a1", "maybe")).await)["error"], "not a valid answer");
        assert_eq!(reply(hub.command(ans("a1", "yes")).await), json!({"t":"answered","id":"a1","ok":true}));
        assert_eq!(*calls.lock().unwrap(), ["answer:a1:yes"]);
    }
    #[tokio::test]
    async fn waiting_refuses_sends_with_the_card_or_the_terminal_message() {
        let (hub, tx, _) = rig();
        tx.send(EngineMsg::Status { working: false, waiting: true, waiting_for: Some("permission prompt".into()) }).await.unwrap();
        settle(&hub, &tx).await;
        assert_eq!(reply(hub.command(send("c1", "x")).await)["error"], "answer in Terminal first");
        tx.send(EngineMsg::Asks(vec![plan_ask("a1")])).await.unwrap();
        settle(&hub, &tx).await;
        assert_eq!(reply(hub.command(send("c2", "x")).await)["error"], "answer the card first");
    }
    #[tokio::test]
    async fn set_and_menu_reply_with_what_the_engine_said() {
        let (hub, _tx, calls) = rig();
        assert_eq!(reply(hub.command(ClientFrame::Set { what: "plan".into(), value: json!(true) }).await), json!({"t":"set","ok":true}));
        assert_eq!(reply(hub.command(ClientFrame::Set { what: "nope".into(), value: json!(1) }).await), json!({"t":"set","ok":false,"error":"not here"}));
        assert_eq!(reply(hub.command(ClientFrame::Menu).await), json!({"t":"menu","commands":[{"name":"compact"}],"models":[],"efforts":[],"agents":[]}));
        assert_eq!(*calls.lock().unwrap(), ["set:plan:true", "set:nope:1"]);
    }
    #[tokio::test]
    async fn a_setting_is_refused_while_a_send_could_be_refused() {
        let (hub, tx, calls) = rig();
        tx.send(EngineMsg::Status { working: false, waiting: true, waiting_for: Some("permission prompt".into()) }).await.unwrap();
        settle(&hub, &tx).await;
        assert_eq!(reply(hub.command(ClientFrame::Set { what: "model".into(), value: json!("opus") }).await), json!({"t":"set","ok":false,"error":"answer in Terminal first"}));
        tx.send(EngineMsg::Status { working: false, waiting: false, waiting_for: None }).await.unwrap();
        settle(&hub, &tx).await;
        hub.command(send("c1", "hello")).await; // pending: Claude may not show it as working yet
        assert_eq!(reply(hub.command(ClientFrame::Set { what: "model".into(), value: json!("opus") }).await)["ok"], false, "/model would join the prompt");
        assert!(!calls.lock().unwrap().iter().any(|c| c.starts_with("set:")), "{:?}", calls.lock().unwrap());
    }
    #[tokio::test]
    async fn codex_and_opencode_settings_change_mid_turn() {
        let calls = Calls::default();
        let (tx, rx) = tokio::sync::mpsc::channel(64);
        let engine = FakeEngine { calls: calls.clone(), sent: Sent::Confirmed, slow: None, has: u64::MAX, fail_rewind: false };
        let hub = Hub::spawn_with("codex", engine, rx, Arc::new(crate::host::log::LogBuffer::new()), "cdash-c".into(), None);
        tx.send(EngineMsg::Ready).await.unwrap();
        tx.send(EngineMsg::Status { working: true, waiting: false, waiting_for: None }).await.unwrap();
        settle(&hub, &tx).await;
        assert_eq!(reply(hub.command(ClientFrame::Set { what: "model".into(), value: json!("gpt-mini") }).await), json!({"t":"set","ok":true}), "an API call, no keys typed");
    }
    #[tokio::test]
    async fn a_command_the_engine_ran_itself_is_done_and_leaves_nothing_pending() {
        let (hub, _tx, calls) = rig_sending(Sent::Done);
        assert_eq!(reply(hub.command(send("c1", "/compact")).await)["status"], "done");
        hub.command(ClientFrame::Stop).await;
        assert_eq!(*calls.lock().unwrap(), ["send:/compact:false"], "nothing pending: Stop interrupts nothing");
    }
    #[tokio::test]
    async fn a_confirmed_path_that_starts_with_a_slash_is_sent_and_stays_pending() {
        let (hub, _tx, calls) = rig_sending(Sent::Confirmed);
        assert_eq!(reply(hub.command(send("c1", "/mnt/x")).await)["status"], "sent");
        hub.command(ClientFrame::Stop).await;
        assert_eq!(*calls.lock().unwrap(), ["send:/mnt/x:false", "stop"], "still pending: Stop reaches the engine");
    }
}
