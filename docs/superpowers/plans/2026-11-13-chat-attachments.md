# Chat Attachments (Part 3) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Attach images and files in dashboard chat (button, paste, drop), send them with a message to Claude Code, Codex and OpenCode, and show them in history as thumbnails and chips, whichever door the message came from.

**Architecture:** Uploads ride the chat WebSocket (an `upload` frame, then binary frames of at most 1 MiB) into private folders under `$XDG_CACHE_HOME/cdash/uploads/<session>/<id>/`. The hub records them by id, so `send` carries ids, never a path. Each engine hands attachments over the way its tool takes them: Claude gets each image path pasted alone (it becomes an `[Image #N]` chip) and other files as `Attached:` lines; the Codex and OpenCode supervisors get absolute paths and read the files themselves (Codex: data URLs; OpenCode: `file://` parts). Images found in history are written once to a content-addressed cache (`…/cdash/media/<sha256>.<ext>`) by the Claude engine or by the supervisor, travel as `ref`s, and are served by `GET /api/chat/media/<ref>`.

**Tech Stack:** Rust (axum 0.8 WebSocket binary frames, tokio fs, and `sha2`, `base64`, `getrandom`, all already direct dependencies), Node 22 ES modules for the supervisors, vanilla JS and CSS, `node --test`, `cargo test`.

**Spec:** `docs/superpowers/specs/2026-11-13-chat-attachments-design.md`, with its evidence in `docs/superpowers/specs/2026-11-13-chat-evidence/` (E2 of `claude-2.1.323-live-spike.md`, `review-checks.md`, section 8 of `codex-opencode-source-notes.md`). Shared names and rules: `docs/superpowers/plans/2026-11-13-chat-parts-2-5-shared.md`. Read both first.

## Global Constraints

- Work in `/mnt/d/git/claude-dashboard/.worktrees/chat-parts-2-5` on `feat/chat-parts-2-5`. Parts 4 and 2 are already done there exactly as the shared contract says; use their names (`EngineMsg::Status { working, waiting, waiting_for }`, the `req`/`res` envelope, `Engine::answer`, `set`, `menu`). No branch setup.
- `export CARGO_TARGET_DIR=/mnt/d/git/claude-dashboard/target` before every cargo command. CI gates: `node --test`, `cargo test --all --locked`, `cargo clippy --all-targets --locked -- -D warnings -D clippy::disallowed_types`.
- Never `git add -A` or `git add .`; `claude-1-261102-19.xapk` stays untracked. Each task ends with one commit of exactly the files in its Files block, with the task's Conventional Commits subject and a body of exactly two lines: `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>` and `Claude-Session: https://claude.ai/code/session_0121cbef6XAzH3a3m9iNVdAr`.
- No new npm packages and no new Rust crates.
- Agent subprocesses go through `Runner` (`crates/agent/src/host/cmd.rs`). Supervisors never write to stdout: it is the native TUI's terminal.
- The Remote Control worker paths (`worker()` in `codex-remote.mjs`, `worker()` and `RemoteBridge` in `opencode-remote.mjs`, `claude-remote.mjs`) do not change. Shared code they call (`replayHistory`, `submitPrompt`, `opencode-api.mjs`, `codex_proxy.rs`) changes only in ways that leave what the worker sends identical; each such task says so.
- The Claude launch command does not change.
- Every new file under `public/` goes into `embedded_asset` in `crates/agent/src/http/serve.rs`. CSS classes and ids never match `/\.send[\s{.:-]/` or contain `send-input`; `app.js` never contains `/api/keys`.
- A supervisor socket line is at most 64 KiB either way (`LINE_MAX`). Media never travels inline on it: images become `ref`s before `feed.accept`.
- Logs carry no message text and no file names. Deliberate shortcuts carry a `ponytail:` comment naming the ceiling.
- Real-session checks use a private tmux server (`tmux -L cdash-test`) and `--model haiku` where the tool allows it, and never touch the default tmux server or the agent on 127.0.0.1:23274.
- Values from the spec. "MB" is read as MiB, which matches OpenCode's own 20 MiB limit and Codex's 32 MiB base64 image budget:
  - 20 MiB per file (`MAX_UPLOAD = 20 * 1024 * 1024`), 10 attachments per message, binary frames of at most 1 MiB;
  - upload id: 16 random lowercase hex characters; stored name: base name of the upload frame's `name`, every character outside `A–Z a–z 0–9 . _ -` and space replaced by `_`, at most 100 bytes, never starting with `.`, `file` when nothing is left, images renamed to their sniffed type's extension;
  - type sniffed from the first bytes (PNG, JPEG, GIF and WebP are images); the client's MIME type is ignored;
  - folders 0700 and files 0600, set explicitly; `uploads/<session>` deleted when the hub sees the session end; upload folders and media files older than 7 days deleted at agent start and once a day;
  - Claude: paste each image path alone, poll every 50 ms for up to 5 s for exactly one new chip, then paste a space and the text, wait Part 1's 400 ms, press Enter; a missing chip clears the box and refuses the message;
  - Codex: images as `data:<mime>;base64,…`, at most 24 MiB of images per message; OpenCode: `files` gets `{uri: "file://<absolute path>", name}` for images and PDFs only; other files go as `Attached: <path>` lines for all three tools;
  - `GET /api/chat/media/<ref>`: `^[0-9a-f]{64}$` else 400; `X-Content-Type-Options: nosniff`, `Content-Security-Policy: default-src 'none'`, `Cache-Control: private, max-age=31536000, immutable`, `Content-Type` from the extension.
- Error strings, verbatim. Agent: `attachment expired`, `too many attachments`, `empty message` (Part 1), `file over 20 MB`, `empty file`, `more bytes than declared`, `one upload at a time`, `upload failed`, `image did not attach`, `too many images for one Codex message`. Client: `Over 20 MB`, `Empty file`, `10 attachments at most`, `Connection lost`, `Not connected`.

## Review Focus

1. A socket that drops in the middle of an upload (a phone locking, a network change) must leave no partial file and no usable id, and the chip must say the upload failed and offer Retry. Tests: Task 3 `refused_parallel_and_abandoned_uploads_leave_nothing_behind`, Task 11 `a dropped socket fails …`.
2. A message with attachments sent while the session works is held with them: the held list shows them, Send now delivers them, Edit puts them back. Tests: Task 2 `a_held_message_keeps_its_attachments_lists_them_and_sends_them`, Task 11 (restore).
3. An image message Claude queues (Send now during a turn) must still become one confirmed prompt with its image, whether Claude records it as a queued prompt or absorbs it mid-turn. Test: Task 4 `queued_absorbed_and_typed_prompts_lose_their_upload_lines_and_image_tokens`.
4. An image typed in Codex's native TUI arrives as tens of MB of data URL: chat must get a `ref`, the agent socket must never carry the bytes, and the transport must not choke on the size. Tests: Task 7 `an_app_server_message_over_the_websocket_defaults_reaches_the_bridge_whole`, Task 9 `image prompts from the native TUI and from history …`.
5. Real file names (spaces, a Windows path from some browsers, non-Latin names, 300 bytes, a PNG named `.JPG`) must be stored under a short safe name that keeps its extension, and an image must end in its real type's extension, because Claude attaches an image only from a path ending in one. Tests: Task 1 `names_are_reduced_…`, Task 3 `a_finished_upload_is_named_typed_by_its_bytes_and_private`.

## Protocol additions

```
client: {"t":"upload","ref":R,"name":N,"size":S}      then binary frames of ≤ 1 MiB, S bytes in total
server: {"t":"uploaded","ref":R,"id":I,"name":N,"size":S,"kind":"image"|"file","mime":M} | {"t":"uploaded","ref":R,"error":E}
client: {"t":"send","cid":C,"text":T,"files":[I…]}   T may be empty when files is not
prompt event gains: "files":[{"kind":"image","ref":H,"mime":M} | {"kind":"file","name":N,"size":S?}]   (key present only when non-empty)
state: "held":[{"id","text","files":[{"id","name","kind","size","mime"}]}]
agent → supervisor: {"t":"send","cid","text","steer","files":[{"path","kind","mime","name","size"}]}   (files only when non-empty)
```

Inside the Claude engine only, the tap's image entries are `{"kind":"image","mime":M,"data":B64}`; the engine swaps `data` for `ref` before the hub sees them.

## File structure

| File | Change |
|---|---|
| `crates/agent/src/chat/media.rs` | New: cache paths, names, sniffing, media cache, sweep, `Attached:` lines, upload writer |
| `crates/agent/src/chat/{mod,wire,queue,hub}.rs` | `media` module; `Send.files`, `Upload`; held files; `Attachment`, `Kind`, `Engine::send`, upload record |
| `crates/agent/src/chat/ws.rs` | `upload` and binary frames |
| `crates/agent/src/chat/tap.rs`, `chat/testdata/claude.jsonl` | Image prompts, `Attached:` lines |
| `crates/agent/src/chat/{tmux_io,claude}.rs` | Image chips, screen capture, history images to refs |
| `crates/agent/src/chat/native.rs`, `collect/{codex,opencode,codex_proxy}.rs` | `files` on `send`, `CDASH_MEDIA_DIR`, transport size limits |
| `crates/agent/src/http/{routes,serve}.rs` | Media route, sweeper, asset |
| `crates/agent/src/collect/chat-feed.mjs` | Shared supervisor helpers; `files` reach `send` |
| `crates/agent/src/collect/codex-remote.mjs` | Data URLs out, refs in |
| `crates/agent/src/collect/{opencode-remote,opencode-api}.mjs` | File parts out, refs in, stream and history limits |
| `public/chat/attach.js` | New: upload queue, composer attachments, image viewer |
| `public/chat/{store,view}.js`, `public/index.html`, `public/style.css` | Attachments on rows; composer and history UI |
| `crates/tauri-app/tauri.conf.json` | `dragDropEnabled: false` |
| `test/*.test.mjs` | `chat-feed`, `codex-native`, `opencode-native`, `opencode-api`, `chat-store`, `frontend-guards`; new `chat-attach` |
| `README.md`, `CLAUDE_FINDINGS.md` | Attachments; image-paste facts |

---

## Phase A: the agent

### Task 1: Media module: names, types, the media cache and the sweep

**Files:**
- Create: `crates/agent/src/chat/media.rs`
- Modify: `crates/agent/src/chat/mod.rs` (`pub mod media;`), `crates/agent/src/http/serve.rs` (`serve` starts the sweeper)

**Interfaces:**
- Produces, in `chat::media`:
  - `pub const MAX_UPLOAD: u64 = 20 * 1024 * 1024`, `pub const MAX_FILES: usize = 10`, `pub const MAX_AGE: Duration = Duration::from_secs(7 * 24 * 3600)`.
  - `pub fn cache_root() -> PathBuf`: `$XDG_CACHE_HOME/cdash` when that is an absolute path, else `$HOME/.cache/cdash` (`/` when `HOME` is unset), through `fn root_from(xdg: Option<OsString>, home: Option<OsString>) -> PathBuf`. Under `#[cfg(test)]` it is `std::env::temp_dir().join(format!("cdash-test-{}", std::process::id())).join("cdash")`, so unit tests never touch the real cache and paths still contain `/cdash/uploads/`.
  - `pub fn uploads_dir(session: &str) -> PathBuf` (`cache_root()/uploads/<session>`), `pub fn media_dir() -> PathBuf` (`cache_root()/media`).
  - `pub(crate) fn private_dirs(dir: &Path) -> std::io::Result<()>`: creates `dir` and sets 0700 on it and on every folder between it and `cache_root()`, `cache_root()` included.
  - `pub fn sniff(head: &[u8]) -> Option<&'static str>` (`image/png`, `image/jpeg`, `image/gif`, `image/webp`, `application/pdf`); `pub fn image_ext(mime: &str) -> Option<&'static str>` (`png`, `jpg`, `gif`, `webp`).
  - `pub fn sanitize_name(raw: &str) -> String`, `pub fn image_name(name: &str, ext: &str) -> String`.
  - `pub fn stash(bytes: &[u8], mime: &str) -> std::io::Result<String>` (the lowercase hex SHA-256 `ref`).
  - `pub fn sweep(max_age: Duration)`, `pub(crate) fn sweep_in(root: &Path, max_age: Duration)`, `pub async fn sweep_daily()`.
  - `pub fn with_attached(text: &str, paths: &[&Path]) -> String`, `pub fn split_attached(text: &str) -> (String, Vec<serde_json::Value>)`.

- [ ] **Step 1: Write the failing tests** in `media.rs` (`#[cfg(test)] pub(crate) mod tests`)

