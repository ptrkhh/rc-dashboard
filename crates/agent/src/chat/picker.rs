//! The Claude slash menu: the command and model lists come from one headless
//! `claude -p` `initialize` per project folder, and typed commands are routed
//! to the pickers, pasted, or refused with a pointer.

use super::tmux_io;
use crate::collect::ctx::Ctx;
use crate::collect::lookup::session_file_for;
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
pub(crate) struct OwnPid(Arc<Ctx>, Option<i32>);

impl OwnPid {
    pub(crate) fn new(ctx: &Arc<Ctx>, pid: Option<i32>) -> Self {
        if let Some(p) = pid {
            ctx.own_pids.lock().unwrap_or_else(|e| e.into_inner()).insert(p);
        }
        Self(Arc::clone(ctx), pid)
    }
}

impl Drop for OwnPid {
    fn drop(&mut self) {
        if let Some(p) = self.1 {
            self.0.own_pids.lock().unwrap_or_else(|e| e.into_inner()).remove(&p);
        }
    }
}

async fn run_initialize(ctx: &Arc<Ctx>, cwd: &Path) -> Result<Value, String> {
    let args = ["-p", "--input-format", "stream-json", "--output-format", "stream-json", "--verbose", "--settings", r#"{"disableAllHooks":true}"#];
    let mut child = ctx.runner.spawn_piped("claude", &args, cwd).map_err(|e| e.to_string())?;
    let _own = OwnPid::new(ctx, child.id().map(|p| p as i32));
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

/// Claude's confirmation title for each picker command. `/model` asks only
/// with a warm cache; `/effort` never asked on 2.1.325.
pub const CONFIRMS: [(&str, &str); 1] = [("model", "Switch model?")];

/// Footer starts and the permission mode each shows, in shift+tab order from bypass.
const FOOTERS: [(&str, &str); 5] = [
    ("⏵⏵ bypass permissions on", "bypassPermissions"),
    ("⏵⏵ auto mode on", "auto"),
    ("⏸ manual mode on", "default"),
    ("⏵⏵ accept edits on", "acceptEdits"),
    ("⏸ plan mode on", "plan"),
];
const PRESSES_MAX: usize = 5;
const SETTLE_WAIT: Duration = Duration::from_secs(2);

/// The permission mode the footer shows. Only the last 3 non-blank lines
/// count, so a transcript line quoting a footer is not read as one.
pub fn footer_mode(screen: &str) -> Option<&'static str> {
    screen.lines().rev().map(str::trim_start).filter(|l| !l.trim().is_empty()).take(3).find_map(|l| {
        FOOTERS.iter().find(|(start, _)| l.starts_with(start)).map(|(_, mode)| *mode)
    })
}

/// The pane shows the dialog titled `title` with the cursor on Yes below it.
pub fn confirm_ready(screen: &str, title: &str) -> bool {
    let lines: Vec<&str> = screen.lines().collect();
    let Some(at) = lines.iter().rposition(|l| l.contains(title)) else { return false };
    lines[at + 1..].iter().any(|l| l.trim_start().starts_with('❯') && l.contains("Yes"))
}

const WAITING: &str = "Answer the open question first";
pub(crate) const BUSY: &str = "Stop the turn or wait for it to finish";

/// Refuses unless the status file's `status` is idle (a background shell counts).
pub(crate) fn idle_gate(status: Option<&str>) -> Result<(), String> {
    match status {
        Some("idle" | "shell") => Ok(()),
        Some("waiting") => Err(WAITING.into()),
        _ => Err(BUSY.into()),
    }
}

async fn idle(ctx: &Ctx, pid: i32) -> Result<(), String> {
    idle_gate(session_file_for(&ctx.claude_dir, pid).await.and_then(|f| f.status).as_deref())
}

async fn read_footer(ctx: &Ctx, name: &str) -> Option<&'static str> {
    footer_mode(&tmux_io::capture(&ctx.runner, name).await.ok()?)
}

