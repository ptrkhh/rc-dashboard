//! The Codex and OpenCode engine: NDJSON over the supervisor's unix socket.
//! The supervisor (`chat-feed.mjs`) numbers events and replays from a
//! position, so this side only remembers the last position and epoch it saw.

use super::hub::{Answer, Attachment, Engine, EngineMsg, Sent};
use crate::host::log::LogBuffer;
use serde::Serialize;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::unix::OwnedWriteHalf;
use tokio::net::UnixStream;
use tokio::sync::{mpsc, oneshot};

/// How long a send waits for its ack, and a page for the source.
pub(crate) const ACK_WAIT: Duration = Duration::from_secs(10);
const RETRY: Duration = Duration::from_secs(1);
/// Consecutive failed connects before the session counts as gone.
const GIVE_UP: u32 = 5;

#[derive(Serialize)]
struct Hello<'a> {
    after: u64,
    epoch: Option<&'a str>,
}

#[derive(Serialize)]
struct SendFrame<'a> {
    t: &'static str,
    cid: &'a str,
    text: &'a str,
    steer: bool,
    #[serde(skip_serializing_if = "<[Attachment]>::is_empty")]
    files: &'a [Attachment],
}

type Acks = Mutex<HashMap<String, oneshot::Sender<Result<Sent, String>>>>;

type Reqs = Mutex<HashMap<u64, oneshot::Sender<Result<Value, String>>>>;

struct Shared {
    reqs: Reqs,
    next_rid: AtomicU64,
    writer: tokio::sync::Mutex<Option<OwnedWriteHalf>>,
    acks: Acks,
    /// The supervisor's epoch the reader last saw.
    epoch: Mutex<Option<String>>,
}

pub struct NativeEngine {
    shared: Arc<Shared>,
    sock: PathBuf,
}

impl NativeEngine {
    async fn write(&self, line: String) -> Result<(), String> {
        let mut w = self.shared.writer.lock().await;
        let Some(w) = w.as_mut() else { return Err("not connected".into()) };
        w.write_all(format!("{line}\n").as_bytes()).await.map_err(|_| "not connected".to_string())
    }

    /// One request/response round trip; concurrent requests are matched by `rid`.
    pub async fn request(&self, op: &str, fields: Value) -> Result<Value, String> {
        let rid = self.shared.next_rid.fetch_add(1, Ordering::Relaxed);
        let mut frame = if fields.is_object() { fields } else { json!({}) };
        frame["t"] = json!("req");
        frame["rid"] = json!(rid);
        frame["op"] = json!(op);
        let (tx, rx) = oneshot::channel();
        self.shared.reqs.lock().unwrap_or_else(|e| e.into_inner()).insert(rid, tx);
        if let Err(e) = self.write(frame.to_string()).await {
            self.shared.reqs.lock().unwrap_or_else(|e| e.into_inner()).remove(&rid);
            return Err(e);
        }
        match tokio::time::timeout(ACK_WAIT, rx).await {
            Ok(Ok(r)) => r,
            Ok(Err(_)) => Err("not connected".into()),
            Err(_) => {
                self.shared.reqs.lock().unwrap_or_else(|e| e.into_inner()).remove(&rid);
                Err("no answer from the session".into())
            }
        }
    }
}

impl Engine for NativeEngine {
    async fn send(&self, text: &str, steer: bool, cid: &str, files: &[Attachment]) -> Result<Sent, String> {
        let (tx, rx) = oneshot::channel();
        self.shared.acks.lock().unwrap_or_else(|e| e.into_inner()).insert(cid.to_string(), tx);
        let line = serde_json::to_string(&SendFrame { t: "send", cid, text, steer, files }).map_err(|e| e.to_string())?;
        if let Err(e) = self.write(line).await {
            self.shared.acks.lock().unwrap_or_else(|e| e.into_inner()).remove(cid);
            return Err(e);
        }
        match tokio::time::timeout(ACK_WAIT, rx).await {
            // The supervisor's own API confirmed it: the hub need not wait for the record.
            Ok(Ok(r)) => r,
            Ok(Err(_)) => Err("not connected".into()),
            Err(_) => {
                self.shared.acks.lock().unwrap_or_else(|e| e.into_inner()).remove(cid);
                Err("no answer from the session".into())
            }
        }
    }