```rust
pub(crate) const PNG: &[u8] = b"\x89PNG\r\n\x1a\n";
pub(crate) fn mode(p: &Path) -> u32 { std::fs::metadata(p).unwrap().permissions().mode() & 0o777 }
fn age(p: &Path, by: Duration) { std::fs::File::open(p).unwrap().set_modified(SystemTime::now() - by).unwrap(); }

#[test]
fn the_cache_root_follows_xdg_only_when_it_is_absolute() {
    let r = |x: Option<&str>, h: Option<&str>| root_from(x.map(Into::into), h.map(Into::into));
    assert_eq!(r(Some("/x/cache"), Some("/home/u")), PathBuf::from("/x/cache/cdash"));
    assert_eq!(r(Some("relative"), Some("/home/u")), PathBuf::from("/home/u/.cache/cdash"));
    assert_eq!(r(None, None), PathBuf::from("/.cache/cdash"));
    assert_eq!(uploads_dir("cdash-a"), cache_root().join("uploads/cdash-a"));
}
#[test]
fn names_are_reduced_to_a_safe_base_name_that_keeps_its_extension() {
    for (raw, want) in [("../x", "x"), ("..", "file"), (".hidden", "hidden"), ("a\0b<c>.txt", "a_b_c_.txt"),
        ("C:\\fakepath\\Shot 1.png", "Shot 1.png"), ("日本.png", "__.png"), ("", "file"), (" . ", "file")] {
        assert_eq!(sanitize_name(raw), want, "{raw:?}");
    }
    let long = sanitize_name(&format!("{}.pdf", "x".repeat(300)));
    assert_eq!((long.len(), long.ends_with(".pdf")), (100, true));
    assert_eq!(sanitize_name(&"é".repeat(150)), "_".repeat(100));
    assert_eq!([image_name("Shot.JPEG", "jpg"), image_name("notes.txt", "png"), image_name("file", "png")], ["Shot.jpg", "notes.png", "file.png"]);
    assert_eq!(image_name(&"z".repeat(100), "webp").len(), 100);
}
#[test]
fn types_come_from_the_bytes_never_from_the_name() {
    let cases: [(&[u8], Option<&str>); 10] = [(PNG, Some("image/png")), (b"\xff\xd8\xff\xe0", Some("image/jpeg")),
        (b"GIF87a", Some("image/gif")), (b"GIF89a", Some("image/gif")), (b"RIFF\0\0\0\0WEBPVP8 ", Some("image/webp")),
        (b"%PDF-1.7", Some("application/pdf")), (b"a text file named x.png", None), (b"RIFF\0\0\0\0WAVEfmt ", None), (b"\x89PN", None), (b"", None)];
    for (head, want) in cases { assert_eq!(sniff(head), want, "{head:?}"); }
    assert_eq!(["image/png", "image/jpeg", "image/gif", "image/webp", "application/pdf"].map(image_ext), [Some("png"), Some("jpg"), Some("gif"), Some("webp"), None]);
}
#[test]
fn an_image_is_stored_once_by_its_hash_privately_and_a_rewrite_refreshes_it() {
    let png = [PNG, b"stash-test"].concat();
    let r = stash(&png, "image/png").unwrap();
    assert_eq!(r, format!("{:x}", Sha256::digest(&png)));
    let f = media_dir().join(format!("{r}.png"));
    assert_eq!((mode(&f), mode(&media_dir()), mode(&cache_root())), (0o600, 0o700, 0o700));
    age(&f, Duration::from_secs(86_400));
    assert_eq!(stash(&png, "image/png").unwrap(), r);
    assert!(std::fs::metadata(&f).unwrap().modified().unwrap() > SystemTime::now() - Duration::from_secs(60), "a rewrite refreshes its age");
    let copies = std::fs::read_dir(media_dir()).unwrap().filter(|e| e.as_ref().unwrap().file_name().to_string_lossy().starts_with(&r)).count();
    assert_eq!(copies, 1, "one file, no temp left behind");
    for (bytes, mime) in [(&b"plain text"[..], "image/png"), (&png[..], "image/jpeg"), (&b"%PDF-1.7"[..], "application/pdf")] {
        assert_eq!(stash(bytes, mime).unwrap_err().kind(), std::io::ErrorKind::InvalidData, "{mime}");
    }
}
#[test]
fn the_sweep_removes_what_has_not_been_touched_for_the_limit() {
    let root = std::env::temp_dir().join(format!("cdash-sweep-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let file = |rel: &str| { let p = root.join(rel); std::fs::create_dir_all(p.parent().unwrap()).unwrap(); std::fs::write(&p, "x").unwrap(); p };
    let (old_up, new_up) = (file("uploads/cdash-a/aaaa/x.txt"), file("uploads/cdash-a/bbbb/y.txt"));
    let (old_media, new_media) = (file("media/1.png"), file("media/2.png"));
    for s in ["uploads/cdash-b", "uploads/cdash-c"] { std::fs::create_dir_all(root.join(s)).unwrap(); }
    for p in [old_up.parent().unwrap().to_path_buf(), root.join("uploads/cdash-b"), old_media.clone()] { age(&p, MAX_AGE + Duration::from_secs(3600)); }
    sweep_in(&root, MAX_AGE);
    assert!(!old_up.exists() && new_up.exists());
    assert!(!root.join("uploads/cdash-b").exists() && root.join("uploads/cdash-c").exists(), "only old empty session folders go");
    assert!(!old_media.exists() && new_media.exists());
    sweep_in(&root.join("missing"), MAX_AGE); // nothing there, no panic
}
#[test]
fn trailing_attached_lines_for_uploads_become_file_entries_and_nothing_else_does() {
    let up = "/home/u/.cache/cdash/uploads/cdash-a/0123456789abcdef";
    let (pdf, txt) = (format!("{up}/r.pdf"), format!("{up}/notes 1.txt"));
    let sent = with_attached("look at these", &[Path::new(&pdf), Path::new(&txt)]);
    assert_eq!(sent, format!("look at these\nAttached: {pdf}\nAttached: {txt}"));
    assert_eq!(split_attached(&sent), ("look at these".into(), vec![json!({"kind":"file","name":"r.pdf"}), json!({"kind":"file","name":"notes 1.txt"})]));
    assert_eq!(split_attached(&with_attached("", &[Path::new(&pdf)])), (String::new(), vec![json!({"kind":"file","name":"r.pdf"})]));
    for kept in ["Attached: /etc/passwd".to_string(), "Attached: rel/cdash/uploads/x".into(), format!("Attached: {pdf}\nthen more")] {
        assert_eq!(split_attached(&kept), (kept.clone(), vec![]), "{kept}");
    }
}
```

- [ ] **Step 2: Run** `cargo test -p cdash-agent chat::media`; expect a compile failure.
- [ ] **Step 3: Implement.**
  - `sanitize_name`: take the text after the last `/` or `\`; replace every character outside the allowed set with one `_`; strip leading `.` and spaces and trailing spaces; empty gives `file`; over 100 bytes, keep the extension (the text after the last `.`, when 1 to 16 characters) and cut the stem so the whole is 100 bytes. `image_name`: the stem (text before the last `.`, or all of it) plus `.<ext>`, the stem cut so the whole is at most 100 bytes.
  - `sniff`: PNG `89 50 4E 47 0D 0A 1A 0A`; JPEG `FF D8 FF`; GIF `GIF87a` or `GIF89a`; WebP `RIFF` at 0 and `WEBP` at 8; PDF `%PDF-`.
  - `stash`: `ErrorKind::InvalidData` unless `image_ext(mime)` is known and `sniff(bytes) == Some(mime)`. Target `media_dir()/<ref>.<ext>`. If it exists, set its modification time to now (`File::set_modified`) and return. Otherwise `private_dirs(&media_dir())`, write `<ref>.<ext>.<pid>-<n>.tmp` (`n` from a static `AtomicU64`) with `create_new` and mode 0600, `set_permissions` 0600, and rename it into place: a file appears only whole, so two writers of one image never expose half of it.
  - `sweep_in`: under `root/uploads`, remove a session folder that is empty and older than `max_age` as it is found, and in the others remove each `<id>` folder older than `max_age`; under `root/media`, remove each file older than `max_age`. Ignore every error, missing folders included. `sweep(max_age)` is `sweep_in(&cache_root(), max_age)`.
  - `sweep_daily`: `tokio::time::interval(24 h)`, first tick at once; each tick awaits `spawn_blocking(|| sweep(MAX_AGE))`. In `serve()`, after the `Ctx` is built: `tokio::spawn(crate::chat::media::sweep_daily());` with `// ponytail: one sweeper per serve() call; an embedder that restarts the server in-process starts a second one, which is harmless.`
  - `with_attached`: the text, then one `Attached: <path>` line per path, joined by `\n`; no leading newline when the text is empty.
  - `split_attached`: while the last line starts with `Attached: /` and contains `/cdash/uploads/`, pop it and record `{"kind":"file","name":<text after its last />}`; return the rest joined by `\n` and `trim_end`ed, and the entries in their original order.
- [ ] **Step 4: Run** the same command; expect 6 passed. Run clippy.
- [ ] **Step 5: Commit** `media.rs`, `chat/mod.rs`, `http/serve.rs`: `feat(chat): media cache, upload names and the daily sweep`.

### Task 2: Wire frames, attachments in the hub, and the new `Engine::send`

**Files:**
- Modify: `crates/agent/src/chat/{wire,queue,hub}.rs`; `crates/agent/src/chat/{claude,native}.rs` (signature only); every other `impl Engine for` and `ClientFrame::Send {` literal in the crate (`grep -rn "impl Engine for\|ClientFrame::Send {" crates/agent/src`)

**Interfaces:**
- Consumes: `media::{MAX_FILES, uploads_dir}`.
- Produces:
  - `ClientFrame::Send { cid: String, text: String, #[serde(default)] files: Vec<String> }`; `ClientFrame::Upload { #[serde(rename = "ref")] reference: String, name: String, size: u64 }` (handled by `ws.rs` in Task 3; the hub ignores it).
  - In `hub.rs`: `#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)] #[serde(rename_all = "lowercase")] pub enum Kind { Image, File }` and `#[derive(Debug, Clone, Serialize)] pub struct Attachment { pub path: PathBuf, pub kind: Kind, pub mime: String, pub name: String, pub size: u64 }`.
  - `Engine::send(&self, text: &str, steer: bool, cid: &str, files: &[Attachment]) -> impl Future<Output = Result<bool, String>> + Send`.
  - `Hub::add_upload(&self, id: String, a: Attachment)` (async; it goes through the command channel, so a `send` that follows it on the same socket always finds it). `hub::new_epoch` becomes `pub(crate)` (Task 3 uses it for upload ids).
  - `queue::Held` gains `pub files: Vec<String>`; `Queue::hold(&mut self, cid: String, text: String, files: Vec<String>) -> String`.
  - Every `held` entry of the `state` frame gains `files: [{id, name, kind, size, mime}]`.

- [ ] **Step 1: Write the failing tests.** In `wire.rs`, add to `client_frames_parse_and_unknown_ones_do_not`:

```rust
assert!(matches!(f(r#"{"t":"send","cid":"c1","text":"hi"}"#), Ok(ClientFrame::Send { files, .. }) if files.is_empty()));
assert!(matches!(f(r#"{"t":"send","cid":"c1","text":"","files":["0123456789abcdef"]}"#), Ok(ClientFrame::Send { files, .. }) if files == ["0123456789abcdef"]));
assert!(matches!(f(r#"{"t":"upload","ref":"r1","name":"a.png","size":5}"#), Ok(ClientFrame::Upload { size: 5, .. })));
assert!(f(r#"{"t":"upload","ref":"r1","name":"a.png","size":-1}"#).is_err());
```

In `hub.rs` tests: `FakeEngine::send` records `send:<text>:<steer>`, plus `:<names joined by ,>` when there are files, so every existing expectation stays as it is. Add `send_with(cid, text, ids: &[&str]) -> ClientFrame` (`send(cid, text)` becomes `send_with(cid, text, &[])`) and `rig_named(name: &str)` (`rig()` is `rig_named("cdash-a")`), and add `async fn upload(hub: &Hub, id: &str, kind: Kind, name: &str) -> PathBuf`, which writes 8 bytes to `tempfile_path(&format!("hub-up-{id}"))/<name>` and registers it with mime `image/png` or `text/plain`. Then:

```rust
#[tokio::test]
async fn sends_carry_their_uploads_and_unknown_or_vanished_ones_are_refused() {
    let (hub, _tx, calls) = rig();
    upload(&hub, "aaaa", Kind::Image, "shot.png").await;
    std::fs::remove_file(upload(&hub, "bbbb", Kind::File, "gone.txt").await).unwrap();
    assert_eq!(ack_of(hub.command(send_with("c1", "look", &["aaaa"])).await)["status"], "sent");
    assert_eq!(ack_of(hub.command(send_with("c2", " ", &["aaaa"])).await)["status"], "sent", "attachments alone are a message");
    for (cid, id) in [("c3", "nope"), ("c4", "bbbb")] {
        let a = ack_of(hub.command(send_with(cid, "x", &[id])).await);
        assert_eq!((a["status"].as_str(), a["error"].as_str()), (Some("refused"), Some("attachment expired")), "{cid}");
    }
    assert_eq!(ack_of(hub.command(send_with("c5", " ", &[])).await)["error"], "empty message");
    assert_eq!(ack_of(hub.command(send_with("c6", "x", &["aaaa"; 11])).await)["error"], "too many attachments");
    assert_eq!(*calls.lock().unwrap(), ["send:look:false:shot.png", "send::false:shot.png"]);
}
#[tokio::test]
async fn a_held_message_keeps_its_attachments_lists_them_and_sends_them() {
    let (hub, tx, calls) = rig();
    upload(&hub, "aaaa", Kind::Image, "shot.png").await;
    let mut sub = hub.subscribe(None, None).await.unwrap();
    tx.send(EngineMsg::Status { working: true, waiting: false, waiting_for: None }).await.unwrap();
    settle().await;
    assert_eq!(ack_of(hub.command(send_with("c1", "later", &["aaaa"])).await)["status"], "held");
    settle().await;
    let state = frames(&mut sub).into_iter().rev().find(|f| f["t"] == "state").unwrap();
    assert_eq!(state["held"][0]["files"], json!([{"id":"aaaa","name":"shot.png","kind":"image","size":8,"mime":"image/png"}]));
    hub.command(ClientFrame::Held { op: HeldOp::Now, id: "h1".into() }).await;
    assert_eq!(*calls.lock().unwrap(), ["send:later:true:shot.png"]);
}
#[tokio::test]
async fn an_ended_session_deletes_its_upload_folder() {
    let (_hub, tx, _) = rig_named("cdash-hub-end");
    let f = media::uploads_dir("cdash-hub-end").join("0123456789abcdef/a.txt");
    std::fs::create_dir_all(f.parent().unwrap()).unwrap();
    std::fs::write(&f, "x").unwrap();
    tx.send(EngineMsg::Ended).await.unwrap();
    for _ in 0..50 {
        if !media::uploads_dir("cdash-hub-end").exists() { return; }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("the session's uploads are still there");
}
```

