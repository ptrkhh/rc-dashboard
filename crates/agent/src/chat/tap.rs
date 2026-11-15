//! Claude Code transcript lines to chat events. Pure: no I/O, never panics.
//!
//! The record shapes are internal to Claude Code (checked on 2.1.322, see
//! `CLAUDE_FINDINGS.md`). Anything not understood produces nothing.

use super::media::{split_attached, MAX_FILES};
use super::CAP;
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::HashMap;

/// Only the fields the rules read; serde skips the rest, so a multi-megabyte
/// `api-request-blob` costs a scan, not an allocation.
#[derive(Deserialize)]
struct Rec {
    #[serde(rename = "type", default)]
    kind: String,
    subtype: Option<String>,
    uuid: Option<String>,
    #[serde(rename = "parentUuid")]
    parent: Option<String>,
    /// A compact boundary's `null` parent hides the record it follows; this names it.
    #[serde(rename = "logicalParentUuid")]
    logical_parent: Option<String>,
    #[serde(rename = "isSidechain", default)]
    sidechain: Option<bool>,
    #[serde(rename = "isMeta", default)]
    meta: Option<bool>,
    /// The summary written after compaction: context for the model, not a prompt.
    #[serde(rename = "isCompactSummary", default)]
    compact: Option<bool>,
    #[serde(rename = "isVisibleInTranscriptOnly", default)]
    transcript_only: Option<bool>,
    #[serde(rename = "promptSource")]
    source: Option<String>,
    operation: Option<String>,
    content: Option<Value>,
    attachment: Option<Value>,
    message: Option<Value>,
    #[serde(rename = "durationMs")]
    duration: Option<Value>,
    #[serde(rename = "isApiErrorMessage", default)]
    api_error: Option<bool>,
    timestamp: Option<String>,
    cwd: Option<String>,
    #[serde(rename = "toolUseResult")]
    tool_use_result: Option<Value>,
}

/// A main-chain record: its parent, and the uuid of the last event emitted at
/// or before it, which is where a branch from it cuts the log back to.
struct Node {
    parent: Option<String>,
    anchor: Option<String>,
    /// A compact boundary: a Claude resumed since loaded nothing before it.
    boundary: bool,
}

/// A prompt Claude's rewind selector lists (Claude Code 2.1.325): typed and
/// queued prompts, image prompts, `!` commands, messages absorbed mid-turn and
/// slash commands recorded as a prompt (`/compact`), but not task-notification
/// turns or commands recorded as `system` (`/rename`, `/context`).
#[derive(Clone, Debug, PartialEq)]
pub struct Listed {
    pub uuid: String,
    /// What chat shows; for a `!` command, what the selector shows.
    pub text: String,
    /// As recorded, with paste and image tokens.
    pub raw: String,
    pub pasted: bool,
    pub images: bool,
    /// The first row after a compaction: a Claude resumed since lists nothing before it.
    pub after_compact: bool,
}

#[derive(Default)]
pub struct Tap {
    open: bool,
    seen: usize,
    understood: bool,
    /// The last main-chain assistant record was an API error message.
    api_error: bool,
    // ponytail: one map entry per record for the conversation's life (~150 bytes each); forget entries before the last compaction if memory ever shows it.
    nodes: HashMap<String, Node>,
    last_main: Option<String>,
    anchor: Option<String>,
    /// The listed prompts by uuid, on any branch.
    prompts: HashMap<String, Listed>,
    /// Tool calls whose results tell us something: id to (name, input).
    calls: HashMap<String, (String, Value)>,
    todo: Vec<Item>,
    tasks: Vec<Value>,
    cwd: Option<String>,
    model: Option<String>,
    /// input, output, cache_read, cache_write; the message id last counted.
    tokens: Option<[u64; 4]>,
    counted: Option<String>,
    used: Option<u64>,
}

struct Item {
    id: Option<String>,
    text: String,
    status: String,
}

// Transcript content is untrusted: each list a session keeps is bounded.
const MAX_CALLS: usize = 64;
const MAX_TASKS: usize = 50;
const MAX_TODOS: usize = 100;
const STATUSES: [&str; 3] = ["pending", "in_progress", "completed"];

// ponytail: one entry per model family, each read from a status-line sample; unknown models show tokens only.
const WINDOWS: &[(&str, u64)] = &[("claude-opus-5-5", 1_000_000), ("claude-sonnet-5-5", 1_000_000), ("claude-fable-5-2", 1_000_000)];

pub fn context_window(model: &str) -> Option<u64> {
    WINDOWS.iter().find(|(p, _)| model.strip_prefix(p).is_some_and(|rest| rest.is_empty() || rest.starts_with('-'))).map(|&(_, w)| w)
}

fn clip(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
}

/// A task id arrives as a string or a number.
fn id_of(v: &Value) -> Option<String> {
    v.as_str().map(|s| clip(s, 80)).or_else(|| v.as_u64().map(|n| n.to_string()))
}

impl Tap {
    pub fn usage(&self) -> Value {
        let model = self.model.as_deref();
        let context = self.used.map(|used| json!({"used":used,"window":model.and_then(context_window)}));
        let tokens = self.tokens.map(|[input, output, cache_read, cache_write]| json!({"input":input,"output":output,"cache_read":cache_read,"cache_write":cache_write}));
        json!({"cwd":self.cwd,"model":model,"context":context,"tokens":tokens,"cost":null})
    }

    pub fn tasks(&self) -> Vec<Value> {
        self.tasks.clone()
    }

    fn todo_event(&self) -> Value {
        let items: Vec<Value> = self.todo.iter().map(|i| json!({"text":i.text,"status":i.status})).collect();
        json!({"type":"cdash","subtype":"todo","items":items})
    }

    fn start_task(&mut self, id: String, kind: &str, title: &str, started: Option<&String>) {
        if self.tasks.iter().any(|t| t["id"] == id.as_str()) {
            return;
        }
        if self.tasks.len() >= MAX_TASKS {
            self.tasks.remove(0);
        }
        self.tasks.push(json!({"id":id,"kind":kind,"title":clip(title, 80),"started":started}));
    }

    fn end_task(&mut self, id: &str) {
        self.tasks.retain(|t| t["id"] != id);
    }

