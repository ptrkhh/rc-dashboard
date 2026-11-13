# Chat Parts 2 to 5: shared contract for the four plans

The four approved specs ship in one pull request from one worktree. Their
plans are written separately but touch the same files, so this document fixes
what they share: the order they run in, the branch, and every name that
crosses a plan boundary. A plan that needs something here uses it under this
name, and the plan that owns it (column "Owner") is the only one that adds it.

Specs: `docs/superpowers/specs/2026-11-13-chat-{prompts-menus,attachments,push-reliability,later-tier}-design.md`,
evidence in `docs/superpowers/specs/2026-11-13-chat-evidence/`.

## Order

Plans run in this order. Each plan assumes every earlier one is done exactly
as written here.

1. **Part 4**, `2026-11-13-chat-push-reliability.md`. First, because its fixes
   are foundations for the rest: the `shell` status, the Codex thread filter,
   `Tail` reporting the end of file, reset acks, the Stop settle rule, the test
   timeouts and the stub-tmux race. It also adds `waiting_for`.
2. **Part 2**, `2026-11-13-chat-prompts-menus.md`. It adds the request/response
   envelope on the supervisor socket and the new `Engine` methods.
3. **Part 3**, `2026-11-13-chat-attachments.md`. It changes `Engine::send` and
   the prompt event's shape.
4. **Part 5**, `2026-11-13-chat-later-tier.md`. It builds on Part 3's prompt
   shape and Part 2's envelope.

## Branch, worktree, build

- Worktree `/mnt/d/git/claude-dashboard/.worktrees/chat-parts-2-5` on branch
  `feat/chat-parts-2-5`, off `master` at `e3385c4` or later. The first commit
  adds the four specs, the evidence folder, this document and the four plans.
- Rust builds and tests use
  `CARGO_TARGET_DIR=/home/p/.cache/chat-parts-2-5-target` (the D: drive is
  nearly full).
- CI commands: `node --test`, `cargo test --all --locked`,
  `cargo clippy --all-targets --locked -- -D warnings -D clippy::disallowed_types`.

## Constraints every plan copies into its Global Constraints

- Never `git add -A` or `git add .`. `claude-1-261102-19.xapk` in the repo
  root stays untracked.
- No new npm packages. New Rust crates only where a spec names them: `ring`
  as a direct dependency of `cdash-agent` at the locked 0.17.14 (Part 4), and
  `tauri-plugin-notification` in `crates/tauri-app` (Part 4).
- Agent subprocesses go through `Runner` (`crates/agent/src/host/cmd.rs`).
- Supervisors never write to stdout: it is the native TUI's terminal.
- Remote Control and Open in Claude keep working. The Remote Control worker
  code paths (`worker()` in `codex-remote.mjs`, `worker()` and `RemoteBridge`
  in `opencode-remote.mjs`, `claude-remote.mjs`) change only where a spec
  says so.
- The Claude launch command changes only as Part 2's spec says (the hook in
  `--settings`, `tmux new-session -e CDASH_HOOK_TOKEN=…`, the `@cdash_hook`
  option).
- Every new file under `public/` is added to the embedded file list in
  `crates/agent/src/http/serve.rs`.
- CSS class names and ids must not match `/\.send[\s{.:-]/` or contain
  `send-input`; `app.js` must not contain `/api/keys`.
- A line on the supervisor socket is at most 64 KiB in either direction
  (`LINE_MAX` in `chat-feed.mjs`). Media never travels inline on it: images
  go through the media cache (Part 3) and travel as `ref`s.
- The agent logs chat activity without message text.
- Mark deliberate shortcuts with a `ponytail:` comment naming the ceiling.
- Real-session checks run on a private tmux server (`tmux -L cdash-test`), with
  `--model haiku` where the tool allows it. They never touch the default tmux
  server or the running agent on 127.0.0.1:23274. `/model` and `/effort` run
  only with `~/.claude/settings.json` backed up first and restored after.
- Commit messages use Conventional Commits and end with:
  `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>` and
  `Claude-Session: https://claude.ai/code/session_0121cbef6XAzH3a3m9iNVdAr`.

