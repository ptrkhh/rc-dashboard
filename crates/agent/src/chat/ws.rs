//! The WebSocket between one browser and a session's hub.

use super::hub::{ClientFrame, Hub};
use super::media::Upload;
use crate::collect::ctx::{Ctx, FocusGuard};
use crate::collect::term::{send_or_drop, PING_EVERY};
use axum::extract::ws::{CloseFrame, Message, WebSocket};
use serde_json::json;
use std::sync::Arc;
use tokio::sync::broadcast::error::RecvError;

/// Close code: the session ended.
const ENDED: u16 = 4000;
/// Close code: this client fell behind; reconnect with the last position.
const BEHIND: u16 = 4002;

pub async fn serve(ctx: Arc<Ctx>, hub: Hub, name: String, after: Option<u64>, epoch: Option<String>, mut ws: WebSocket) {
    ctx.host.log.push(format!("chat open {name}"));
    if let Some((code, reason)) = pump(&ctx, &name, &hub, after, epoch, &mut ws).await {
        let _ = ws.send(Message::Close(Some(CloseFrame { code, reason: reason.into() }))).await;
        // Read the browser's Close reply, as `term::bridge` does, so it sees the code.
        let drain = async { while let Some(Ok(_)) = ws.recv().await {} };
        let _ = tokio::time::timeout(std::time::Duration::from_secs(1), drain).await;
    }
    ctx.host.log.push(format!("chat close {name}"));
}

fn refused(ctx: &Ctx, name: &str, reference: &str, e: &str) -> String {
    ctx.host.log.push(format!("chat upload refused {name}: {e}"));
    json!({"t": "uploaded", "ref": reference, "error": e}).to_string()
}

fn ends_session(f: &str) -> bool {
    f.contains("\"ended\":true")
        && serde_json::from_str::<serde_json::Value>(f).is_ok_and(|v| v["t"] == "state" && v["ended"] == true)
}