Tests that create files under `uploads_dir(..)` use a session name no other test uses, because ending a hub deletes its folder.

- [ ] **Step 2: Run** `cargo test -p cdash-agent chat::`; expect a compile failure.
- [ ] **Step 3: Implement.**
  - `Queue::hold` stores the ids; update `queue.rs`'s own test calls with `vec![]`.
  - The actor keeps `uploads: HashMap<String, Attachment>`, filled by `Cmd::Upload { id, a }`. `fn resolve(&self, ids: &[String]) -> Result<Vec<Attachment>, &'static str>`: more than `MAX_FILES` ids gives `too many attachments`; an id not recorded, or whose `path` no longer exists, gives `attachment expired`.
  - Send: `sanitize`, but `Err("empty message")` with at least one file becomes the text `""`; then `resolve` (refusing with its error) before Part 1's hold and deliver rules. Every delivery (direct, Send now, automatic release) resolves the ids again at that moment and passes the attachments to `engine.send`; an id that expired meanwhile refuses that message with `attachment expired` and it leaves the queue.
  - `state()`: each held entry gains `files` built from `uploads`; ids no longer recorded are left out.
  - A `ClientFrame::Upload` reaching the actor is ignored.
  - `end()` also runs `tokio::spawn(tokio::fs::remove_dir_all(media::uploads_dir(&self.name)))`, result ignored.
  - `ClaudeEngine::send` and `NativeEngine::send` take `_files: &[Attachment]` and ignore it until Tasks 5 and 7. Every other `impl Engine for` gets the new signature; every `ClientFrame::Send` literal gets `files: vec![]`.
- [ ] **Step 4: Run** `cargo test -p cdash-agent`; expect all passed. Run clippy.
- [ ] **Step 5: Commit** the files changed: `feat(chat): attachments in send frames, the hub and the engine interface`.

### Task 3: Uploads on the chat socket

**Files:**
- Modify: `crates/agent/src/chat/media.rs` (`Upload`), `crates/agent/src/chat/ws.rs`

**Interfaces:**
- Consumes: Task 1's `sanitize_name`, `image_name`, `sniff`, `image_ext`, `private_dirs`, `uploads_dir`, `MAX_UPLOAD`; Task 2's `Attachment`, `Kind`, `Hub::add_upload`, `hub::new_epoch`.
- Produces:
  - `pub struct Upload` (its folder is `uploads_dir(session)/<id>`; the bytes go to `.part` until finished) with `pub async fn begin(session: &str, name: &str, size: u64) -> Result<Upload, String>`, `pub async fn write(&mut self, chunk: &[u8]) -> Result<bool, String>` (`Ok(true)` once all declared bytes arrived), `pub async fn finish(self) -> Result<(String, Attachment), String>`; `impl Drop for Upload` removes the folder unless `finish` succeeded.
  - Errors: `empty file` (size 0) and `file over 20 MB` (size over `MAX_UPLOAD`), both before anything is created; `more bytes than declared` (folder removed); `upload failed` (any I/O error).
  - `ws.rs` replies `{"t":"uploaded","ref","id","name","size","kind","mime"}` or `{"t":"uploaded","ref","error"}`; a second `upload` while one is open gets `one upload at a time` and the open one carries on; binary frames with no open upload are ignored; a socket that closes drops, and so deletes, its open upload. Log lines: `chat upload <session>` and `chat upload refused <session>: <error>`.

- [ ] **Step 1: Write the failing tests.** In `media.rs`:

```rust
#[tokio::test]
async fn an_upload_over_the_limit_or_empty_is_refused_before_anything_is_written() {
    assert_eq!(Upload::begin("cdash-up-limit", "big.bin", MAX_UPLOAD + 1).await.err().as_deref(), Some("file over 20 MB"));
    assert_eq!(Upload::begin("cdash-up-limit", "e.txt", 0).await.err().as_deref(), Some("empty file"));
    assert!(!uploads_dir("cdash-up-limit").exists(), "nothing was created");
    drop(Upload::begin("cdash-up-limit", "ok.bin", MAX_UPLOAD).await.unwrap());
    assert_eq!(std::fs::read_dir(uploads_dir("cdash-up-limit")).unwrap().count(), 0, "an unfinished upload leaves no folder");
}
#[tokio::test]
async fn bytes_past_the_declared_size_abort_the_upload_and_delete_it() {
    let mut u = Upload::begin("cdash-up-over", "a.txt", 4).await.unwrap();
    assert_eq!(u.write(b"ab").await, Ok(false));
    assert_eq!(u.write(b"cdef").await, Err("more bytes than declared".to_string()));
    drop(u);
    assert_eq!(std::fs::read_dir(uploads_dir("cdash-up-over")).unwrap().count(), 0);
}
#[tokio::test]
async fn a_finished_upload_is_named_typed_by_its_bytes_and_private() {
    let png = [PNG, b"0123"].concat();
    let mut u = Upload::begin("cdash-up-done", "../My Shot.JPG", png.len() as u64).await.unwrap();
    assert_eq!(u.write(&png[..5]).await, Ok(false));
    assert_eq!(u.write(&png[5..]).await, Ok(true));
    let (id, a) = u.finish().await.unwrap();
    assert!(id.len() == 16 && id.bytes().all(|b| b.is_ascii_hexdigit()));
    assert_eq!(a.path, uploads_dir("cdash-up-done").join(&id).join("My Shot.png"), "a PNG named .JPG is stored as .png");
    assert_eq!((a.kind, a.mime.as_str(), a.name.as_str(), a.size), (Kind::Image, "image/png", "My Shot.png", 12));
    assert_eq!((std::fs::read(&a.path).unwrap(), mode(&a.path)), (png, 0o600));
    for d in [cache_root().join("uploads"), uploads_dir("cdash-up-done"), uploads_dir("cdash-up-done").join(&id)] { assert_eq!(mode(&d), 0o700, "{d:?}"); }
    let mut t = Upload::begin("cdash-up-done", "notes.png", 5).await.unwrap();
    assert_eq!(t.write(b"hello").await, Ok(true));
    let (_, f) = t.finish().await.unwrap();
    assert_eq!((f.kind, f.mime.as_str(), f.name.as_str()), (Kind::File, "application/octet-stream", "notes.png"), "a text file named .png stays a file");
}
```

In `ws.rs` (the existing `chat_server` stub answers `4242|` for any name, so each test uses its own session name):

```rust
#[tokio::test]
async fn an_upload_on_the_chat_socket_returns_an_id_that_send_accepts() {
    let (b, _dir) = chat_server("upload");
    let mut ws = connect(b.addr, "name=cdash-upload-flow", None).await.unwrap();
    next_of(&mut ws, "state").await;
    ws.send(WsMsg::Text(r#"{"t":"upload","ref":"r1","name":"notes.txt","size":11}"#.into())).await.unwrap();
    for part in [&b"hello"[..], b" world"] { ws.send(WsMsg::Binary(part.to_vec().into())).await.unwrap(); }
    let up = next_of(&mut ws, "uploaded").await;
    assert_eq!((up["ref"].as_str(), up["name"].as_str(), up["kind"].as_str(), up["size"].as_u64()), (Some("r1"), Some("notes.txt"), Some("file"), Some(11)));
    let id = up["id"].as_str().unwrap().to_string();
    assert_eq!(std::fs::read(media::uploads_dir("cdash-upload-flow").join(&id).join("notes.txt")).unwrap(), b"hello world");
    ws.send(WsMsg::Text(format!(r#"{{"t":"send","cid":"c1","text":"read it","files":["{id}"]}}"#).into())).await.unwrap();
    assert_eq!(next_of(&mut ws, "ack").await["status"], "sent");
    let log = b.ctx.host.log.lines().join("\n");
    assert!(log.contains("chat upload cdash-upload-flow") && !log.contains("notes.txt") && !log.contains("read it"));
}
#[tokio::test]
async fn refused_parallel_and_abandoned_uploads_leave_nothing_behind() {
    let (b, _dir) = chat_server("upload-refused");
    let mut ws = connect(b.addr, "name=cdash-upload-gone", None).await.unwrap();
    next_of(&mut ws, "state").await;
    let text = |s: String| WsMsg::Text(s.into());
    ws.send(text(format!(r#"{{"t":"upload","ref":"r1","name":"big.bin","size":{}}}"#, media::MAX_UPLOAD + 1))).await.unwrap();
    assert_eq!(next_of(&mut ws, "uploaded").await, json!({"t":"uploaded","ref":"r1","error":"file over 20 MB"}));
    ws.send(text(r#"{"t":"upload","ref":"r2","name":"a.txt","size":4}"#.into())).await.unwrap();
    ws.send(WsMsg::Binary(b"abcdef".to_vec().into())).await.unwrap();
    assert_eq!(next_of(&mut ws, "uploaded").await, json!({"t":"uploaded","ref":"r2","error":"more bytes than declared"}));
    ws.send(text(r#"{"t":"upload","ref":"r3","name":"b.txt","size":10}"#.into())).await.unwrap();
    ws.send(text(r#"{"t":"upload","ref":"r4","name":"c.txt","size":10}"#.into())).await.unwrap();
    assert_eq!(next_of(&mut ws, "uploaded").await, json!({"t":"uploaded","ref":"r4","error":"one upload at a time"}));
    ws.send(WsMsg::Binary(b"abc".to_vec().into())).await.unwrap();
    drop(ws); // the socket closes in the middle of r3
    let dir = media::uploads_dir("cdash-upload-gone");
    for _ in 0..50 {
        if std::fs::read_dir(&dir).map(|d| d.count() == 0).unwrap_or(true) { return; }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("an abandoned upload is still on disk");
}
```

- [ ] **Step 2: Run** `cargo test -p cdash-agent chat::media chat::ws`; expect failures.
- [ ] **Step 3: Implement.**
  - `begin`: refuse first; `id = new_epoch()`; `private_dirs(&folder)`; create `.part` with `create_new` and mode 0600, then `set_permissions` 0600.
  - `write`: when `got + chunk.len() > size`, remove the folder and return the error; else append, keep the first 16 bytes in `head`, and return `Ok(got == size)`.
  - `finish`: `mime = sniff(&head).unwrap_or("application/octet-stream")`; `kind` is `Image` exactly when `image_ext(mime)` is known; the name is `sanitize_name(raw)`, passed through `image_name(…, ext)` for images; rename `.part` to it; disarm `Drop`.
  - `ws::pump` gains `name: &str` and `log: &LogBuffer` and holds `Option<(String, Upload)>`. A text frame that parses as `ClientFrame::Upload` is handled here and never reaches `hub.command`. `Message::Binary` goes to `write`; on `Ok(true)`, `finish`, then `hub.add_upload(id.clone(), a.clone()).await`, then the reply.
- [ ] **Step 4: Run** the same command; expect all passed. Run clippy.
- [ ] **Step 5: Commit** `media.rs`, `ws.rs`: `feat(chat): uploads over the chat socket`.

### Task 4: The Claude tap reads image prompts and attached files

**Files:**
- Modify: `crates/agent/src/chat/tap.rs`, `crates/agent/src/chat/testdata/claude.jsonl` (two lines appended), `crates/agent/src/chat/hub.rs` (one test)

**Interfaces:**
- Consumes: `media::split_attached`.
- Produces: prompt events gain `files` (only when non-empty): images first, as `{"kind":"image","mime":M,"data":B64}` (Task 5 swaps `data` for `ref`), then `{"kind":"file","name":N}` entries from trailing `Attached:` lines. `cdash/queued` text and steered prompts are cleaned the same way.

- [ ] **Step 1: Write the failing tests** in `tap.rs`

```rust
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
```

Append these two lines at the end of `testdata/claude.jsonl` (after any lines Parts 4 and 2 added), the record shape of E2:

```
{"type":"user","uuid":"g1","parentUuid":"q1","isSidechain":false,"promptSource":"typed","imagePasteIds":[1],"message":{"role":"user","content":[{"type":"text","text":"[Image #1] What color is this image?"},{"type":"image","source":{"type":"base64","media_type":"image/png","data":"iVBORw0KGgo="}}]}}
{"type":"user","uuid":"g2","parentUuid":"g1","isSidechain":false,"isMeta":true,"message":{"role":"user","content":[{"type":"text","text":"[Image: source: /tmp/red.png]"}]}}
```

