//! Claude's permission hook: questions and plan approvals from a real dialog
//! are held as chat cards until chat answers them. Served on its own loopback
//! listener; see `serve::serve`.

use super::tmux_io;
use crate::collect::ctx::Ctx;
use crate::collect::lookup::session_file_for;
use crate::collect::validate::assert_kill_name;
use axum::body::Bytes;
use axum::extract::{ConnectInfo, DefaultBodyLimit, Query, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::{Extension, Json, Router};
use serde::Deserialize;
use serde_json::{json, Value};
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};
use subtle::ConstantTimeEq;

/// Largest hook request body; axum answers 413 above it.
pub const BODY_MAX: usize = 1 << 20;
/// How long after a request arrives the status file may take to show its dialog.
pub const DIALOG_WINDOW: Duration = Duration::from_millis(300);
const DIALOG_POLL: Duration = Duration::from_millis(50);

/// Only the hook route: nothing else is served on the hook port.
pub fn router(ctx: Arc<Ctx>) -> Router {
    Router::new()
        .route("/hook/claude", axum::routing::post(hook))
        .layer(DefaultBodyLimit::max(BODY_MAX))
        .with_state(ctx)
}

pub fn loopback(peer: &SocketAddr) -> bool {
    peer.ip().to_canonical().is_loopback()
}

#[derive(Deserialize)]
struct Target {
    #[serde(default)]
    pane: String,
}

async fn hook(
    State(ctx): State<Arc<Ctx>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Query(q): Query<Target>,
    headers: HeaderMap,
    stop: Option<Extension<tokio::sync::watch::Receiver<bool>>>,
    body: Bytes,
) -> Response {
    let log = &ctx.host.log;
    // An unchecked name is not echoed into the log.
    let name_ok = assert_kill_name(&q.pane).is_ok();
    let pane = if name_ok { q.pane.as_str() } else { "(bad name)" };
    let refused = |code: StatusCode, why: &str| {
        log.push(format!("hook refused {pane}: {why}"));
        code.into_response()
    };
    if !loopback(&peer) {
        return refused(StatusCode::FORBIDDEN, "not loopback");
    }
    if !name_ok {
        return refused(StatusCode::BAD_REQUEST, "bad name");
    }
    let Some(p) = tmux_io::pane(&ctx.runner, pane).await else {
        return refused(StatusCode::NOT_FOUND, "no such pane");
    };
    let token = headers.get(header::AUTHORIZATION).and_then(|v| v.to_str().ok()).and_then(|v| v.strip_prefix("Bearer "));
    let token_ok = match (token, p.hook.as_deref()) {
        (Some(t), Some(h)) => bool::from(t.as_bytes().ct_eq(h.as_bytes())),
        _ => false,
    };
    if !token_ok {
        return refused(StatusCode::UNAUTHORIZED, "bad token");
    }
    let Ok(req) = serde_json::from_slice::<Value>(&body) else {
        return refused(StatusCode::BAD_REQUEST, "not json");
    };
    let tool = req["tool_name"].as_str().unwrap_or("");
    // An empty 200 is no decision: Claude shows its own dialog.
    let empty = |why: &str| {
        log.push(format!("hook {pane} {tool}: empty ({why})"));
        StatusCode::OK.into_response()
    };
    if !matches!(tool, "AskUserQuestion" | "ExitPlanMode") {
        return empty("other tool");
    }
    // Set for subagent hooks; those have no dialog to answer.
    if req.get("agent_id").is_some() {
        return empty("subagent");
    }
    if !dialog_open(&ctx, p.pid).await {
        return empty("no dialog");
    }
    let Ok(hub) = super::hub_for(&ctx, pane).await else {
        return empty("no chat");
    };
    let Some(live) = hub.claude.as_ref() else {
        return empty("not a Claude chat");
    };
    let Some(decided) = live.lock().unwrap_or_else(|e| e.into_inner()).hold(&req, Instant::now()) else {
        return empty("not a card");
    };
    log.push(format!("hook {pane} {tool}: card"));
    tokio::select! {
        d = decided => match d {
            Ok(decision) => Json(json!({"hookSpecificOutput":{"hookEventName":"PermissionRequest","decision":decision}})).into_response(),
            Err(_) => empty("settled"),
        },
        () = crate::http::routes::stopped(stop) => empty("agent stopping"),
    }
}