    /// `<task-notification>` text, wherever it was recorded: a finished task ends.
    fn notified(&mut self, text: &str) {
        for chunk in text.split("<task-notification>").skip(1) {
            let done = between(chunk, "<status>", "</status>").is_some_and(|s| ["completed", "failed", "killed", "stopped"].contains(&s.trim()));
            if let (true, Some(id)) = (done, between(chunk, "<task-id>", "</task-id>")) {
                self.end_task(id.trim());
            }
        }
    }

    /// What a record tells us beyond its events: usage, the checklist, running tasks.
    fn facts(&mut self, r: &Rec) -> Vec<Value> {
        let mut out = Vec::new();
        match r.kind.as_str() {
            "assistant" => self.assistant_facts(r, &mut out),
            "user" => match r.message.as_ref().map(|m| &m["content"]) {
                Some(Value::Array(blocks)) => blocks.iter().filter(|b| b["type"] == "tool_result").for_each(|b| self.result_facts(r, b, &mut out)),
                Some(Value::String(text)) => self.notified(text),
                _ => {}
            },
            "queue-operation" if r.operation.as_deref() == Some("enqueue") => {
                if let Some(t) = r.content.as_ref().and_then(Value::as_str) {
                    self.notified(t);
                }
            }
            "attachment" => {
                let a = r.attachment.as_ref().unwrap_or(&Value::Null);
                match a["type"].as_str() {
                    Some("queued_command") => self.notified(a["prompt"].as_str().unwrap_or("")),
                    Some("task_status") if a["status"] == "running" => {
                        if let Some(id) = id_of(&a["taskId"]) {
                            let kind = if a["taskType"] == "local_bash" { "shell" } else { "agent" };
                            let title = a["description"].as_str().or(a["shell"]["command"].as_str()).unwrap_or("");
                            self.start_task(id, kind, title, r.timestamp.as_ref());
                        }
                    }
                    _ => {}
                }
            }
            _ => {}
        }
        out
    }