In `the_fixture_becomes_exactly_these_events_in_this_order`, add `"prompt:typed:What color is this image?"` as the last expected tag; in `details_chat_depends_on`, assert the last event's `files` equals `json!([{"kind":"image","mime":"image/png","data":"iVBORw0KGgo="}])`. In `hub.rs`, with Task 2's helpers:

```rust
#[tokio::test(start_paused = true)]
async fn a_message_with_a_pdf_is_confirmed_by_the_prompt_the_tap_reads_back() {
    let (hub, tx, _) = rig();
    let pdf = media::uploads_dir("cdash-hub-pdf").join("0123456789abcdef/r.pdf"); // contains /cdash/uploads/
    std::fs::create_dir_all(pdf.parent().unwrap()).unwrap();
    std::fs::write(&pdf, b"%PDF-1.7").unwrap();
    hub.add_upload("pppp".into(), Attachment { path: pdf.clone(), kind: Kind::File, mime: "application/pdf".into(), name: "r.pdf".into(), size: 8 }).await;
    let mut sub = hub.subscribe(None, None).await.unwrap();
    assert_eq!(ack_of(hub.command(send_with("c1", "read this", &["pppp"])).await)["status"], "sent");
    let typed = json!({"type":"user","uuid":"u1","parentUuid":"p","promptSource":"typed","message":{"role":"user","content":format!("read this\nAttached: {}", pdf.display())}});
    for ev in crate::chat::tap::Tap::default().feed(&typed.to_string()) { tx.send(EngineMsg::Ev(ev)).await.unwrap(); }
    tokio::time::sleep(CONFIRM_AFTER + Duration::from_secs(1)).await;
    let f = frames(&mut sub);
    assert!(f.iter().any(|x| x["ev"]["cid"] == "c1" && x["ev"]["files"] == json!([{"kind":"file","name":"r.pdf"}])));
    assert!(!f.iter().any(|x| x["t"] == "ack" && x["status"] == "unconfirmed"), "confirmed, not unconfirmed");
}
```