/// Whether the status file shows a question or plan dialog within `DIALOG_WINDOW`.
async fn dialog_open(ctx: &Ctx, pid: i32) -> bool {
    let until = Instant::now() + DIALOG_WINDOW;
    loop {
        let f = session_file_for(&ctx.claude_dir, pid).await;
        if f.is_some_and(|f| f.status.as_deref() == Some("waiting") && matches!(f.waiting_for.as_deref(), Some("input needed" | "permission prompt"))) {
            return true;
        }
        if Instant::now() >= until {
            return false;
        }
        tokio::time::sleep(DIALOG_POLL).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chat::asks::tests::ask_request;
    use crate::chat::claude::{self, tests::write_session};
    use crate::chat::hub::Hub;
    use crate::chat::wire::ClientFrame;
    use serde_json::{json, Value};
    use std::path::PathBuf;

    /// The hook router over a Claude fixture whose stub tmux answers `answer`.
    async fn hook_server(tag: &str, answer: &str) -> (SocketAddr, Arc<Ctx>, PathBuf) {
        let (ctx, dir) = claude::tests::fixture_with(&format!("hook-{tag}"), answer);
        let addr = crate::http::serve::tests::serve_router(router(Arc::clone(&ctx))).await;
        (addr, ctx, dir)
    }

    async fn post(addr: SocketAddr, query: &str, token: Option<&str>, body: String) -> (u16, String) {
        let mut req = reqwest::Client::new()
            .post(format!("http://{addr}/hook/claude?{query}"))
            .header("content-type", "application/json")
            .body(body);
        if let Some(t) = token {
            req = req.bearer_auth(t);
        }
        let res = req.send().await.expect("the hook listener answers");
        (res.status().as_u16(), res.text().await.unwrap_or_default())
    }

    /// The id of the first ask, once the hub's state shows one.
    async fn ask_id(hub: &Hub) -> String {
        for _ in 0..100 {
            let sub = hub.subscribe(None, None).await.expect("hub running");
            let state: Value = serde_json::from_str(sub.backlog.last().expect("a state frame")).unwrap();
            if let Some(id) = state["asks"][0]["id"].as_str() {
                return id.to_string();
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        panic!("no ask within 5 s");
    }

    #[tokio::test]
    async fn the_route_refuses_bad_names_wrong_tokens_big_bodies_and_dead_panes() {
        let (addr, _ctx, dir) = hook_server("refuse", "4242||tok-1").await;
        let ok = ask_request("p1").to_string();
        assert_eq!(post(addr, "pane=other", Some("tok-1"), ok.clone()).await.0, 400);
        assert_eq!(post(addr, "pane=cdash-a", None, ok.clone()).await.0, 401);
        assert_eq!(post(addr, "pane=cdash-a", Some("tok-2"), ok.clone()).await.0, 401);
        assert_eq!(post(addr, "pane=cdash-a", Some("tok-1"), "x".repeat(BODY_MAX + 1)).await.0, 413);
        claude::tests::make_tmux_fail(&dir);
        assert_eq!(post(addr, "pane=cdash-a", Some("tok-1"), ok).await.0, 404, "not a live pane");
        assert!(loopback(&"127.0.0.1:9".parse().unwrap()) && loopback(&"[::1]:9".parse().unwrap()));
        assert!(!loopback(&"192.168.1.20:9".parse().unwrap()));
    }
    #[tokio::test]
    async fn a_pane_without_a_hook_token_refuses_every_token() {
        let (addr, _ctx, _dir) = hook_server("no-token", "4242||").await;
        let ok = ask_request("p1").to_string();
        assert_eq!(post(addr, "pane=cdash-a", Some(""), ok.clone()).await.0, 401);
        assert_eq!(post(addr, "pane=cdash-a", Some("tok-1"), ok).await.0, 401);
    }
    #[tokio::test]
    async fn other_tools_subagents_and_requests_without_a_dialog_are_answered_empty() {
        let (addr, _ctx, dir) = hook_server("empty", "4242||tok-1").await;
        write_session(&dir, r#"{"sessionId":"sid-1","cwd":"/proj","status":"waiting","waitingFor":"permission prompt"}"#);
        let started = std::time::Instant::now();
        let bash = json!({"prompt_id":"p","tool_name":"Bash","tool_input":{"command":"ls"}}).to_string();
        assert_eq!(post(addr, "pane=cdash-a", Some("tok-1"), bash).await, (200, String::new()));
        let mut sub = ask_request("p1");
        sub["agent_id"] = json!("a6cc68d2a048556bd");
        assert_eq!(post(addr, "pane=cdash-a", Some("tok-1"), sub.to_string()).await, (200, String::new()));
        assert!(started.elapsed() < DIALOG_WINDOW, "neither waits for the dialog check");
        write_session(&dir, r#"{"sessionId":"sid-1","cwd":"/proj","status":"busy"}"#);
        assert_eq!(post(addr, "pane=cdash-a", Some("tok-1"), ask_request("p1").to_string()).await, (200, String::new()), "no dialog within 300 ms");
    }
    #[tokio::test]
    async fn a_question_from_a_real_dialog_is_held_until_chat_answers() {
        let (addr, ctx, dir) = hook_server("card", "4242||tok-1").await;
        write_session(&dir, r#"{"sessionId":"sid-1","cwd":"/proj","status":"waiting","waitingFor":"input needed"}"#);
        let held = tokio::spawn(post(addr, "pane=cdash-a", Some("tok-1"), ask_request("p1").to_string()));
        let hub = crate::chat::hub_for(&ctx, "cdash-a").await.unwrap();
        let id = ask_id(&hub).await;
        let answers = json!({"q0":{"choices":["Blue"]},"q1":{"choices":["Pear"]}});
        let reply = hub.command(ClientFrame::Answer { id, choice: None, text: None, answers: Some(answers) }).await.unwrap();
        assert_eq!(serde_json::from_str::<Value>(&reply).unwrap()["ok"], true);
        let (status, body) = held.await.unwrap();
        assert_eq!(status, 200);
        assert_eq!(serde_json::from_str::<Value>(&body).unwrap(), json!({"hookSpecificOutput":{"hookEventName":"PermissionRequest","decision":{"behavior":"allow","updatedInput":{"questions":ask_request("p1")["tool_input"]["questions"],"answers":{"Pick a color":"Blue","Pick fruits":["Pear"]}}}}}));
        let log = ctx.host.log.lines().join("\n");
        assert!(log.contains("hook cdash-a AskUserQuestion: card") && !log.contains("tok-1") && !log.contains("Pick a color"));
    }
    #[tokio::test]
    async fn a_held_hook_is_answered_empty_when_the_agent_stops() {
        let (ctx, dir) = claude::tests::fixture_with("hook-stop", "4242||tok-1");
        write_session(&dir, r#"{"sessionId":"sid-1","cwd":"/proj","status":"waiting","waitingFor":"input needed"}"#);
        let (stop, rx) = tokio::sync::watch::channel(false);
        let app = router(Arc::clone(&ctx)).layer(axum::Extension(rx));
        let addr = crate::http::serve::tests::serve_router(app).await;
        let held = tokio::spawn(post(addr, "pane=cdash-a", Some("tok-1"), ask_request("p1").to_string()));
        ask_id(&crate::chat::hub_for(&ctx, "cdash-a").await.unwrap()).await;
        stop.send(true).unwrap();
        let got = tokio::time::timeout(Duration::from_secs(5), held).await.expect("answered on stop").unwrap();
        assert_eq!(got, (200, String::new()));
    }
}