/// Turns plan mode on or off by pressing shift+tab until the footer shows
/// plan (on) or bypassPermissions (off), checking it after every press.
pub async fn switch_plan(ctx: &Arc<Ctx>, name: &str, pid: i32, on: bool) -> Result<(), String> {
    const UNKNOWN: &str = "Plan mode is unknown here; use Terminal";
    const STUCK: &str = "Plan mode did not switch; use Terminal";
    let target = if on { "plan" } else { "bypassPermissions" };
    idle(ctx, pid).await?;
    let start = read_footer(ctx, name).await.ok_or(UNKNOWN)?;
    let mut mode = start;
    for _ in 0..PRESSES_MAX {
        if mode == target {
            return Ok(());
        }
        idle(ctx, pid).await?;
        tmux_io::key(&ctx.runner, name, "BTab").await?;
        let deadline = Instant::now() + Duration::from_secs(1);
        let mut now = read_footer(ctx, name).await;
        while now.is_none_or(|m| m == mode) && Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(50)).await;
            now = read_footer(ctx, name).await;
        }
        mode = match now {
            None => return Err(UNKNOWN.into()),
            // Unchanged, or back at the start: a cycle without the target.
            Some(m) if m == mode || m == start => return Err(STUCK.into()),
            Some(m) => m,
        };
    }
    if mode == target { Ok(()) } else { Err(STUCK.into()) }
}
/// Text pasted but not submitted (a picker command, or image paths and a prompt from a failed send), still in the input box.
/// A picker command pasted but not submitted, still in the input box.
pub type Leftover = Mutex<Option<String>>;

/// No dialog is open: the status file says the input box takes keys.
pub async fn input_open(ctx: &Ctx, pid: i32) -> bool {
    let status = session_file_for(&ctx.claude_dir, pid).await.and_then(|f| f.status);
    matches!(status.as_deref(), Some("idle" | "shell" | "busy"))
}

/// Clears a picker command left in the input box, once no dialog is open
/// (a turn's input box takes the clear). The flag stays until it worked.
pub async fn clear_leftover(ctx: &Ctx, name: &str, pid: i32, leftover: &Leftover) {
    let Some(text) = leftover.lock().unwrap_or_else(|e| e.into_inner()).clone() else { return };
    if !input_open(ctx, pid).await {
        return;
    }
    match tmux_io::clear_input(&ctx.runner, name, &text).await {
        Ok(()) => *leftover.lock().unwrap_or_else(|e| e.into_inner()) = None,
        Err(e) => ctx.host.log.push(format!("chat clear failed {name}: {e}")),
    }
}