async fn pump(ctx: &Ctx, name: &str, hub: &Hub, after: Option<u64>, epoch: Option<String>, ws: &mut WebSocket) -> Option<(u16, &'static str)> {
    let Some(mut sub) = hub.subscribe(after, epoch).await else { return Some((ENDED, "session ended")) };
    let mut ended = false;
    for f in sub.backlog {
        ended = ends_session(&f);
        if !send_or_drop(ws, Message::Text(f.as_ref().into())).await {
            return None;
        }
    }
    if ended {
        return Some((ENDED, "session ended"));
    }
    let mut ping = tokio::time::interval(PING_EVERY);
    ping.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut heard = true;
    let mut focus = FocusGuard::new(ctx, name);
    // The one open upload and its `ref`; dropping it deletes its folder.
    let mut upload: Option<(String, Upload)> = None;
    loop {
        tokio::select! {
            _ = ping.tick() => {
                if !std::mem::take(&mut heard) || !send_or_drop(ws, Message::Ping(axum::body::Bytes::new())).await {
                    return None;
                }
            }
            live = sub.live.recv() => match live {
                Ok(f) => {
                    if !send_or_drop(ws, Message::Text(f.as_ref().into())).await {
                        return None;
                    }
                    if ends_session(&f) {
                        return Some((ENDED, "session ended"));
                    }
                }
                Err(RecvError::Lagged(_)) => return Some((BEHIND, "fell behind")),
                Err(RecvError::Closed) => return Some((ENDED, "session ended")),
            },
            msg = ws.recv() => {
                heard = true;
                match msg {
                    Some(Ok(Message::Text(t))) => {
                        let Ok(frame) = serde_json::from_str::<ClientFrame>(t.as_str()) else { continue };
                        if let ClientFrame::Focus { on } = frame {
                            focus.set(on);
                            continue;
                        }
                        if let ClientFrame::Upload { reference, name: file, size } = frame {
                            let begun = if upload.is_some() { Err("one upload at a time".into()) } else { Upload::begin(name, &file, size).await };
                            match begun {
                                Ok(u) => upload = Some((reference, u)),
                                Err(e) => {
                                    if !send_or_drop(ws, Message::Text(refused(ctx, name, &reference, &e).into())).await {
                                        return None;
                                    }
                                }
                            }
                            continue;
                        }
                        if let Some(reply) = hub.command(frame).await {
                            if !send_or_drop(ws, Message::Text(reply.as_ref().into())).await {
                                return None;
                            }
                        }
                    }
                    Some(Ok(Message::Binary(b))) => {
                        let Some((_, u)) = upload.as_mut() else { continue };
                        let written = u.write(&b).await;
                        if matches!(written, Ok(false)) {
                            continue;
                        }
                        // Done or failed: the slot empties either way, and a failed upload's folder goes with it.
                        let Some((reference, u)) = upload.take() else { continue };
                        let reply = match written {
                            Ok(_) => match u.finish().await {
                                Ok((id, a)) => {
                                    hub.add_upload(id.clone(), a.clone()).await;
                                    ctx.host.log.push(format!("chat upload {name}"));
                                    json!({"t": "uploaded", "ref": reference, "id": id, "name": a.name, "size": a.size, "kind": a.kind, "mime": a.mime}).to_string()
                                }
                                Err(e) => refused(ctx, name, &reference, &e),
                            },
                            Err(e) => refused(ctx, name, &reference, &e),
                        };
                        if !send_or_drop(ws, Message::Text(reply.into())).await {
                            return None;
                        }
                    }
                    Some(Ok(Message::Close(_))) | Some(Err(_)) | None => return None,
                    Some(Ok(_)) => {}
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::chat::claude::tests::{append, fixture_lines, fixture_with, write_session};
    use crate::chat::media;
    use crate::chat::tmux_io::tests::{eventually, tempfile_path};
    use crate::collect::ctx::Ctx;
    use crate::http::serve::{serve, tests::cfg_for};
    use futures_util::{SinkExt, StreamExt};
    use serde_json::{json, Value};
    use std::path::Path;
    use std::sync::Arc;
    use std::time::Duration;
    use tokio_tungstenite::tungstenite::{self, client::IntoClientRequest, Message as WsMsg};

    type Ws = tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

    struct Srv {
        addr: std::net::SocketAddr,
        ctx: Arc<Ctx>,
    }

    fn tempdir(tag: &str) -> std::path::PathBuf {
        let d = tempfile_path(&format!("ws-{tag}"));
        std::fs::create_dir_all(d.join("sessions")).unwrap();
        d
    }

    /// The real router over a ctx whose PATH holds only a stub tmux answering
    /// `answer`; `serve` would resolve the real PATH instead.
    async fn chat_server_with_pane(tag: &str, answer: &str) -> (Srv, std::path::PathBuf) {
        let (ctx, dir) = fixture_with(&format!("ws-{tag}"), answer);
        write_session(&dir, r#"{"sessionId":"sid-1","cwd":"/proj","status":"idle"}"#);
        let app = crate::http::serve::router(Arc::clone(&ctx), Arc::clone(&cfg_for(dir.clone()).auth), None, None);
        let addr = crate::http::serve::tests::serve_router(app).await;
        (Srv { addr, ctx }, dir)
    }

    async fn chat_server(tag: &str) -> (Srv, std::path::PathBuf) {
        chat_server_with_pane(tag, "4242|").await
    }

    fn append_transcript(dir: &Path, lines: &str) {
        append(&dir.join("projects/-proj/sid-1.jsonl"), lines);
    }

    async fn connect(addr: std::net::SocketAddr, query: &str, origin: Option<&str>) -> Result<Ws, tungstenite::Error> {
        let mut req = format!("ws://{addr}/api/chat?{query}").into_client_request().unwrap();
        if let Some(o) = origin {
            req.headers_mut().insert("origin", o.parse().unwrap());
        }
        tokio_tungstenite::connect_async(req).await.map(|(ws, _)| ws)
    }

    fn status_of(r: Result<Ws, tungstenite::Error>) -> u16 {
        match r {
            Err(tungstenite::Error::Http(res)) => res.status().as_u16(),
            Err(e) => panic!("expected an HTTP refusal, got {e}"),
            Ok(_) => panic!("expected an HTTP refusal, got an open socket"),
        }
    }

    async fn next_json(ws: &mut Ws) -> Value {
        tokio::time::timeout(Duration::from_secs(30), async {
            loop {
                match ws.next().await {
                    Some(Ok(WsMsg::Text(t))) => return serde_json::from_str(t.as_str()).unwrap(),
                    Some(Ok(_)) => {}
                    other => panic!("socket ended: {other:?}"),
                }
            }
        })
        .await
        .expect("no frame within 30 s")
    }

    async fn next_of(ws: &mut Ws, t: &str) -> Value {
        loop {
            let f = next_json(ws).await;
            if f["t"] == t {
                return f;
            }
        }
    }

    #[tokio::test]
    async fn refusals_happen_before_the_upgrade() {
        let b = serve(cfg_for(tempdir("chat-refuse"))).await.unwrap();
        assert_eq!(status_of(connect(b.addr, "name=cdash-x", Some("https://evil.example")).await), 403);
        assert_eq!(status_of(connect(b.addr, "name=other", None).await), 400);
        assert_eq!(status_of(connect(b.addr, "name=cdash-gone", None).await), 409, "no such pane");
    }
    #[tokio::test]
    async fn a_client_reads_history_sends_a_prompt_and_resumes_without_repeats() {
        let (b, dir) = chat_server("flow").await;                        // transcript = fixture lines 1..9, status idle
        let mut ws = connect(b.addr, "name=cdash-a", None).await.unwrap();
        let hello = next_json(&mut ws).await;
        assert_eq!((hello["t"].as_str(), hello["tool"].as_str(), hello["head"].as_u64()), (Some("hello"), Some("claude"), Some(5)));
        let epoch = hello["epoch"].as_str().unwrap().to_string();
        for seq in 1..=5 { assert_eq!(next_json(&mut ws).await["seq"], seq); }
        assert_eq!(next_json(&mut ws).await["t"], "state");
        ws.send(WsMsg::Text(r#"{"t":"send","cid":"c1","text":"zebra\u001b[201~"}"#.into())).await.unwrap();
        assert_eq!(next_of(&mut ws, "ack").await["status"], "sent");
        let args = std::fs::read_to_string(dir.join("tmux-args")).unwrap();
        assert!(args.contains("paste-buffer -p -d -b cdash-chat-cdash-a -t =cdash-a:"));
        append_transcript(&dir, &fixture_lines(13..14));           // line 13, the prompt "slow job"
        assert_eq!(next_of(&mut ws, "ev").await["seq"], 6);
        drop(ws);
        let mut again = connect(b.addr, &format!("name=cdash-a&after=6&epoch={epoch}"), None).await.unwrap();
        assert_eq!(next_json(&mut again).await["reset"], false);
        assert_eq!(next_json(&mut again).await["t"], "state", "nothing is replayed");
        let log = b.ctx.host.log.lines().join("\n");
        assert!(log.contains("chat open cdash-a") && log.contains("chat send cdash-a") && !log.contains("zebra"));
    }
    #[tokio::test]
    async fn focus_frames_feed_the_count_and_a_socket_that_drops_counts_as_off() {
        let (b, _dir) = chat_server("focus").await;
        let mut ws = connect(b.addr, "name=cdash-a", None).await.unwrap();
        next_of(&mut ws, "state").await;
        ws.send(WsMsg::Text(r#"{"t":"focus","on":true}"#.into())).await.unwrap();
        eventually("chat focus counted", || b.ctx.focused("cdash-a")).await;
        drop(ws); // no "focus off": the phone went to sleep
        eventually("dropped socket uncounted", || !b.ctx.focused("cdash-a")).await;
    }
    #[tokio::test]
    async fn an_upload_on_the_chat_socket_returns_an_id_that_send_accepts() {
        let (b, _dir) = chat_server("upload").await;
        let mut ws = connect(b.addr, "name=cdash-upload-flow", None).await.unwrap();
        next_of(&mut ws, "state").await;
        ws.send(WsMsg::Text(r#"{"t":"upload","ref":"r1","name":"notes.txt","size":11}"#.into())).await.unwrap();
        for part in [&b"hello"[..], b" world"] { ws.send(WsMsg::Binary(part.to_vec().into())).await.unwrap(); }
        let up = next_of(&mut ws, "uploaded").await;
        assert_eq!((up["ref"].as_str(), up["name"].as_str(), up["kind"].as_str(), up["size"].as_u64(), up["mime"].as_str()),
            (Some("r1"), Some("notes.txt"), Some("file"), Some(11), Some("application/octet-stream")));
        let id = up["id"].as_str().unwrap().to_string();
        let dir = media::uploads_dir("cdash-upload-flow").unwrap();
        assert_eq!(std::fs::read(dir.join(&id).join("notes.txt")).unwrap(), b"hello world");
        ws.send(WsMsg::Text(format!(r#"{{"t":"send","cid":"c1","text":"read it","files":["{id}"]}}"#).into())).await.unwrap();
        assert_eq!(next_of(&mut ws, "ack").await["status"], "sent");
        let log = b.ctx.host.log.lines().join("\n");
        assert!(log.contains("chat upload cdash-upload-flow") && !log.contains("notes.txt") && !log.contains("read it"));
        let _ = std::fs::remove_dir_all(dir);
    }
    #[tokio::test]
    async fn refused_parallel_and_abandoned_uploads_leave_nothing_behind() {
        let (b, _dir) = chat_server("upload-refused").await;
        let mut ws = connect(b.addr, "name=cdash-upload-gone", None).await.unwrap();
        next_of(&mut ws, "state").await;
        let text = |s: String| WsMsg::Text(s.into());
        ws.send(WsMsg::Binary(b"stray".to_vec().into())).await.unwrap(); // no open upload: ignored
        ws.send(text(format!(r#"{{"t":"upload","ref":"r1","name":"big.bin","size":{}}}"#, media::MAX_UPLOAD + 1))).await.unwrap();
        assert_eq!(next_of(&mut ws, "uploaded").await, json!({"t":"uploaded","ref":"r1","error":"file over 20 MB"}));
        ws.send(text(r#"{"t":"upload","ref":"r2","name":"a.txt","size":4}"#.into())).await.unwrap();
        ws.send(WsMsg::Binary(b"abcdef".to_vec().into())).await.unwrap();
        assert_eq!(next_of(&mut ws, "uploaded").await, json!({"t":"uploaded","ref":"r2","error":"more bytes than declared"}));
        ws.send(text(r#"{"t":"upload","ref":"r3","name":"b.txt","size":10}"#.into())).await.unwrap();
        ws.send(text(r#"{"t":"upload","ref":"r4","name":"c.txt","size":10}"#.into())).await.unwrap();
        assert_eq!(next_of(&mut ws, "uploaded").await, json!({"t":"uploaded","ref":"r4","error":"one upload at a time"}));
        ws.send(WsMsg::Binary(b"abc".to_vec().into())).await.unwrap();
        let dir = media::uploads_dir("cdash-upload-gone").unwrap();
        eventually("r3 is on disk", || std::fs::read_dir(&dir).is_ok_and(|d| d.count() == 1)).await;
        drop(ws); // the socket closes in the middle of r3
        eventually("the abandoned upload is gone", || std::fs::read_dir(&dir).map_or(true, |d| d.count() == 0)).await;
        let log = b.ctx.host.log.lines().join("\n");
        assert!(log.contains("chat upload refused cdash-upload-gone: one upload at a time") && !log.contains("big.bin"));
    }
    #[tokio::test]
    async fn garbage_frames_are_ignored_and_a_bare_terminal_pane_has_no_chat() {
        let (b, _dir) = chat_server("garbage").await;
        let mut ws = connect(b.addr, "name=cdash-a", None).await.unwrap();
        ws.send(WsMsg::Text("not json".into())).await.unwrap();
        ws.send(WsMsg::Text(r#"{"t":"stop"}"#.into())).await.unwrap();
        assert_eq!(next_of(&mut ws, "state").await["ended"], false, "still connected");
        let (t, _d) = chat_server_with_pane("bare", "4242|terminal").await;
        assert_eq!(status_of(connect(t.addr, "name=cdash-a", None).await), 409);
    }
}