## Rust names

### `ClientFrame` (`crates/agent/src/chat/wire.rs`)

| Variant (JSON `t`) | Fields | Owner |
|---|---|---|
| `Focus` (`focus`) | `on: bool` | Part 4 |
| `Answer` (`answer`) | `id: String`, `choice: Option<String>`, `text: Option<String>`, `answers: Option<Value>` | Part 2 |
| `Set` (`set`) | `what: String`, `value: Value` | Part 2 |
| `Menu` (`menu`) | none | Part 2 |
| `Send` gains `files` | `#[serde(default)] files: Vec<String>` (upload ids) | Part 3 |
| `Upload` (`upload`) | `#[serde(rename = "ref")] reference: String`, `name: String`, `size: u64` | Part 3 |
| `Rewind` (`rewind`) | `uuid: String`, `#[serde(default)] files: bool` | Part 5 |
| `RewindUndo` (`rewind_undo`) | none | Part 5 |
| `Aside` (`aside`) | `id: String`, `text: String` | Part 5 |
| `AsideCancel` (`aside_cancel`) | `id: String` | Part 5 |

Binary WebSocket frames carry upload bytes (Part 3); `ws.rs` routes them to
the pending `Upload` of that connection.

### `Engine` (`crates/agent/src/chat/hub.rs`)

New methods have default bodies, so engines and test fakes that do not
support them keep compiling.

| Method | Owner |
|---|---|
| `answer(&self, id: &str, a: &Answer) -> Result<(), String>` (default `Err("not supported")`) | Part 2 |
| `set(&self, what: &str, value: &Value) -> Result<(), String>` (default `Err`) | Part 2 |
| `menu(&self) -> Value` (default `json!({})`) | Part 2 |
| `send(&self, text: &str, steer: bool, cid: &str, files: &[Attachment]) -> Result<bool, String>`: the signature change, applied to every engine and fake | Part 3 |
| `rewind(&self, uuid: &str, files: bool) -> Result<String, String>`: `Ok` carries the prompt text for the composer (default `Err`) | Part 5 |
| `rewind_undo(&self) -> Result<(), String>` (default `Err`) | Part 5 |
| `aside(&self, id: &str, text: &str) -> Result<tokio::sync::mpsc::Receiver<AsideChunk>, String>` (default `Err`) | Part 5 |
| `aside_cancel(&self, id: &str)` (default no-op) | Part 5 |

All methods are `async` in the trait's existing style:
`fn …(…) -> impl Future<Output = …> + Send`.

The types live in `hub.rs`:

- `pub struct Answer { pub choice: Option<String>, pub text: Option<String>, pub answers: Option<Value> }` (Part 2).
- `pub struct Attachment { pub path: PathBuf, pub kind: Kind, pub mime: String, pub name: String, pub size: u64 }`,
  with `pub enum Kind { Image, File }` (Part 3).
- `pub struct AsideChunk { pub text: String, pub done: bool }` (Part 5).

### `EngineMsg` (`hub.rs`)

