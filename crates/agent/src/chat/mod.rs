//! Dashboard chat: one hub per chatted session, fed by an engine that reads the
//! tool's own record and types into it. See
//! `docs/superpowers/specs/2026-11-12-chat-core-design.md`.

pub mod asks;
pub mod claude;
pub mod ece;
pub mod hook;
pub mod hub;
pub mod media;
pub mod native;
pub mod notify;
pub mod picker;
#[cfg(test)]
mod perf;
pub mod push;
pub mod queue;
pub mod rewind;
pub mod tap;
pub mod tmux_io;
pub mod wire;
pub mod ws;

/// Largest chat message, in bytes, after sanitizing.
pub const MAX_TEXT: usize = 32 * 1024;
/// Tool input and output cap, in characters, as the bridges already cap them.
pub const CAP: usize = 65_536;
/// One serialized JSON frame, shared between subscribers without copying.
pub type Frame = std::sync::Arc<str>;

use crate::collect::ctx::Ctx;
use crate::collect::validate::Refused;
use std::sync::Arc;

/// The running hub for a pane, started on first use.
pub async fn hub_for(ctx: &Arc<Ctx>, name: &str) -> Result<hub::Hub, Refused> {
    let mut chats = ctx.chats.lock().await;
    chats.retain(|_, h| !h.is_closed());
    if let Some(h) = chats.get(name) {
        return Ok(h.clone());
    }
    let no_chat = || Refused::BadRequest("this session has no chat".into());
    let pane = tmux_io::pane(&ctx.runner, name).await.ok_or_else(no_chat)?;
    let pid = pane.pid;
    if pane.bare {
        return Err(no_chat());
    }
    // Same order as `collect_sessions`: OpenCode, then Codex, else Claude.
    let restart = || Refused::BadRequest("restart this session to enable chat".into());
    let mut project = None;
    let native = if let Some(s) = crate::collect::opencode::snapshot(ctx, name, pid).await {
        project = last_component(&s.dir);
        Some(("opencode", s.chat_sock.ok_or_else(restart)?))
    } else if let Some(s) = crate::collect::codex::snapshot(ctx, name, pid).await {
        project = last_component(&s.dir);
        // A legacy (non-native) Codex pane has no supervisor to talk to.
        Some(("codex", s.chat_sock.filter(|_| s.native_terminal).ok_or_else(restart)?))
    } else {
        None
    };
    if native.is_none() {
        let sf = crate::collect::lookup::session_file_for(&ctx.claude_dir, pid).await;
        project = sf.and_then(|f| f.cwd).and_then(|c| last_component(&c));
    }
    let notes = notify::Sink { ctx: Arc::clone(ctx), project: project.unwrap_or_else(|| name.to_string()) };
    let log = Arc::clone(&ctx.host.log);
    let hub = match native {
        Some((tool, sock)) => {
            let (engine, rx) = native::start(sock.into(), Arc::clone(&log), name.to_string());
            hub::Hub::spawn_with(tool, engine, rx, log, name.to_string(), Some(notes))
        }
        None => {
            let (mut engine, rx) = claude::start(Arc::clone(ctx), name.to_string(), pid);
            engine.hooked = pane.hook.is_some() && ctx.hook_port.is_some();
            let live = engine.live();
            let mut hub = hub::Hub::spawn_with("claude", engine, rx, log, name.to_string(), Some(notes));
            hub.claude = Some(live);
            hub
        }
    };
    chats.insert(name.to_string(), hub.clone());
    Ok(hub)
}

fn last_component(dir: &str) -> Option<String> {
    std::path::Path::new(dir).file_name().map(|n| n.to_string_lossy().into_owned())
}
