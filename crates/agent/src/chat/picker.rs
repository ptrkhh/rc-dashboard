//! The Claude slash menu: the command and model lists come from one headless
//! `claude -p` `initialize` per project folder, and typed commands are routed
//! to the pickers, pasted, or refused with a pointer.

use crate::collect::ctx::Ctx;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, Mutex};
use std::time::{Duration, Instant};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

pub const EFFORTS: [&str; 6] = ["low", "medium", "high", "xhigh", "max", "auto"];
/// Commands the menu does not list and a bare typed name refuses.
pub const HIDDEN: &[&str] = &[
    "model", "effort", "config", "permissions", "resume", "rewind", "usage", "cost", "stats", "status", "tasks", "mcp",
    "plugin", "hooks", "agents", "memory", "export", "login", "logout", "theme", "diff",
];
pub const INIT_WAIT: Duration = Duration::from_secs(10);
pub const REFRESH_AFTER: Duration = Duration::from_secs(3600);

#[derive(Debug, PartialEq)]
pub enum Route {
    Paste,
    Set(&'static str, Value),
    Refuse(String),
}

pub fn route(text: &str) -> Route {
    let Some(rest) = text.trim().strip_prefix('/') else { return Route::Paste };
    let (name, arg) = rest.split_once(char::is_whitespace).map_or((rest, ""), |(n, a)| (n, a.trim()));
    let pointer = match name {
        "btw" => "Use Ask aside".to_string(),
        "plan" if arg.is_empty() => return Route::Set("plan", json!(true)),
        "model" | "effort" if !arg.is_empty() => {
            return Route::Set(if name == "model" { "model" } else { "effort" }, json!(arg));
        }
        _ if !arg.is_empty() || !HIDDEN.contains(&name) => return Route::Paste,
        "model" | "effort" => "Use the model chip at the top of the chat".into(),
        "usage" | "cost" | "stats" => "Use the usage sheet".into(),
        "rewind" => "Use Rewind to here on a message".into(),
        "tasks" => "Use the running list".into(),
        "diff" => "Use Changes at the top of the chat".into(),
        _ => format!("Use Terminal for /{name}"),
    };
    Route::Refuse(pointer)
}

pub fn menu_from(init: &Value) -> Value {
    let empty = Vec::new();
    let commands: Vec<Value> = init["commands"]
        .as_array()
        .unwrap_or(&empty)
        .iter()
        .filter_map(|c| {
            let name = c["name"].as_str().filter(|n| !HIDDEN.contains(n))?;
            let mut o = json!({"name": name, "description": c["description"].as_str().unwrap_or("").chars().take(200).collect::<String>()});
            if let Some(h) = c["argumentHint"].as_str().filter(|h| !h.is_empty()) {
                o["hint"] = json!(h);
            }
            Some(o)
        })
        .collect();
    let models: Vec<Value> = init["models"]
        .as_array()
        .unwrap_or(&empty)
        .iter()
        .filter(|m| m["value"].as_str().is_some_and(|v| !v.starts_with("claude-")))
        .map(|m| json!({"value": m["value"], "label": m["displayName"], "model": m["resolvedModel"]}))
        .collect();
    json!({"commands": commands, "models": models, "efforts": EFFORTS, "agents": []})
}

type Cache = HashMap<PathBuf, (Instant, Value)>;
// The lock is never held across an await.
static CACHE: LazyLock<Mutex<Cache>> = LazyLock::new(Default::default);

fn cache() -> std::sync::MutexGuard<'static, Cache> {
    CACHE.lock().unwrap_or_else(|e| e.into_inner())
}

#[cfg(test)]
pub fn seed(cwd: &Path, init: Value) {
    cache().insert(cwd.to_path_buf(), (Instant::now(), init));
}

/// The `initialize` response for a folder, cached per folder. A copy over
/// `REFRESH_AFTER` old is returned at once and reloaded in the background.
// ponytail: a failed refresh keeps the old copy for another hour.
pub async fn initialize(ctx: &Arc<Ctx>, cwd: &Path) -> Result<Value, String> {
    let hit = cache().get_mut(cwd).map(|(at, v)| {
        let stale = at.elapsed() >= REFRESH_AFTER;
        if stale {
            *at = Instant::now(); // one reload at a time
        }
        (stale, v.clone())
    });
    if let Some((stale, v)) = hit {
        if stale {
            let (ctx, cwd) = (Arc::clone(ctx), cwd.to_path_buf());
            tokio::spawn(async move {
                let _ = load(&ctx, &cwd).await;
            });
        }
        return Ok(v);
    }
    load(ctx, cwd).await
}

