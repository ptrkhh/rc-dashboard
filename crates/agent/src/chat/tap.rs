//! Claude Code transcript lines to chat events. Pure: no I/O, never panics.
//!
//! The record shapes are internal to Claude Code (checked on 2.1.322, see
//! `CLAUDE_FINDINGS.md`). Anything not understood produces nothing.

use super::{media::split_attached, CAP};
use serde::Deserialize;
use serde_json::{json, Value};

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
}

/// The last prompt rule 7 emitted, for spotting a handed-back prompt.
struct Typed {
    queued: bool,
    parent: Option<String>,
    uuid: Option<String>,
}

#[derive(Default)]
pub struct Tap {
    open: bool,
    seen: usize,
    understood: bool,
    last: Option<Typed>,
    /// The last main-chain assistant record was an API error message.
    api_error: bool,
}

impl Tap {
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
        if [r.sidechain, r.meta, r.compact, r.transcript_only].contains(&Some(true)) {
            return Vec::new();
        }
        let out = match r.kind.as_str() {
            "queue-operation" => match (r.operation.as_deref(), r.content.as_ref().and_then(parts)) {
                (Some("enqueue"), Some((text, _))) if !injected(&text) => vec![json!({"type":"cdash","subtype":"queued","text":clean(&text).0})],
                _ => Vec::new(),
            },
            "attachment" => steer(&r).into_iter().collect(),
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
        if r.kind == "user" || r.kind == "assistant" {
            self.seen += 1;
            self.understood |= !out.is_empty();
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

    /// Rule 7 and 8: a `user` record with string content.
    fn typed(&mut self, r: &Rec, text: &str, mut files: Vec<Value>) -> Vec<Value> {
        let text = &strip_image_tokens(text);
        let t = text.trim_start();
        if let Some(cmd) = between(t, "<bash-input>", "</bash-input>") {
            return vec![json!({"type":"cdash","subtype":"shell","uuid":r.uuid,"command":cmd.trim()})];
        }
        if let Some(stdout) = between(t, "<bash-stdout>", "</bash-stdout>") {
            let stderr = between(t, "<bash-stderr>", "</bash-stderr>").unwrap_or("");
            return vec![json!({"type":"cdash","subtype":"shell_output","uuid":r.uuid,"stdout":stdout,"stderr":stderr})];
        }
        if r.source.as_deref() == Some("system") || injected(t) {
            return Vec::new();
        }
        let prompt = match between(t, "<command-name>", "</command-name>") {
            Some(name) => {
                let args = between(t, "<command-args>", "</command-args>").unwrap_or("");
                format!("{} {}", name.trim(), args.trim()).trim().to_string()
            }
            None => unwrap_pastes(text),
        };
        let (prompt, attached) = split_attached(&prompt);
        files.extend(attached);
        self.api_error = false;
        let queued = r.source.as_deref() == Some("queued");
        let mut out = Vec::new();
        if let Some(prev) = &self.last {
            // Claude hands a prompt stopped before any reply back to the input
            // box and writes the next one with the same parent.
            if !queued && !prev.queued && prev.parent.is_some() && prev.parent == r.parent {
                out.push(json!({"type":"cdash","subtype":"retract","uuid":prev.uuid}));
            }
        }
        self.last = Some(Typed { queued, parent: r.parent.clone(), uuid: r.uuid.clone() });
        out.push(prompt_event(r.uuid.as_deref(), prompt.trim(), if queued { "queued" } else { "typed" }, files));
        out
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

/// Text tools inject for the model's benefit; never shown as a user message.
fn injected(text: &str) -> bool {
    let t = text.trim_start();
    ["<task-notification>", "<system-reminder>", "<local-command-"].iter().any(|p| t.starts_with(p))
}

fn prompt_event(uuid: Option<&str>, text: &str, src: &str, files: Vec<Value>) -> Value {
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

/// Rule 3: a message Claude absorbed mid-turn has no `user` record, only this.
fn steer(r: &Rec) -> Option<Value> {
    let a = r.attachment.as_ref()?;
    if a["type"] != "queued_command" || a["commandMode"] != "prompt" {
        return None;
    }
    let (text, mut files) = parts(&a["prompt"])?;
    let (text, attached) = clean(&text);
    files.extend(attached);
    Some(prompt_event(r.uuid.as_deref(), &text, "steer", files))
}

/// Rule 5: one event per `text` or `tool_use` block; thinking is status only.
fn assistant(r: &Rec) -> Vec<Value> {
    let Some(m) = &r.message else { return Vec::new() };
    let Some(blocks) = m["content"].as_array() else { return Vec::new() };
    blocks
        .iter()
        .filter_map(|b| {
            let block = match b["type"].as_str()? {
                "text" => json!({"type":"text","text":b["text"].as_str()?}),
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
            "prompt:typed:handed back", "cdash:retract", "prompt:typed:second try",
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
        assert_eq!(evs[16]["uuid"], "u4", "the handed-back prompt is the one withdrawn");
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
}