/// Types `/<cmd> <value>` and answers only the confirmation that command
/// raises, after reading it on screen. Ok means Claude took the value: no
/// dialog and idle after the watch, or its confirmation pressed. Text it
/// could not submit or clear is put in `leftover`.
pub async fn type_setting(ctx: &Arc<Ctx>, name: &str, pid: i32, cmd: &str, value: &str, leftover: &Leftover) -> Result<(), String> {
    let title = CONFIRMS.iter().find(|(c, _)| *c == cmd).map(|(_, t)| *t);
    let text = format!("/{cmd} {value}");
    idle(ctx, pid).await?;
    clear_leftover(ctx, name, pid, leftover).await;
    tmux_io::paste_raw(&ctx.runner, name, &text).await?;
    tokio::time::sleep(tmux_io::PASTE_PAUSE).await;
    if let Err(e) = idle(ctx, pid).await {
        // Behind a dialog the text waits; the next send or picker clears it.
        *leftover.lock().unwrap_or_else(|e| e.into_inner()) = Some(text);
        clear_leftover(ctx, name, pid, leftover).await;
        return Err(e);
    }
    tmux_io::key(&ctx.runner, name, "Enter").await?;
    let deadline = Instant::now() + SETTLE_WAIT;
    let mut status = None;
    while Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(100)).await;
        let Some(f) = session_file_for(&ctx.claude_dir, pid).await else { continue };
        status = f.status;
        if status.as_deref() != Some("waiting") {
            continue;
        }
        if f.waiting_for.as_deref() != Some("dialog open") {
            return Err(WAITING.into());
        }
        // The command's own result also reads `dialog open`, for about 0.2 s
        // (`/effort` on 2.1.325); only a dialog still open at the end is unexpected.
        if let Some(title) = title {
            if confirm_ready(&tmux_io::capture(&ctx.runner, name).await.unwrap_or_default(), title) {
                return tmux_io::key(&ctx.runner, name, "Enter").await;
            }
        }
    }
    match status.as_deref() {
        Some("idle" | "shell") => Ok(()),
        Some("waiting") => {
            let screen = tmux_io::capture(&ctx.runner, name).await.unwrap_or_default();
            ctx.host.log.push(format!("chat /{cmd} unexpected dialog {name}:\n{screen}"));
            tmux_io::escape(&ctx.runner, name).await?;
            Err("Claude opened an unexpected dialog; closed it".into())
        }
        _ => Err(format!("Claude did not confirm /{cmd}; check Terminal")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chat::claude::tests::{ctx_with_path, write_session};
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

    #[test]
    fn the_footer_parser_reads_the_five_modes() {
        for (line, mode) in [("⏵⏵ bypass permissions on (shift+tab to cycle) · ← 1 agent", "bypassPermissions"),
                             ("⏵⏵ auto mode on (shift+tab to cycle) · ← 1 agent", "auto"),
                             ("⏸ manual mode on · ← 1 agent", "default"),
                             ("⏵⏵ accept edits on (shift+tab to cycle) · ← 1 agent", "acceptEdits"),
                             ("⏸ plan mode on (shift+tab to cycle) · ← 1 agent", "plan")] {
            assert_eq!(footer_mode(&format!("● reply\n\n  {line}\n")), Some(mode), "{line}");
        }
        assert_eq!(footer_mode("⏵⏵ bypass permissions on · 1 shell · ← 1 agent"), Some("bypassPermissions"));
        assert_eq!(footer_mode("● said ⏸ plan mode on\n  ⏵⏵ auto mode on"), Some("auto"), "the bottom line wins");
        assert_eq!(footer_mode("no footer here"), None);
    }
    #[test]
    fn a_confirmation_counts_only_with_its_title_and_the_cursor_on_yes() {
        assert!(confirm_ready(" Switch model?\n ❯ 1. Yes\n   2. No\n", "Switch model?"));
        assert!(!confirm_ready(" Switch model?\n   1. Yes\n ❯ 2. No\n", "Switch model?"));
        assert!(!confirm_ready(" Background work is running\n ❯ 1. Exit and stop tasks\n", "Switch model?"));
        assert!(!confirm_ready(" ❯ 1. Yes\n Switch model?\n", "Switch model?"));
    }
    #[test]
    fn a_footer_quoted_in_the_transcript_is_not_the_footer() {
        assert_eq!(footer_mode("  ⏸ plan mode on\n● one\n● two\n\n● three\n\n"), None);
    }

    /// `<dir>/tmux-args` lines that press a key.
    fn sent_keys(dir: &Path) -> Vec<String> {
        let args = std::fs::read_to_string(dir.join("tmux-args")).unwrap_or_default();
        args.lines().filter(|l| l.starts_with("send-keys")).map(String::from).collect()
    }

    fn presses(dir: &Path) -> usize {
        sent_keys(dir).iter().filter(|l| l.ends_with(" BTab")).count()
    }

    /// An idle session in `<dir>/sessions` and a `tmux` stub running `body`
    /// after logging its arguments to `<dir>/tmux-args`; `$D` is `dir`.
    fn stub_ctx(tag: &str, body: &str) -> (Arc<Ctx>, PathBuf) {
        let dir = tempfile_path(tag);
        std::fs::create_dir_all(dir.join("sessions")).unwrap();
        write_session(&dir, r#"{"sessionId":"sid-1","cwd":"/proj","status":"idle"}"#);
        let script = format!("#!/bin/sh\nD='{}'\nprintf '%s\\n' \"$*\" >> \"$D/tmux-args\"\n{body}", dir.display());
        let bin = install_stub(&dir, "tmux", &script);
        (ctx_with_path(dir.clone(), format!("{bin}:/usr/bin:/bin")), dir)
    }

    /// A pane whose footer cycles through `<dir>/footers` one line per BTab.
    fn cycle_ctx(tag: &str) -> (Arc<Ctx>, PathBuf) {
        let body = r#"i=0; [ -f "$D/mode" ] && read i < "$D/mode"
case "$*" in
  send-keys*BTab) echo $(( (i + 1) % 5 )) > "$D/mode" ;;
  capture-pane*) printf '● reply\n\n'; sed -n "$((i + 1))p" "$D/footers" ;;
  display-message*) echo '4242||' ;;
esac
"#;
        let (ctx, dir) = stub_ctx(&format!("cycle-{tag}"), body);
        let footers = ["⏵⏵ bypass permissions on (shift+tab to cycle) · ← 1 agent", "⏵⏵ auto mode on (shift+tab to cycle)", "⏸ manual mode on",
                       "⏵⏵ accept edits on (shift+tab to cycle)", "⏸ plan mode on (shift+tab to cycle)"];
        std::fs::write(dir.join("footers"), footers.map(|f| format!("{f}\n")).concat()).unwrap();
        (ctx, dir)
    }

    /// A pane showing `screen` whose first Enter opens a dialog (`waiting`,
    /// `dialog open`; rewrite `waiting.json` for another outcome); later
    /// Enters and Escape close it. A `pasted.json` is put on the paste.
    fn confirm_ctx(tag: &str, screen: &str) -> (Arc<Ctx>, PathBuf) {
        let body = r#"put() { cp "$D/$1.json" "$D/sessions/.t" && mv -f "$D/sessions/.t" "$D/sessions/4242.json"; }
case "$*" in
  send-keys*Enter) if [ -e "$D/entered" ]; then put idle; else : > "$D/entered"; put waiting
                     if [ -e "$D/transient" ]; then (sleep 0.2; put idle) >/dev/null 2>&1 & fi; fi ;;
  send-keys*Escape) put idle ;;
  paste-buffer*) if [ -f "$D/pasted.json" ]; then put pasted; fi ;;
  capture-pane*) cat "$D/screen" ;;