- [ ] **Step 2: Run** `cargo test -p cdash-agent chat::tap chat::hub`; expect failures.
- [ ] **Step 3: Implement** in `tap.rs`:
  - `fn parts(content: &Value) -> Option<(String, Vec<Value>)>`: a string is its own text; a block list without `tool_result` blocks gives its `text` blocks joined with `""` and its `image` blocks whose `source.type` is `base64` as `{kind:"image", mime: source.media_type, data: source.data}`; anything else is `None`.
  - `fn strip_image_tokens(text: &str) -> String`: removes each `[Image #<digits>]` and one space right after it.
  - Rule 6: a list holding a `tool_result` block, or a text block starting `[Request interrupted by user`, is handled as today. Any other list goes through `parts`, then down the string-prompt path (rule 7 with rule 8's retract) carrying its images; a list with no text and no usable image gives nothing.
  - Rule 7, for strings and lists alike: `strip_image_tokens` first, then today's checks and `unwrap_pastes`, then `split_attached`, then trim. `prompt_event` gains a `files: Vec<Value>` argument and sets the key only when it is non-empty (images, then file entries).
  - Rules 2 and 3 read `queue-operation.content` and `attachment.prompt` through `parts` (string or block list) and apply the same cleaning; the steered prompt carries files.
- [ ] **Step 4: Run** the same command; expect all passed. Run clippy.
- [ ] **Step 5: Commit** `tap.rs`, `testdata/claude.jsonl`, `hub.rs`: `feat(chat): Claude tap shows image prompts and attached files`.

### Task 5: The Claude engine: images become chips, files become paths, history images become refs

**Files:**
- Modify: `crates/agent/src/chat/tmux_io.rs`, `crates/agent/src/chat/claude.rs`, `CLAUDE_FINDINGS.md`

**Interfaces:**
- Consumes: `media::{with_attached, stash, media_dir}`, `Attachment`, `Kind`.
- Produces:
  - `tmux_io::paste_raw(runner: &Runner, name: &str, text: &str) -> Result<(), String>` (load-buffer and bracketed paste-buffer, no Enter; `paste` becomes `paste_raw`, 400 ms, Enter).
  - `tmux_io::capture(runner: &Runner, name: &str) -> Result<String, String>` (`tmux capture-pane -p -t =<name>:`).
  - `tmux_io::input_chips(screen: &str) -> usize`: the `[Image #<digits>]` tokens from the last line whose trimmed start is `❯` to the end of the screen (the input box; the conversation above it also shows chips), or on the whole screen when no such line exists.
  - `tmux_io::attach_image(runner: &Runner, name: &str, path: &Path, wait: Duration) -> Result<(), String>`; `tmux_io::CHIP_POLL: Duration = 50 ms`.
  - `claude::CHIP_WAIT: Duration = 5 s`; `ClaudeEngine` gains `chip_wait: Duration` (`start` sets `CHIP_WAIT`).
  - `claude::stash_images(ev: Value, log: &LogBuffer) -> Value`.
  - Test helpers in `tmux_io::tests`: Part 4's `install_stub(dir, "tmux", body)` (reuse it; do not add another writer) and `chip_stub(dir: &Path, after_paste: Option<&str>) -> String`: its `capture-pane` prints `<dir>/screen` (`❯ ` at first), its `load-buffer` appends the pasted file to `<dir>/pastes` followed by `\n--\n`, and its `paste-buffer` moves `<dir>/after-paste` (written from `after_paste`) over `<dir>/screen`.

- [ ] **Step 1: Write the failing tests.** In `tmux_io.rs`:

```rust
#[test]
fn chips_are_counted_in_the_input_box_only() {
    let screen = "❯ [Image #1] What color?\n  ⎿  [Image #1]\n● Red.\n────\n❯ [Image #2] [Image #3]\n────\n  ⏵⏵ bypass permissions on";
    assert_eq!(input_chips(screen), 2, "the conversation above the box does not count");
    assert_eq!(input_chips("❯ \n────\n  ⏵⏵ bypass permissions on"), 0);
    assert_eq!(input_chips("[Image #7] and no prompt line"), 1, "no box found: the whole screen counts");
}
#[tokio::test]
async fn an_image_path_is_pasted_alone_and_attach_waits_for_its_chip() {
    let dir = tempfile_path("chip-ok");
    std::fs::create_dir_all(&dir).unwrap();
    let runner = Runner::new(chip_stub(&dir, Some("❯ [Image #3]")), Arc::new(LogBuffer::new()));
    attach_image(&runner, "cdash-a", Path::new("/u/up/Shot 1.png"), Duration::from_secs(2)).await.unwrap();
    assert_eq!(std::fs::read_to_string(dir.join("pastes")).unwrap(), "/u/up/Shot 1.png\n--\n");
    let args = std::fs::read_to_string(dir.join("tmux-args")).unwrap();
    assert!(args.starts_with("capture-pane -p -t =cdash-a:"));
    assert!(args.contains("paste-buffer -p -d -b cdash-chat-cdash-a -t =cdash-a:") && !args.contains("send-keys"), "no Enter");
}
#[tokio::test]
async fn an_image_that_never_shows_a_chip_fails_within_the_wait() {
    let dir = tempfile_path("chip-none");
    std::fs::create_dir_all(&dir).unwrap();
    let runner = Runner::new(chip_stub(&dir, None), Arc::new(LogBuffer::new()));
    let started = std::time::Instant::now();
    assert_eq!(attach_image(&runner, "cdash-a", Path::new("/u/a.png"), Duration::from_millis(300)).await, Err("image did not attach".to_string()));
    assert!(started.elapsed() < Duration::from_secs(2));
}
```

In `claude.rs`, with `fn att(path: &str, kind: Kind) -> Attachment` (mime `image/png` or `application/pdf`, name the base name, size 8) and `fn engine(ctx: &Arc<Ctx>, wait: Duration) -> ClaudeEngine` (built directly, name `cdash-a`):

```rust
#[tokio::test]
async fn images_go_first_then_the_text_with_attached_lines_then_enter() {
    let (ctx, dir) = fixture_with("send-img", "");
    chip_stub(&dir, Some("❯ [Image #1]"));
    let files = [att("/u/up/a.png", Kind::Image), att("/u/up/r.pdf", Kind::File)];
    assert_eq!(engine(&ctx, Duration::from_secs(2)).send("what is this", false, "c1", &files).await, Ok(false), "the transcript confirms it");
    assert_eq!(std::fs::read_to_string(dir.join("pastes")).unwrap(), "/u/up/a.png\n--\n what is this\nAttached: /u/up/r.pdf\n--\n");
    assert_eq!(std::fs::read_to_string(dir.join("tmux-args")).unwrap().lines().last(), Some("send-keys -t =cdash-a: Enter"));
}
#[tokio::test]
async fn an_image_that_does_not_attach_clears_the_box_and_nothing_is_sent() {
    let (ctx, dir) = fixture_with("send-noimg", "");
    chip_stub(&dir, None);
    assert_eq!(engine(&ctx, Duration::from_millis(300)).send("x", false, "c1", &[att("/u/up/a.png", Kind::Image)]).await, Err("image did not attach".to_string()));
    let args = std::fs::read_to_string(dir.join("tmux-args")).unwrap();
    assert!(args.lines().last().unwrap().starts_with("send-keys -t =cdash-a: C-u") && !args.contains("Enter"));
}
#[tokio::test]
async fn image_blocks_reach_the_hub_as_media_refs() {
    use base64::Engine as _;
    let (ctx, dir) = fixture_with("stash", "");
    let png = [crate::chat::media::tests::PNG, b"engine"].concat();
    let b64 = |b: &[u8]| base64::engine::general_purpose::STANDARD.encode(b);
    let line = json!({"type":"user","uuid":"g9","parentUuid":"s2","promptSource":"typed","message":{"role":"user","content":[
        {"type":"text","text":"[Image #1] [Image #2] see"},
        {"type":"image","source":{"type":"base64","media_type":"image/png","data":b64(&png)}},
        {"type":"image","source":{"type":"base64","media_type":"image/png","data":b64(b"not an image")}}]}});
    append(&dir.join("projects/-proj/sid-1.jsonl"), &format!("{line}\n"));
    let (_engine, mut rx) = start(ctx, "cdash-a".into(), 4242);
    let got = collect_until_ready(&mut rx).await;
    let ev = got.iter().find_map(|m| match m { EngineMsg::Ev(e) if e["uuid"] == "g9" => Some(e), _ => None }).unwrap();
    let r = format!("{:x}", <sha2::Sha256 as sha2::Digest>::digest(&png));
    assert_eq!(ev["files"], json!([{"kind":"image","ref":r,"mime":"image/png"}, {"kind":"file","name":"image"}]));
    assert!(crate::chat::media::media_dir().join(format!("{r}.png")).exists());
}
```

- [ ] **Step 2: Run** `cargo test -p cdash-agent chat::tmux_io chat::claude`; expect failures.
- [ ] **Step 3: Implement.**
  - `attach_image`: `before = input_chips(&capture(..)?)`; `paste_raw` the path, alone; then every `CHIP_POLL` capture again until `input_chips == before + 1` (`Ok`), or until `wait` has passed (`Err("image did not attach")`). A failing `capture` counts as not attached.
  - `ClaudeEngine::send` keeps Part 2's slash-command routing in front (pickers, refused commands): a message routed there never reaches the attachment path. Only a plain prompt goes on:
  - `ClaudeEngine::send`: split `files` into images and others; `body = with_attached(text, other paths)`. With no images, `paste(body)` as in Part 1. Otherwise `attach_image` each image in order; on the first failure call `self.clear_input(&<the image paths joined by \n>)` and return `Err("image did not attach")`; then `paste(" " + body)`. Return `Ok(false)` as today (the transcript confirms).
  - `stash_images`: for each `files` entry carrying `data`, decode it (`base64` STANDARD) and `media::stash(&bytes, mime)`; success gives `{kind:"image", ref, mime}`, failure gives `{kind:"file", name:"image"}` and logs `chat image not cached <session>`. `run` passes every event through it before `EngineMsg::Ev`. `// ponytail: decodes, hashes and writes on the engine task; move to spawn_blocking if large transcripts stall the tick.`
- [ ] **Step 4: Run** the same command and the whole agent suite; expect all passed. Run clippy.
- [ ] **Step 5: Record the facts** under "Dashboard chat tap" in `CLAUDE_FINDINGS.md`, as observed on 2.1.323 and not a supported contract: a bracketed paste of a line holding only an image's absolute path attaches it as `[Image #N]` (within 32 ms; spaces in the path are fine); `N` counts up across the session; a path followed by text on the same line stays text; an Enter sent before the chip is drawn is dropped; the record's content is a block list, text (`[Image #N] <text>`) then `{type:"image", source:{type:"base64", media_type, data}}`, with `imagePasteIds`, followed by an `isMeta` record `[Image: source: <path>]`.
- [ ] **Step 6: Commit** `tmux_io.rs`, `claude.rs`, `CLAUDE_FINDINGS.md`: `feat(chat): Claude sends images as pasted chips and caches history images`.

### Task 6: The media route

**Files:**
- Modify: `crates/agent/src/http/routes.rs` (handler and tests), `crates/agent/src/http/serve.rs` (route)

**Interfaces:**
- Consumes: `media::{media_dir, stash}`.
- Produces: `pub async fn get_chat_media(axum::extract::Path(reference): axum::extract::Path<String>) -> Response`; `get "/api/chat/media/{ref}" => routes::get_chat_media` inside `guarded_routes!`, so it sits behind the existing auth and joins `GUARDED_PATHS`.

- [ ] **Step 1: Write the failing test** in `routes.rs`, with `async fn raw_status(addr: SocketAddr, path: &str) -> u16`, which writes `GET <path> HTTP/1.1`, `Host` and `Connection: close` on a `tokio::net::TcpStream` and parses the status line, because reqwest resolves `..` and `%2e%2e` before sending:

```rust
#[tokio::test]
async fn chat_media_is_served_only_by_hash_and_only_as_an_image() {
    let b = serve(cfg_for(tempdir("media"))).await.unwrap();
    let png = [crate::chat::media::tests::PNG, b"route"].concat();
    let r = crate::chat::media::stash(&png, "image/png").unwrap();
    let res = reqwest::get(format!("http://{}/api/chat/media/{r}", b.addr)).await.unwrap();
    assert_eq!(res.status(), 200);
    for (h, v) in [("content-type", "image/png"), ("x-content-type-options", "nosniff"),
        ("content-security-policy", "default-src 'none'"), ("cache-control", "private, max-age=31536000, immutable")] {
        assert_eq!(res.headers()[h], v, "{h}");
    }
    assert_eq!(res.bytes().await.unwrap().as_ref(), png.as_slice());
    for bad in ["..".to_string(), "%2e%2e".into(), r.to_uppercase(), r[..63].into(), format!("{r}0"), "..%2f..%2fetc%2fpasswd".into()] {
        assert_eq!(raw_status(b.addr, &format!("/api/chat/media/{bad}")).await, 400, "{bad}");
    }
    assert_eq!(raw_status(b.addr, &format!("/api/chat/media/{}", "0".repeat(64))).await, 404);
}
```

- [ ] **Step 2: Run** `cargo test -p cdash-agent chat_media`; expect failure.
- [ ] **Step 3: Implement.** 400 unless the ref is 64 characters of `0-9a-f`; try `media_dir()/<ref>.{png,jpg,gif,webp}` in that order with `tokio::fs::read`; the first hit answers 200 with the four headers (content type from the extension that matched); none gives 404. `// ponytail: reads the whole file per request; stream it if large originals are viewed often.`
- [ ] **Step 4: Run** `cargo test -p cdash-agent` and `cargo test -p cdash-agent --test auth_integration --test password_integration` (the new path joins the bypass checks); expect all passed. Run clippy.
- [ ] **Step 5: Commit** `routes.rs`, `serve.rs`: `feat(chat): media route for cached history images`.

### Task 7: Native sessions: attachment paths, the media folder, and image-sized Codex messages

**Files:**
- Modify: `crates/agent/src/chat/native.rs`, `crates/agent/src/collect/codex.rs`, `crates/agent/src/collect/opencode.rs`, `crates/agent/src/collect/codex_proxy.rs`

**Interfaces:**
- Consumes: `Attachment`, `Kind`, `media::media_dir`.
- Produces:
  - The `send` line gains `files` (`#[serde(skip_serializing_if = "<[Attachment]>::is_empty")] files: &'a [Attachment]` on `SendFrame`).
  - Every Codex and OpenCode pane starts with `-e CDASH_MEDIA_DIR=<media_dir()>` on its `tmux new-session` (the contract names `spawn.rs`, but these launches live in `codex::launch` and `opencode::launch`; Claude panes get nothing).
  - `codex_proxy::config() -> WebSocketConfig` with `max_message_size(None)` and `max_frame_size(None)`, used on both connect paths; `async fn unix(path: &OsStr) -> Result<WebSocketStream<UnixStream>, Box<dyn Error>>`; `pump(socket, input: impl AsyncRead + Unpin, output: impl AsyncWrite + Unpin)` (`forward` passes stdin and stdout). Codex's app-server echoes each user message, data URLs included (up to 32 MiB of images), to every connection, and `thread/turns/list` returns them too; tungstenite's default 16 MiB frame limit makes the proxy exit, which takes the supervisor and the Remote Control worker down with it. What the worker sends is unchanged.

- [ ] **Step 1: Write the failing tests.** In `native.rs`:

```rust
#[tokio::test]
async fn attachments_travel_to_the_supervisor_as_paths_never_bytes() {
    let (sock, mut sup) = fake_supervisor("files");
    let (engine, _rx) = start(sock, log(), "cdash-a".into());
    sup.read_line().await;
    sup.write_line(r#"{"t":"hello","epoch":"e1","head":0}"#).await;
    let files = [Attachment { path: "/u/up/a.png".into(), kind: Kind::Image, mime: "image/png".into(), name: "a.png".into(), size: 8 }];
    let sending = tokio::spawn(async move { engine.send("look", false, "c3", &files).await });
    assert_eq!(sup.read_line().await, r#"{"t":"send","cid":"c3","text":"look","steer":false,"files":[{"path":"/u/up/a.png","kind":"image","mime":"image/png","name":"a.png","size":8}]}"#);
    sup.write_line(r#"{"t":"ack","cid":"c3","ok":true}"#).await;
    assert_eq!(sending.await.unwrap(), Ok(true));
}
```

In `codex.rs`, in the test that asserts `CDASH_CODEX_AGENT=`, and in `opencode.rs`'s `opencode_launch_keeps_full_model_identity_and_validates_variant`, add `assert!(args.contains(&format!("-e CDASH_MEDIA_DIR={}", crate::chat::media::media_dir().display())));`. In `codex_proxy.rs`:

```rust
#[tokio::test]
async fn an_app_server_message_over_the_websocket_defaults_reaches_the_bridge_whole() {
    let dir = std::env::temp_dir().join(format!("cdash-proxy-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("app.sock");
    let listener = tokio::net::UnixListener::bind(&path).unwrap();
    let big = "x".repeat(24 * 1024 * 1024); // one frame, over tungstenite's 16 MiB default
    let sent = big.clone();
    tokio::spawn(async move {
        let (s, _) = listener.accept().await.unwrap();
        let mut ws = tokio_tungstenite::accept_async(s).await.unwrap();
        ws.send(Message::text(sent)).await.unwrap();
        ws.close(None).await.unwrap();
    });
    let (_keep_open, input) = tokio::io::duplex(64); // stdin that never ends
    let mut out = Vec::new();
    pump(unix(path.as_os_str()).await.unwrap(), input, &mut out).await.unwrap();
    assert_eq!(out.len(), big.len() + 1);
}
```

- [ ] **Step 2: Run** `cargo test -p cdash-agent chat::native codex opencode codex_proxy`; expect failures.
- [ ] **Step 3: Implement** as the Interfaces say. `NativeEngine::send` passes `files` into `SendFrame`. In `codex_proxy.rs`: `client_async_with_config(…, Some(config()))` and `connect_async_with_config(…, Some(config()), false)`, with `// ponytail: no read limit; the peer is Codex's own app-server on a private socket. Cap it if this proxy ever faces another peer.`
- [ ] **Step 4: Run** the same command and the whole agent suite; expect all passed. Run clippy.
- [ ] **Step 5: Commit** the four files: `feat(chat): native sessions get attachment paths and the media folder`.

## Phase B: the supervisors

### Task 8: Shared supervisor helpers in `chat-feed.mjs`

**Files:**
- Modify: `crates/agent/src/collect/chat-feed.mjs`, `test/chat-feed.test.mjs`

**Interfaces:**
- Produces, exported from `chat-feed.mjs` (both supervisors already ship and import it):
  - `withAttached(text: string, paths: string[]) -> string` and `splitAttached(text: string) -> { text: string, files: {kind: 'file', name: string}[] }`, the same rule as Task 1's Rust pair;
  - `sniffImage(bytes: Uint8Array) -> 'image/png' | 'image/jpeg' | 'image/gif' | 'image/webp' | null`;
  - `stashMedia(dir: string | undefined, bytes: Buffer) -> Promise<{ ref: string, mime: string } | null>`: `null` when `dir` is unset (a pane started without `CDASH_MEDIA_DIR`) or the bytes are not one of the four images; otherwise as Task 1's `stash`: `mkdir` recursive with mode 0o700 then `chmod` 0o700, `<ref>.<ext>` written through a `.tmp` name with flag `wx` and mode 0o600, `chmod` 0o600, renamed; an existing file gets `utimes` now.
  - `ChatFeed` calls `send(text, { steer, cid, files })`: `files` is the line's `files` array without entries lacking a string `path`, `[]` when absent.

- [ ] **Step 1: Write the failing tests** in `test/chat-feed.test.mjs`

```js
const PNG = Buffer.concat([Buffer.from('89504e470d0a1a0a', 'hex'), Buffer.from('feed')]);
test('upload lines round-trip and nothing else is touched', () => {
  const up = '/home/u/.cache/cdash/uploads/cdash-a/0123456789abcdef';
  const sent = withAttached('look at these', [`${up}/r.pdf`, `${up}/notes 1.txt`]);
  assert.equal(sent, `look at these\nAttached: ${up}/r.pdf\nAttached: ${up}/notes 1.txt`);
  assert.deepEqual(splitAttached(sent), { text: 'look at these', files: [{ kind: 'file', name: 'r.pdf' }, { kind: 'file', name: 'notes 1.txt' }] });
  assert.deepEqual(splitAttached(withAttached('', [`${up}/r.pdf`])), { text: '', files: [{ kind: 'file', name: 'r.pdf' }] });
  for (const kept of ['Attached: /etc/passwd', 'Attached: rel/cdash/uploads/x', `Attached: ${up}/a.txt\nthen more`])
    assert.deepEqual(splitAttached(kept), { text: kept, files: [] });
});
test('images are stored once by content in a private folder, and nothing else is', async t => {
  const root = await mkdtemp(join(tmpdir(), 'cdash-media-')); t.after(() => rm(root, { recursive: true, force: true }));
  const dir = join(root, 'cdash', 'media');
  const a = await stashMedia(dir, PNG);
  assert.deepEqual(a, { ref: createHash('sha256').update(PNG).digest('hex'), mime: 'image/png' });
  const file = join(dir, a.ref + '.png');
  assert.deepEqual([(await stat(file)).mode & 0o777, (await stat(dir)).mode & 0o777], [0o600, 0o700]);
  const past = new Date(Date.now() - 86_400_000);
  await utimes(file, past, past);
  await stashMedia(dir, PNG);
  assert.ok((await stat(file)).mtimeMs > past.getTime() + 60_000, 'a rewrite refreshes its age');
  assert.deepEqual(await readdir(dir), [a.ref + '.png'], 'one file, no temp left');
  assert.equal(await stashMedia(dir, Buffer.from('%PDF-1.7')), null);
  assert.equal(await stashMedia(undefined, PNG), null, 'no folder from the agent, no media');
  assert.equal(sniffImage(Buffer.from('RIFF\0\0\0\0WEBPVP8 ')), 'image/webp');
});
test('a send line hands its attachments to the supervisor', async t => {
  const dir = await mkdtemp(join(tmpdir(), 'cdash-feed-')); t.after(() => rm(dir, { recursive: true, force: true }));
  const got = [];
  const feed = new ChatFeed({ send: async (text, o) => { got.push(o.files); }, stop: async () => {} }); t.after(() => feed.close());
  const a = await client(await feed.listen(dir), { after: 0, epoch: null });
  const f = { path: '/u/up/a.png', kind: 'image', mime: 'image/png', name: 'a.png', size: 8 };
  a.write({ t: 'send', cid: 'c1', text: 'x', steer: false, files: [f, { kind: 'file' }] });
  a.write({ t: 'send', cid: 'c2', text: 'y', steer: false });
  await until(() => got.length === 2);
  assert.deepEqual(got, [[f], []]);
  a.close();
});
```

- [ ] **Step 2: Run** `node --test test/chat-feed.test.mjs`; expect failures.
- [ ] **Step 3: Implement** as the Interfaces say.
- [ ] **Step 4: Run** `node --test`; expect all passed.
- [ ] **Step 5: Commit** both files: `feat(chat): shared attachment helpers for the supervisors`.

### Task 9: Codex supervisor: data URLs out, refs in

**Order with Part 2:** the supervisor's slash-command routing (Part 2) runs before any of this; only a plain prompt reaches the attachment handling below.

**Files:**
- Modify: `crates/agent/src/collect/codex-remote.mjs` (`nativeLauncher`'s feed, `chatMessages`, a new `chatPrompt`, `replayHistory`'s signature), `test/codex-native.test.mjs`

**Interfaces:**
- Consumes: Task 8's helpers; `process.env.CDASH_MEDIA_DIR`.
- Produces:
  - `const CODEX_IMAGES_MAX = 24 * 1024 * 1024`.
  - `async function chatPrompt(item) -> { text, files }` for a `userMessage` item.
  - `async function chatMessages(m, sid)` (now async; the `control` handler awaits it).
  - `replayHistory(codex, threadId, remoteSid, emit, submitted = new Set(), shapeUser = textOnly)`, with `shapeUser(item) -> Promise<{ content, files? }>`. The default returns today's text blocks and skips an item with none, so `worker()`'s call, with five arguments, sends exactly what it sends today.

- [ ] **Step 1: Write the failing tests.** In the fixture: add `CDASH_MEDIA_DIR: join(dir, 'media')` to the supervisor's env; give the fake's `thread/turns/list` user item `u1` a second block `{type:'image',url:'data:image/png;base64,<PNG>'}` (the existing chat test still sees one `user` and one `assistant` event); and make the local `client()` also keep `sizes`, the length of every raw line. Then:

```js
const PNG = Buffer.concat([Buffer.from('89504e470d0a1a0a', 'hex'), Buffer.from('hist')]);  // also the fixture's u1 image
const sha = b => createHash('sha256').update(b).digest('hex');
test('chat sends Codex images as data URLs and other files as paths, within its image budget', { timeout: 20000 }, async t => {
  const f = await fixture(t);
  await f.wait(() => f.native()?.chat_sock);
  const c = await client(f.native().chat_sock, { after: 0, epoch: null });
  await until(() => c.lines.some(l => l.t === 'ready'));
  const png = join(f.dir, 'a.png'); await writeFile(png, PNG);
  const image = { path: png, kind: 'image', mime: 'image/png', name: 'a.png', size: PNG.length };
  c.write({ t: 'send', cid: 'c1', text: 'look', steer: false, files: [image, { path: '/u/up/r.pdf', kind: 'file', mime: 'application/pdf', name: 'r.pdf', size: 9 }] });
  await until(() => f.rpc('turn/start'));
  assert.deepEqual(f.rpc('turn/start').params.input, [{ type: 'text', text: 'look\nAttached: /u/up/r.pdf', text_elements: [] },
    { type: 'image', url: 'data:image/png;base64,' + PNG.toString('base64') }]);
  const big = { ...image, size: 13 * 1024 * 1024 };
  c.write({ t: 'send', cid: 'c2', text: 'two big ones', steer: false, files: [big, big] });
  await until(() => c.lines.some(l => l.t === 'ack' && l.cid === 'c2'));
  assert.deepEqual(c.lines.find(l => l.t === 'ack' && l.cid === 'c2'), { t: 'ack', cid: 'c2', ok: false, error: 'too many images for one Codex message' });
  c.close();
});
test('image prompts from the native TUI and from history reach chat as cache refs, never as bytes', { timeout: 20000 }, async t => {
  const f = await fixture(t, undefined, process.platform, true);
  await f.wait(() => f.native()?.chat_sock);
  const c = await client(f.native().chat_sock, { after: 0, epoch: null });
  await until(() => c.lines.some(l => l.t === 'ready'));
  const history = c.lines.find(l => l.t === 'ev' && l.ev.type === 'user').ev;
  assert.deepEqual([history.message.content[0].text, history.files], ['hello', [{ kind: 'image', ref: sha(PNG), mime: 'image/png' }]]);
  const huge = Buffer.concat([PNG, Buffer.alloc(300 * 1024)]);
  const up = '/home/u/.cache/cdash/uploads/cdash-test/0123456789abcdef';
  await f.inject({ method: 'item/completed', params: { turnId: 't2', item: { id: 'u2', type: 'userMessage', content: [
    { type: 'text', text: `see\nAttached: ${up}/r.pdf` }, { type: 'image', url: 'data:image/png;base64,' + huge.toString('base64') },
    { type: 'localImage', path: join(f.dir, 'gone.png') }] } } });
  await until(() => c.lines.some(l => l.t === 'ev' && l.ev.message?.id === 'u2'));
  const ev = c.lines.find(l => l.t === 'ev' && l.ev.message?.id === 'u2').ev;
  assert.equal(ev.message.content[0].text, 'see');
  assert.deepEqual(ev.files, [{ kind: 'image', ref: sha(huge), mime: 'image/png' }, { kind: 'file', name: 'gone.png' }, { kind: 'file', name: 'r.pdf' }]);
  assert.ok(existsSync(join(f.dir, 'media', sha(huge) + '.png')));
  assert.ok(c.sizes.every(n => n < 64 * 1024), 'no line on the agent socket carries image bytes');
  c.close();
});
```

- [ ] **Step 2: Run** `node --test test/codex-native.test.mjs`; expect the new tests to fail.
- [ ] **Step 3: Implement.**
  - `chatPrompt(item)`: `splitAttached` of the text blocks joined with `''`. Each `image` block whose `url` starts with `data:` is decoded from the base64 after the comma and passed to `stashMedia(process.env.CDASH_MEDIA_DIR, bytes)`: `{kind:'image', ref, mime}`, or `{kind:'file', name:'image'}` when that returns `null`; other URLs are skipped. Each `localImage` is read and stashed the same way; a file that is gone or not an image gives `{kind:'file', name: basename(path)}`. `files` is the images in content order, then the `Attached:` entries.
  - `chatMessages`: a completed `userMessage` becomes `{type:'user', message:{id, role:'user', content:[{type:'text', text}]}, files?}` (key only when non-empty), emitted when there is text or there are files. The chat replay passes `shapeUser = async item => { const p = await chatPrompt(item); return { content: [{ type: 'text', text: p.text }], ...(p.files.length ? { files: p.files } : {}) }; }`, and `replayHistory` puts the returned `files` on the emitted message.
  - The feed's `send(text, { steer, files })`: refuse with `Error('too many images for one Codex message')` when the `size`s of the image files add up to more than `CODEX_IMAGES_MAX`, before reading anything; `input` is a text block holding `withAttached(text, <paths of the other files>)` (left out when that is empty), then `{type:'image', url:'data:<mime>;base64,<file contents>'}` per image in order. `turn/steer` and `turn/start` take that `input` as they take text today.
- [ ] **Step 4: Run** `node --test`; expect all passed, including the untouched worker tests in `test/codex-remote.test.mjs`.
- [ ] **Step 5: Commit** both files: `feat(chat): Codex chat sends and shows image attachments`.

### Task 10: OpenCode: file parts out, refs in, and the stream and history limits

**Order with Part 2:** the supervisor's slash-command routing (Part 2) runs before any of this; only a plain prompt reaches the attachment handling below.

**Files:**
- Modify: `crates/agent/src/collect/opencode-api.mjs`, `crates/agent/src/collect/opencode-remote.mjs` (`submitPrompt`, a new `chatPrompt`, and `supervise`), `test/opencode-api.test.mjs`, `test/opencode-native.test.mjs`

**Interfaces:**
- Consumes: Task 8's helpers; `process.env.CDASH_MEDIA_DIR`.
- Produces:
  - `OpenCode` instances get `bodyMax = 8 * 1024 * 1024`, `rowMax = 320 * 1024 * 1024` (10 PDFs of 20 MiB are about 267 MiB of base64; V8 strings stop at about 512 MiB) and `eventMax = 1024 * 1024`. `request()` allows `rowMax` for paths under `/api/session/<id>/message` and `bodyMax` for the rest. `messages()` reads a page refused as too large again from the same cursor with `limit: '1'`, then goes back to pages of 100. `subscribe()` skips a frame longer than `eventMax` up to its blank line instead of dropping the stream (`session.inbox.enqueued` always carries the full base64). These also apply to the worker, which today stops on the same limits; what it sends is unchanged.
  - `submitPrompt(client, state, remoteId, text, save, files = [])` puts `files` in the POST body only when non-empty; the worker passes five arguments.
  - `async function chatPrompt(m, rowFiles) -> message`: `m`'s text through `splitAttached`; each row file with an `image/` mime decoded and stashed (`{kind:'image', ref, mime}`); any other row file, or an image that would not stash, becomes `{kind:'file', name: f.name || 'file', size}`, with `size` worked out from the base64 length without decoding; then the `Attached:` entries. `files` is set only when non-empty.

- [ ] **Step 1: Write the failing tests.** In `test/opencode-api.test.mjs`:

```js
test('an event bigger than the limit is skipped and the stream stays up', async () => {
  let connections = 0;
  const server = createServer((req, res) => {
    connections++;
    res.writeHead(200, { 'content-type': 'text/event-stream' });
    res.write('data: {"type":"one"}\n\n'); res.write(`data: {"type":"big","x":"${'y'.repeat(2 * 1024 * 1024)}"}\n\n`); res.write('data: {"type":"two"}\n\n');
  });
  await new Promise(resolve => server.listen(0, '127.0.0.1', resolve));
  try {
    const client = new OpenCode({ url: `http://127.0.0.1:${server.address().port}`, password: 'fixture' });
    const controller = new AbortController(), events = [];
    await client.subscribe(event => { events.push(event.type); if (events.length === 2) controller.abort(); }, () => {}, controller.signal);
    assert.deepEqual([events, connections], [['one', 'two'], 1]);
  } finally { server.closeAllConnections(); await new Promise(resolve => server.close(resolve)); }
});
test('message rows may exceed the body limit, and a page too large is read one row at a time', async () => {
  const rows = ['r1', 'r2'].map(id => ({ id, type: 'user', text: 'x'.repeat(40 * 1024) })), limits = [];
  const server = createServer((req, res) => {
    const url = new URL(req.url, 'http://localhost');
    if (url.pathname.endsWith('/message/r1')) return res.end(JSON.stringify({ data: rows[0] }));
    if (url.pathname === '/api/model') return res.end('x'.repeat(2048));
    limits.push(url.searchParams.get('limit'));
    const start = Number(url.searchParams.get('cursor') ?? 0), limit = Number(url.searchParams.get('limit'));
    res.end(JSON.stringify({ data: rows.slice(start, start + limit), cursor: { next: start + limit < rows.length ? String(start + limit) : null } }));
  });
  await new Promise(resolve => server.listen(0, '127.0.0.1', resolve));
  try {
    const client = new OpenCode({ url: `http://127.0.0.1:${server.address().port}`, password: 'fixture' });
    Object.assign(client, { bodyMax: 1024, rowMax: 64 * 1024 });
    assert.deepEqual((await client.messages('ses_x')).map(r => r.id), ['r1', 'r2']);
    assert.deepEqual(limits, ['100', '1', '100']);
    assert.equal((await client.request('/api/session/ses_x/message/r1')).data.id, 'r1');
    await assert.rejects(client.request('/api/model'), /too large/i);
  } finally { server.closeAllConnections(); await new Promise(resolve => server.close(resolve)); }
});
```

In `test/opencode-native.test.mjs`, the chat `fixture()` passes `CDASH_MEDIA_DIR: join(dir, 'media')`, and with `{ withFiles: true }` also `CDASH_TEST_FILES: '1'`, which makes the fake's message list end with `{id:'msg_f', type:'user', text:'see\nAttached: /h/.cache/cdash/uploads/cdash-fixture/0123456789abcdef/n.txt', files:[{data:<PNG base64>, mime:'image/png', name:'a.png', source:{type:'inline'}}, {data:<PDF base64>, mime:'application/pdf', name:'r.pdf', source:{type:'uri', uri:'file:///x/r.pdf'}}]}`. Then:

```js
const PNG = Buffer.concat([Buffer.from('89504e470d0a1a0a', 'hex'), Buffer.from('row')]), PDF = Buffer.from('%PDF-1.7 tiny');
const sha = b => createHash('sha256').update(b).digest('hex');
test('chat sends OpenCode images and PDFs as file parts and other files as paths', { timeout: 20000 }, async t => {
  const f = await fixture(t);
  await f.wait(() => f.native()?.chat_sock);
  const c = await client(f.native().chat_sock, { after: 0, epoch: null });
  await until(() => c.lines.some(l => l.t === 'ready'));
  const file = (path, kind, mime) => ({ path, kind, mime, name: path.split('/').pop(), size: 8 });
  c.write({ t: 'send', cid: 'c1', text: 'look', steer: false, files: [file('/u/up/a b.png', 'image', 'image/png'),
    file('/u/up/r.pdf', 'file', 'application/pdf'), file('/u/up/n.txt', 'file', 'application/octet-stream')] });
  await until(() => f.posted('/prompt'));
  assert.equal(f.posted('/prompt').text, 'look\nAttached: /u/up/n.txt');
  assert.deepEqual(f.posted('/prompt').files, [{ uri: 'file:///u/up/a%20b.png', name: 'a b.png' }, { uri: 'file:///u/up/r.pdf', name: 'r.pdf' }]);
  c.close();
});
test('a user row with files reaches chat as refs and chips, without its base64', { timeout: 20000 }, async t => {
  const f = await fixture(t, { withFiles: true });
  await f.wait(() => f.native()?.chat_sock);
  const c = await client(f.native().chat_sock, { after: 0, epoch: null });
  await until(() => c.lines.some(l => l.t === 'ev' && l.ev.message?.id === 'msg_f'));
  const ev = c.lines.find(l => l.t === 'ev' && l.ev.message?.id === 'msg_f').ev;
  assert.equal(ev.message.content[0].text, 'see');
  assert.deepEqual(ev.files, [{ kind: 'image', ref: sha(PNG), mime: 'image/png' }, { kind: 'file', name: 'r.pdf', size: PDF.length }, { kind: 'file', name: 'n.txt' }]);
  assert.ok(!JSON.stringify(c.lines).includes(PNG.toString('base64')));
  c.close();
});
```

- [ ] **Step 2: Run** `node --test test/opencode-api.test.mjs test/opencode-native.test.mjs`; expect the new tests to fail.
- [ ] **Step 3: Implement.**
  - `subscribe`: when the buffer passes `eventMax` with no `\n\n` in it, drop it and enter a skip state that discards input (after the same `\r\n` to `\n` rewrite, keeping the last two characters to catch a boundary split across chunks) up to the next `\n\n`, then parse normally again.
  - In `supervise`, inside the existing chat block: `const rowFiles = new Map()`. `ChatBridge` gains `async emitHistory(row, onlyTools)`: when `row.type === 'user'` and it has `files`, `rowFiles.set(row.id, row.files)` before `super.emitHistory(…)` and `rowFiles.delete(row.id)` after, in `finally`, so file data never outlives the call. The sink passes every `user` message that is not a tool result through `chatPrompt(m, rowFiles.get(m.message?.id))` before `feed.accept`. `historyMessages` and `RemoteBridge` stay as they are.
  - The feed's `send(text, { cid, files })`: `parts` are the images and the files whose mime is `application/pdf`, as `{ uri: pathToFileURL(f.path).href, name: f.name }`; the text is `withAttached(text, <paths of the rest>)`; call `submitPrompt(client, chatState, 'dash:' + cid, text, async () => {}, parts)`.
- [ ] **Step 4: Run** `node --test`; expect all passed, including the untouched tests in `test/opencode-remote.test.mjs`.
- [ ] **Step 5: Commit** the four files: `feat(chat): OpenCode chat sends and shows attachments within its stream and history limits`.

## Phase C: the chat screen

### Task 11: Attachments in the store, and the upload queue

**Files:**
- Create: `public/chat/attach.js`, `test/chat-attach.test.mjs`
- Modify: `public/chat/store.js`, `test/chat-store.test.mjs`

**Interfaces:**
- Produces:
  - `cdashChatStore.addMine(s, cid, text, files = [])`; a `user` row gains `files`: from a prompt event, its `files` entries that are an `image` with `ref` matching `/^[0-9a-f]{64}$/` or a `file` with a string `name`; from my own message, the upload entries it was sent with.
  - `globalThis.cdashAttach = { LIMIT: 20 * 1024 * 1024, MAX: 10, CHUNK: 1024 * 1024, Uploads, fmtSize }` (same IIFE shape as `store.js`; DOM only inside functions). `fmtSize(n)` gives `900 B`, `1.5 KB`, `2.5 MB`, `20 MB`.
  - `new Uploads({ send, buffered, onChange })`, where `send(data) -> boolean` is false when the socket is not open, `buffered()` is the socket's `bufferedAmount`, and `onChange()` asks for a render. `items` are `{ key, file?, name, size, kind?, mime?, id?, state: 'queued' | 'uploading' | 'done' | 'failed' | 'refused', progress, error? }`. Methods: `add(file) -> item`, `remove(key)`, `retry(key)`, `restore(files)` (`{id, name, kind, size, mime}` entries become `done` items), `onFrame(f)` (an `uploaded` frame), `lost()`, `ready() -> [{id, name, kind, size, mime}]`, `pending() -> boolean` (any item not `done`), `clear()` (drops every item, abandoning an upload in flight so the next `add` starts afresh, and revokes each `item.url` object URL that was set).
  - Upload order: one file at a time. `{"t":"upload","ref":<item.key>,"name","size"}`, then `CHUNK` slices sent only while `buffered() < CHUNK` (checked every 50 ms), `progress` being the share of frames sent, then it waits for the matching `uploaded` frame. `add` refuses at once, never sending: over `LIMIT` → `Over 20 MB`; size 0 → `Empty file`; when `MAX` items that are not refused already exist → `10 attachments at most`. `lost()` turns `uploading` and `queued` items into `failed` / `Connection lost`; `send` returning false gives `failed` / `Not connected`.

- [ ] **Step 1: Write the failing tests.** In `test/chat-store.test.mjs`:

```js
test('prompt rows carry their attachments and drop malformed ones', () => {
  const ref = 'ab'.repeat(32);
  const s = feed([hello(), ev(1, prompt('u1', 'see', { files: [{ kind: 'image', ref, mime: 'image/png' }, { kind: 'image', ref: '../../api/kill' },
    { kind: 'file', name: 'r.pdf', size: 9 }, { kind: 'file' }] }))]);
  assert.deepEqual(S.rows(s)[0].files, [{ kind: 'image', ref, mime: 'image/png' }, { kind: 'file', name: 'r.pdf', size: 9 }]);
});
test('my attachments show until the prompt event replaces them', () => {
  let s = feed([hello()]);
  const files = [{ id: '0123456789abcdef', name: 'a.png', kind: 'image', size: 8, mime: 'image/png' }];
  S.addMine(s, 'c1', '', files);
  assert.deepEqual(S.rows(s).map(r => [r.text, r.status, r.files]), [['', 'sending', files]]);
  s = S.apply(s, ev(1, prompt('u1', '', { cid: 'c1', files: [{ kind: 'image', ref: 'ab'.repeat(32), mime: 'image/png' }] })));
  assert.deepEqual(S.rows(s).map(r => [r.status, r.files[0].ref]), [['sent', 'ab'.repeat(32)]]);
});
```

In the new `test/chat-attach.test.mjs`:

```js
import test from 'node:test';
import assert from 'node:assert/strict';
await import('../public/chat/attach.js');
const A = globalThis.cdashAttach, MiB = 1024 * 1024;
const until = async (f, ms = 2000) => { const t = Date.now(); while (!f()) { if (Date.now() - t > ms) throw Error('timed out'); await new Promise(r => setTimeout(r, 10)); } };
function rig() {
  const sent = []; let open = true;
  const u = new A.Uploads({ send: d => open && (sent.push(d), true), buffered: () => 0, onChange: () => {} });
  return { u, sent, setOpen: v => { open = v; } };
}
const frames = sent => sent.filter(d => typeof d === 'string').map(d => JSON.parse(d));