/// Runs one headless `claude -p` and caches its answer.
async fn load(ctx: &Arc<Ctx>, cwd: &Path) -> Result<Value, String> {
    let r = tokio::time::timeout(INIT_WAIT, run_initialize(ctx, cwd)).await.unwrap_or_else(|_| Err("timed out".into()));
    match &r {
        Ok(v) => {
            cache().insert(cwd.to_path_buf(), (Instant::now(), v.clone()));
        }
        Err(e) => ctx.host.log.push(format!("chat menu failed: {e}")),
    }
    r
}

/// Registers a pid in `Ctx.own_pids` until dropped.
struct OwnPid<'a>(&'a Ctx, Option<i32>);

impl Drop for OwnPid<'_> {
    fn drop(&mut self) {
        if let Some(p) = self.1 {
            self.0.own_pids.lock().unwrap_or_else(|e| e.into_inner()).remove(&p);
        }
    }
}

async fn run_initialize(ctx: &Arc<Ctx>, cwd: &Path) -> Result<Value, String> {
    let args = ["-p", "--input-format", "stream-json", "--output-format", "stream-json", "--verbose", "--settings", r#"{"disableAllHooks":true}"#];
    let mut child = ctx.runner.spawn_piped("claude", &args, cwd).map_err(|e| e.to_string())?;
    let pid = child.id().map(|p| p as i32);
    if let Some(p) = pid {
        ctx.own_pids.lock().unwrap_or_else(|e| e.into_inner()).insert(p);
    }
    let _own = OwnPid(ctx, pid);
    let mut stdin = child.stdin.take().ok_or("no stdin")?;
    let mut lines = BufReader::new(child.stdout.take().ok_or("no stdout")?).lines();
    let req = json!({"type": "control_request", "request_id": "1", "request": {"subtype": "initialize"}});
    stdin.write_all(format!("{req}\n").as_bytes()).await.map_err(|e| e.to_string())?;
    let answer = loop {
        let line = lines.next_line().await.map_err(|e| e.to_string())?.ok_or("claude ended without an answer")?;
        let Ok(v) = serde_json::from_str::<Value>(&line) else { continue };
        if v["type"] == "control_response" && v["response"]["request_id"] == "1" {
            break v["response"]["response"].clone();
        }
    };
    drop(stdin);
    let _ = child.wait().await;
    if answer.is_object() { Ok(answer) } else { Err("no initialize response".into()) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chat::claude::tests::ctx_with_path;
    use crate::chat::tmux_io::tests::{install_stub, tempfile_path};

    #[test]
    fn the_menu_lists_what_chat_can_run_and_only_model_aliases() {
        let m = menu_from(&serde_json::from_str(include_str!("testdata/initialize.json")).unwrap());
        let names: Vec<&str> = m["commands"].as_array().unwrap().iter().map(|c| c["name"].as_str().unwrap()).collect();
        assert_eq!(names, ["clear", "compact", "superpowers:brainstorming"]);
        assert_eq!(m["commands"][1], json!({"name":"compact","description":"Free up context by summarizing the conversation so far","hint":"<optional custom summarization instructions>"}));
        assert_eq!(m["models"], json!([{"value":"default","label":"Default (recommended)","model":"claude-opus-5-5"},{"value":"opus","label":"Opus 5.5","model":"claude-opus-5-5"},{"value":"haiku","label":"Haiku 5.5","model":"claude-haiku-5-5"}]));
        assert_eq!((m["efforts"].clone(), m["agents"].clone()), (json!(["low","medium","high","xhigh","max","auto"]), json!([])));
    }

    #[test]
    fn typed_commands_are_routed_pasted_or_refused_with_a_pointer() {
        let refuse = |s: &str| Route::Refuse(s.into());
        assert_eq!(route("/model haiku"), Route::Set("model", json!("haiku")));
        assert_eq!(route("/effort  high "), Route::Set("effort", json!("high")));
        assert_eq!(route("/plan"), Route::Set("plan", json!(true)));
        assert_eq!(route("/model"), refuse("Use the model chip at the top of the chat"));
        assert_eq!(route("/config"), refuse("Use Terminal for /config"));
        assert_eq!(route("/cost"), refuse("Use the usage sheet"));
        assert_eq!(route("/rewind"), refuse("Use Rewind to here on a message"));
        assert_eq!(route("/tasks"), refuse("Use the running list"));
        assert_eq!(route("/diff"), refuse("Use Changes at the top of the chat"));
        assert_eq!(route("/btw what did I ask?"), refuse("Use Ask aside"));
        assert_eq!(route("/btw"), refuse("Use Ask aside"));
        for pasted in ["/compact", "/plan make it faster", "/config permissionMode=plan", "hello /model", "/clear"] {
            assert_eq!(route(pasted), Route::Paste, "{pasted}");
        }
    }

    /// A temp claude dir whose `bin/claude` records its calls and, when
    /// `answers`, replies to the initialize request with the fixture.
    fn fake_claude(tag: &str, answers: bool) -> (Arc<Ctx>, PathBuf) {
        let dir = tempfile_path(tag);
        std::fs::create_dir_all(&dir).unwrap();
        let init: Value = serde_json::from_str(include_str!("testdata/initialize.json")).unwrap();
        std::fs::write(dir.join("init.json"), serde_json::to_string(&init).unwrap()).unwrap();
        let reply = if answers {
            format!(
                "printf '%s\\n' '{{\"type\":\"system\",\"subtype\":\"hook_started\"}}'\nprintf '{{\"type\":\"control_response\",\"response\":{{\"subtype\":\"success\",\"request_id\":\"1\",\"response\":%s}}}}\\n' \"$(cat '{}/init.json')\"\ncat >/dev/null\n",
                dir.display()
            )
        } else {
            String::new()
        };
        let script = format!("#!/bin/sh\nprintf '%s|%s\\n' \"$(pwd)\" \"$*\" >> '{}/claude-calls'\nread line\n{reply}", dir.display());
        let bin = install_stub(&dir, "claude", &script);
        (ctx_with_path(dir.clone(), format!("{bin}:/usr/bin:/bin")), dir)
    }

    #[tokio::test]
    async fn initialize_runs_headless_with_hooks_off_in_the_session_folder_once() {
        let (ctx, dir) = fake_claude("init-ok", true);
        let cwd = dir.join("proj");
        std::fs::create_dir_all(&cwd).unwrap();
        let init = initialize(&ctx, &cwd).await.unwrap();
        assert_eq!(init["models"][1]["value"], "opus");
        initialize(&ctx, &cwd).await.unwrap();
        let calls = std::fs::read_to_string(dir.join("claude-calls")).unwrap();
        assert_eq!(calls.lines().count(), 1, "cached per folder");
        assert_eq!(calls.trim(), format!(r#"{}|-p --input-format stream-json --output-format stream-json --verbose --settings {{"disableAllHooks":true}}"#, cwd.display()));
        assert!(ctx.own_pids.lock().unwrap().is_empty(), "the pid is forgotten once the process is gone");
    }

    #[tokio::test]
    async fn a_claude_that_exits_without_replying_fails_fast() {
        let (ctx, dir) = fake_claude("init-dead", false);
        let started = Instant::now();
        assert!(initialize(&ctx, &dir).await.is_err());
        assert!(started.elapsed() < INIT_WAIT);
        assert!(ctx.own_pids.lock().unwrap().is_empty());
        assert!(ctx.host.log.lines().iter().any(|l| l.contains("chat menu failed")));
    }

    #[tokio::test]
    async fn a_stale_copy_is_returned_at_once_and_reloaded_in_the_background() {
        let (ctx, dir) = fake_claude("init-stale", true);
        cache().insert(dir.clone(), (Instant::now() - REFRESH_AFTER, json!({"commands":[],"models":[]})));
        assert_eq!(initialize(&ctx, &dir).await.unwrap(), json!({"commands":[],"models":[]}));
        for _ in 0..300 {
            if cache()[&dir].1["models"].as_array().is_some_and(|m| m.len() == 4) {
                return;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        panic!("the reload never replaced the copy");
    }
}