esac
"#;
        let (ctx, dir) = stub_ctx(&format!("confirm-{tag}"), body);
        std::fs::write(dir.join("screen"), screen).unwrap();
        std::fs::write(dir.join("idle.json"), r#"{"sessionId":"sid-1","cwd":"/proj","status":"idle"}"#).unwrap();
        std::fs::write(dir.join("waiting.json"), r#"{"sessionId":"sid-1","cwd":"/proj","status":"waiting","waitingFor":"dialog open"}"#).unwrap();
        (ctx, dir)
    }

    #[tokio::test]
    async fn the_plan_switch_presses_shift_tab_until_the_footer_agrees_and_never_while_busy() {
        let (ctx, dir) = cycle_ctx("plan"); // status idle; the footer starts at bypass
        switch_plan(&ctx, "cdash-a", 4242, true).await.unwrap();
        assert_eq!(presses(&dir), 4, "bypass, auto, manual, accept edits, plan");
        switch_plan(&ctx, "cdash-a", 4242, true).await.unwrap();
        assert_eq!(presses(&dir), 4, "already on: no key");
        switch_plan(&ctx, "cdash-a", 4242, false).await.unwrap();
        assert_eq!(presses(&dir), 5);
        write_session(&dir, r#"{"sessionId":"sid-1","cwd":"/proj","status":"busy"}"#);
        assert_eq!(switch_plan(&ctx, "cdash-a", 4242, true).await, Err("Stop the turn or wait for it to finish".into()));
        write_session(&dir, r#"{"sessionId":"sid-1","cwd":"/proj","status":"waiting","waitingFor":"input needed"}"#);
        assert_eq!(switch_plan(&ctx, "cdash-a", 4242, true).await, Err("Answer the open question first".into()));
        write_session(&dir, r#"{"sessionId":"sid-1","cwd":"/proj","status":"shell"}"#);
        switch_plan(&ctx, "cdash-a", 4242, true).await.unwrap();
        assert_eq!(presses(&dir), 9, "shell is idle with a background shell");
        std::fs::write(dir.join("footers"), "no footer\n".repeat(5)).unwrap();
        assert!(switch_plan(&ctx, "cdash-a", 4242, false).await.is_err());
        assert_eq!(presses(&dir), 9, "an unknown footer is never pressed through");
    }
    #[tokio::test]
    async fn model_and_effort_are_typed_and_only_their_own_confirmation_is_accepted() {
        let (ctx, dir) = confirm_ctx("yes", " Switch model?\n ❯ 1. Yes\n   2. No\n");
        type_setting(&ctx, "cdash-a", 4242, "model", "haiku", &Leftover::default()).await.unwrap();
        let keys = sent_keys(&dir);
        assert_eq!(keys, ["send-keys -t =cdash-a: Enter", "send-keys -t =cdash-a: Enter"], "Enter after the paste, Enter on Yes");
        let (ctx, dir) = confirm_ctx("other", " Background work is running\n ❯ 1. Exit and stop tasks\n");
        assert!(type_setting(&ctx, "cdash-a", 4242, "model", "haiku", &Leftover::default()).await.is_err());
        assert_eq!(sent_keys(&dir), ["send-keys -t =cdash-a: Enter", "send-keys -t =cdash-a: Escape"]);
    }
    #[tokio::test]
    async fn a_cycle_without_the_target_stops_back_at_the_start() {
        let (ctx, dir) = cycle_ctx("no-plan");
        let footers = std::fs::read_to_string(dir.join("footers")).unwrap().replace("⏸ plan mode on", "⏵⏵ bypass permissions on");
        std::fs::write(dir.join("footers"), footers).unwrap();
        assert_eq!(switch_plan(&ctx, "cdash-a", 4242, true).await, Err("Plan mode did not switch; use Terminal".into()));
        assert_eq!(presses(&dir), 4, "stops when bypass shows again");
    }
    #[tokio::test]
    async fn a_footer_that_does_not_change_stops_after_one_press() {
        let (ctx, dir) = cycle_ctx("stuck");
        std::fs::write(dir.join("footers"), "⏵⏵ bypass permissions on\n".repeat(5)).unwrap();
        assert_eq!(switch_plan(&ctx, "cdash-a", 4242, true).await, Err("Plan mode did not switch; use Terminal".into()));
        assert_eq!(presses(&dir), 1);
    }
    #[tokio::test]
    async fn a_question_opened_during_the_watch_gets_no_key() {
        let (ctx, dir) = confirm_ctx("question", " Which file?\n ❯ 1. Yes\n");
        std::fs::write(dir.join("waiting.json"), r#"{"sessionId":"sid-1","cwd":"/proj","status":"waiting","waitingFor":"input needed"}"#).unwrap();
        assert_eq!(type_setting(&ctx, "cdash-a", 4242, "model", "haiku", &Leftover::default()).await, Err("Answer the open question first".into()));
        assert_eq!(sent_keys(&dir), ["send-keys -t =cdash-a: Enter"], "only the command's own Enter");
    }
    #[tokio::test]
    async fn no_dialog_and_idle_is_success_and_a_watch_ending_busy_is_not() {
        let idle = r#"{"sessionId":"sid-1","cwd":"/proj","status":"idle"}"#;
        let (ctx, dir) = confirm_ctx("no-dialog", "");
        std::fs::write(dir.join("waiting.json"), idle).unwrap();
        type_setting(&ctx, "cdash-a", 4242, "effort", "high", &Leftover::default()).await.unwrap();
        assert_eq!(sent_keys(&dir), ["send-keys -t =cdash-a: Enter"]);
        let (ctx, dir) = confirm_ctx("busy-end", "");
        std::fs::write(dir.join("waiting.json"), idle.replace("idle", "busy")).unwrap();
        assert_eq!(type_setting(&ctx, "cdash-a", 4242, "effort", "high", &Leftover::default()).await, Err("Claude did not confirm /effort; check Terminal".into()));
    }
    #[tokio::test]
    async fn a_command_whose_output_briefly_reads_as_a_dialog_succeeds_without_escape() {
        // 2.1.325: `/effort medium` read `waiting`/`dialog open` for ~175 ms while its result was drawn, then `idle`.
        for cmd in ["effort", "model"] {
            let (ctx, dir) = confirm_ctx(&format!("transient-{cmd}"), "❯ /effort medium\n  ⎿  Set effort level to medium (saved as your default for new sessions)\n");
            std::fs::write(dir.join("transient"), "").unwrap();
            type_setting(&ctx, "cdash-a", 4242, cmd, "medium", &Leftover::default()).await.unwrap();
            assert_eq!(sent_keys(&dir), ["send-keys -t =cdash-a: Enter"], "{cmd}: no Escape");
        }
    }
    #[test]
    fn only_model_asks_for_confirmation() {
        // `/effort` applied with no dialog three times on 2.1.325.
        assert_eq!(CONFIRMS.iter().map(|(c, _)| *c).collect::<Vec<_>>(), ["model"]);
    }
    #[tokio::test]
    async fn a_turn_starting_during_the_paste_gets_no_enter_and_the_box_is_cleared() {
        let (ctx, dir) = confirm_ctx("late-busy", "");
        std::fs::write(dir.join("pasted.json"), r#"{"sessionId":"sid-1","cwd":"/proj","status":"busy"}"#).unwrap();
        assert_eq!(type_setting(&ctx, "cdash-a", 4242, "model", "haiku", &Leftover::default()).await, Err("Stop the turn or wait for it to finish".into()));
        let keys = sent_keys(&dir);
        assert_eq!(keys.len(), 1);
        assert!(keys[0].starts_with("send-keys -t =cdash-a: C-u"), "{keys:?}");
    }
}