    async fn stop(&self) -> Result<(), String> {
        self.write(r#"{"t":"stop"}"#.into()).await
    }

    async fn answer(&self, id: &str, a: &Answer) -> Result<(), String> {
        self.request("answer", json!({"id": id, "answer": a})).await.map(|_| ())
    }

    async fn set(&self, what: &str, value: &Value) -> Result<(), String> {
        self.request("set", json!({"what": what, "value": value})).await.map(|_| ())
    }

    async fn menu(&self) -> Value {
        self.request("menu", json!({})).await.unwrap_or_else(|_| json!({}))
    }

    /// A second connection that resumes after `from - 1`; the supervisor's
    /// replay ends with its `state` line. The hub bounds how long it waits.
    async fn history(&self, from: u64, to: u64) -> Result<Vec<Value>, String> {
        let epoch = self.shared.epoch.lock().unwrap_or_else(|e| e.into_inner()).clone().ok_or("not connected")?;
        let (r, mut w) = UnixStream::connect(&self.sock).await.map_err(|e| e.to_string())?.into_split();
        let hello = serde_json::to_string(&Hello { after: from.saturating_sub(1), epoch: Some(&epoch) }).map_err(|e| e.to_string())?;
        w.write_all(format!("{hello}\n").as_bytes()).await.map_err(|e| e.to_string())?;
        let (mut lines, mut out) = (BufReader::new(r).lines(), Vec::new());
        while let Ok(Some(line)) = lines.next_line().await {
            let Ok(mut v) = serde_json::from_str::<Value>(&line) else { continue };
            match v["t"].as_str() {
                Some("hello") if v["epoch"].as_str() != Some(epoch.as_str()) => return Err("the session restarted".to_string()),
                Some("ev") => {
                    let seq = v["seq"].as_u64().unwrap_or(0);
                    if (from..=to).contains(&seq) {
                        out.push(v["ev"].take());
                    }
                    if seq >= to {
                        break;
                    }
                }
                Some("state") => break,
                _ => {}
            }
        }
        Ok(out)
    }
}

pub fn start(sock: PathBuf, log: Arc<LogBuffer>, name: String) -> (NativeEngine, mpsc::Receiver<EngineMsg>) {
    // Roomy: the hub reads this channel while it waits on an ack the reader
    // routes, and Codex streams deltas.
    let (tx, rx) = mpsc::channel(4096);
    let shared = Arc::new(Shared { reqs: Mutex::new(HashMap::new()), next_rid: AtomicU64::new(1), writer: tokio::sync::Mutex::new(None), acks: Mutex::new(HashMap::new()), epoch: Mutex::new(None) });
    tokio::spawn(run(sock.clone(), Arc::clone(&shared), tx, log, name));
    (NativeEngine { shared, sock }, rx)
}

async fn run(sock: PathBuf, shared: Arc<Shared>, tx: mpsc::Sender<EngineMsg>, log: Arc<LogBuffer>, name: String) {
    let mut after = 0u64;
    let mut epoch: Option<String> = None;
    let mut failures = 0;
    loop {
        let stream = match UnixStream::connect(&sock).await {
            Ok(s) => s,
            Err(e) => {
                failures += 1;
                if failures >= GIVE_UP {
                    log.push(format!("chat feed gone {name}: {e}"));
                    let _ = tx.send(EngineMsg::Ended).await;
                    return;
                }
                tokio::time::sleep(RETRY).await;
                continue;
            }
        };
        failures = 0;
        let (r, mut w) = stream.into_split();
        {
            // Stored before the first line goes out, so a send right after the
            // supervisor's hello finds the connection.
            let mut slot = shared.writer.lock().await;
            let hello = serde_json::to_string(&Hello { after, epoch: epoch.as_deref() }).unwrap_or_default();
            if w.write_all(format!("{hello}\n").as_bytes()).await.is_ok() {
                *slot = Some(w);
            }
        }
        let mut lines = BufReader::new(r).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            let Ok(mut v) = serde_json::from_str::<Value>(&line) else { continue };
            let msg = match v["t"].as_str() {
                Some("hello") => {
                    let e = v["epoch"].as_str().map(str::to_string);
                    let replaced = epoch.is_some() && e != epoch;
                    epoch = e;
                    shared.epoch.lock().unwrap_or_else(|e| e.into_inner()).clone_from(&epoch);
                    if !replaced {
                        continue;
                    }
                    // A new supervisor: its positions start again.
                    after = 0;
                    EngineMsg::Reset
                }
                Some("ev") => {
                    let seq = v["seq"].as_u64().unwrap_or(0);
                    if seq <= after {
                        continue;
                    }
                    after = seq;
                    EngineMsg::Ev(v["ev"].take())
                }
                Some("state") => EngineMsg::Status {
                    working: v["working"].as_bool().unwrap_or(false),
                    waiting: v["waiting"].as_bool().unwrap_or(false),
                    waiting_for: None,
                },
                Some("asks") => EngineMsg::Asks(v["asks"].as_array().cloned().unwrap_or_default()),
                Some("settings") => EngineMsg::Settings(v["settings"].take()),
                Some("res") => {
                    let waiter = v["rid"].as_u64().and_then(|rid| shared.reqs.lock().unwrap_or_else(|e| e.into_inner()).remove(&rid));
                    if let Some(w) = waiter {
                        let result = if v["ok"] == true { Ok(v["data"].take()) } else { Err(v["error"].as_str().unwrap_or("failed").to_string()) };
                        let _ = w.send(result);
                    }
                    continue;
                }
                Some("ready") => EngineMsg::Ready,
                Some("delta") => EngineMsg::Delta {
                    id: v["id"].as_str().unwrap_or_default().to_string(),
                    text: v["text"].as_str().unwrap_or_default().to_string(),
                },
                Some("ack") => {
                    let cid = v["cid"].as_str().unwrap_or_default();
                    let waiter = shared.acks.lock().unwrap_or_else(|e| e.into_inner()).remove(cid);
                    if let Some(w) = waiter {
                        let result = if v["ok"] == true { Ok(if v["done"] == true { Sent::Done } else { Sent::Confirmed }) } else { Err(v["error"].as_str().unwrap_or("not sent").to_string()) };
                        let _ = w.send(result);
                    }
                    continue;
                }
                _ => continue,
            };
            if tx.send(msg).await.is_err() {
                return; // the hub is gone
            }
        }
        *shared.writer.lock().await = None;
        // Waiters for acks on the dropped connection learn it at once.
        shared.acks.lock().unwrap_or_else(|e| e.into_inner()).clear();
        shared.reqs.lock().unwrap_or_else(|e| e.into_inner()).clear();
        tokio::time::sleep(RETRY).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chat::tmux_io::tests::tempfile_path;
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    use tokio::net::UnixListener;

    fn log() -> Arc<LogBuffer> {
        Arc::new(LogBuffer::new())
    }

    /// The supervisor side, driven line by line by the test.
    struct FakeSup {
        listener: UnixListener,
        conn: Option<(BufReader<tokio::net::unix::OwnedReadHalf>, tokio::net::unix::OwnedWriteHalf)>,
    }
    impl FakeSup {
        async fn conn(&mut self) -> &mut (BufReader<tokio::net::unix::OwnedReadHalf>, tokio::net::unix::OwnedWriteHalf) {
            if self.conn.is_none() {
                let (s, _) = tokio::time::timeout(std::time::Duration::from_secs(30), self.listener.accept()).await.expect("the engine connects").unwrap();
                let (r, w) = s.into_split();
                self.conn = Some((BufReader::new(r), w));
            }
            self.conn.as_mut().unwrap()
        }
        async fn read_line(&mut self) -> String {
            let (r, _) = self.conn().await;
            let mut line = String::new();
            tokio::time::timeout(std::time::Duration::from_secs(30), r.read_line(&mut line)).await.expect("a line within 30 s").unwrap();
            line.trim_end().to_string()
        }
        async fn write_line(&mut self, line: &str) {
            let (_, w) = self.conn().await;
            w.write_all(format!("{line}\n").as_bytes()).await.unwrap();
        }
        async fn drop_connection(&mut self) {
            self.conn = None;
        }
    }

    fn fake_supervisor(tag: &str) -> (PathBuf, FakeSup) {
        let dir = tempfile_path(&format!("native-{tag}"));
        std::fs::create_dir_all(&dir).unwrap();
        let sock = dir.join("chat.sock");
        let listener = UnixListener::bind(&sock).unwrap();
        (sock, FakeSup { listener, conn: None })
    }

    #[tokio::test]
    async fn events_state_and_ready_flow_through_and_a_dropped_socket_resumes_without_repeats() {
        let (sock, mut sup) = fake_supervisor("flow");                      // accepts connections, exposes read_line()/write_line()
        let (_engine, mut rx) = start(sock, log(), "cdash-a".into());
        assert_eq!(sup.read_line().await, r#"{"after":0,"epoch":null}"#);
        sup.write_line(r#"{"t":"hello","epoch":"e1","head":1}"#).await;
        sup.write_line(r#"{"t":"ev","seq":1,"ev":{"type":"result","subtype":"success"}}"#).await;
        sup.write_line(r#"{"t":"state","working":true,"waiting":false}"#).await;
        sup.write_line(r#"{"t":"ready"}"#).await;
        sup.write_line(r#"{"t":"delta","id":"m1","text":"Hi"}"#).await;
        assert!(matches!(rx.recv().await, Some(EngineMsg::Ev(_))));
        assert!(matches!(rx.recv().await, Some(EngineMsg::Status { working: true, waiting: false, waiting_for: None })));
        assert!(matches!(rx.recv().await, Some(EngineMsg::Ready)));
        assert!(matches!(rx.recv().await, Some(EngineMsg::Delta { .. })));
        sup.drop_connection().await;
        assert_eq!(sup.read_line().await, r#"{"after":1,"epoch":"e1"}"#, "resumes from where it was");
        sup.write_line(r#"{"t":"hello","epoch":"e2","head":0}"#).await;      // the supervisor was replaced
        assert!(matches!(rx.recv().await, Some(EngineMsg::Reset)));
    }
    #[tokio::test]
    async fn send_waits_for_the_ack_and_stop_is_fire_and_forget() {
        let (sock, mut sup) = fake_supervisor("send");
        let (engine, _rx) = start(sock, log(), "cdash-a".into());
        sup.read_line().await;
        sup.write_line(r#"{"t":"hello","epoch":"e1","head":0}"#).await;
        let sending = tokio::spawn(async move { (engine.send("hi", true, "c1", &[]).await, engine) });
        assert_eq!(sup.read_line().await, r#"{"t":"send","cid":"c1","text":"hi","steer":true}"#);
        sup.write_line(r#"{"t":"ack","cid":"c1","ok":true}"#).await;
        let (sent, engine) = sending.await.unwrap();
        assert_eq!(sent, Ok(Sent::Confirmed), "the supervisor confirmed it");
        let doing = tokio::spawn(async move { (engine.send("/compact", false, "c3", &[]).await, engine) });
        sup.read_line().await;
        sup.write_line(r#"{"t":"ack","cid":"c3","ok":true,"done":true}"#).await;
        let (done, engine) = doing.await.unwrap();
        assert_eq!(done, Ok(Sent::Done), "the supervisor ran it itself");
        let failing = tokio::spawn(async move { (engine.send("no", false, "c2", &[]).await, engine) });
        sup.read_line().await;
        sup.write_line(r#"{"t":"ack","cid":"c2","ok":false,"error":"no turn"}"#).await;
        let (failed, engine) = failing.await.unwrap();
        assert_eq!(failed, Err("no turn".to_string()));
        engine.stop().await.unwrap();
        assert_eq!(sup.read_line().await, r#"{"t":"stop"}"#);
    }
    /// Accept the next connection on the supervisor's socket, answer it with
    /// `reply`, and return the hello it was sent.
    async fn answer_once(sup: &FakeSup, reply: &str) -> String {
        let (s, _) = tokio::time::timeout(std::time::Duration::from_secs(30), sup.listener.accept()).await.expect("a second connection").unwrap();
        let (r, mut w) = s.into_split();
        let mut hello = String::new();
        BufReader::new(r).read_line(&mut hello).await.unwrap();
        w.write_all(reply.as_bytes()).await.unwrap();
        hello.trim_end().to_string()
    }

    #[tokio::test]
    async fn history_reads_a_range_over_a_second_connection_and_refuses_another_epoch() {
        let (sock, mut sup) = fake_supervisor("history");
        let (engine, mut rx) = start(sock, log(), "cdash-a".into());
        sup.read_line().await;
        sup.write_line(r#"{"t":"hello","epoch":"e1","head":4}"#).await;
        sup.write_line(r#"{"t":"ready"}"#).await;
        assert!(matches!(rx.recv().await, Some(EngineMsg::Ready)));
        let engine = Arc::new(engine);
        let e = Arc::clone(&engine);
        let reading = tokio::spawn(async move { e.history(2, 3).await });
        let hello = answer_once(&sup, concat!(
            r#"{"t":"hello","epoch":"e1","head":4}"#, "\n",
            r#"{"t":"ev","seq":2,"ev":{"n":2}}"#, "\n",
            r#"{"t":"ev","seq":3,"ev":{"n":3}}"#, "\n",
            r#"{"t":"ev","seq":4,"ev":{"n":4}}"#, "\n",
        )).await;
        assert_eq!(hello, r#"{"after":1,"epoch":"e1"}"#);
        assert_eq!(reading.await.unwrap(), Ok(vec![serde_json::json!({"n":2}), serde_json::json!({"n":3})]));
        let e = Arc::clone(&engine);
        let reading = tokio::spawn(async move { e.history(1, 1).await });
        answer_once(&sup, concat!(r#"{"t":"hello","epoch":"e2","head":0}"#, "\n")).await;
        assert!(reading.await.unwrap().is_err(), "another supervisor's positions are not these");
    }

    #[tokio::test]
    async fn requests_carry_their_own_rid_and_each_resolves_with_its_response() {
        let (sock, mut sup) = fake_supervisor("req");
        let (engine, _rx) = start(sock, log(), "cdash-a".into());
        let engine = Arc::new(engine);
        sup.read_line().await;
        sup.write_line(r#"{"t":"hello","epoch":"e1","head":0}"#).await;
        let (a, b) = (Arc::clone(&engine), Arc::clone(&engine));
        let set = tokio::spawn(async move { a.set("plan", &json!(true)).await });
        let first: Value = serde_json::from_str(&sup.read_line().await).unwrap();
        assert_eq!((first["t"].as_str(), first["op"].as_str(), first["what"].as_str(), &first["value"]), (Some("req"), Some("set"), Some("plan"), &json!(true)));
        let menu = tokio::spawn(async move { b.menu().await });
        let second: Value = serde_json::from_str(&sup.read_line().await).unwrap();
        assert_ne!(first["rid"], second["rid"]);
        sup.write_line(&json!({"t":"res","rid":second["rid"],"ok":true,"data":{"commands":[{"name":"compact"}]}}).to_string()).await;
        sup.write_line(&json!({"t":"res","rid":first["rid"],"ok":false,"error":"nope"}).to_string()).await;
        assert_eq!(menu.await.unwrap()["commands"][0]["name"], "compact");
        assert_eq!(set.await.unwrap(), Err("nope".to_string()));
        let answer = tokio::spawn(async move { engine.answer("41", &Answer { choice: Some("yes".into()), ..Default::default() }).await });
        let third: Value = serde_json::from_str(&sup.read_line().await).unwrap();
        assert_eq!((third["op"].as_str(), third["id"].as_str(), &third["answer"]), (Some("answer"), Some("41"), &json!({"choice":"yes"})));
        sup.write_line(&json!({"t":"res","rid":third["rid"],"ok":true}).to_string()).await;
        assert_eq!(answer.await.unwrap(), Ok(()));
    }
    #[tokio::test]
    async fn asks_and_settings_lines_become_engine_messages() {
        let (sock, mut sup) = fake_supervisor("asks");
        let (_engine, mut rx) = start(sock, log(), "cdash-a".into());
        sup.read_line().await;
        sup.write_line(r#"{"t":"hello","epoch":"e1","head":0}"#).await;
        sup.write_line(r#"{"t":"asks","asks":[{"id":"41","kind":"question"}]}"#).await;
        sup.write_line(r#"{"t":"settings","settings":{"model":"m"}}"#).await;
        assert!(matches!(rx.recv().await, Some(EngineMsg::Asks(a)) if a[0]["id"] == "41"));
        assert!(matches!(rx.recv().await, Some(EngineMsg::Settings(s)) if s["model"] == "m"));
    }

    #[tokio::test(start_paused = true)]
    async fn a_request_times_out_ignores_its_late_response_and_a_dropped_connection_fails_pending_ones() {
        let (sock, mut sup) = fake_supervisor("reqtimeout");
        let (engine, _rx) = start(sock, log(), "cdash-a".into());
        let engine = Arc::new(engine);
        sup.read_line().await;
        sup.write_line(r#"{"t":"hello","epoch":"e1","head":0}"#).await;
        let e = Arc::clone(&engine);
        let slow = tokio::spawn(async move { e.request("x", json!({})).await });
        let first: Value = serde_json::from_str(&sup.read_line().await).unwrap();
        assert_eq!(slow.await.unwrap(), Err("no answer from the session".to_string()));
        sup.write_line(&json!({"t":"res","rid":first["rid"],"ok":true}).to_string()).await;
        let e = Arc::clone(&engine);
        let pending = tokio::spawn(async move { e.request("y", json!({})).await });
        sup.read_line().await;
        sup.drop_connection().await;
        assert_eq!(pending.await.unwrap(), Err("not connected".to_string()));
    }

    #[tokio::test(start_paused = true)]
    async fn a_supervisor_that_is_gone_ends_the_engine() {
        let sock = tempfile_path("gone").join("chat.sock");                  // nothing listens there
        let (_engine, mut rx) = start(sock, log(), "cdash-a".into());
        assert!(matches!(rx.recv().await, Some(EngineMsg::Ended)));
    }

    #[tokio::test]
    async fn attachments_travel_to_the_supervisor_as_paths_never_bytes() {
        use crate::chat::hub::Kind;
        let (sock, mut sup) = fake_supervisor("files");
        let (engine, _rx) = start(sock, log(), "cdash-a".into());
        sup.read_line().await;
        sup.write_line(r#"{"t":"hello","epoch":"e1","head":0}"#).await;
        let files = [Attachment { path: "/u/up/a.png".into(), kind: Kind::Image, mime: "image/png".into(), name: "a.png".into(), size: 8 }];
        let sending = tokio::spawn(async move { engine.send("look", false, "c3", &files).await });
        assert_eq!(sup.read_line().await, r#"{"t":"send","cid":"c3","text":"look","steer":false,"files":[{"path":"/u/up/a.png","kind":"image","mime":"image/png","name":"a.png","size":8}]}"#);
        sup.write_line(r#"{"t":"ack","cid":"c3","ok":true}"#).await;
        assert_eq!(sending.await.unwrap(), Ok(Sent::Confirmed));
    }
}
