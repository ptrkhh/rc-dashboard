//! What a hub tells the notifier: a session finished, failed or wants an answer.

use crate::collect::ctx::Ctx;
use serde_json::json;
use std::sync::Arc;
use std::time::Duration;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum NoteKind {
    Done,
    Error,
    Ask,
}

impl NoteKind {
    pub fn as_str(self) -> &'static str {
        match self {
            NoteKind::Done => "done",
            NoteKind::Error => "error",
            NoteKind::Ask => "ask",
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Note {
    pub session: String,
    pub project: String,
    pub kind: NoteKind,
    pub at: u64,
}

impl Note {
    /// Exactly what a push carries: no message text, tool or file names.
    pub fn payload(&self) -> String {
        json!({"s":self.session,"p":self.project,"k":self.kind.as_str(),"t":self.at}).to_string()
    }
}

/// A hub's way to publish notes for its session.
pub struct Sink {
    pub ctx: Arc<Ctx>,
    pub project: String,
}

impl Sink {
    pub fn note(&self, session: &str, kind: NoteKind) {
        if self.ctx.focused(session) {
            return;
        }
        let at = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs());
        let _ = self.ctx.notes.send(Note { session: session.into(), project: self.project.clone(), kind, at });
    }
}

/// Open a hub for every session that is not a bare terminal, so their notes
/// flow. Refusals (legacy sessions, vanished panes) are ignored.
pub async fn ensure_hubs(ctx: &Arc<Ctx>) {
    let out = ctx.runner.run("tmux", &["list-panes", "-a", "-F", crate::parse::tmux::PANE_FORMAT], "notify-panes").await;
    for pane in crate::parse::tmux::parse_tmux_panes(&out).into_iter().filter(|p| !p.terminal) {
        let _ = super::hub_for(ctx, &pane.name).await;
    }
}

/// The notifier: keeps hubs open while anyone could be told, and pushes notes.
/// A `serve()`-based test must not hold a subscription or notify stream for 5 s
/// or more: the tick would list the real tmux server (P4-6).
pub fn spawn(ctx: Arc<Ctx>, mut stop: tokio::sync::watch::Receiver<bool>) {
    let mut notes = ctx.notes.subscribe();
    tokio::spawn(async move {
        let http = super::push::client();
        let mut tick = tokio::time::interval(Duration::from_secs(5));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tokio::select! {
                _ = stop.changed() => return,
                _ = tick.tick() => {
                    if !super::push::subscriptions(&ctx).await.is_empty() || ctx.notes.receiver_count() > 1 {
                        ensure_hubs(&ctx).await;
                    }
                }
                note = notes.recv() => match note {
                    Ok(n) => super::push::send(&ctx, &http, &n).await,
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
                    Err(_) => return,
                },
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chat::claude::tests::{append, ctx_with_path, fixture_lines, fixture_with, write_session};
    use crate::chat::push::{self, Sub};
    use crate::chat::tmux_io::tests::{eventually, install_stub, tempfile_path};
    use crate::chat::ece;
    use crate::collect::ctx::FocusGuard;
    use serde_json::{json, Value};
    use std::sync::Mutex;

    struct Req {
        id: String,
        headers: axum::http::HeaderMap,
        body: Vec<u8>,
    }
    impl Req {
        fn header(&self, k: &str) -> &str {
            self.headers.get(k).and_then(|v| v.to_str().ok()).unwrap_or("")
        }
    }

    #[derive(Clone)]
    struct Inbox {
        got: Arc<Mutex<Vec<Req>>>,
        base: String,
    }
    impl Inbox {
        fn take(&self) -> Vec<Req> {
            std::mem::take(&mut *self.got.lock().unwrap())
        }
        async fn wait(&self, n: usize) -> Vec<Req> {
            eventually("the pushes arrived", || self.got.lock().unwrap().len() >= n).await;
            self.take()
        }
    }

    /// A push service on 127.0.0.1: 410 for id `gone`, 201 otherwise.
    async fn fake_push_service() -> Inbox {
        use axum::{extract::{Path, State}, http::{HeaderMap, StatusCode}, routing::post, Router};
        let got = Arc::new(Mutex::new(Vec::new()));
        let app = Router::new()
            .route(
                "/push/{id}",
                post(|State(g): State<Arc<Mutex<Vec<Req>>>>, Path(id): Path<String>, headers: HeaderMap, body: axum::body::Bytes| async move {
                    let code = if id == "gone" { StatusCode::GONE } else { StatusCode::CREATED };
                    g.lock().unwrap().push(Req { id, headers, body: body.to_vec() });
                    code
                }),
            )
            .with_state(Arc::clone(&got));
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", l.local_addr().unwrap());
        tokio::spawn(async move {
            let _ = axum::serve(l, app).await;
        });
        Inbox { got, base }
    }

    fn device() -> (ring::agreement::EphemeralPrivateKey, Vec<u8>, [u8; 16]) {
        use ring::{agreement, rand::{SecureRandom, SystemRandom}};
        let rng = SystemRandom::new();
        let k = agreement::EphemeralPrivateKey::generate(&agreement::ECDH_P256, &rng).unwrap();
        let public = k.compute_public_key().unwrap().as_ref().to_vec();
        let mut auth = [0u8; 16];
        rng.fill(&mut auth).unwrap();
        (k, public, auth)
    }

    fn sub(inbox: &Inbox, id: &str, key: &[u8], auth: &[u8]) -> Sub {
        Sub { endpoint: format!("{}/push/{id}", inbox.base), p256dh: ece::b64(key), auth: ece::b64(auth), added: 1 }
    }

    /// `fixture_with`, with a tmux that lists one Claude pane `cdash-a` (pid 4242).
    fn notifier_fixture(tag: &str) -> (Arc<Ctx>, std::path::PathBuf) {
        let (ctx, dir) = fixture_with(tag, "");
        let script = "#!/bin/sh\ncase \"$1\" in\nlist-panes) echo 'cdash-a|4242|1785050000||/proj';;\ndisplay-message) echo '4242|';;\nesac\nexit 0\n";
        install_stub(&dir, "tmux", script);
        (ctx, dir)
    }

    #[test]
    fn the_payload_carries_session_project_kind_and_time_only() {
        let n = Note { session: "cdash-a".into(), project: "my \"proj\"".into(), kind: NoteKind::Ask, at: 1_789_000_000 };
        assert_eq!(
            serde_json::from_str::<Value>(&n.payload()).unwrap(),
            json!({"s":"cdash-a","p":"my \"proj\"","k":"ask","t":1_789_000_000u64})
        );
    }

    #[tokio::test]
    async fn a_note_reaches_every_device_encrypted_and_a_gone_device_is_dropped() {
        let dir = tempfile_path("push-send");
        std::fs::create_dir_all(&dir).unwrap();
        let ctx = ctx_with_path(dir, String::new());
        let inbox = fake_push_service().await;
        let (ka, pa, aa) = device();
        // A closed port: connecting to 127.0.0.1:1 hangs under WSL2.
        let closed = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap();
        let dead = Sub { endpoint: format!("http://{closed}/push/dead"), p256dh: ece::b64(&pa), auth: ece::b64(&aa), added: 1 };
        push::add(&ctx, dead).await.unwrap();
        push::add(&ctx, sub(&inbox, "gone", &pa, &aa)).await.unwrap();
        push::add(&ctx, sub(&inbox, "a", &pa, &aa)).await.unwrap();
        push::send(&ctx, &push::client(), &Note { session: "cdash-a".into(), project: "proj".into(), kind: NoteKind::Ask, at: 7 }).await;
        let got = inbox.take();
        let mut ids: Vec<_> = got.iter().map(|r| r.id.as_str()).collect();
        ids.sort();
        assert_eq!(ids, ["a", "gone"], "a dead or gone device does not stop the next");
        let got: Vec<_> = got.into_iter().filter(|r| r.id == "a").collect();
        let log = ctx.host.log.lines().join("\n");
        assert!(log.contains("push failed unreachable 127.0.0.1") && !log.contains("cdash-a"), "logs name the host only");
        let r = &got[0];
        assert_eq!((r.header("content-encoding"), r.header("ttl"), r.header("urgency")), ("aes128gcm", "86400", "high"));
        assert_eq!(r.header("topic").len(), 32);
        assert!(r.header("authorization").starts_with("vapid t="));
        assert_eq!(serde_json::from_slice::<Value>(&ece::decrypt(ka, &pa, &aa, &r.body)).unwrap(), json!({"s":"cdash-a","p":"proj","k":"ask","t":7}));
        assert_eq!(push::subscriptions(&ctx).await.len(), 2, "410 removed the gone one; an unreachable one stays");
    }

    #[tokio::test]
    async fn a_live_turn_end_and_a_long_wait_reach_every_device_but_not_while_someone_looks() {
        let (ctx, dir) = notifier_fixture("e2e");
        let transcript = dir.join("projects/-proj/sid-1.jsonl");
        let inbox = fake_push_service().await;
        let ((ka, pa, aa), (kb, pb, ab)) = (device(), device());
        push::add(&ctx, sub(&inbox, "a", &pa, &aa)).await.unwrap();
        push::add(&ctx, sub(&inbox, "b", &pb, &ab)).await.unwrap();
        let (_stop, stop) = tokio::sync::watch::channel(false);
        spawn(Arc::clone(&ctx), stop);
        eventually("the notifier opened a hub", || ctx.chats.try_lock().is_ok_and(|c| c.contains_key("cdash-a"))).await;
        tokio::time::sleep(Duration::from_secs(1)).await;
        assert!(inbox.take().is_empty(), "finished turns in history never notify");
        append(&transcript, &(fixture_lines(13..14) + &fixture_lines(9..10)));
        let done = inbox.wait(2).await;
        assert_eq!(serde_json::from_slice::<Value>(&ece::decrypt(ka, &pa, &aa, &done[0].body)).unwrap()["k"], "done");
        write_session(&dir, r#"{"sessionId":"sid-1","cwd":"/proj","status":"waiting","waitingFor":"permission prompt"}"#);
        let ask = inbox.wait(2).await;
        assert_eq!(serde_json::from_slice::<Value>(&ece::decrypt(kb, &pb, &ab, &ask[1].body)).unwrap()["k"], "ask");
        let mut looking = FocusGuard::new(&ctx, "cdash-a");
        looking.set(true);
        write_session(&dir, r#"{"sessionId":"sid-1","cwd":"/proj","status":"idle"}"#);
        append(&transcript, &(fixture_lines(13..14) + &fixture_lines(9..10)));
        tokio::time::sleep(Duration::from_secs(2)).await;
        assert!(inbox.take().is_empty(), "a focused chat: nothing is pushed");
    }
}