| Change | Owner |
|---|---|
| `Status { working: bool, waiting: bool, waiting_for: Option<String> }` (the Claude status file's `waitingFor`; `None` for Codex and OpenCode) | Part 4 |
| `Asks(Vec<Value>)`: the engine's full current set of pending asks | Part 2 |
| `Settings(Value)`: the session's current model, effort, plan, agent, variant | Part 2 |
| `Usage(Value)` | Part 5 |
| `Tasks(Vec<Value>)` | Part 5 |

A branch cut (Part 5) is an ordinary `Ev` holding
`{"type":"cdash","subtype":"cut","after":…}`, which the hub acts on.

### Hub `state` frame keys

Part 1: `working`, `waiting`, `ended`, `unreadable`, `held`.
Part 4 adds `waiting_for`. Part 2 adds `asks`, `settings`, `hooks`. Part 5 adds
`usage`, `tasks`. Every key is always present (`null` or empty when unknown),
so clients never test for absence.

### Agent → client frames, besides `state`

| `t` | Owner |
|---|---|
| `answered {id, ok, error?}` | Part 2 |
| `set {ok, error?}` | Part 2 |
| `menu {commands, models, efforts, agents}` | Part 2 |
| `uploaded {ref, id?, name?, size?, kind?, mime?, error?}` | Part 3 |
| `rewound {uuid, ok, text?, attachments?, error?}` (`attachments` is the count the composer note uses) | Part 5 |
| `aside {id, text, done, error?}` | Part 5 |

### Shared state outside the hub

| Name | Where | Owner |
|---|---|---|
| `Ctx.focus: Mutex<HashMap<String, usize>>` (session name to focused connections, fed by chat and Terminal sockets) | `crates/agent/src/collect/ctx.rs` | Part 4 |
| `Ctx.notes: broadcast::Sender<Note>` | `ctx.rs` | Part 4 |
| `Ctx.hook_port: Option<u16>` (the bound hook listener, `None` when its port was taken) | `ctx.rs` | Part 2 |
| `crates/agent/src/chat/media.rs`: `cache_root() -> PathBuf` (`$XDG_CACHE_HOME/cdash`, else `~/.cache/cdash`), `uploads_dir(session: &str) -> PathBuf`, `media_dir() -> PathBuf`, `stash(bytes: &[u8], mime: &str) -> io::Result<String>` (returns the SHA-256 hex `ref`), `sweep(max_age: Duration)` | new file | Part 3 |
| Engines skip `~/.claude/sessions/<pid>.json` files of processes the agent started itself (headless `initialize`, side questions): `Ctx.own_pids: Mutex<HashSet<i32>>` | `ctx.rs` and the session collector | Part 2 (first user); Part 5 reuses it |
| `CDASH_MEDIA_DIR` in every Codex and OpenCode pane's environment (`tmux new-session -e`), set to `media_dir()`; supervisors write media there and never compute the folder themselves | `collect/codex.rs`, `collect/opencode.rs` (where those panes are launched) | Part 3 |
| `CDASH_HOOK_TOKEN` in every Claude pane's environment, plus the `@cdash_hook` session option | `spawn.rs` | Part 2 |

## The supervisor socket

Part 1's lines stay as they are: agent to supervisor `send` and `stop`;
supervisor to agent `hello`, `ev`, `state`, `ready`, `delta` and `ack`.

**Request and response (Part 2 adds the envelope).**

- Agent to supervisor: `{"t":"req","rid":<u64>,"op":<name>,…op fields}`.
- Supervisor to agent: `{"t":"res","rid":<u64>,"ok":bool,"error"?:string,"data"?:any}`,
  exactly one per `req`.
- `native.rs` matches responses to requests by `rid` and times out after
  10 s, as it does for `ack`.

| `op` | Fields | Owner |
|---|---|---|
| `answer` | `id`, `answer` | Part 2 |
| `set` | `what`, `value` | Part 2 |
| `menu` | none; `data` is `{commands, models, efforts, agents}` | Part 2 |
| `rewind` | `uuid`, `files`; `data` is `{text}` | Part 5 |
| `rewind_undo` | none | Part 5 |
| `aside` | `id`, `text`; the answer streams as `aside` lines, below | Part 5 |
| `aside_cancel` | `id` | Part 5 |

`send` gains `files: [{path, kind, mime, name, size}]` (Part 3).

**Unsolicited supervisor lines.** Each is also replayed after `hello` on every
new connection, so a reconnecting agent always has the latest:

| Line | Owner |
|---|---|
| `{"t":"asks","asks":[…]}` (the full set) | Part 2 |
| `{"t":"settings","settings":{…}}` | Part 2 |
| `{"t":"usage","usage":{…}}` | Part 5 |
| `{"t":"tasks","tasks":[…]}` | Part 5 |
| `{"t":"aside","id","text","done","error"?}` (not replayed) | Part 5 |

## Front end

| File | Owner |
|---|---|
| Focus reporting in `public/chat/view.js` and the Terminal overlay; the notifications switch and Web Push subscription in `public/app.js`; `push` and `notificationclick` in `public/sw.js` | Part 4 |
| `public/chat/cards.js` (question and plan cards), `public/chat/menu.js` (slash menu and the pickers sheet) | Part 2 |
| `public/chat/attach.js` (composer attachments, uploads over the socket, the image viewer) | Part 3 |
| `public/chat/sheets.js` (diff, usage, running list, side chat), `public/chat/highlight.js` | Part 5 |

`public/chat/store.js` stays the pure model, tested with `node --test`.
`view.js` renders from it. Each part extends both through small exported
functions, not by rewriting them.

## Names the plans added (later plans reuse them)

From the Part 2 plan:

- `Engine::hooks(&self) -> bool`, a plain method with default `true`. It fills
  the hub's `hooks` state key.
- `NativeEngine::request(&self, op: &str, fields: Value) -> Result<Value, String>`.
  It sends one `req` and waits for its `res`. Part 5 uses it for `rewind`,
  `rewind_undo`, `aside` and `aside_cancel`.
- `ChatFeed` (`chat-feed.mjs`):
  - `new ChatFeed({ send, stop, ops = {} })`, where `ops` maps an op name to
    `async (req) => data`, and a missing op answers `'not supported'`;
  - `putAsk(ask)`, `dropAsk(id)` and `setSettings(settings)`.
  Part 5 adds its ops to the same table and its own snapshot setters next to
  these.
- `Runner::spawn_piped(&self, program: &str, args: &[&str], cwd: &Path) -> std::io::Result<tokio::process::Child>`.
  It pipes stdin and stdout, discards stderr and sets `kill_on_drop(true)`.
  The caller adds the child's pid to `Ctx.own_pids` and removes it when the
  child exits. Part 5's Claude side question uses it.
- `hook::router`, `DEFAULT_HOOK_PORT` (23275), `Config.hook_port`.

From the Part 4 plan:

- `Hub::spawn_with(…, Option<Sink>)`: `Hub::spawn` stays as it is for callers
  that need no note sink.
- `crate::chat::tmux_io::tests::install_stub(dir: &Path, name: &str, script: &str) -> String`
  is the only way tests write a stub executable (any program, not only
  tmux). It returns the `bin` folder to put first on `PATH`.
- The Codex supervisor's main-thread filter (`mainThread` in
  `codex-remote.mjs`). Part 5's running list reads subagent threads through
  it, not around it.