    fn assistant_facts(&mut self, r: &Rec, out: &mut Vec<Value>) {
        let Some(m) = &r.message else { return };
        if m["model"] != "<synthetic>" && m["usage"].is_object() {
            let n = |k: &str| m["usage"][k].as_u64().unwrap_or(0);
            let (input, output, read, write) = (n("input_tokens"), n("output_tokens"), n("cache_read_input_tokens"), n("cache_creation_input_tokens"));
            // A reply split over records repeats its usage: count it once.
            // Non-adjacent repeats do not occur (0 in 24,064 records checked); adjacent dedupe suffices.
            let id = m["id"].as_str();
            if id.is_none() || id != self.counted.as_deref() {
                self.counted = id.map(String::from);
                let t = self.tokens.get_or_insert([0; 4]);
                for (slot, v) in t.iter_mut().zip([input, output, read, write]) {
                    *slot = slot.saturating_add(v);
                }
            }
            self.used = Some(input.saturating_add(read).saturating_add(write));
            self.model = m["model"].as_str().map(|s| clip(s, 80));
        }
        for b in m["content"].as_array().into_iter().flatten().filter(|b| b["type"] == "tool_use") {
            let (Some(id), Some(name)) = (b["id"].as_str(), b["name"].as_str()) else { continue };
            if name == "TodoWrite" {
                self.todo = b["input"]["todos"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .take(MAX_TODOS)
                    .map(|t| Item {
                        id: None,
                        text: clip(t["content"].as_str().unwrap_or(""), 200),
                        status: t["status"].as_str().filter(|s| STATUSES.contains(s)).unwrap_or("pending").to_string(),
                    })
                    .collect();
                out.push(self.todo_event());
            } else if ["Bash", "Agent", "TaskCreate", "TaskUpdate", "TaskStop"].contains(&name) {
                if self.calls.len() >= MAX_CALLS {
                    self.calls.clear(); // results that never came
                }
                self.calls.insert(id.to_string(), (name.to_string(), b["input"].clone()));
            }
        }
    }

    fn result_facts(&mut self, r: &Rec, b: &Value, out: &mut Vec<Value>) {
        let Some((name, input)) = b["tool_use_id"].as_str().and_then(|id| self.calls.remove(id)) else { return };
        let res = r.tool_use_result.as_ref().unwrap_or(&Value::Null);
        let ok = b["is_error"].as_bool() != Some(true);
        match name.as_str() {
            "Bash" => {
                if let Some(id) = id_of(&res["backgroundTaskId"]) {
                    let title = input["description"].as_str().or(input["command"].as_str()).unwrap_or("");
                    self.start_task(id, "shell", title, r.timestamp.as_ref());
                }
            }
            "Agent" if res["isAsync"] == true => {
                if let Some(id) = id_of(&res["agentId"]) {
                    self.start_task(id, "agent", input["description"].as_str().unwrap_or(""), r.timestamp.as_ref());
                }
            }
            "TaskCreate" if ok => {
                if let (Some(id), true) = (id_of(&res["task"]["id"]), self.todo.len() < MAX_TODOS) {
                    let text = clip(input["subject"].as_str().unwrap_or(""), 200);
                    self.todo.push(Item { id: Some(id), text, status: "pending".into() });
                    out.push(self.todo_event());
                }
            }
            "TaskUpdate" if ok => {
                let Some(id) = id_of(&input["taskId"]) else { return };
                let Some(at) = self.todo.iter().position(|i| i.id.as_deref() == Some(&id)) else { return };
                if input["status"] == "deleted" {
                    self.todo.remove(at);
                } else {
                    let i = &mut self.todo[at];
                    if let Some(s) = input["status"].as_str().filter(|s| STATUSES.contains(s)) {
                        i.status = s.to_string();
                    }
                    if let Some(subject) = input["subject"].as_str() {
                        i.text = clip(subject, 200);
                    }
                }
                out.push(self.todo_event());
            }
            "TaskStop" => {
                if let Some(id) = id_of(&res["task_id"]) {
                    self.end_task(&id);
                }
            }
            _ => {}
        }
    }

    /// The current branch's listed prompts, oldest first: the parents from the
    /// last main-chain record, across compactions (the selector of the Claude
    /// that compacted lists the prompts before them), up to an unknown or `null` parent.
    pub fn listed(&self) -> Vec<Listed> {
        let chain = std::iter::successors(self.last_main.as_deref(), |u| self.nodes.get(*u)?.parent.as_deref());
        let mut out: Vec<Listed> = Vec::new();
        // A parent cycle in a damaged transcript ends at the first record seen twice.
        let mut seen = std::collections::HashSet::new();
        for u in chain.take_while(|u| seen.insert(*u)) {
            out.extend(self.prompts.get(u).cloned());
            // Walking back, the row pushed last is the first after the boundary.
            if let Some(p) = out.last_mut().filter(|_| self.nodes.get(u).is_some_and(|n| n.boundary)) {
                p.after_compact = true;
            }
        }
        out.reverse();
        out
    }

    fn list(&mut self, r: &Rec, text: String, raw: &str, images: bool) {
        if let Some(uuid) = &r.uuid {
            let pasted = raw.contains("<pasted_content");
            self.prompts.insert(uuid.clone(), Listed { uuid: uuid.clone(), text, raw: raw.to_string(), pasted, images, after_compact: false });
        }
    }

    /// True from a prompt until the next `result` event.
    pub fn turn_open(&self) -> bool {
        self.open
    }

    /// Five `user`/`assistant` records read and none produced an event: the
    /// format changed under us, and showing nothing is better than guessing.
    pub fn blind(&self) -> bool {
        self.seen >= 5 && !self.understood
    }

    pub fn feed(&mut self, line: &str) -> Vec<Value> {
        let Ok(r) = serde_json::from_str::<Rec>(line) else { return Vec::new() };
        if r.sidechain == Some(true) {
            return Vec::new();
        }
        // On the main chain but never shown: bookkeeping only.
        let hidden = [r.meta, r.compact, r.transcript_only].contains(&Some(true));
        if r.cwd.is_some() {
            self.cwd.clone_from(&r.cwd);
        }
        let facts = if hidden { Vec::new() } else { self.facts(&r) };
        let mut out = match r.kind.as_str() {
            _ if hidden => Vec::new(),
            "queue-operation" => match (r.operation.as_deref(), r.content.as_ref().and_then(parts)) {
                (Some("enqueue"), Some((text, _))) if !injected(&text) => vec![json!({"type":"cdash","subtype":"queued","text":clean(&text).0})],
                _ => Vec::new(),
            },
            "attachment" => self.steer(&r).into_iter().collect(),
            "system" if r.subtype.as_deref() == Some("turn_duration") => {
                let subtype = if std::mem::take(&mut self.api_error) { "error_during_execution" } else { "success" };
                let mut ev = json!({"type":"result","subtype":subtype});
                if let Some(d) = r.duration.filter(Value::is_number) {
                    ev["duration_ms"] = d;
                }
                vec![ev]
            }
            "assistant" => {
                self.api_error = r.api_error == Some(true);
                assistant(&r)
            }
            "user" => match r.message.as_ref().map(|m| &m["content"]) {
                Some(Value::Array(blocks)) if is_tool_record(blocks) => tool_results(&r, blocks),
                Some(c @ Value::Array(_)) => match parts(c) {
                    Some((text, images)) if !(text.is_empty() && images.is_empty()) => self.typed(&r, &text, images),
                    _ => Vec::new(),
                },
                Some(Value::String(text)) => self.typed(&r, text, Vec::new()),
                _ => Vec::new(),
            },
            _ => Vec::new(),
        };
        out.extend(facts);
        if !hidden && (r.kind == "user" || r.kind == "assistant") {
            self.seen += 1;
            self.understood |= !out.is_empty();
        }
        for ev in out.iter_mut().filter(|e| !is_cut(e)) {
            if !ev["uuid"].is_string() {
                if let Some(u) = &r.uuid {
                    ev["uuid"] = json!(u);
                }
            }
            if let Some(t) = &r.timestamp {
                ev["ts"] = json!(t);
            }
        }
        if let Some(u) = out.iter().rev().filter(|e| !is_cut(e)).find_map(|e| e["uuid"].as_str()) {
            self.anchor = Some(u.to_string());
        }
        if let Some(u) = &r.uuid {
            let parent = r.parent.clone().or_else(|| r.logical_parent.clone());
            let boundary = r.subtype.as_deref() == Some("compact_boundary");
            self.nodes.insert(u.clone(), Node { parent, anchor: self.anchor.clone(), boundary });
            self.last_main = Some(u.clone());
        }
        for ev in &out {
            match ev["type"].as_str() {
                Some("result") => self.open = false,
                Some("user") if ev["message"]["content"].is_string() => self.open = true,
                _ => {}
            }
        }
        out
    }

    /// Rule 7: a `user` record with string content, or Part 3's image prompt.
    fn typed(&mut self, r: &Rec, raw: &str, mut files: Vec<Value>) -> Vec<Value> {
        let text = &strip_image_tokens(raw);
        let t = text.trim_start();
        if let Some(cmd) = between(t, "<bash-input>", "</bash-input>") {
            self.list(r, format!("! {}", cmd.trim()), raw, false);
            return vec![json!({"type":"cdash","subtype":"shell","uuid":r.uuid,"command":cmd.trim()})];
        }
        if let Some(stdout) = between(t, "<bash-stdout>", "</bash-stdout>") {
            let stderr = between(t, "<bash-stderr>", "</bash-stderr>").unwrap_or("");
            return vec![json!({"type":"cdash","subtype":"shell_output","uuid":r.uuid,"stdout":stdout,"stderr":stderr})];
        }
        if r.source.as_deref() == Some("system") || injected(t) {
            return Vec::new();
        }
        let slash = between(t, "<command-name>", "</command-name>");
        let prompt = match slash {
            Some(name) => {
                let args = between(t, "<command-args>", "</command-args>").unwrap_or("");
                format!("{} {}", name.trim(), args.trim()).trim().to_string()
            }
            None => unwrap_pastes(text),
        };
        let (prompt, attached) = split_attached(&prompt);
        let images = !files.is_empty();
        files.extend(attached);
        self.api_error = false;
        let src = if r.source.as_deref() == Some("queued") { "queued" } else { "typed" };
        let mut out = Vec::new();
        // A prompt that does not continue from the record before it branches
        // from an older one (a rewind, or a prompt handed back unanswered).
        if let Some(node) = r.parent.as_ref().filter(|p| self.last_main.as_ref() != Some(*p)).and_then(|p| self.nodes.get(p)) {
            out.push(json!({"type":"cdash","subtype":"cut","after":node.anchor}));
        }
        let mut ev = prompt_event(r.uuid.as_deref(), prompt.trim(), src, files);
        if raw != ev["message"]["content"] {
            ev["raw"] = json!(raw);
        }
        self.list(r, prompt.trim().to_string(), raw, images);
        out.push(ev);
        out
    }

    /// Rule 3: a message Claude absorbed mid-turn has no `user` record, only this.
    fn steer(&mut self, r: &Rec) -> Option<Value> {
        let a = r.attachment.as_ref()?;
        if a["type"] != "queued_command" || a["commandMode"] != "prompt" {
            return None;
        }
        let (raw, mut files) = parts(&a["prompt"])?;
        let images = !files.is_empty();
        let (text, attached) = clean(&raw);
        files.extend(attached);
        if text.is_empty() && files.is_empty() {
            return None;
        }
        self.list(r, text.clone(), &raw, images);
        Some(prompt_event(r.uuid.as_deref(), &text, "steer", files))
    }
}

/// Claude Code records a long bracketed paste, which is how chat sends text, as
/// `<pasted_content id="N">\n…\n</pasted_content id="N">`. Replace each complete
/// block with its contents so the prompt reads, and matches, as what was sent.
/// Anything not understood is left as written.
fn unwrap_pastes(text: &str) -> String {
    const OPEN: &str = "<pasted_content id=\"";
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    let mut changed = false;
    while let Some(start) = rest.find(OPEN) {
        let after = &rest[start + OPEN.len()..];
        let Some(id_end) = after.find("\">") else { break };
        let close = format!("</pasted_content id=\"{}\">", &after[..id_end]);
        let body = &after[id_end + 2..];
        let Some(body_end) = body.find(&close) else { break };
        let inner = &body[..body_end];
        let inner = inner.strip_prefix('\n').unwrap_or(inner);
        out.push_str(&rest[..start]);
        out.push_str(inner.strip_suffix('\n').unwrap_or(inner));
        rest = &body[body_end + close.len()..];
        changed = true;
    }
    out.push_str(rest);
    if changed { out.trim().to_string() } else { out }
}

pub(crate) fn is_cut(ev: &Value) -> bool {
    ev["type"] == "cdash" && ev["subtype"] == "cut"
}

/// Text tools inject for the model's benefit; never shown as a user message.
fn injected(text: &str) -> bool {
    let t = text.trim_start();
    ["<task-notification>", "<system-reminder>", "<local-command-"].iter().any(|p| t.starts_with(p))
}

fn prompt_event(uuid: Option<&str>, text: &str, src: &str, mut files: Vec<Value>) -> Value {
    files.truncate(MAX_FILES);
    let mut ev = json!({"type":"user","uuid":uuid,"message":{"role":"user","content":text},"src":src});
    if !files.is_empty() {
        ev["files"] = Value::Array(files);
    }
    ev
}

/// A string is its own text; a block list gives its text blocks joined and its
/// base64 image blocks as `{kind:"image",mime,data}`. Anything else is `None`.
fn parts(content: &Value) -> Option<(String, Vec<Value>)> {
    let blocks = match content {
        Value::String(s) => return Some((s.clone(), Vec::new())),
        Value::Array(b) if !b.iter().any(|b| b["type"] == "tool_result") => b,
        _ => return None,
    };
    let text = blocks.iter().filter(|b| b["type"] == "text").filter_map(|b| b["text"].as_str()).collect();
    let images = blocks
        .iter()
        .filter(|b| b["type"] == "image" && b["source"]["type"] == "base64")
        .filter_map(|b| Some(json!({"kind":"image","mime":b["source"]["media_type"].as_str()?,"data":b["source"]["data"].as_str()?})))
        .collect();
    Some((text, images))
}

/// Rule 6 owns a list with a `tool_result` or the interrupt marker.
fn is_tool_record(blocks: &[Value]) -> bool {
    blocks.iter().any(|b| b["type"] == "tool_result" || b["text"].as_str().is_some_and(|t| t.starts_with("[Request interrupted by user")))
}

/// Remove each `[Image #<digits>]` and one space right after it.
fn strip_image_tokens(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(i) = rest.find("[Image #") {
        let after = &rest[i + 8..];
        let digits = after.bytes().take_while(u8::is_ascii_digit).count();
        out.push_str(&rest[..i]);
        if digits > 0 && after[digits..].starts_with(']') {
            rest = after[digits + 1..].strip_prefix(' ').unwrap_or(&after[digits + 1..]);
        } else {
            out.push_str("[Image #");
            rest = after;
        }
    }
    out + rest
}

/// Image tokens, pastes and upload lines out; what is left is what was typed.
fn clean(text: &str) -> (String, Vec<Value>) {
    let (t, files) = split_attached(&unwrap_pastes(&strip_image_tokens(text)));
    (t.trim().to_string(), files)
}


/// Rule 5: one event per `text`, `tool_use` or readable `thinking` block.
fn assistant(r: &Rec) -> Vec<Value> {
    let Some(m) = &r.message else { return Vec::new() };
    let Some(blocks) = m["content"].as_array() else { return Vec::new() };
    blocks
        .iter()
        .filter_map(|b| {
            let block = match b["type"].as_str()? {
                "text" => json!({"type":"text","text":b["text"].as_str()?}),
                "thinking" => json!({"type":"thinking","thinking":Some(b["thinking"].as_str()?).filter(|t| !t.is_empty())?}),
                "tool_use" => {
                    let input = &b["input"];
                    let input = if input.to_string().len() > CAP { json!({"truncated":true}) } else { input.clone() };
                    json!({"type":"tool_use","id":b["id"],"name":b["name"],"input":input})
                }
                _ => return None,
            };
            Some(json!({"type":"assistant","uuid":r.uuid,"message":{"id":m["id"],"role":"assistant","content":[block]}}))
        })
        .collect()
}

/// Rule 6: tool results, and the marker Claude writes when a turn is stopped.
fn tool_results(r: &Rec, blocks: &[Value]) -> Vec<Value> {
    blocks
        .iter()
        .filter_map(|b| match b["type"].as_str()? {
            "tool_result" => Some(json!({"type":"user","uuid":r.uuid,"message":{"role":"user","content":[{
                "type":"tool_result",
                "tool_use_id":b["tool_use_id"],
                "content":cap(&flatten(&b["content"])),
                "is_error":b["is_error"].as_bool().unwrap_or(false),
            }]}})),
            "text" if b["text"].as_str()?.starts_with("[Request interrupted by user") => {
                Some(json!({"type":"result","subtype":"interrupted"}))
            }
            _ => None,
        })
        .collect()
}

fn flatten(content: &Value) -> String {
    match content {
        Value::String(s) => s.clone(),
        Value::Array(parts) => parts.iter().filter_map(|p| p["text"].as_str()).collect::<Vec<_>>().join("\n"),
        _ => String::new(),
    }
}

fn cap(s: &str) -> String {
    match s.char_indices().nth(CAP) {
        Some((i, _)) => s[..i].to_string(),
        None => s.to_string(),
    }
}

fn between<'a>(s: &'a str, open: &str, close: &str) -> Option<&'a str> {
    let start = s.find(open)? + open.len();
    let end = s[start..].find(close)? + start;
    Some(&s[start..end])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_turn_that_ended_on_an_api_error_ends_with_an_error_result() {
        let mut tap = Tap::default();
        let end = r#"{"type":"system","subtype":"turn_duration","durationMs":5,"uuid":"s9"}"#;
        tap.feed(r#"{"type":"user","uuid":"u9","promptSource":"typed","message":{"role":"user","content":"go"}}"#);
        tap.feed(r#"{"type":"assistant","uuid":"k9","isApiErrorMessage":true,"message":{"id":"m9","role":"assistant","content":[{"type":"text","text":"API Error: Rate limit reached"}]}}"#);
        assert_eq!(tap.feed(end)[0]["subtype"], "error_during_execution");
        assert_eq!(tap.feed(end)[0]["subtype"], "success", "reported once");
        tap.feed(r#"{"type":"assistant","uuid":"k9","isApiErrorMessage":true,"message":{"id":"m9","role":"assistant","content":[{"type":"text","text":"API Error"}]}}"#);
        tap.feed(r#"{"type":"assistant","uuid":"k10","message":{"id":"m10","role":"assistant","content":[{"type":"text","text":"ok"}]}}"#);
        assert_eq!(tap.feed(end)[0]["subtype"], "success");
    }

    fn run() -> (Vec<serde_json::Value>, Tap) {
        let mut tap = Tap::default();
        let evs = include_str!("testdata/claude.jsonl").lines().flat_map(|l| tap.feed(l)).collect();
        (evs, tap)
    }
    fn tag(e: &serde_json::Value) -> String {
        let c = &e["message"]["content"];
        match (e["type"].as_str().unwrap(), e["subtype"].as_str()) {
            ("user", _) if c.is_string() => format!("prompt:{}:{}", e["src"].as_str().unwrap(), c.as_str().unwrap()),
            ("user", _) => format!("tool_result:{}", c[0]["tool_use_id"].as_str().unwrap()),
            ("assistant", _) => format!("assistant:{}", c[0]["type"].as_str().unwrap()),
            (t, Some(s)) => format!("{t}:{s}"),
            (t, None) => t.to_string(),
        }
    }
    #[test]
    fn the_fixture_becomes_exactly_these_events_in_this_order() {
        let (evs, tap) = run();
        let tags: Vec<String> = evs.iter().map(tag).collect();
        assert_eq!(tags, [
            "prompt:typed:run the tests", "assistant:tool_use", "tool_result:toolu_1", "assistant:text", "result:success",
            "cdash:shell", "cdash:shell_output",
            "prompt:typed:slow job", "assistant:tool_use", "cdash:queued", "prompt:queued:also update README",
            "tool_result:toolu_2", "result:interrupted", "assistant:text", "result:success",
            "prompt:typed:handed back", "cdash:cut", "prompt:typed:second try",
            "assistant:tool_use", "cdash:queued", "tool_result:toolu_3", "prompt:steer:use -j4",
            "prompt:typed:What color is this image?",
        ]);
        assert!(tap.turn_open(), "the steered message has not been answered yet");
    }
    #[test]
    fn details_chat_depends_on() {
        let (evs, _) = run();
        assert_eq!(evs[2]["message"]["content"][0]["content"], "12 passed", "block lists are flattened to text");
        assert_eq!(evs[4]["duration_ms"], 7773);
        assert_eq!(evs[5]["command"], "echo hi");
        assert_eq!((evs[6]["stdout"].as_str(), evs[6]["stderr"].as_str()), (Some("hi"), Some("")));
        assert_eq!(evs[9]["text"], "also update README");
        assert_eq!(evs[16]["after"], "s3", "a handed-back prompt is cut back to its turn's end");
        assert_eq!((evs[4]["uuid"].as_str(), evs[12]["uuid"].as_str()), (Some("s2"), Some("i1")), "results carry their record's uuid");
        assert_eq!(evs[21]["uuid"], "q1", "an absorbed message takes the attachment's id");
        assert_eq!(evs[1]["message"]["id"], "msg_1");
        assert_eq!(evs.last().unwrap()["files"], json!([{"kind":"image","mime":"image/png","data":"iVBORw0KGgo="}]));
    }
    #[test]
    fn oversized_and_unknown_lines_are_skipped_without_losing_what_follows() {
        let mut tap = Tap::default();
        let blob = format!(r#"{{"type":"api-request-blob","body":"{}"}}"#, "x".repeat(3 * 1024 * 1024));
        assert!(tap.feed(&blob).is_empty());
        assert!(tap.feed(r#"{"type":"brand-new-record","uuid":"z"}"#).is_empty());
        let big = format!(r#"{{"type":"user","uuid":"r","message":{{"role":"user","content":[{{"type":"tool_result","tool_use_id":"t","content":"{}"}}]}}}}"#, "y".repeat(70_000));
        let ev = &tap.feed(&big)[0];
        assert_eq!(ev["message"]["content"][0]["content"].as_str().unwrap().chars().count(), crate::chat::CAP);
    }
    #[test]
    fn a_transcript_whose_shape_changed_is_reported_instead_of_shown_wrong() {
        let mut tap = Tap::default();
        for i in 0..5 { assert!(tap.feed(&format!(r#"{{"type":"assistant","uuid":"k{i}","payload":{{"parts":["new shape"]}}}}"#)).is_empty()); }
        assert!(tap.blind());
        let (_, healthy) = run();
        assert!(!healthy.blind());
    }
    #[test]
    fn a_prompt_recorded_as_pasted_content_reads_as_the_text_that_was_pasted() {
        // Claude Code 2.1.323 wraps a long bracketed paste, which is how chat sends text.
        let mut tap = Tap::default();
        let pasted = r#"{"type":"user","uuid":"p1","parentUuid":"x","promptSource":"typed","message":{"role":"user","content":"\n\n<pasted_content id=\"7722\">\nfirst line\nsecond line\n</pasted_content id=\"7722\">\n"}}"#;
        assert_eq!(tap.feed(pasted)[0]["message"]["content"], "first line\nsecond line");
        let mixed = r#"{"type":"user","uuid":"p2","parentUuid":"y","promptSource":"typed","message":{"role":"user","content":"look at this:\n\n<pasted_content id=\"8\">\nerror: boom\n</pasted_content id=\"8\">\n"}}"#;
        assert_eq!(tap.feed(mixed)[0]["message"]["content"], "look at this:\n\nerror: boom", "typed text around a paste stays");
        let unclosed = r#"{"type":"user","uuid":"p3","parentUuid":"z","promptSource":"typed","message":{"role":"user","content":"<pasted_content id=\"9\">\nhalf"}}"#;
        assert_eq!(tap.feed(unclosed)[0]["message"]["content"], "<pasted_content id=\"9\">\nhalf", "anything not understood is left as written");
    }

    const IMG: &str = r#"{"type":"image","source":{"type":"base64","media_type":"image/png","data":"iVBORw0KGgo="}}"#;
    fn rec(uuid: &str, content: Value) -> String {
        json!({"type":"user","uuid":uuid,"parentUuid":format!("p-{uuid}"),"promptSource":"typed","message":{"role":"user","content":content}}).to_string()
    }
    #[test]
    fn an_image_prompt_becomes_its_text_and_its_image_blocks_in_either_order() {
        let img: Value = serde_json::from_str(IMG).unwrap();
        let mut tap = Tap::default();
        let ev = &tap.feed(&rec("i1", json!([img, {"type":"text","text":"[Image #1] What color?"}])))[0];
        assert_eq!((ev["message"]["content"].as_str(), ev["src"].as_str()), (Some("What color?"), Some("typed")));
        assert_eq!(ev["files"], json!([{"kind":"image","mime":"image/png","data":"iVBORw0KGgo="}]));
        let two = &tap.feed(&rec("i2", json!([{"type":"text","text":"[Image #2] [Image #3] both"}, img, img])))[0];
        assert_eq!((two["message"]["content"].as_str(), two["files"].as_array().map(Vec::len)), (Some("both"), Some(2)));
        assert_eq!(tap.feed(&rec("i3", json!([{"type":"text","text":"[Image #4] "}, img])))[0]["message"]["content"], "", "an image alone is a prompt");
        assert_eq!(tap.feed(&rec("i4", json!([{"type":"text","text":"[Request interrupted by user]"}])))[0]["subtype"], "interrupted");
        assert!(tap.feed(&rec("i5", json!([{"type":"image","source":{"type":"url","url":"https://x"}}]))).is_empty(), "nothing usable, nothing shown");
    }
    #[test]
    fn queued_absorbed_and_typed_prompts_lose_their_upload_lines_and_image_tokens() {
        let pdf = "/home/u/.cache/cdash/uploads/cdash-a/0123456789abcdef/r.pdf";
        let img: Value = serde_json::from_str(IMG).unwrap();
        let mut tap = Tap::default();
        let typed = &tap.feed(&rec("t1", json!(format!("read this\nAttached: {pdf}"))))[0];
        assert_eq!((typed["message"]["content"].as_str(), &typed["files"]), (Some("read this"), &json!([{"kind":"file","name":"r.pdf"}])));
        let queued = &tap.feed(&json!({"type":"queue-operation","operation":"enqueue","content":format!("[Image #5] later\nAttached: {pdf}")}).to_string())[0];
        assert_eq!(queued["text"], "later", "the hub matches queued text against what chat sent");
        let steer = &tap.feed(&json!({"type":"attachment","uuid":"q9","attachment":{"type":"queued_command","commandMode":"prompt",
            "prompt":[{"type":"text","text":format!("[Image #5] later\nAttached: {pdf}")}, img]}}).to_string())[0];
        assert_eq!((steer["src"].as_str(), steer["message"]["content"].as_str()), (Some("steer"), Some("later")));
        assert_eq!(steer["files"].as_array().unwrap().iter().map(|f| f["kind"].as_str().unwrap()).collect::<Vec<_>>(), ["image", "file"]);
    }
    #[test]
    fn an_empty_absorbed_prompt_shows_nothing_and_one_event_holds_at_most_ten_files() {
        let mut tap = Tap::default();
        let empty = |p: Value| json!({"type":"attachment","uuid":"q8","attachment":{"type":"queued_command","commandMode":"prompt","prompt":p}}).to_string();
        assert!(tap.feed(&empty(json!(""))).is_empty());
        assert!(tap.feed(&empty(json!([{"type":"text","text":"[Image #1] "}]))).is_empty(), "a token alone is no prompt");
        let img: Value = serde_json::from_str(IMG).unwrap();
        let many = &tap.feed(&rec("m1", Value::Array(vec![img; 12])))[0];
        assert_eq!(many["files"].as_array().map(Vec::len), Some(crate::chat::media::MAX_FILES));
    }

    fn id8(v: &Value) -> String { v.as_str().unwrap_or("none").chars().take(8).collect() }
    fn short(e: &Value) -> String {
        match (e["type"].as_str().unwrap(), e["subtype"].as_str()) {
            ("cdash", Some("cut")) => format!("cut:{}", id8(&e["after"])),
            ("user", _) if e["src"].is_string() => format!("prompt:{}", id8(&e["uuid"])),
            ("result", _) => format!("result:{}", id8(&e["uuid"])),
            ("assistant", _) => format!("assistant:{}", e["message"]["content"][0]["type"].as_str().unwrap()),
            (t, s) => format!("{t}:{}", s.unwrap_or("-")),
        }
    }
    fn feed_all(text: &str) -> Vec<Value> { let mut tap = Tap::default(); text.lines().flat_map(|l| tap.feed(l)).collect() }
    #[test]
    fn a_prompt_parented_to_an_older_turn_end_cuts_back_to_it() {
        let evs = feed_all(include_str!("testdata/rewind.jsonl"));
        assert_eq!(evs.iter().map(short).collect::<Vec<_>>(), [
            "prompt:96de607e", "assistant:text", "result:37929adb", "prompt:dd5d85d6", "assistant:text", "result:8a67cba9",
            "prompt:ea31531f", "assistant:text", "result:44ede06f", "cut:37929adb", "prompt:6fd3884f", "assistant:text", "result:db5a7c16",
        ]);
        assert_eq!(evs[9]["after"], "37929adb-63f0-4e64-b002-3f3c2f2a84b4");
        assert_eq!(evs[10]["ts"], "2026-11-13T04:34:10.043Z");
        assert_eq!(evs[0]["raw"], "[Image #1] What color is this image? Answer in one word.", "raw keeps the image token");
        assert!(evs[3].get("raw").is_none(), "no raw when it equals the text");
    }
    #[test]
    fn compaction_and_unknown_parents_never_cut() {
        let evs = feed_all(include_str!("testdata/compact.jsonl"));
        assert_eq!(evs.iter().map(short).collect::<Vec<_>>(), ["result:1f2a686a", "prompt:a7e885f8"]);
        let mut tap = Tap::default();
        for l in include_str!("testdata/rewind.jsonl").lines().take(5) { tap.feed(l); }
        let a = tap.feed(r#"{"type":"user","uuid":"o1","parentUuid":"never-seen","promptSource":"typed","message":{"role":"user","content":"x"}}"#);
        let b = tap.feed(r#"{"type":"user","uuid":"o2","parentUuid":null,"promptSource":"typed","message":{"role":"user","content":"y"}}"#);
        assert!(a.iter().chain(&b).all(|e| e["subtype"] != "cut"));
    }
    #[test]
    fn sidechains_stay_off_the_chain_and_hidden_records_stay_on_it() {
        let cuts = |lines: &[&str]| feed_all(&lines.join("\n")).iter().filter(|e| is_cut(e)).map(|e| e["after"].clone()).collect::<Vec<_>>();
        let end = r#"{"type":"system","subtype":"turn_duration","uuid":"s1","parentUuid":null}"#;
        let side = r#"{"type":"assistant","uuid":"x1","parentUuid":"s1","isSidechain":true,"message":{"id":"m","role":"assistant","content":[{"type":"text","text":"sub"}]}}"#;
        let next = r#"{"type":"user","uuid":"u2","parentUuid":"s1","promptSource":"typed","message":{"role":"user","content":"next"}}"#;
        assert!(cuts(&[end, side, next]).is_empty(), "a sidechain record does not move the chain's end");
        let meta = r#"{"type":"user","uuid":"m1","parentUuid":"s1","isMeta":true,"message":{"role":"user","content":"Caveat"}}"#;
        let info = r#"{"type":"system","subtype":"informational","uuid":"i1","parentUuid":"m1"}"#;
        let back = r#"{"type":"user","uuid":"u3","parentUuid":"m1","promptSource":"typed","message":{"role":"user","content":"back"}}"#;
        assert_eq!(cuts(&[end, meta, info, back]), [json!("s1")], "a hidden record is on the chain, so branching from it cuts");
    }

    #[test]
    fn the_branch_lists_its_prompts_oldest_first_and_skips_abandoned_ones() {
        // Claude Code 2.1.325 lists `!` commands (b1) and absorbed messages (q1), not notification turns (n1).
        let mut tap = Tap::default();
        for l in include_str!("testdata/claude.jsonl").lines() { tap.feed(l); }
        let listed = tap.listed();
        assert_eq!(listed.iter().map(|p| p.uuid.as_str()).collect::<Vec<_>>(), ["u1", "b1", "u2", "u5", "q1", "g1"]);
        assert_eq!((listed[1].text.as_str(), listed[1].raw.as_str()), ("! echo hi", "<bash-input> echo hi</bash-input>"), "shown as the selector shows it");
        assert_eq!((listed[5].text.as_str(), listed[5].raw.as_str(), listed[5].images), ("What color is this image?", "[Image #1] What color is this image?", true));
        let slash = r#"{"type":"user","uuid":"c1","parentUuid":"g2","promptSource":"typed","message":{"role":"user","content":"<command-name>/review</command-name><command-args>x</command-args>"}}"#;
        let pasted = r#"{"type":"user","uuid":"p1","parentUuid":"c1","promptSource":"typed","message":{"role":"user","content":"<pasted_content id=\"2\">\nlog\n</pasted_content id=\"2\">"}}"#;
        tap.feed(slash);
        tap.feed(pasted);
        let tail: Vec<_> = tap.listed().into_iter().skip(6).map(|p| (p.uuid, p.text, p.pasted)).collect();
        let row = |u: &str, t: &str, pasted| (u.to_string(), t.to_string(), pasted);
        assert_eq!(tail, [row("c1", "/review x", false), row("p1", "log", true)], "a slash command recorded as a prompt is a row");
    }

    #[test]
    fn the_branch_runs_on_across_a_compaction_and_lists_the_compact_command() {
        // As 2.1.325 writes a Terminal `/compact`; its selector lists p1, `/compact`, h1.
        let lines = [
            r#"{"type":"user","uuid":"p1","parentUuid":"s0","promptSource":"typed","message":{"role":"user","content":"Reply with only: g1"}}"#,
            r#"{"type":"system","subtype":"turn_duration","uuid":"t1","parentUuid":"p1"}"#,
            r#"{"type":"system","subtype":"compact_boundary","uuid":"cb","parentUuid":null,"logicalParentUuid":"t1"}"#,
            r#"{"type":"user","uuid":"cs","parentUuid":"cb","isCompactSummary":true,"isVisibleInTranscriptOnly":true,"message":{"role":"user","content":"This session is being continued"}}"#,
            r#"{"type":"user","uuid":"cv","parentUuid":"cs","isMeta":true,"message":{"role":"user","content":"<local-command-caveat>Caveat</local-command-caveat>"}}"#,
            r#"{"type":"user","uuid":"cc","parentUuid":"cv","message":{"role":"user","content":"<command-name>/compact</command-name>\n            <command-message>compact</command-message>\n            <command-args></command-args>"}}"#,
            r#"{"type":"user","uuid":"co","parentUuid":"cc","message":{"role":"user","content":"<local-command-stdout>Compacted</local-command-stdout>"}}"#,
            r#"{"type":"user","uuid":"h1","parentUuid":"co","promptSource":"typed","message":{"role":"user","content":"Reply with only: h1"}}"#,
        ];
        let mut tap = Tap::default();
        lines.iter().for_each(|l| drop(tap.feed(l)));
        let listed: Vec<_> = tap.listed().into_iter().map(|p| (p.uuid, p.text, p.after_compact)).collect();
        let row = |u: &str, t: &str, after| (u.to_string(), t.to_string(), after);
        assert_eq!(listed, [row("p1", "Reply with only: g1", false), row("cc", "/compact", true), row("h1", "Reply with only: h1", false)], "a resumed Claude lists from `/compact` on");
    }
    #[test]
    fn a_parent_cycle_lists_each_prompt_once() {
        let mut tap = Tap::default();
        for l in [
            r#"{"type":"system","subtype":"turn_duration","uuid":"o1","parentUuid":null}"#,
            r#"{"type":"system","subtype":"turn_duration","uuid":"o2","parentUuid":"o1"}"#,
            r#"{"type":"system","subtype":"turn_duration","uuid":"o3","parentUuid":"o2"}"#,
            r#"{"type":"system","subtype":"turn_duration","uuid":"o4","parentUuid":"o3"}"#,
            r#"{"type":"user","uuid":"a","parentUuid":"b","promptSource":"typed","message":{"role":"user","content":"one"}}"#,
            r#"{"type":"user","uuid":"b","parentUuid":"a","promptSource":"typed","message":{"role":"user","content":"two"}}"#,
            r#"{"type":"system","subtype":"turn_duration","uuid":"x1","parentUuid":"b"}"#,
            r#"{"type":"system","subtype":"turn_duration","uuid":"x2","parentUuid":"x1"}"#,
            r#"{"type":"system","subtype":"turn_duration","uuid":"x3","parentUuid":"x2"}"#,
        ] {
            drop(tap.feed(l));
        }
        assert_eq!(tap.listed().len(), 2);
    }

    const FACTS: &str = include_str!("testdata/facts.jsonl");
    #[test]
    fn readable_thinking_shows_and_signature_only_thinking_does_not() {
        assert_eq!(feed_all(FACTS)[0]["message"]["content"][0], json!({"type":"thinking","thinking":"Check the tests first."}));
        assert!(!feed_all(include_str!("testdata/rewind.jsonl")).iter().any(|e| e["message"]["content"][0]["type"] == "thinking"));
    }
    #[test]
    fn the_checklist_follows_todowrite_and_the_task_tools() {
        let lists: Vec<Vec<String>> = feed_all(FACTS).iter().filter(|e| e["subtype"] == "todo")
            .map(|e| e["items"].as_array().unwrap().iter().map(|i| format!("{}:{}", i["text"].as_str().unwrap(), i["status"].as_str().unwrap())).collect())
            .collect();
        let three = ["Read:completed", "Fix:in_progress", "Test:pending"];
        assert_eq!(lists, [three.to_vec(), [&three[..], &["Ship it:pending"]].concat(), [&three[..], &["Ship it:completed"]].concat()]);
    }
    #[test]
    fn usage_counts_each_reply_once_and_context_is_the_latest_reply() {
        let mut tap = Tap::default();
        for l in FACTS.lines() { tap.feed(l); }
        let u = tap.usage();
        assert_eq!(u["tokens"], json!({"input":8,"output":30,"cache_read":1900,"cache_write":150}));
        assert_eq!(u["context"], json!({"used":1055,"window":context_window("claude-opus-5-5")}));
        assert_eq!((u["model"].as_str(), u["cwd"].as_str(), &u["cost"]), (Some("claude-opus-5-5"), Some("/proj/.worktrees/a"), &Value::Null));
        assert_eq!(context_window("claude-mystery-9"), None, "unknown models show tokens only");
        assert_eq!(context_window("claude-opus-5-50"), None, "a prefix must end at a dash");
        assert_eq!(context_window("claude-opus-5-5-20261101"), Some(1_000_000));
    }
    #[test]
    fn running_tasks_start_from_results_and_end_from_notifications_stops_and_restatements() {
        let mut tap = Tap::default();
        let mut seen = Vec::new();
        for l in FACTS.lines() { tap.feed(l); seen.push(tap.tasks().iter().map(|t| t["id"].as_str().unwrap().to_string()).collect::<Vec<_>>()); }
        assert_eq!(seen[3], ["bo29v19cm"]);
        assert_eq!(seen[5], ["bo29v19cm", "a6cc68d2a048556bd"]);
        assert_eq!(seen[6], ["a6cc68d2a048556bd"], "the enqueued notification ends the shell");
        assert!(seen[8].is_empty(), "TaskStop ends the agent");
        assert_eq!(seen[9], ["bw2ckxil8"], "restated after compaction");
        assert!(seen[14].is_empty(), "a queued_command notification ends it");
        let mut first = Tap::default();
        for l in FACTS.lines().take(4) { first.feed(l); }
        assert_eq!(first.tasks()[0], json!({"id":"bo29v19cm","kind":"shell","title":"Sleep a minute","started":"2026-11-13T10:00:05.000Z"}));
    }
    #[test]
    fn what_a_session_keeps_is_bounded_and_odd_records_change_nothing() {
        let mut tap = Tap::default();
        for i in 0..200 {
            let call = json!({"type":"assistant","uuid":format!("a{i}"),"message":{"id":format!("m{i}"),"model":"<synthetic>","content":[{"type":"tool_use","id":format!("t{i}"),"name":"Bash","input":{"command":"x".repeat(500)}}]}});
            let res = json!({"type":"user","uuid":format!("r{i}"),"toolUseResult":{"backgroundTaskId":format!("b{i}")},"message":{"role":"user","content":[{"type":"tool_result","tool_use_id":format!("t{i}"),"content":"ok"}]}});
            tap.feed(&call.to_string());
            tap.feed(&res.to_string());
        }
        assert_eq!(tap.tasks().len(), MAX_TASKS);
        assert_eq!(tap.tasks()[0]["title"].as_str().unwrap().chars().count(), 80);
        assert!(tap.calls.len() <= MAX_CALLS);
        assert_eq!(tap.usage()["tokens"], Value::Null, "synthetic replies count for nothing");
        let odd = [r#"{"type":"user","toolUseResult":7,"message":{"content":[{"type":"tool_result","tool_use_id":5}]}}"#, r#"{"type":"attachment","attachment":{"type":"task_status","status":"running","taskId":[]}}"#, r#"{"type":"queue-operation","operation":"enqueue","content":"<task-notification><status>killed"}"#];
        for l in odd { tap.feed(l); }
        assert_eq!(tap.tasks().len(), MAX_TASKS);
    }
}