test('files go one at a time: the upload frame, 1 MiB frames, then the answer', async () => {
  const { u, sent } = rig();
  const a = u.add(new File([new Uint8Array(2.5 * MiB)], 'a.bin')), b = u.add(new File(['hi'], 'b.txt'));
  await until(() => sent.length === 4);
  assert.deepEqual(JSON.parse(sent[0]), { t: 'upload', ref: a.key, name: 'a.bin', size: 2.5 * MiB });
  assert.deepEqual(sent.slice(1).map(d => d.byteLength), [MiB, MiB, MiB / 2]);
  assert.deepEqual([a.state, a.progress, b.state], ['uploading', 1, 'queued'], 'the next waits for the answer');
  u.onFrame({ t: 'uploaded', ref: a.key, id: '0123456789abcdef', name: 'a.bin', size: 2.5 * MiB, kind: 'file', mime: 'application/octet-stream' });
  await until(() => frames(sent).length === 2);
  u.onFrame({ t: 'uploaded', ref: b.key, error: 'upload failed' });
  assert.deepEqual([b.state, b.error, u.pending()], ['failed', 'upload failed', true]);
  u.remove(b.key);
  assert.deepEqual([u.pending(), u.ready()], [false, [{ id: '0123456789abcdef', name: 'a.bin', kind: 'file', size: 2.5 * MiB, mime: 'application/octet-stream' }]]);
});
test('over 20 MB, an eleventh attachment and empty files are refused on the chip and never sent', async () => {
  const { u, sent } = rig();
  const big = u.add(new File([new Uint8Array(A.LIMIT + 1)], 'big.bin')), empty = u.add(new File([], 'e.txt'));
  for (let i = 0; i < A.MAX; i++) u.add(new File(['x'], `f${i}.txt`));
  const extra = u.add(new File(['x'], 'eleven.txt'));
  assert.deepEqual([big.error, empty.error, extra.error], ['Over 20 MB', 'Empty file', '10 attachments at most']);
  await new Promise(r => setTimeout(r, 50));
  assert.ok(!frames(sent).some(f => ['big.bin', 'e.txt', 'eleven.txt'].includes(f.name)));
});
test('a dropped socket fails what was not finished, Retry sends it again, and restore brings sent ones back', async () => {
  const { u, sent, setOpen } = rig();
  const a = u.add(new File(['abc'], 'a.txt'));
  await until(() => sent.length === 2);
  setOpen(false); u.lost();
  assert.deepEqual([a.state, a.error], ['failed', 'Connection lost']);
  setOpen(true); u.retry(a.key);
  await until(() => frames(sent).length === 2);
  assert.equal(frames(sent)[1].ref, a.key);
  u.clear();
  u.restore([{ id: 'feedfeedfeedfeed', name: 'shot.png', kind: 'image', size: 8, mime: 'image/png' }]);
  assert.deepEqual([u.ready().map(f => f.id), u.pending()], [['feedfeedfeedfeed'], false]);
});
test('sizes read the way the chips show them', () => {
  assert.deepEqual([900, 1536, 2.5 * MiB, 20 * MiB].map(A.fmtSize), ['900 B', '1.5 KB', '2.5 MB', '20 MB']);
});
```

- [ ] **Step 2: Run** `node --test test/chat-store.test.mjs test/chat-attach.test.mjs`; expect failures.
- [ ] **Step 3: Implement** as the Interfaces say. Item keys are `'u' + counter`. A `File` is read one slice at a time (`file.slice(o, o + CHUNK).arrayBuffer()`), never whole.
- [ ] **Step 4: Run** `node --test`; expect all passed.
- [ ] **Step 5: Commit** the four files: `feat(chat): attachments in the chat store and the upload queue`.

### Task 12: Composer attachments, history thumbnails and the image viewer

**Files:**
- Modify: `public/chat/attach.js` (DOM part), `public/chat/view.js`, `public/index.html`, `public/style.css`, `crates/agent/src/http/serve.rs` (asset arm), `crates/agent/src/http/routes.rs` (asset test), `crates/tauri-app/tauri.conf.json`, `test/frontend-guards.test.mjs`, `README.md`

**Interfaces:**
- Consumes: Task 11's `cdashAttach.Uploads`, `fmtSize` and the store's row `files`; the `uploaded` frame; `GET /api/chat/media/<ref>`.
- Produces:
  - `cdashAttach.wire(current: () => Uploads | null, changed: () => void)`: `#chat-attach` opens `#chat-file`; `#chat-file` changes, files pasted into `#chat-input` (the default is prevented only when the paste holds files) and files dropped on `#chat` (`dragover` prevented while the drag holds files, class `chat-drop` while over it) all go to `add`; `#chat-chips` clicks on `[data-remove]` and `[data-retry]`.
  - `cdashAttach.renderChips(uploads)`: rebuilds `#chat-chips` (hidden when empty), one `.chat-chip` per item: a thumbnail from an object URL of the file for `image/*` files (kept in `item.url`), else the name, then the size, a progress bar while uploading, the error text, Retry when failed, and a remove control.
  - `cdashAttach.view(src)`: shows `#chat-viewer` (`showModal`) with `#chat-viewer-img`, pushing `{ ...history.state, viewer: true }`; Escape (the dialog's `cancel`, prevented) and a click call `history.back()`; a `popstate` whose state has no `viewer` closes it, so Android back closes the image and not the chat.
  - Markup: inside `#chat-form`, before the textarea, `<button id="chat-attach" class="chat-attach" type="button" aria-label="Attach files">` with an inline paperclip SVG (as `#chat-term` carries its icon) and `<input id="chat-file" type="file" multiple hidden>`; `<div id="chat-chips" class="chat-chips" hidden></div>` directly above the form; after `#chat-sheet`, `<dialog id="chat-viewer" class="chat-viewer" aria-label="Image"><img id="chat-viewer-img" alt=""></dialog>`; `<script src="chat/attach.js"></script>` before `chat/view.js`.

- [ ] **Step 1: Write the failing tests.** In `test/frontend-guards.test.mjs`, add `'chat/attach.js'` to the script list of `chat scripts load before app.js and the chat markup exists` and `'chat-attach', 'chat-file', 'chat-chips', 'chat-viewer', 'chat-viewer-img'` to its id list, then:

```js
test('attachments never become markup', () => {
  const attach = readFileSync(new URL('../public/chat/attach.js', import.meta.url), 'utf8');
  assert.ok(!/innerHTML|insertAdjacentHTML|document\.write|eval\(/.test(attach));
  assert.match(readFileSync(new URL('../public/index.html', import.meta.url), 'utf8'), /<input id="chat-file" type="file" multiple hidden>/);
});
test('the desktop app lets dropped files reach the page', () => {
  const conf = JSON.parse(readFileSync(new URL('../crates/tauri-app/tauri.conf.json', import.meta.url), 'utf8'));
  assert.equal(conf.app.windows[0].dragDropEnabled, false);
});
```

In `routes.rs`, add `("/chat/attach.js", "cdashAttach")` to `chat_assets_are_embedded`.
- [ ] **Step 2: Run** `node --test test/frontend-guards.test.mjs` and `cargo test -p cdash-agent chat_assets`; expect failures.
- [ ] **Step 3: Implement the markup, asset and config:** the markup above; the `"chat/attach.js"` arm in `embedded_asset`; `"dragDropEnabled": false` on the one window in `tauri.conf.json`.
- [ ] **Step 4: Implement `view.js`.**
  - `open` creates `c.uploads = new cdashAttach.Uploads({ send, buffered, onChange: schedule })` over the open socket; `close` calls `c.uploads.clear()`; `wireOnce` calls `cdashAttach.wire(() => c?.uploads ?? null, schedule)`.
  - `connect` keeps `x.base`, the http(s) origin of the socket URL it resolved (`ws:` → `http:`, `wss:` → `https:`, same host), so thumbnails go to `agent_base` in the apps without touching `app.js`. `onmessage` hands `uploaded` frames to `x.uploads.onFrame` instead of the store; `onclose` calls `x.uploads.lost()`.
  - Composer: Send is disabled while `c.uploads.pending()` (label `Uploading…` while a chip uploads); text or at least one ready chip may be sent; Stop shows only when the box is empty and there are no chips. Submit sends `{t:'send', cid, text, files: <ids of ready()>}`, calls `addMine(state, cid, text, ready())` and `c.uploads.clear()`. `renderComposer` calls `cdashAttach.renderChips(c.uploads)`.
  - Held Edit, a refused or unconfirmed message's Edit, and Send again also restore, or resend, their `files`.
  - `build` for user rows: when `r.files` is non-empty, a `.chat-files` grid comes first: images with a `ref` as `<button class="chat-thumb" data-view=…>` around `<img loading="lazy" decoding="async" alt="Image">` with `src = x.base + '/api/chat/media/' + ref`, then `.chat-file` chips showing the name and `fmtSize(size)` (my own images have no `ref` yet and show as chips); the bubble follows only when there is text. A click on `[data-view]` calls `cdashAttach.view`.
- [ ] **Step 5: Add styles** under the chat section of `style.css`, with the existing tokens so both themes work: `.chat-attach`; `.chat-chips` and `.chat-chip` (thumbnail, name, size, progress bar, error in `var(--danger)`); `.chat-files` as a grid of up to three thumbnails per row, square cells with `object-fit: cover`; `.chat-file`; `.chat.chat-drop` with an accent outline; `.chat-viewer` filling the screen with the image `object-fit: contain` on a dark backdrop.
- [ ] **Step 6: Document** in `README.md`'s Chat section: attach with the paperclip, paste or drop (20 MB per file, 10 per message); images show as thumbnails, tap for full screen; Claude Code gets images as pasted attachments and other files as paths it reads itself, Codex and OpenCode as their own attachments; uploads stay on the host in `~/.cache/cdash/uploads/<session>` (or under `$XDG_CACHE_HOME`), readable only by you, deleted when the session ends and after 7 days; images seen in history are kept in `~/.cache/cdash/media` and removed after 7 days unused; Codex and OpenCode sessions started before the upgrade need a restart to show images.
- [ ] **Step 7: Run** `node --test` and `cargo test -p cdash-agent`; expect all passed. Open the page from a test agent (Task 13, steps 2 and 4) and check chips, progress, remove, paste, drop and the viewer by hand.
- [ ] **Step 8: Commit** the files listed: `feat(chat): composer attachments, history thumbnails and the image viewer`.

## Phase D: real sessions

### Task 13: Acceptance on real sessions

**Files:** none (results go in the pull request description). Everything below runs in WSL from the worktree, with `CARGO_TARGET_DIR` exported. Launching through the test agent is the dashboard's real path: like every dashboard launch it registers a Remote Control session for Claude and Codex, records the project in recent places, and leaves the threads in the resumable lists.

- [ ] **Step 1: Gates and build.** `node --test`, `cargo test --all --locked`, `cargo clippy --all-targets --locked -- -D warnings -D clippy::disallowed_types`, `cargo build -p cdash-agent --locked`. Expected: all pass.
- [ ] **Step 2: A test agent on a private tmux server.**

```bash
S=$HOME/.cache/cdash-p3-check; rm -rf "$S"; mkdir -p "$S/bin" "$S/cache" "$S/proj"
printf '#!/bin/sh\nexec %s -L cdash-test "$@"\n' "$(command -v tmux)" > "$S/bin/tmux"; chmod 755 "$S/bin/tmux"
PATH="$S/bin:$PATH" SHELL=/bin/sh sh -lc 'command -v tmux'   # must print $S/bin/tmux, or stop here
( cd "$S" && PATH="$S/bin:$PATH" SHELL=/bin/sh XDG_CACHE_HOME="$S/cache" PORT=23299 CDASH_AUTH=none \
  nohup "$CARGO_TARGET_DIR/debug/cdash-agent" > "$S/agent.log" 2>&1 & echo $! > "$S/agent.pid" )
curl -s http://127.0.0.1:23299/api/health   # {"ok":true}
```

The agent puts its login-shell PATH first, so the `sh -lc` line must show the wrapper winning before anything starts.
- [ ] **Step 3: Test files and the chat client.** `$S/make-media.py` (run with `python3 "$S/make-media.py" "$S"`) and `$S/chat-check.mjs`:

```python
import os, struct, sys, zlib
d = sys.argv[1]
def png(w, h, row):
    chunk = lambda t, b: struct.pack('>I', len(b)) + t + b + struct.pack('>I', zlib.crc32(t + b))
    raw = b''.join(b'\0' + row(y) for y in range(h))
    return b'\x89PNG\r\n\x1a\n' + chunk(b'IHDR', struct.pack('>IIBBBBB', w, h, 8, 2, 0, 0, 0)) + chunk(b'IDAT', zlib.compress(raw, 0)) + chunk(b'IEND', b'')
red = png(64, 64, lambda y: b'\xff\0\0' * 64)
for n, b in [('red.png', red), ('Shot 1.png', red), ('big.png', png(2300, 2200, lambda y: os.urandom(6900)))]: open(f'{d}/{n}', 'wb').write(b)
blob = os.urandom(15_000_000)
open(f'{d}/big.pdf', 'wb').write(b'%PDF-1.4\n1 0 obj<</Type/Catalog/Pages 2 0 R>>endobj\n2 0 obj<</Type/Pages/Kids[3 0 R]/Count 1>>endobj\n3 0 obj<</Type/Page/Parent 2 0 R/MediaBox[0 0 200 200]>>endobj\n'
    + b'4 0 obj<</Length %d>>stream\n' % len(blob) + blob + b'\nendstream endobj\ntrailer<</Root 1 0 R>>\n%%EOF\n')
open(f'{d}/notes.txt', 'w').write('The secret word is heliotrope.\n')
```

```js
// node chat-check.mjs <ws base> <session> <text | -> [file…]: uploads the files, sends, prints what follows until a result.
import { readFileSync } from 'node:fs';
import { basename } from 'node:path';
const [base, name, text, ...paths] = process.argv.slice(2);
const ws = new WebSocket(`${base}/api/chat?name=${encodeURIComponent(name)}`);
const waiting = []; let live = false;
const next = t => new Promise(r => waiting.push([t, r]));
ws.onmessage = e => {
  const f = JSON.parse(e.data), i = waiting.findIndex(([t]) => t === f.t);
  if (i >= 0) waiting.splice(i, 1)[0][1](f);
  if (f.t === 'ack') console.log('ack', f.status, f.error ?? '');
  if (f.t !== 'ev' || !live) return;
  const v = f.ev, c = v.message?.content;
  if (v.type === 'user' && !(Array.isArray(c) && c.some(b => b.type === 'tool_result')))
    console.log('prompt', JSON.stringify(typeof c === 'string' ? c : c.map(b => b.text).join('')), 'cid', v.cid ?? '-', 'files', JSON.stringify(v.files ?? []));
  if (v.type === 'assistant') for (const b of c) if (b.type === 'text') console.log('reply', b.text.slice(0, 200));
  if (v.type === 'result') { console.log('result', v.subtype); process.exit(0); }
};
await new Promise(r => { ws.onopen = r; });
await next('state');
const ids = [];
for (const p of text === '-' ? [] : paths) {
  const bytes = readFileSync(p);
  ws.send(JSON.stringify({ t: 'upload', ref: p, name: basename(p), size: bytes.length }));
  for (let o = 0; o < bytes.length; o += 1 << 20) ws.send(bytes.subarray(o, o + (1 << 20)));
  const u = await next('uploaded'); console.log('uploaded', JSON.stringify(u)); if (u.error) process.exit(1);
  ids.push(u.id);
}
live = true;
if (text !== '-') ws.send(JSON.stringify({ t: 'send', cid: 'check-' + Date.now(), text, files: ids }));
setTimeout(() => { console.log('timeout'); process.exit(2); }, 180_000);
```

- [ ] **Step 4: Claude Code.** `N=$(curl -s -H 'content-type: application/json' -d "{\"dir\":\"$S/proj\",\"model\":\"haiku\",\"effort\":\"low\"}" http://127.0.0.1:23299/api/launch | sed 's/.*"name":"\([^"]*\)".*/\1/')`. `tmux -L cdash-test ls` lists `$N` and `tmux ls` does not (if it does, stop the test agent and go back to step 2). After about 5 s:
  1. `node "$S/chat-check.mjs" ws://127.0.0.1:23299 "$N" "What color is this image, and what is the secret word in the attached file? Answer in four words." "$S/Shot 1.png" "$S/notes.txt"`: two `uploaded` lines (`image` with `.png`, `file`), `ack sent`, a `prompt` with the text exactly as sent, a `cid`, and `files` holding one image `ref` and `notes.txt`; a reply naming red and heliotrope; `result success`.
  2. `curl -s -o /dev/null -w '%{http_code} %{content_type}\n' http://127.0.0.1:23299/api/chat/media/<ref>` prints `200 image/png`; `ls -l "$S/cache/cdash/media" "$S/cache/cdash/uploads/$N"/*` shows `-rw-------` files.
  3. `tmux -L cdash-test capture-pane -p -t "=$N:"` shows the prompt beginning `[Image #1]`.
  4. An image alone: `node "$S/chat-check.mjs" ws://127.0.0.1:23299 "$N" "" "$S/red.png"`: `ack sent`, a prompt with text `""`, one image and a `cid`, and a reply.
  5. The Terminal door: start `node "$S/chat-check.mjs" ws://127.0.0.1:23299 "$N" - &`, then bracketed-paste `$S/red.png` alone into the pane (`printf %s "$S/red.png" > "$S/p.txt"; tmux -L cdash-test load-buffer -b chk "$S/p.txt"; tmux -L cdash-test paste-buffer -p -d -b chk -t "=$N:"`), wait 1 s, paste ` Reply with one word: the color.` the same way, wait 0.4 s, `tmux -L cdash-test send-keys -t "=$N:" Enter`. The watcher prints that prompt with one image `ref` and the reply.
  6. In a browser at http://127.0.0.1:23299, open the card's Chat: the thumbnails show; tapping one opens it full screen and Escape closes it; the paperclip (two files), a pasted screenshot and a dropped file make chips with progress; a 21 MB file shows `Over 20 MB` and is not uploaded; a message sent while Claude works is held with its chips, Edit puts them back, and Send now delivers them with a thumbnail in history.
- [ ] **Step 5: Codex.** Launch with `{"dir":"$S/proj","model":"codex","effort":"low"}` (Codex has no haiku) and wait about 10 s.
  1. The check of step 4.1 with `red.png` and `notes.txt`: the same results, and the media file appears in `$S/cache/cdash/media`.
  2. `node "$S/chat-check.mjs" ws://127.0.0.1:23299 "$N" "Reply with: ok" "$S/big.png"`: `ack sent`, the prompt with one image `ref`, a reply and a result; the pane is still alive afterwards (`tmux -L cdash-test ls`). This is the 20 MB echo the proxy limit of Task 7 lets through.
  3. In the native TUI: with the watcher of step 4.5 running, bracketed-paste the absolute path of `red.png` alone, add ` Reply with one word: the color.`, press Enter. The watcher shows the prompt with an image `ref`. Record how the TUI showed the attachment.
- [ ] **Step 6: OpenCode.** Pick a model with `curl -s "http://127.0.0.1:23299/api/models?dir=$S/proj"`, a Haiku one when listed, else the cheapest; launch with `{"dir":"$S/proj","model":"opencode:<provider/model>"}`.
  1. The check of step 4.1: the same results.
  2. `node "$S/chat-check.mjs" ws://127.0.0.1:23299 "$N" "Say in one word what the image looks like." "$S/big.png" "$S/big.pdf"`: `ack sent` (not `refused: no answer from the session`; if it is, raise the native ack wait for sends with attachments, with a test, and say so in the pull request), then the prompt with one image `ref` and `big.pdf` with its size, then a result. Then `node "$S/chat-check.mjs" ws://127.0.0.1:23299 "$N" "Reply with: ok"` gets its reply, so the stream survived, and `grep 'chat feed' ~/.claude/cdash-opencode/controls/$N/native.log` shows nothing new.
  3. The native TUI check of step 5.3 in the OpenCode pane.
- [ ] **Step 7: Clean up.** For each session, `curl -s -H 'content-type: application/json' -d "{\"name\":\"$N\"}" http://127.0.0.1:23299/api/kill`; after a few seconds `ls "$S/cache/cdash/uploads"` no longer lists a session whose chat was open. Then `tmux -L cdash-test kill-server`, `kill "$(cat "$S/agent.pid")"`, `rm -rf "$S"`.
- [ ] **Step 8: Record** every result in the pull request description, with what each native TUI did on a pasted image path. Commit only if a check forced a fix, with the fix's test.