- The Android service lives in a local Tauri plugin crate,
  `crates/tauri-plugin-cdash-watch`, because `crates/tauri-app/gen/android`
  is regenerated on every release. Its one dependency, `tauri-plugin`, is
  already in `Cargo.lock`.
- `tauri-plugin-notification` 2.6+ needs `tauri` ^2.12, so `Cargo.lock` moves
  `tauri` from 2.11.5 to 2.12.x, matching the pinned CLI 2.12.1.

From the Part 5 plan:

- `AsideChunk` gains `error: Option<String>`. The `aside` frame to the client
  already carries `error`.
- `Hub::folder()`, `Hub::aside(…)` and `Hub::aside_cancel(…)`. The session's
  current folder travels as `cwd` inside the usage snapshot.
- A cut reloads clients with a full fresh start: a `hello` marked as a reset,
  the recent events, then `state`. A bare `reset` would make today's clients
  drop the events the cut keeps.
- New event fields:
  - `raw` (only when it differs from the shown text);
  - `ts`;
  - thinking blocks;
  - `cdash` subtypes `todo` and `staged`;
  - `src: "steer"` on Codex prompts;
  - `diff` on OpenCode tool results.

## Cross-plan rules

- **Part 3 keeps Part 2's routing in front of `send`.** Slash commands
  (pickers, refused commands, Codex and OpenCode command routing) are
  decided before any attachment handling, in the Claude engine and in both
  supervisors. Only plain prompt text goes down the attachment path.
- **Part 5 replaces Part 2's `/btw` refusal** with routing to side chat.
- A Claude pane launched while the hook port is taken gets no hook and no
  token (Part 2's spec). "Every Claude pane" in the table above means every
  pane launched with the hook.
