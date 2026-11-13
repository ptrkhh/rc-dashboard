# Chat Prompts and Menus (Part 2) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Chat answers questions and plan approvals as cards for Claude Code, Codex and OpenCode, and gains a slash-command menu and pickers for model, effort, plan mode, agent and variant.

**Architecture:** Every engine reports its pending questions and plan approvals ("asks") and its current settings to the hub, which keeps them as state and passes card answers, picker changes and menu requests back through three new `Engine` methods. Claude Code reaches the agent through an HTTP `PermissionRequest` hook on a dedicated loopback listener; the Claude engine holds each hook request until chat answers it or the transcript shows it was settled elsewhere, and it types `/model`, `/effort` and shift+tab only while idle and after a screen check. The Codex and OpenCode supervisors turn server requests and forms into asks and serve answers, settings and menus through a request/response envelope on the existing chat socket.

**Tech Stack:** Rust (axum 0.8, tokio, serde_json, subtle, getrandom), Node 22 ES modules for the supervisors, vanilla JavaScript and CSS, `node --test`, `cargo test`.

**Spec:** `docs/superpowers/specs/2026-11-13-chat-prompts-menus-design.md` (owner-approved: no permission cards or permission modes; OpenCode keeps `--auto`). Shared names and run order: `docs/superpowers/plans/2026-11-13-chat-parts-2-5-shared.md`. Evidence: `docs/superpowers/specs/2026-11-13-chat-evidence/` (E1 and E5 in `claude-2.1.323-live-spike.md`, `review-checks.md`).

## Global Constraints

Workspace and order:
- Part 4 (`2026-11-13-chat-push-reliability.md`) is done exactly as the shared contract describes: `EngineMsg::Status { working, waiting, waiting_for }`, the hub state key `waiting_for`, `shell` counted as idle, `Ctx.focus`, `Tail` end-of-file reporting, and the Codex main-thread filter in `codex-remote.mjs`. This plan edits that code, not `master`.
- Work in `/mnt/d/git/claude-dashboard/.worktrees/chat-parts-2-5` on branch `feat/chat-parts-2-5` (it exists). Rust builds and tests use `CARGO_TARGET_DIR=/home/p/.cache/chat-parts-2-5-target`.
- CI commands: `node --test`, `cargo test --all --locked`, `cargo clippy --all-targets --locked -- -D warnings -D clippy::disallowed_types`.

From the shared contract:
- Never `git add -A` or `git add .`. `claude-1-261102-19.xapk` in the repo root stays untracked.
- No new npm packages. New Rust crates only where a spec names them (`ring` and `tauri-plugin-notification`, both Part 4). Part 2 adds none.
- Agent subprocesses go through `Runner` (`crates/agent/src/host/cmd.rs`).
- Supervisors never write to stdout: it is the native TUI's terminal.
- Remote Control and Open in Claude keep working. The Remote Control worker code paths (`worker()` in `codex-remote.mjs`, `worker()` and `RemoteBridge` in `opencode-remote.mjs`, `claude-remote.mjs`) change only where a spec says so. Part 2 changes none of them: it edits `nativeLauncher`, `supervise` and `ChatBridge`.
- The Claude launch command changes only as Part 2's spec says (the hook in `--settings`, `tmux new-session -e CDASH_HOOK_TOKEN=…`, the `@cdash_hook` option).
- Every new file under `public/` is added to the embedded file list in `crates/agent/src/http/serve.rs`.
- CSS class names and ids must not match `/\.send[\s{.:-]/` or contain `send-input`; `app.js` must not contain `/api/keys`.
- A line on the supervisor socket is at most 64 KiB in either direction (`LINE_MAX` in `chat-feed.mjs`).
- The agent logs chat activity without message text: no answer, plan or question text, no hook body and no token in any log line.
- Mark deliberate shortcuts with a `ponytail:` comment naming the ceiling.
- Real-session checks run on a private tmux server (`tmux -L cdash-test`), with `--model haiku` where the tool allows it. They never touch the default tmux server or the running agent on 127.0.0.1:23274. `/model` and `/effort` run only with `~/.claude/settings.json` backed up first and restored after.
- Commit messages use Conventional Commits and end with the two lines `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>` and `Claude-Session: https://claude.ai/code/session_0121cbef6XAzH3a3m9iNVdAr`. Commit steps below give only the subject.

From the spec:
- Every tool keeps every action allowed: Claude `--dangerously-skip-permissions`, Codex `approvalPolicy: "never"` with `danger-full-access`, OpenCode `--auto`. No permission cards and no permission modes.
- Hook listener: `127.0.0.1`, port `CDASH_HOOK_PORT`, default `23275`, carrying only `POST /hook/claude?pane=<name>`; body limit 1 MB; loopback peers only; bearer token equal to the pane's `@cdash_hook`, compared in constant time. Port taken: log it and launch Claude without the hook.
- Token: 32 random hex characters per launch, in the pane's environment (`tmux new-session -e`) and the session option `@cdash_hook`, never in `claude`'s own arguments.
- Hook entry: `"type":"http"`, header `Authorization: Bearer $CDASH_HOOK_TOKEN`, `allowedEnvVars: ["CDASH_HOOK_TOKEN"]`, `timeout: 604800`, plus (plan author's call) `"matcher":"AskUserQuestion|ExitPlanMode"`. A request for any other tool is still answered empty at once.
  - Matcher support: Claude Code's hooks documentation (`https://code.claude.com/docs/en/hooks`, "Matcher patterns" and "PermissionRequest", as fetched for the 2.1.323 research on 2026-11-13) lists `PermissionRequest` among the events whose matcher filters on the tool name ("Matches on tool name, same values as PreToolUse"), and `AskUserQuestion|ExitPlanMode` takes its exact-string path. The evidence notes do not cover matchers and the live spike used none, so Task 13 verifies it and holds the fallback (no matcher).
- Timings: a hook request becomes a card only if the status file shows `waiting` with `input needed` or `permission prompt` within 300 ms of its arrival; the Terminal banner shows when the session waits with no ask for 500 ms (at once for `waitingFor: "dialog open"`); a held hook settles when the status file has not shown `waiting` for 2 s; after `/model` or `/effort` the engine watches for 2 s; the Plan switch presses shift+tab at most 5 times; the command list is refreshed in the background when the menu opens and the cached copy is over 1 hour old.
- Claude efforts: `low, medium, high, xhigh, max, auto`. Claude pickers act only while the status is `idle` or `shell`; the model and effort pickers carry "Also becomes your default for new Claude sessions"; during a turn the sheet says "Stop the turn or wait for it to finish".
- Hidden Claude commands (not listed; refused when typed bare): model, effort, config, permissions, resume, rewind, usage, cost, stats, status, tasks, mcp, plugin, hooks, agents, memory, export, login, logout, theme, diff. `/model x` and `/effort x` take the picker path, `/plan` is the Plan switch, `/btw` is Part 5's side chat.
- No key press into a Claude dialog that chat has not confirmed on screen. No settings file is written, except the default Claude itself saves on `/model` and `/effort`.
- Checked against Claude Code 2.1.323, Codex app-server 0.167 and OpenCode 2.2.6.

## Review Focus

1. A Codex question asked by a subagent thread must still become a card, although Part 4 keeps subagent notifications out of the conversation. Task 8, test `questions from any thread become cards…`.
2. Two pending requests (two `AskUserQuestion` calls, or a question and a plan) must stay two cards; answering or settling one never touches the other. Task 2, `two_pending_cards_are_answered_and_settled_independently`.
3. A long free-text answer or a long Codex plan must not break the 64 KiB socket line: the answer is refused with "answer too long", and the plan card shows a cut plan. Task 1 (`answers_must_cover_every_question_with_listed_labels`) and Task 3 (`asks and settings go out live…`).
4. After `/model` or `/effort`, a dialog other than the expected confirmation gets Escape and an error, never Enter. Task 7, `model_and_effort_are_typed_and_only_their_own_confirmation_is_accepted`.
5. A headless `initialize` that fails must leave the menu with no commands at once, efforts still listed, and the hub responsive. Task 6, `a_claude_that_never_answers_leaves_an_empty_menu` and `without_a_command_list_the_menu_still_offers_the_effort_levels`.

## Protocol reference

**Asks** (`EngineMsg::Asks`, hub state `asks`, supervisor line `{"t":"asks","asks":[…]}`):

```
{"id":ID,"kind":"question","tool":"claude"|"codex"|"opencode","questions":[{"id":QID,"header":TEXT,"question":TEXT,"multi":B,"other":B,"secret":B,"options":[{"label":L,"description":TEXT}]}]}
{"id":ID,"kind":"plan","tool":"claude"|"codex","plan":MARKDOWN}
```

Ask ids: Claude, 16 random hex characters; Codex, `String(<JSON-RPC request id>)` for questions and `implement:<turnId>` for "Implement this plan?"; OpenCode, the form id. Question ids: Claude `q<index>`, Codex the question's `id`, OpenCode the field's `key`.

**Answers** (client frame; `hub::Answer`; the `answer` op's `answer` field):

```
{"t":"answer","id":ID,"choice":"yes"|"no","text":TEXT?}                    plan cards: Claude Approve/Keep planning, Codex Yes/No
{"t":"answer","id":ID,"answers":{QID:{"choices":[L…],"text":TEXT?}}}        question cards; choices are option labels
```

| Tool | single choice | multiple choice | no options |
|---|---|---|---|
| Claude, keyed by question text | `text` if non-empty, else `choices[0]` | `[...choices, text]` (text only if non-empty) | `text` |
| Codex, `{answers: string[]}` per id | `[choices[0] ?? "None of the above", "user_note: " + text]` (note only if text) | none in Codex | `[text]` |
| OpenCode, keyed by field key, labels mapped to option `value`s | `text` if non-empty, else `value(choices[0])` | `[...values(choices), text]` | `text` |

**Settings** (`EngineMsg::Settings`, hub state `settings`, supervisor line `{"t":"settings","settings":{…}}`): always the five keys `model, effort, plan, agent, variant`, `null` where unknown or not the tool's. Claude `model` is the full id from the transcript (`claude-opus-5-5`).

**Menu** (hub reply `{"t":"menu",…}`; the `menu` op's `data`):

```
{"commands":[{"name":N,"description":TEXT,"hint":TEXT?}],"models":[{"value":V,"label":TEXT,"model":ID?,"efforts":[E]?,"variants":[V]?}],"efforts":[E],"agents":[{"value":ID,"label":TEXT}]}
```

Hub replies: `{"t":"answered","id":ID,"ok":B,"error"?:TEXT}` and `{"t":"set","ok":B,"error"?:TEXT}`. Client: `{"t":"set","what":"model"|"effort"|"plan"|"agent"|"variant","value":…}` and `{"t":"menu"}`.

**Supervisor envelope** (contract): agent `{"t":"req","rid":N,"op":"answer"|"set"|"menu",…}` (`answer`: `id`, `answer`; `set`: `what`, `value`); supervisor exactly one `{"t":"res","rid":N,"ok":B,"error"?,"data"?}`; `asks` and `settings` lines are pushed on change and replayed after `hello`.

**Claude `--settings` with the hook** (one argument):

```
{"tui":"fullscreen","env":{"CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC":""},"hooks":{"PermissionRequest":[{"matcher":"AskUserQuestion|ExitPlanMode","hooks":[{"type":"http","url":"http://127.0.0.1:<port>/hook/claude?pane=<name>","headers":{"Authorization":"Bearer $CDASH_HOOK_TOKEN"},"allowedEnvVars":["CDASH_HOOK_TOKEN"],"timeout":604800}]}]}}
```

**Hook decisions.** The response body is `{"hookSpecificOutput":{"hookEventName":"PermissionRequest","decision":D}}`, or empty (200) for no decision.

| Card answer | D |
|---|---|
| Question | `{"behavior":"allow","updatedInput":{"questions":<echo>,"answers":{<question text>:<value>}}}` |
| Plan, `yes` (Approve) | `{"behavior":"allow","updatedInput":{"plan":<echo>},"updatedPermissions":[{"type":"setMode","mode":"bypassPermissions","destination":"session"}]}` |
| Plan, `no` (Keep planning) | `{"behavior":"deny","message":<text, or "Keep planning." when empty>}` |

## File structure

| File | Responsibility |
|---|---|
| `crates/agent/src/chat/asks.rs` (new) | Answer checks for every tool; Claude hook cards: building, matching to transcript records, settling, decisions. Pure |
| `crates/agent/src/chat/hook.rs` (new) | The hook listener's router and its one route |
| `crates/agent/src/chat/picker.rs` (new) | Claude slash routing, headless `initialize` menu and cache, footer parser, Plan switch, `/model` and `/effort` |
| `crates/agent/src/chat/testdata/initialize.json` (new) | A trimmed real `initialize` reply |
| `public/chat/cards.js`, `public/chat/menu.js` (new) | Cards; slash menu, header chip and pickers sheet |
| `test/chat-cards.test.mjs`, `test/chat-menu.test.mjs` (new) | Their pure helpers |

Modified: `chat/{hub,wire,mod,claude,native,tmux_io}.rs`, `collect/{ctx,spawn,external}.rs`, `host/cmd.rs`, `http/{serve,routes}.rs`, `collect/{chat-feed,codex-remote,opencode-remote}.mjs`, `crates/tauri-app/src/lib.rs` and `crates/agent/tests/{auth,password,opencode}_integration.rs` (one new `Config` field each), `public/chat/{store,view}.js`, `public/index.html`, `public/style.css`, `test/{chat-store,chat-feed,codex-native,opencode-native,frontend-guards}.test.mjs`, `README.md`, `CLAUDE_FINDINGS.md`.

---

### Task 1: Hub asks, settings, answers, pickers and menu

**Files:**
- Create: `crates/agent/src/chat/asks.rs` (the answer check; Task 2 adds the rest)
- Modify: `crates/agent/src/chat/hub.rs`, `crates/agent/src/chat/wire.rs`, `crates/agent/src/chat/mod.rs` (`pub mod asks;`)

**Interfaces:**
- Consumes: Part 4's `EngineMsg::Status { working, waiting, waiting_for }`.
- Produces:
  - `wire::ClientFrame::Answer { id: String, choice: Option<String>, text: Option<String>, answers: Option<Value> }`, `ClientFrame::Set { what: String, value: Value }`, `ClientFrame::Menu` (JSON `t`: `answer`, `set`, `menu`).
  - `hub::Answer { pub choice: Option<String>, pub text: Option<String>, pub answers: Option<Value> }`, deriving `Debug, Clone, Default, PartialEq, Serialize, Deserialize`, each field `#[serde(default, skip_serializing_if = "Option::is_none")]`.
  - `Engine` default methods: `answer(&self, id: &str, a: &Answer) -> impl Future<Output = Result<(), String>> + Send` and `set(&self, what: &str, value: &Value) -> impl Future<Output = Result<(), String>> + Send` (both `Err("not supported")`), `menu(&self) -> impl Future<Output = Value> + Send` (`json!({})`), and the synchronous `hooks(&self) -> bool` (`true`), read once where the actor is built (Part 4's `Hub::spawn_with`, which `Hub::spawn` calls).
  - `EngineMsg::Asks(Vec<Value>)`, `EngineMsg::Settings(Value)`.
  - `asks::check_answer(ask: &Value, a: &Answer) -> Result<(), &'static str>`.
  - State frame keys `asks` (array, `[]`), `settings` (object, `{}` until reported), `hooks` (bool).

- [ ] **Step 1: Write the failing tests.** In `wire.rs`, extend `client_frames_parse_and_unknown_ones_do_not`:

```rust
assert!(matches!(f(r#"{"t":"answer","id":"a1","choice":"yes"}"#), Ok(ClientFrame::Answer { answers: None, .. })));
assert!(matches!(f(r#"{"t":"answer","id":"a1","answers":{"q0":{"choices":["Red"]}}}"#), Ok(ClientFrame::Answer { answers: Some(_), .. })));
assert!(matches!(f(r#"{"t":"set","what":"plan","value":true}"#), Ok(ClientFrame::Set { .. })));
assert!(matches!(f(r#"{"t":"menu"}"#), Ok(ClientFrame::Menu)));
assert!(f(r#"{"t":"set","what":"plan"}"#).is_err(), "value is required");
```

In `asks.rs`:

```rust
fn question() -> Value {
    json!({"id":"a1","kind":"question","tool":"codex","questions":[
        {"id":"q0","header":"Color","question":"Pick a color","multi":false,"other":true,"secret":false,"options":[{"label":"Red","description":""},{"label":"Blue","description":""}]},
        {"id":"q1","header":"Fruits","question":"Pick fruits","multi":true,"other":false,"secret":false,"options":[{"label":"Apple","description":""},{"label":"Pear","description":""}]}]})
}
fn with(answers: Value) -> Answer { Answer { answers: Some(answers), ..Default::default() } }
#[test]
fn answers_must_cover_every_question_with_listed_labels() {
    let q = question();
    assert_eq!(check_answer(&q, &with(json!({"q0":{"choices":["Red"]},"q1":{"choices":["Apple","Pear"]}}))), Ok(()));
    assert_eq!(check_answer(&q, &with(json!({"q0":{"choices":[],"text":"Green"},"q1":{"choices":["Pear"]}}))), Ok(()));
    assert_eq!(check_answer(&q, &with(json!({"q0":{"choices":["Red"]}}))), Err("answer every question"));
    assert_eq!(check_answer(&q, &with(json!({"q0":{"choices":["Red"]},"q1":{"choices":[],"text":" "}}))), Err("answer every question"));
    for bad in [json!({"q0":{"choices":["Green"]},"q1":{"choices":["Pear"]}}),      // not an option
                json!({"q0":{"choices":["Red","Blue"]},"q1":{"choices":["Pear"]}}), // two picks on a single choice
                json!({"q0":{"choices":["Red"]},"q1":{"choices":["Pear"],"text":"Kiwi"}})] { // free text where none is allowed
        assert_eq!(check_answer(&q, &with(bad)), Err("not a valid answer"));
    }
    let long = "x".repeat(crate::chat::MAX_TEXT);
    assert_eq!(check_answer(&q, &with(json!({"q0":{"choices":[],"text":long},"q1":{"choices":["Pear"]}}))), Err("answer too long"));
}
#[test]
fn plan_answers_are_yes_or_no() {
    let p = json!({"id":"a2","kind":"plan","tool":"claude","plan":"# P"});
    let c = |choice: &str, text: Option<&str>| Answer { choice: Some(choice.into()), text: text.map(Into::into), answers: None };
    assert_eq!(check_answer(&p, &c("yes", None)), Ok(()));
    assert_eq!(check_answer(&p, &c("no", Some("Add a step"))), Ok(()));
    assert_eq!(check_answer(&p, &c("approve", None)), Err("not a valid answer"));
}
```

In `hub.rs`, give `FakeEngine` `answer` (records `answer:<id>:<choice or ->`, returns `Ok`), `set` (records `set:<what>:<value>`, returns `Err("not here")` for `what == "nope"`) and `menu` (returns `json!({"commands":[{"name":"compact"}]})`). Construct `Status` with `waiting_for` as Part 4 defined it. Helpers: `plan_ask(id)` returns `{"id":id,"kind":"plan","tool":"claude","plan":"# P"}`; `reply(f)` parses a reply frame.

```rust
#[tokio::test]
async fn asks_settings_and_hooks_are_state_and_always_present() {
    let (hub, tx, _) = rig();
    let last = |s: Subscription| serde_json::from_str::<Value>(s.backlog.last().unwrap()).unwrap();
    let st = last(hub.subscribe(None, None).await.unwrap());
    assert_eq!((&st["asks"], &st["settings"], &st["hooks"]), (&json!([]), &json!({}), &json!(true)));
    tx.send(EngineMsg::Asks(vec![plan_ask("a1")])).await.unwrap();
    tx.send(EngineMsg::Settings(json!({"model":"m","effort":"high","plan":true,"agent":null,"variant":null}))).await.unwrap();
    settle().await;
    let st = last(hub.subscribe(None, None).await.unwrap());
    assert_eq!((st["asks"][0]["id"].as_str(), st["settings"]["effort"].as_str()), (Some("a1"), Some("high")));
}
#[tokio::test]
async fn only_a_pending_ask_reaches_the_engine_and_a_bad_answer_does_not() {
    let (hub, tx, calls) = rig();
    let ans = |id: &str, c: &str| ClientFrame::Answer { id: id.into(), choice: Some(c.into()), text: None, answers: None };
    assert_eq!(reply(hub.command(ans("gone", "yes")).await), json!({"t":"answered","id":"gone","ok":false,"error":"already answered"}));
    tx.send(EngineMsg::Asks(vec![plan_ask("a1")])).await.unwrap();
    settle().await;
    assert_eq!(reply(hub.command(ans("a1", "maybe")).await)["error"], "not a valid answer");
    assert_eq!(reply(hub.command(ans("a1", "yes")).await), json!({"t":"answered","id":"a1","ok":true}));
    assert_eq!(*calls.lock().unwrap(), ["answer:a1:yes"]);
}
#[tokio::test]
async fn waiting_refuses_sends_with_the_card_or_the_terminal_message() {
    let (hub, tx, _) = rig();
    tx.send(EngineMsg::Status { working: false, waiting: true, waiting_for: Some("permission prompt".into()) }).await.unwrap();
    settle().await;
    assert_eq!(ack_of(hub.command(send("c1", "x")).await)["error"], "answer in Terminal first");
    tx.send(EngineMsg::Asks(vec![plan_ask("a1")])).await.unwrap();
    settle().await;
    assert_eq!(ack_of(hub.command(send("c2", "x")).await)["error"], "answer the card first");
}
#[tokio::test]
async fn set_and_menu_reply_with_what_the_engine_said() {
    let (hub, _tx, calls) = rig();
    assert_eq!(reply(hub.command(ClientFrame::Set { what: "plan".into(), value: json!(true) }).await), json!({"t":"set","ok":true}));
    assert_eq!(reply(hub.command(ClientFrame::Set { what: "nope".into(), value: json!(1) }).await), json!({"t":"set","ok":false,"error":"not here"}));
    assert_eq!(reply(hub.command(ClientFrame::Menu).await), json!({"t":"menu","commands":[{"name":"compact"}],"models":[],"efforts":[],"agents":[]}));
    assert_eq!(*calls.lock().unwrap(), ["set:plan:true", "set:nope:1"]);
}
```

- [ ] **Step 2: Run** `cargo test -p cdash-agent chat::` and confirm the new tests fail to compile.
- [ ] **Step 3: Implement.**
  - `check_answer`: first, `serde_json::to_string(a)` longer than `MAX_TEXT` bytes gives `"answer too long"` (the answer travels on the 64 KiB socket line). `kind: "plan"`: `choice` must be `yes` or `no`. `kind: "question"`: for every question, the entry `answers[q.id]` with `choices` (default `[]`) and `text` (default `""`, trimmed); a choice that is not an option label, more than one choice when `multi` is false, or non-empty text when `other` is false is `"not a valid answer"`; no choice and no text is `"answer every question"`. Anything else is `"not a valid answer"`.
  - Actor fields `asks: Vec<Value>`, `settings: Value` (`json!({})`) and `hooks: bool` (from `engine.hooks()` in `Hub::spawn_with`, before the engine moves into the actor). `state()` adds the three keys. `Asks` and `Settings` replace their field.
  - `refusal()`: the `waiting` branch says `"answer the card first"` when `asks` is non-empty, else `"answer in Terminal first"`.
  - `Answer`: an id not in `asks` replies `already answered` without calling the engine; then `check_answer` against that ask; then `engine.answer`. Every outcome replies `answered`, with `error` when not ok. Log `chat answer <name>`.
  - `Set`: `engine.set`, reply `set`; log `chat set <name> <what>`. `Menu`: `engine.menu()`, reply `menu` with `commands`, `models`, `efforts` and `agents` each defaulting to `[]`.
  - `end()` also clears `asks`.
- [ ] **Step 4: Run** the same command; expect all passed. Run clippy.
- [ ] **Step 5: Commit** `feat(chat): hub asks, settings, answers and menu`.

### Task 2: Claude hook cards

**Files:**
- Modify: `crates/agent/src/chat/asks.rs`

**Interfaces:**
- Consumes: `hub::Answer`.
- Produces:
  - `asks::Cards` (`Default`) with `add(&mut self, req: &Value, now: std::time::Instant) -> Option<tokio::sync::oneshot::Receiver<Value>>`, `answer(&mut self, id: &str, a: &Answer) -> Result<(), String>`, `seen(&mut self, line: &str)`, `tick(&mut self, waiting: bool, now: Instant)`, `list(&self) -> Vec<Value>`, `clear(&mut self)`.
  - `asks::decision(tool: &str, input: &Value, a: &Answer) -> Value`; `asks::SETTLE_NET: Duration = 2 s`.
  - Test helpers, `pub(crate)` in `asks::tests` for Tasks 4 and 5: `ask_request(prompt) -> Value`, `plan_request(prompt, plan) -> Value`, `use_line(prompt, id, name, input: &Value) -> String`, `result_line(id) -> String`.

- [ ] **Step 1: Write the failing tests.** `ask_request` is the spike's request 1 (E1a): `{"session_id":"s","prompt_id":P,"permission_mode":"bypassPermissions","hook_event_name":"PermissionRequest","tool_name":"AskUserQuestion","tool_input":{"questions":[{"question":"Pick a color","header":"Color","options":[{"label":"Red","description":"Red"},{"label":"Blue","description":"Blue"}],"multiSelect":false},{"question":"Pick fruits","header":"Fruits","options":[{"label":"Apple","description":"Apple"},{"label":"Pear","description":"Pear"}],"multiSelect":true}]}}`. `plan_request` has `"permission_mode":"plan"`, `"tool_name":"ExitPlanMode"` and `"tool_input":{"plan":PLAN,"planFilePath":"/home/u/.claude/plans/p.md"}`. `use_line` is an assistant record `{"type":"assistant","uuid":"u-ID","promptId":P,"isSidechain":false,"message":{"id":"m","role":"assistant","content":[{"type":"tool_use","id":ID,"name":NAME,"input":INPUT}]}}`; `result_line` is `{"type":"user","uuid":"r-ID","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":ID,"content":"done"}]}}`.

```rust
#[tokio::test]
async fn a_question_becomes_a_card_and_its_answer_is_the_exact_decision() {
    let mut cards = Cards::default();
    let rx = cards.add(&ask_request("p1"), Instant::now()).unwrap();
    let ask = cards.list().remove(0);
    let id = ask["id"].as_str().unwrap().to_string();
    assert!(id.len() == 16 && id.bytes().all(|b| b.is_ascii_hexdigit()));
    assert_eq!(ask, json!({"id":id,"kind":"question","tool":"claude","questions":[
        {"id":"q0","header":"Color","question":"Pick a color","multi":false,"other":true,"secret":false,"options":[{"label":"Red","description":"Red"},{"label":"Blue","description":"Blue"}]},
        {"id":"q1","header":"Fruits","question":"Pick fruits","multi":true,"other":true,"secret":false,"options":[{"label":"Apple","description":"Apple"},{"label":"Pear","description":"Pear"}]}]}));
    let a = Answer { answers: Some(json!({"q0":{"choices":["Red"]},"q1":{"choices":["Apple","Pear"]}})), ..Default::default() };
    cards.answer(&id, &a).unwrap();
    assert_eq!(rx.await.unwrap(), json!({"behavior":"allow","updatedInput":{"questions":ask_request("p1")["tool_input"]["questions"],"answers":{"Pick a color":"Red","Pick fruits":["Apple","Pear"]}}}));
    assert!(cards.list().is_empty());
    assert_eq!(cards.answer(&id, &a), Err("already answered".into()));
}
#[test]
fn every_decision_row_is_exactly_the_spec_s() {
    let input = json!({"plan":"# Plan\n1. a","planFilePath":"/p.md"});
    let c = |choice: &str, text: Option<&str>| Answer { choice: Some(choice.into()), text: text.map(Into::into), answers: None };
    assert_eq!(decision("ExitPlanMode", &input, &c("yes", None)), json!({"behavior":"allow","updatedInput":{"plan":"# Plan\n1. a"},"updatedPermissions":[{"type":"setMode","mode":"bypassPermissions","destination":"session"}]}));
    assert_eq!(decision("ExitPlanMode", &input, &c("no", Some("Add a third step"))), json!({"behavior":"deny","message":"Add a third step"}));
    assert_eq!(decision("ExitPlanMode", &input, &c("no", Some("  "))), json!({"behavior":"deny","message":"Keep planning."}));
    let free = Answer { answers: Some(json!({"q0":{"choices":["Red"],"text":"Teal"},"q1":{"choices":["Pear"],"text":"Kiwi"}})), ..Default::default() };
    assert_eq!(decision("AskUserQuestion", &ask_request("p")["tool_input"], &free)["updatedInput"]["answers"],
               json!({"Pick a color":"Teal","Pick fruits":["Pear","Kiwi"]}), "free text wins a single choice and joins a multiple one");
}
#[tokio::test]
async fn a_card_settles_when_its_tool_use_gets_a_result_even_if_the_record_came_first() {
    let mut cards = Cards::default();
    let input = ask_request("p1")["tool_input"].clone();
    cards.seen(&use_line("p1", "toolu_1", "AskUserQuestion", &input)); // the record raced ahead of the hook
    let rx = cards.add(&ask_request("p1"), Instant::now()).unwrap();
    cards.seen(&result_line("toolu_other"));
    assert_eq!(cards.list().len(), 1);
    cards.seen(&result_line("toolu_1"));
    assert!(cards.list().is_empty());
    assert!(rx.await.is_err(), "settled elsewhere: the hook gets no decision");
}
#[tokio::test]
async fn matching_takes_equal_input_then_the_oldest_so_a_stale_plan_record_still_settles() {
    let mut cards = Cards::default();
    let rx = cards.add(&plan_request("p2", "# New plan"), Instant::now()).unwrap();
    cards.seen(&use_line("p2", "toolu_old", "ExitPlanMode", &json!({"plan":"# Previous plan"}))); // E1c
    cards.seen(&use_line("p9", "toolu_x", "ExitPlanMode", &json!({"plan":"# New plan"})));
    cards.seen(&result_line("toolu_x"));
    assert_eq!(cards.list().len(), 1, "another prompt_id never matches");
    cards.seen(&result_line("toolu_old"));
    assert!(cards.list().is_empty() && rx.await.is_err());
    let rx = cards.add(&plan_request("p3", "# B"), Instant::now()).unwrap();
    cards.seen(&use_line("p3", "toolu_a", "ExitPlanMode", &json!({"plan":"# A"})));
    cards.seen(&use_line("p3", "toolu_b", "ExitPlanMode", &json!({"plan":"# B"})));
    cards.seen(&result_line("toolu_a"));
    assert_eq!(cards.list().len(), 1, "equal input beats age");
    cards.seen(&result_line("toolu_b"));
    assert!(cards.list().is_empty() && rx.await.is_err());
}
#[tokio::test]
async fn the_status_net_and_a_closed_connection_settle_cards() {
    let t0 = Instant::now();
    let mut cards = Cards::default();
    let rx = cards.add(&ask_request("p1"), t0).unwrap();
    cards.tick(true, t0 + Duration::from_secs(5));
    cards.tick(false, t0 + Duration::from_secs(6));
    assert_eq!(cards.list().len(), 1, "2 s after the last waiting, not after arrival");
    cards.tick(false, t0 + Duration::from_millis(7100));
    assert!(cards.list().is_empty() && rx.await.is_err());
    drop(cards.add(&ask_request("p2"), t0).unwrap()); // Claude closed the hook connection
    cards.tick(true, t0);
    assert!(cards.list().is_empty());
}
#[tokio::test]
async fn two_pending_cards_are_answered_and_settled_independently() {
    let mut cards = Cards::default();
    let now = Instant::now();
    let rx1 = cards.add(&ask_request("p1"), now).unwrap();
    let rx2 = cards.add(&plan_request("p1", "# P"), now).unwrap();
    let ids: Vec<String> = cards.list().iter().map(|a| a["id"].as_str().unwrap().to_string()).collect();
    assert_ne!(ids[0], ids[1]);
    cards.seen(&use_line("p1", "toolu_q", "AskUserQuestion", &ask_request("p1")["tool_input"]));
    cards.answer(&ids[1], &Answer { choice: Some("yes".into()), ..Default::default() }).unwrap();
    assert_eq!(cards.list().len(), 1);
    cards.seen(&result_line("toolu_q"));
    assert!(cards.list().is_empty());
    assert!(rx1.await.is_err());
    assert_eq!(rx2.await.unwrap()["behavior"], "allow");
}
#[test]
fn requests_that_cannot_be_cards_are_refused() {
    let mut cards = Cards::default();
    for req in [json!({"tool_name":"Bash","tool_input":{"command":"ls"}}),
                json!({"tool_name":"AskUserQuestion","tool_input":{"questions":[]}}),
                json!({"tool_name":"ExitPlanMode","tool_input":{}})] {
        assert!(cards.add(&req, Instant::now()).is_none());
    }
    assert!(cards.list().is_empty());
}
```

- [ ] **Step 2: Run** `cargo test -p cdash-agent chat::asks`; expect a compile failure.
- [ ] **Step 3: Implement.**
  - A card keeps its ask, `tool_name`, `tool_input`, `prompt_id` (missing is `""`), the `oneshot::Sender<Value>`, its arrival order and `last_waiting` (set to `now` by `add`). Open uses are `{id, prompt, name, input}` in arrival order.
  - `add`: `AskUserQuestion` needs a non-empty `questions` array of objects with a string `question`; each becomes `{"id":"q<i>","header":…,"question":…,"multi":multiSelect,"other":true,"secret":false,"options":[{label,description}]}` (missing strings `""`, missing options `[]`). `ExitPlanMode` needs a string `plan`. Anything else is `None`. The id is 8 bytes from `getrandom::fill` as hex.
  - `seen`: a line containing `"tool_use"` and `"AskUserQuestion"` or `"ExitPlanMode"` is parsed through a private struct (`promptId`, `isSidechain`, `message.content`); each such `tool_use` block of a record that is not a sidechain becomes an open use. A line containing `"tool_result"`, read only while uses are open, closes every use whose id is a `tool_use_id` in it; a card matched to a closed use is removed and its sender dropped.
  - Matching is recomputed after every change, not kept: cards in arrival order each take, among open uses with the same prompt and tool name not taken by an earlier card, the one whose input equals the card's, else the oldest.
  - `tick`: drop cards whose `sender.is_closed()`; when `waiting`, set every card's `last_waiting = now`; otherwise drop cards with `now - last_waiting > SETTLE_NET`.
  - `answer`: unknown id is `Err("already answered")`; remove the card, send `decision(...)`; a failed send is also `Err("already answered")`.
  - `decision`: the rows of the protocol reference. Question values follow its Claude column; `questions` is echoed from `tool_input` unchanged.
- [ ] **Step 4: Run**; expect all passed. Run clippy.
- [ ] **Step 5: Commit** `feat(chat): Claude hook cards, matching and decisions`.

### Task 3: Request and response envelope on the supervisor socket

**Files:**
- Modify: `crates/agent/src/collect/chat-feed.mjs`, `test/chat-feed.test.mjs`, `crates/agent/src/chat/native.rs`

**Interfaces:**
- Produces (JS): `new ChatFeed({ send, stop, ops = {} })`, where `ops` maps an op name to `async (req) => data`; `feed.putAsk(ask)`, `feed.dropAsk(id)`, getter `feed.asks`, `feed.setSettings(settings)`.
- Produces (Rust): `NativeEngine::request(&self, op: &str, fields: Value) -> Result<Value, String>` (Part 5 reuses it); `Engine::answer`, `set` and `menu` for `NativeEngine`; `asks` lines become `EngineMsg::Asks`, `settings` lines `EngineMsg::Settings`.

- [ ] **Step 1: Write the failing tests.** In `test/chat-feed.test.mjs`, using the file's `client` and `until`:

```js
test('every req gets exactly one res, matched by rid; unknown ops and errors come back as errors', async t => {
  const dir = await mkdtemp(join(tmpdir(), 'cdash-feed-')); t.after(() => rm(dir, { recursive: true, force: true }));
  const feed = new ChatFeed({ send: async () => {}, stop: async () => {}, ops: {
    menu: async () => ({ commands: [{ name: 'compact' }] }),
    set: async ({ what }) => { if (what === 'bad') throw Error('nope'); },
    big: async () => ({ blob: 'x'.repeat(70 * 1024) }),
  } });
  t.after(() => feed.close());
  const a = await client(await feed.listen(dir), { after: 0, epoch: null });
  a.write({ t: 'req', rid: 1, op: 'menu' }); a.write({ t: 'req', rid: 2, op: 'set', what: 'bad', value: 1 });
  a.write({ t: 'req', rid: 3, op: 'rewind' }); a.write({ t: 'req', rid: 4, op: 'big' });
  a.write({ t: 'req', rid: 5, op: 'set', what: 'plan', value: true });
  await until(() => a.lines.filter(l => l.t === 'res').length === 5);
  assert.deepEqual(a.lines.filter(l => l.t === 'res'), [
    { t: 'res', rid: 1, ok: true, data: { commands: [{ name: 'compact' }] } },
    { t: 'res', rid: 2, ok: false, error: 'nope' },
    { t: 'res', rid: 3, ok: false, error: 'not supported' },
    { t: 'res', rid: 4, ok: false, error: 'reply too large' },
    { t: 'res', rid: 5, ok: true, data: null }]);
  a.close();
});
test('asks and settings go out live and are replayed after hello; a huge plan is cut to fit a line', async t => {
  const dir = await mkdtemp(join(tmpdir(), 'cdash-feed-')); t.after(() => rm(dir, { recursive: true, force: true }));
  const feed = new ChatFeed({ send: async () => {}, stop: async () => {} }); t.after(() => feed.close());
  const path = await feed.listen(dir);
  const settings = { model: 'm', effort: 'low', plan: false, agent: null, variant: null };
  feed.putAsk({ id: '7', kind: 'question', tool: 'codex', questions: [] });
  feed.setSettings(settings);
  const a = await client(path, { after: 0, epoch: null });
  await until(() => a.lines.some(l => l.t === 'settings'));
  assert.deepEqual(a.lines.map(l => l.t), ['hello', 'state', 'asks', 'settings']);
  assert.equal(a.lines[2].asks[0].id, '7');
  feed.putAsk({ id: 'implement:t1', kind: 'plan', tool: 'codex', plan: '# P\n' + 'y'.repeat(200 * 1024) });
  feed.dropAsk('7'); feed.dropAsk('nothing');
  await until(() => a.lines.filter(l => l.t === 'asks').length === 3);
  const last = a.lines.filter(l => l.t === 'asks').at(-1);
  assert.deepEqual(last.asks.map(x => x.id), ['implement:t1']);
  assert.ok(Buffer.byteLength(JSON.stringify(last)) < 64 * 1024 && last.asks[0].plan.startsWith('# P\nyyy'));
  feed.setSettings({ ...settings });
  await new Promise(r => setTimeout(r, 50));
  assert.equal(a.lines.filter(l => l.t === 'settings').length, 1, 'unchanged settings are not sent again');
  a.close();
});
```

In `native.rs` (the engine behind an `Arc` so two requests can run at once):

```rust
#[tokio::test]
async fn requests_carry_their_own_rid_and_each_resolves_with_its_response() {
    let (sock, mut sup) = fake_supervisor("req");
    let (engine, _rx) = start(sock, log(), "cdash-a".into());
    let engine = Arc::new(engine);
    sup.read_line().await;
    sup.write_line(r#"{"t":"hello","epoch":"e1","head":0}"#).await;
    let (a, b) = (Arc::clone(&engine), Arc::clone(&engine));
    let set = tokio::spawn(async move { a.set("plan", &json!(true)).await });
    let first: Value = serde_json::from_str(&sup.read_line().await).unwrap();
    assert_eq!((first["t"].as_str(), first["op"].as_str(), first["what"].as_str(), &first["value"]), (Some("req"), Some("set"), Some("plan"), &json!(true)));
    let menu = tokio::spawn(async move { b.menu().await });
    let second: Value = serde_json::from_str(&sup.read_line().await).unwrap();
    assert_ne!(first["rid"], second["rid"]);
    sup.write_line(&json!({"t":"res","rid":second["rid"],"ok":true,"data":{"commands":[{"name":"compact"}]}}).to_string()).await;
    sup.write_line(&json!({"t":"res","rid":first["rid"],"ok":false,"error":"nope"}).to_string()).await;
    assert_eq!(menu.await.unwrap()["commands"][0]["name"], "compact");
    assert_eq!(set.await.unwrap(), Err("nope".to_string()));
    let answer = tokio::spawn(async move { engine.answer("41", &Answer { choice: Some("yes".into()), ..Default::default() }).await });
    let third: Value = serde_json::from_str(&sup.read_line().await).unwrap();
    assert_eq!((third["op"].as_str(), third["id"].as_str(), &third["answer"]), (Some("answer"), Some("41"), &json!({"choice":"yes"})));
    sup.write_line(&json!({"t":"res","rid":third["rid"],"ok":true}).to_string()).await;
    assert_eq!(answer.await.unwrap(), Ok(()));
}
#[tokio::test]
async fn asks_and_settings_lines_become_engine_messages() {
    let (sock, mut sup) = fake_supervisor("asks");
    let (_engine, mut rx) = start(sock, log(), "cdash-a".into());
    sup.read_line().await;
    sup.write_line(r#"{"t":"hello","epoch":"e1","head":0}"#).await;
    sup.write_line(r#"{"t":"asks","asks":[{"id":"41","kind":"question"}]}"#).await;
    sup.write_line(r#"{"t":"settings","settings":{"model":"m"}}"#).await;
    assert!(matches!(rx.recv().await, Some(EngineMsg::Asks(a)) if a[0]["id"] == "41"));
    assert!(matches!(rx.recv().await, Some(EngineMsg::Settings(s)) if s["model"] == "m"));
}
```

- [ ] **Step 2: Run** `node --test test/chat-feed.test.mjs` and `cargo test -p cdash-agent chat::native`; expect failures.
- [ ] **Step 3: Implement `chat-feed.mjs`.**
  - `#request` handles `{t:'req'}` before `send` and `stop`: call `this.#ops[m.op](m)`; a missing op throws `'not supported'`. Reply `{t:'res', rid, ok:true, data: result ?? null}` or `{t:'res', rid, ok:false, error: message}`. A reply line longer than `LINE_MAX` bytes (`Buffer.byteLength`) becomes `{t:'res', rid, ok:false, error:'reply too large'}`.
  - Asks are a `Map` by id in insertion order. `putAsk` sets and publishes; `dropAsk` publishes only when something was removed; the getter returns the values. A published `asks` line longer than `LINE_MAX` is rebuilt with each `plan` cut to its first 8,000 characters plus `\n\n…`. `// ponytail: only plans are cut; many huge questions would still overflow.`
  - `setSettings` publishes only when its JSON differs from the last one.
  - `#attach` sends, after `state` and before `ready`, the current `asks` line (also when empty, so a reconnecting agent drops asks settled meanwhile) and the `settings` line when settings exist. Part 1's test `a client gets the log, state, ready and then live events; reconnecting resumes` lists every line type in order: add `'asks'` after `'state'` in its first expected sequence, wait for 5 lines instead of 4, and start its second comparison at `slice(5)`.
- [ ] **Step 4: Implement `native.rs`.** `Shared` gains `reqs: Mutex<HashMap<u64, oneshot::Sender<Result<Value, String>>>>` and an `AtomicU64` counter starting at 1. `request` merges `t`, `rid` and `op` into `fields` (an object), writes the line, and waits `ACK_WAIT`: timeout is `Err("no answer from the session")`, a dropped sender `Err("not connected")`. The reader routes `res` (`ok` gives `Ok(data)` with `null` for no data, else `Err(error or "failed")`), maps `asks` and `settings`, and clears `reqs` with `acks` when the connection drops. `answer` sends `{"id","answer"}`; `set` sends `{"what","value"}`; `menu` sends `{}` and turns an error into `json!({})`.
- [ ] **Step 5: Run** both commands; expect all passed. Run clippy.
- [ ] **Step 6: Commit** `feat(chat): request and response envelope on the supervisor socket`.

### Task 4: Claude engine holds hook cards and reports settings

**Files:**
- Modify: `crates/agent/src/chat/claude.rs`, `crates/agent/src/chat/hub.rs` (`Hub.claude`), `crates/agent/src/chat/mod.rs` (`hub_for`), `crates/agent/src/chat/tmux_io.rs` (`pane`), `crates/agent/src/collect/ctx.rs` (`hook_port`)

**Interfaces:**
- Consumes: `asks::Cards`, `hub::{Engine, EngineMsg, Answer}`.
- Produces:
  - `claude::Live { pub cards: Cards, pub plan: Option<bool>, pub model: Option<String>, pub effort: Option<String> }` (`Default`); `pub type LiveRef = Arc<std::sync::Mutex<Live>>`.
  - `ClaudeEngine.hooked: bool` (`pub(crate)`, `false` from `start`), `ClaudeEngine::live(&self) -> LiveRef`; `Engine::answer` and `Engine::hooks` for `ClaudeEngine`.
  - `claude::model_effort(line: &str) -> Option<(String, Option<String>)>`.
  - `Hub.claude: Option<claude::LiveRef>` (`pub(crate)`, `None` from `Hub::spawn_with`).
  - `tmux_io::Pane { pub pid: i32, pub bare: bool, pub hook: Option<String> }`; `tmux_io::pane(runner, name) -> Option<Pane>`.
  - `Ctx.hook_port: Option<u16>` (`None` from `Ctx::new`; Task 5 sets it).

- [ ] **Step 1: Write the failing tests.** In `claude.rs`, with Task 2's helpers and the file's `fixture_ctx`, `append`, `collect_until_ready`, `next_of`:

```rust
#[tokio::test]
async fn hook_cards_publish_settle_from_the_transcript_and_settings_follow_the_last_reply() {
    let (ctx, dir) = fixture_ctx("cards");
    write_session(&dir, r#"{"sessionId":"sid-1","cwd":"/proj","status":"waiting","waitingFor":"input needed"}"#); // keeps the 2 s net out of it
    let (engine, mut rx) = start(ctx, "cdash-a".into(), 4242);
    collect_until_ready(&mut rx).await;
    let hook = engine.live().lock().unwrap().cards.add(&ask_request("p1"), std::time::Instant::now()).unwrap();
    next_of(&mut rx, |m| matches!(m, EngineMsg::Asks(a) if a.len() == 1 && a[0]["kind"] == "question")).await;
    let t = dir.join("projects/-proj/sid-1.jsonl");
    append(&t, &format!("{}\n{}\n", use_line("p1", "toolu_9", "AskUserQuestion", &ask_request("p1")["tool_input"]), result_line("toolu_9")));
    next_of(&mut rx, |m| matches!(m, EngineMsg::Asks(a) if a.is_empty())).await;
    assert!(hook.await.is_err());
    append(&t, concat!(
        r#"{"type":"assistant","uuid":"k8","effort":"low","message":{"id":"m8","model":"claude-haiku-5-5","role":"assistant","content":[{"type":"text","text":"ok"}]}}"#, "\n",
        r#"{"type":"assistant","uuid":"k9","isSidechain":true,"effort":"max","message":{"id":"s9","model":"claude-fable-5-2","role":"assistant","content":[{"type":"text","text":"sub"}]}}"#, "\n"));
    let EngineMsg::Settings(s) = next_of(&mut rx, |m| matches!(m, EngineMsg::Settings(s) if s["model"] == "claude-haiku-5-5")).await else { unreachable!() };
    assert_eq!(s, json!({"model":"claude-haiku-5-5","effort":"low","plan":null,"agent":null,"variant":null}), "a subagent's reply is not the session's model");
}
#[tokio::test]
async fn a_chat_answer_resolves_the_held_hook_and_an_approved_plan_leaves_plan_mode() {
    let (ctx, _dir) = fixture_ctx("answer");
    let (engine, mut rx) = start(ctx, "cdash-a".into(), 4242);
    collect_until_ready(&mut rx).await;
    let live = engine.live();
    let hook = { let mut l = live.lock().unwrap(); l.plan = Some(true); l.cards.add(&plan_request("p1", "# P"), std::time::Instant::now()).unwrap() };
    let id = live.lock().unwrap().cards.list()[0]["id"].as_str().unwrap().to_string();
    engine.answer(&id, &Answer { choice: Some("yes".into()), ..Default::default() }).await.unwrap();
    assert_eq!(hook.await.unwrap()["behavior"], "allow");
    assert_eq!(live.lock().unwrap().plan, Some(false));
    assert_eq!(engine.answer(&id, &Answer::default()).await, Err("already answered".into()));
    assert!(!engine.hooks(), "start leaves hooked false; hub_for sets it");
}
```

In `tmux_io.rs`:

```rust
#[tokio::test]
async fn pane_reads_the_pid_the_kind_and_the_hook_token() {
    for (i, (answer, bare, hook)) in [("4242|", false, None), ("4242|terminal|", true, None), ("4242||tok-1", false, Some("tok-1"))].into_iter().enumerate() {
        let dir = tempfile_path(&format!("pane-{i}"));
        std::fs::create_dir_all(&dir).unwrap();
        let runner = Runner::new(stub_tmux(&dir, answer, 0), Arc::new(LogBuffer::new()));
        let p = pane(&runner, "cdash-a").await.unwrap();
        assert_eq!((p.pid, p.bare, p.hook.as_deref()), (4242, bare, hook));
    }
}
```

- [ ] **Step 2: Run** `cargo test -p cdash-agent chat::`; expect failures.
- [ ] **Step 3: Implement.**
  - `tmux_io::pane` reads `#{pane_pid}|#{@cdash_kind}|#{@cdash_hook}`; a missing or empty third field is `None`.
  - `start` creates one `LiveRef` shared by the engine and the run loop. In the run loop: every line from the tail also goes to `cards.seen`; after each batch, the last line containing `"type":"assistant"` and not `"isSidechain":true` goes through `model_effort` (parse `message.model` and the top-level `effort`; a model of `<synthetic>` is ignored), which sets `live.model` and `live.effort`. Each tick, once ready, call `cards.tick(status == Some("waiting"), now)`, then send `Asks(cards.list())` and `Settings({"model","effort","plan","agent":null,"variant":null})` whenever each differs from the last one sent (the first always goes). Whenever the run loop returns, `cards.clear()`.
  - `ClaudeEngine::answer`: under the lock, note whether the id is a plan card, call `cards.answer`, and on a plan `yes` set `plan = Some(false)`. `hooks` returns `hooked`.
  - `hub_for`, Claude branch: `engine.hooked = pane.hook.is_some() && ctx.hook_port.is_some()`, take `engine.live()`, build the hub as Part 4's `hub_for` does (`Hub::spawn_with` with its `Sink`), and set `hub.claude = Some(live)` before the hub is stored (or returned from Part 4's `OnceCell` initializer, if its Task 27 ran).
- [ ] **Step 4: Run**; expect all passed, including Part 1's `ws` tests (their stub answers `4242|`). Run clippy.
- [ ] **Step 5: Commit** `feat(chat): Claude engine holds hook cards and reports settings`.

### Task 5: Hook listener and the Claude launch change

**Files:**
- Create: `crates/agent/src/chat/hook.rs`
- Modify: `crates/agent/src/chat/mod.rs` (`pub mod hook;`), `crates/agent/src/http/serve.rs`, `crates/agent/src/collect/spawn.rs`, `crates/tauri-app/src/lib.rs`, `crates/agent/tests/auth_integration.rs`, `crates/agent/tests/password_integration.rs`, `crates/agent/tests/opencode_integration.rs`, `README.md`

**Interfaces:**
- Consumes: `tmux_io::pane` (`Pane.hook`), `hub_for`, `Hub.claude`, `Live.cards.add`, `lookup::session_file_for`, `validate::assert_kill_name`.
- Produces: `hook::router(ctx: Arc<Ctx>) -> axum::Router` (route `POST /hook/claude`), `hook::BODY_MAX: usize = 1 << 20`, `hook::DIALOG_WINDOW: Duration = 300 ms`, `hook::loopback(peer: &SocketAddr) -> bool`; `serve::DEFAULT_HOOK_PORT: u16 = 23275`; `Config.hook_port: Option<u16>`; `spawn::hook_settings(hook: Option<(u16, &str)>) -> String`.

- [ ] **Step 1: Write the failing tests.** In `hook.rs`: `hook_server(tag, answer) -> (SocketAddr, Arc<Ctx>, PathBuf)` builds the ctx with `claude::tests::fixture_with(tag, answer)` and serves `router(ctx)` on `127.0.0.1:0` with `into_make_service_with_connect_info::<SocketAddr>()`, as `ws.rs` serves its router; `post(addr, query, token: Option<&str>, body: String) -> (u16, String)` posts with `reqwest`; `ask_id(&hub) -> String` subscribes every 50 ms, for up to 5 s, until the last backlog frame (`state`) has `asks[0].id`.

```rust
#[tokio::test]
async fn the_route_refuses_bad_names_wrong_tokens_big_bodies_and_dead_panes() {
    let (addr, _ctx, dir) = hook_server("refuse", "4242||tok-1");
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
async fn other_tools_subagents_and_requests_without_a_dialog_are_answered_empty() {
    let (addr, _ctx, dir) = hook_server("empty", "4242||tok-1");
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
    let (addr, ctx, dir) = hook_server("card", "4242||tok-1");
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
}
```

In `serve.rs`:

```rust
#[tokio::test]
async fn the_hook_listener_has_its_own_port_and_a_taken_port_is_logged_not_fatal() {
    let mut cfg = cfg_for(tempdir("hook-on"));
    cfg.hook_port = Some(0);
    let b = serve(cfg).await.unwrap();
    let port = b.ctx.hook_port.expect("bound");
    assert_eq!(http_post(&format!("http://127.0.0.1:{port}/hook/claude?pane=bad"), "{}").await.0, 400);
    assert_ne!(http_post(&format!("http://{}/hook/claude?pane=bad", b.addr), "{}").await.0, 400, "not on the dashboard's port");
    let held = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let mut cfg = cfg_for(tempdir("hook-taken"));
    cfg.hook_port = Some(held.local_addr().unwrap().port());
    let b = serve(cfg).await.expect("a taken hook port is not a boot error");
    assert_eq!(b.ctx.hook_port, None);
    assert!(b.ctx.host.log.lines().iter().any(|l| l.contains("hook listener off")));
}
```

In `spawn.rs` (`ctx_with_hook(dir, port)` is `ctx_with_tmux(dir, 0)` built with `hook_port = Some(port)` before the `Arc`):

```rust
#[test]
fn the_hook_settings_are_exactly_the_spec_s() {
    assert_eq!(hook_settings(None), r#"{"tui":"fullscreen","env":{"CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC":""}}"#);
    assert_eq!(hook_settings(Some((23275, "cdash-p-1200-abc"))), r#"{"tui":"fullscreen","env":{"CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC":""},"hooks":{"PermissionRequest":[{"matcher":"AskUserQuestion|ExitPlanMode","hooks":[{"type":"http","url":"http://127.0.0.1:23275/hook/claude?pane=cdash-p-1200-abc","headers":{"Authorization":"Bearer $CDASH_HOOK_TOKEN"},"allowedEnvVars":["CDASH_HOOK_TOKEN"],"timeout":604800}]}]}}"#);
}
#[tokio::test]
async fn with_the_hook_listener_up_a_launch_adds_the_hook_and_keeps_the_token_out_of_claude_s_arguments() {
    let d = tempdir("hook-launch");
    let ctx = ctx_with_hook(d.clone(), 23999).await;
    let name = launch_session(&ctx, d.to_str().unwrap(), "haiku", "low", None).await.unwrap();
    let args = std::fs::read_to_string(d.join("tmux-args")).unwrap();
    let launch = args.lines().next().unwrap();
    let token = launch.split("-e CDASH_HOOK_TOKEN=").nth(1).unwrap().split(' ').next().unwrap();
    assert!(token.len() == 32 && token.bytes().all(|b| b.is_ascii_hexdigit()));
    assert!(launch.starts_with(&format!("new-session -d -s {name} -c {} -e CDASH_HOOK_TOKEN={token} claude --settings ", d.display())));
    assert!(launch.contains(&format!("--settings {} ", hook_settings(Some((23999, &name))))));
    assert!(launch.ends_with(&format!(" ; set-option -t {name} @cdash_hook {token}")));
    assert!(launch.contains("--remote-control "));
    assert_eq!(launch.matches(token).count(), 2, "only in -e and the option");
}
```

- [ ] **Step 2: Run** `cargo test -p cdash-agent hook spawn serve`; expect failures.
- [ ] **Step 3: Implement `hook.rs`.** `router` carries only the route, with `DefaultBodyLimit::max(BODY_MAX)` and a `Bytes` body (axum answers 413). The handler, in order:
  1. Peer not loopback: 403. Pane fails `assert_kill_name`: 400. `tmux_io::pane` is `None`: 404. The bearer token missing, or not equal to `Pane.hook` under `subtle::ConstantTimeEq`: 401. Body not JSON: 400.
  2. `tool_name` not `AskUserQuestion` or `ExitPlanMode`, or an `agent_id` present (hooks docs: set for subagent hooks): 200 with an empty body at once.
  3. Read `session_file_for(claude_dir, pane.pid)` now and every 50 ms until `DIALOG_WINDOW` passes; a card needs `status == "waiting"` with `waitingFor` `input needed` or `permission prompt`. Otherwise 200 empty.
  4. `hub_for`, then `hub.claude`; under its lock set `plan = Some(permission_mode == "plan")` and `cards.add(&body, Instant::now())`. Any failure: 200 empty.
  5. Await the receiver: a decision gives 200 with `{"hookSpecificOutput":{"hookEventName":"PermissionRequest","decision":…}}`; a dropped sender gives 200 empty. If Claude closes the connection, the dropped receiver lets the next `tick` remove the card.
  - Log `hook refused <pane>: <reason>`, `hook <pane> <tool>: card` and `hook <pane> <tool>: empty (<reason>)`; never the token or the body.
- [ ] **Step 4: Implement the listener.** `Config.hook_port: Option<u16>`: `from_env` reads `CDASH_HOOK_PORT` (default `Some(DEFAULT_HOOK_PORT)`; an unparsable value is a boot error `CDASH_HOOK_PORT: not a port number: <v>`, as `PORT` is); `cfg_for` and the three integration tests use `None`; `crates/tauri-app/src/lib.rs` `server_config()` uses `Some(cdash_agent::http::serve::DEFAULT_HOOK_PORT)`, because the desktop app's own port is random. `serve()` binds `(Ipv4Addr::LOCALHOST, port)` before building `Ctx`, sets `ctx.hook_port` to the bound port before the `Arc`, and on failure logs `hook listener off: 127.0.0.1:<port>: <error>; Claude sessions launch without the hook`. The hook server runs on its own task that `select!`s between `axum::serve(…)` and a second receiver of the stop watch, so `Bound::stop` releases the port without waiting for held hooks.
- [ ] **Step 5: Implement the launch.** `hook_settings` returns the exact strings of the test. In `spawn_claude`, when `ctx.hook_port` is `Some(port)`: a token from 16 `getrandom` bytes as hex (failure is `Refused::Failed`, as `names::new_sid`), `-e CDASH_HOOK_TOKEN=<token>` after `-c <dir>`, `--settings <hook_settings(Some((port, &name)))>`, and, after `--remote-control <display name>`, the arguments `;`, `set-option`, `-t`, `<name>`, `@cdash_hook`, `<token>`: one `tmux` call, in the form `spawn_terminal` uses, so no poll sees the pane without its token. `tmux` runs a multi-argument command with `execvp`, so `$CDASH_HOOK_TOKEN` reaches Claude literally and Claude fills it in itself. Without a hook port the command is unchanged.
- [ ] **Step 6: Document** in `README.md`'s settings table, after `CDASH_BIND`: `| **CDASH_HOOK_PORT** | 23275 | Loopback port where Claude Code sessions send questions and plan approvals to chat. If another agent holds it, new sessions answer in Terminal. |`
- [ ] **Step 7: Run** `cargo test --all --locked` and clippy; expect all passed.
- [ ] **Step 8: Commit** `feat(chat): Claude permission hook listener and launch`.

### Task 6: Claude slash menu

**Files:**
- Create: `crates/agent/src/chat/picker.rs`, `crates/agent/src/chat/testdata/initialize.json`
- Modify: `crates/agent/src/chat/mod.rs` (`pub mod picker;`), `crates/agent/src/chat/claude.rs` (`menu`, `send`), `crates/agent/src/host/cmd.rs`, `crates/agent/src/collect/ctx.rs`, `crates/agent/src/collect/external.rs`

**Interfaces:**
- Consumes: Part 4's test helper `tmux_io::tests::install_stub(dir: &Path, name: &str, script: &str) -> String` (writes `<dir>/bin/<name>` through a child process; returns `<dir>/bin`).
- Produces:
  - `Runner::spawn_piped(&self, program: &str, args: &[&str], cwd: &Path) -> std::io::Result<tokio::process::Child>`: stdin and stdout piped, stderr null, `kill_on_drop(true)`, `PATH` set. The caller bounds every await on the child with `tokio::time::timeout`.
  - `Ctx.own_pids: std::sync::Mutex<HashSet<i32>>` (contract name); `external_sessions` skips those pids.
  - `picker::EFFORTS: [&str; 6]`, `picker::HIDDEN: &[&str]` (the spec's list), `picker::Route { Paste, Set(&'static str, Value), Refuse(String) }` (`Debug, PartialEq`), `picker::route(text: &str) -> Route`.
  - `picker::INIT_WAIT: Duration = 10 s`, `picker::REFRESH_AFTER: Duration = 1 h`, `picker::initialize(ctx: &Arc<Ctx>, cwd: &Path) -> Result<Value, String>` (the inner `response` object, cached per folder), `picker::menu_from(init: &Value) -> Value`, `#[cfg(test)] picker::seed(cwd: &Path, init: Value)`.

- [ ] **Step 1: Create the fixture** `testdata/initialize.json`, one JSON object: `commands` = `clear` (`argumentHint` `[name]`, `aliases` `["reset","new"]`, `builtin`), `compact` (description `Free up context by summarizing the conversation so far`, `argumentHint` `<optional custom summarization instructions>`, `builtin`), `config`, `model`, `usage` (`aliases` `["cost","stats"]`) and the skill `superpowers:brainstorming` (`argumentHint` `""`, no `builtin`); `models` = `{"value":"default","resolvedModel":"claude-opus-5-5","displayName":"Default (recommended)"}`, `opus` → `claude-opus-5-5` "Opus 5.5", `haiku` → `claude-haiku-5-5` "Haiku 5.5", and the pinned `claude-opus-4-8` "Opus 4.8". Shapes as in E5.
- [ ] **Step 2: Write the failing tests** in `picker.rs`:

```rust
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
    for pasted in ["/compact", "/plan make it faster", "/config permissionMode=plan", "hello /model", "/clear"] {
        assert_eq!(route(pasted), Route::Paste, "{pasted}");
    }
}
#[tokio::test]
async fn initialize_runs_headless_with_hooks_off_in_the_session_folder_once() {
    let (ctx, dir) = fake_claude("init-ok", true);     // bin/claude answers the request with the fixture
    let cwd = dir.join("proj"); std::fs::create_dir_all(&cwd).unwrap();
    let init = initialize(&ctx, &cwd).await.unwrap();
    assert_eq!(init["models"][1]["value"], "opus");
    initialize(&ctx, &cwd).await.unwrap();
    let calls = std::fs::read_to_string(dir.join("claude-calls")).unwrap();
    assert_eq!(calls.lines().count(), 1, "cached per folder");
    assert_eq!(calls.trim(), format!(r#"{}|-p --input-format stream-json --output-format stream-json --verbose --settings {{"disableAllHooks":true}}"#, cwd.display()));
    assert!(ctx.own_pids.lock().unwrap().is_empty(), "the pid is forgotten once the process is gone");
}
#[tokio::test]
async fn a_claude_that_never_answers_leaves_an_empty_menu() {
    let (ctx, dir) = fake_claude("init-dead", false);  // bin/claude exits without replying
    let started = std::time::Instant::now();
    assert!(initialize(&ctx, &dir).await.is_err());
    assert!(started.elapsed() < INIT_WAIT);
}
```

`fake_claude(tag, answers)` makes a temp claude dir, writes `bin/claude` with `install_stub`, and returns `claude::tests::ctx_with_path(dir, bin)`. The script appends `$(pwd)|$*` to `<dir>/claude-calls` and reads one line. When `answers`, it prints `{"type":"system","subtype":"hook_started"}`, then `{"type":"control_response","response":{"subtype":"success","request_id":"1","response":` + the fixture on one line (the test writes it to `<dir>/init.json` with `serde_json::to_string`) + `}}`, then reads stdin to its end; otherwise it exits at once.

In `claude.rs` (the cwd is unique because the cache is process-wide):

```rust
#[tokio::test]
async fn without_a_command_list_the_menu_still_offers_the_effort_levels() {
    let (ctx, dir) = fixture_ctx("menu");                 // no claude on PATH
    write_session(&dir, r#"{"sessionId":"sid-1","cwd":"/no-such-dir-menu","status":"idle"}"#);
    let (engine, mut rx) = start(ctx, "cdash-a".into(), 4242);
    collect_until_ready(&mut rx).await;
    assert_eq!(engine.menu().await, json!({"commands":[],"models":[],"efforts":["low","medium","high","xhigh","max","auto"],"agents":[]}));
}
```

In `external.rs`, add to the sdk-cli test: a `cli` session file whose pid is in `ctx.own_pids` is not reported.

- [ ] **Step 3: Run** `cargo test -p cdash-agent chat:: external`; expect failures.
- [ ] **Step 4: Implement.**
  - `route` (sanitized text): `/model <x>` and `/effort <x>` are `Set` with the trimmed argument; bare `/plan` is `Set("plan", true)`; `/btw` with or without text is refused (Part 5 routes it); a bare hidden name is refused with: model, effort "Use the model chip at the top of the chat"; usage, cost, stats "Use the usage sheet"; rewind "Use Rewind to here on a message"; tasks "Use the running list"; diff "Use Changes at the top of the chat"; any other "Use Terminal for /<name>". Everything else is `Paste`.
  - `initialize`: a `static CACHE: LazyLock<std::sync::Mutex<HashMap<PathBuf, (Instant, Value)>>>` (the lock is never held across an await). A miss loads now. A hit older than `REFRESH_AFTER` returns the cached copy and spawns a reload that replaces it on success.
  - Loading: `spawn_piped("claude", ["-p","--input-format","stream-json","--output-format","stream-json","--verbose","--settings",r#"{"disableAllHooks":true}"#], cwd)`; add the child's pid to `ctx.own_pids`; write `{"type":"control_request","request_id":"1","request":{"subtype":"initialize"}}` and a newline; read stdout lines until `type == "control_response"` with `response.request_id == "1"` and take `response.response`; drop stdin; wait for the exit; remove the pid. All of it inside `INIT_WAIT`. End of output, timeout and bad JSON are `Err`; the pid is removed on every path. Log `chat menu failed: <error>`.
  - `menu_from`: commands not in `HIDDEN`, as `{name, description (first 200 characters), hint}` with `hint` only when `argumentHint` is non-empty; models whose `value` does not start with `claude-`, as `{value, label: displayName, model: resolvedModel}`; `efforts` is `EFFORTS`; `agents` is `[]`.
  - `ClaudeEngine::send`: `route(text)`: `Paste` pastes as before (`Ok(false)`); `Set(what, v)` calls `self.set(what, &v)` and gives `Ok(true)` (confirmed: no transcript prompt follows); `Refuse(why)` is `Err(why)`.
  - `ClaudeEngine::menu`: the `cwd` from the session file, `initialize`, `menu_from`. On any error: `{"commands":[],"models":[],"efforts":EFFORTS,"agents":[]}`.
- [ ] **Step 5: Run**; expect all passed. Run clippy.
- [ ] **Step 6: Commit** `feat(chat): Claude slash menu from a headless initialize`.

### Task 7: Claude model, effort and plan pickers

**Files:**
- Modify: `crates/agent/src/chat/picker.rs`, `crates/agent/src/chat/tmux_io.rs`, `crates/agent/src/chat/claude.rs` (`set`; the footer read in `menu`)

**Interfaces:**
- Consumes: `picker::initialize`, `menu_from`, `EFFORTS`; `tmux_io::paste`, `escape`.
- Produces: `tmux_io::capture(runner, name) -> Result<String, String>` (`capture-pane -p -t =<name>:`), `tmux_io::key(runner, name, key: &str) -> Result<(), String>`; `picker::footer_mode(screen: &str) -> Option<&'static str>`, `picker::CONFIRMS: [(&str, &str); 2]` = `[("model", "Switch model?"), ("effort", "Change effort level?")]` (provisional until Task 13 records them), `picker::confirm_ready(screen: &str, title: &str) -> bool`, `picker::switch_plan(ctx: &Arc<Ctx>, name: &str, pid: i32, on: bool) -> Result<(), String>`, `picker::type_setting(ctx: &Arc<Ctx>, name: &str, pid: i32, cmd: &str, value: &str) -> Result<(), String>`; `Engine::set` for `ClaudeEngine`.

- [ ] **Step 1: Write the failing tests** in `picker.rs`:

```rust
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
}
#[tokio::test]
async fn the_plan_switch_presses_shift_tab_until_the_footer_agrees_and_never_while_busy() {
    let (ctx, dir) = cycle_ctx("plan");     // status idle; the footer starts at bypass
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
    type_setting(&ctx, "cdash-a", 4242, "model", "haiku").await.unwrap();
    let keys = sent_keys(&dir);
    assert_eq!(keys, ["send-keys -t =cdash-a: Enter", "send-keys -t =cdash-a: Enter"], "Enter after the paste, Enter on Yes");
    let (ctx, dir) = confirm_ctx("other", " Background work is running\n ❯ 1. Exit and stop tasks\n");
    assert!(type_setting(&ctx, "cdash-a", 4242, "model", "haiku").await.is_err());
    assert_eq!(sent_keys(&dir), ["send-keys -t =cdash-a: Enter", "send-keys -t =cdash-a: Escape"]);
}
```

Helpers, written with `install_stub` and the session helpers from `claude::tests`. `presses(dir)` counts `BTab` in `tmux-args`; `sent_keys(dir)` lists its `send-keys` lines. `cycle_ctx` writes `<dir>/footers` with the five footer lines in cycle order (bypass, auto, manual, accept edits, plan) and a `tmux` that logs `$*` to `tmux-args`, keeps an index in `<dir>/mode` (default 0), on `send-keys … BTab` stores `(index + 1) % 5`, on `capture-pane` prints `● reply`, a blank line and line `index + 1` of `footers`, and on `display-message` prints `4242||`. `confirm_ctx(tag, screen)` writes `<dir>/screen` and `idle.json` / `waiting.json` session files (the second with `"status":"waiting","waitingFor":"dialog open"`), starts idle, and has a `tmux` that logs `$*`, copies `waiting.json` into `sessions/4242.json` on the first `send-keys … Enter`, copies `idle.json` on later Enters and on `Escape`, and prints `screen` on `capture-pane`.

In `claude.rs`:

```rust
#[tokio::test]
async fn set_takes_only_listed_values_and_refuses_before_typing() {
    let (ctx, dir) = fixture_ctx("set");
    write_session(&dir, r#"{"sessionId":"sid-1","cwd":"/proj-set","status":"busy"}"#);   // a cwd no other test caches
    picker::seed(std::path::Path::new("/proj-set"), serde_json::from_str(include_str!("testdata/initialize.json")).unwrap());
    let (engine, mut rx) = start(ctx, "cdash-a".into(), 4242);
    collect_until_ready(&mut rx).await;
    assert_eq!(engine.set("model", &json!("gpt-9")).await, Err("not a model this session offers".into()));
    assert_eq!(engine.set("effort", &json!("ultracode")).await, Err("not an effort level".into()));
    assert_eq!(engine.set("plan", &json!("yes")).await, Err("plan takes true or false".into()));
    assert_eq!(engine.set("agent", &json!("build")).await, Err("not supported".into()));
    assert_eq!(engine.set("model", &json!("haiku")).await, Err("Stop the turn or wait for it to finish".into()));
    assert!(!std::fs::read_to_string(dir.join("tmux-args")).unwrap_or_default().contains("send-keys"));
}
```

- [ ] **Step 2: Run** `cargo test -p cdash-agent chat::picker chat::claude`; expect failures.
- [ ] **Step 3: Implement.**
  - `footer_mode`: scan lines from the bottom; the first whose trimmed start is `⏵⏵ bypass permissions on`, `⏵⏵ auto mode on`, `⏸ manual mode on`, `⏵⏵ accept edits on` or `⏸ plan mode on` gives `bypassPermissions`, `auto`, `default`, `acceptEdits` or `plan`.
  - `confirm_ready`: the screen contains `title`, and a line whose trimmed text starts with `❯` contains `Yes`.
  - Idle gate (private), read from `session_file_for` right before each key: `idle` or `shell` pass; `waiting` is `"Answer the open question first"`; anything else `"Stop the turn or wait for it to finish"`.
  - `switch_plan`: gate; `capture` and `footer_mode` (`None` is `"Plan mode is unknown here; use Terminal"`); the target is `plan` or `bypassPermissions`; if not there, up to 5 times: gate, `key("BTab")`, then `capture` every 50 ms for up to 1 s until the footer changes (no change or `None` is an error), done when it shows the target. Five presses without it is `"Plan mode did not switch; use Terminal"`.
  - `type_setting`: gate; `paste("/<cmd> <value>")`; then every 100 ms for 2 s read the session file: while not `waiting`, keep watching (2 s with no dialog is success); `waiting` with `waitingFor == "dialog open"` and `confirm_ready(capture, title of cmd in CONFIRMS)` presses `Enter` and succeeds; any other `waiting` presses `escape` and fails with `"Claude opened an unexpected dialog; closed it"`.
  - `ClaudeEngine::set`: `plan` needs a bool, then `switch_plan`, then `live.plan = Some(on)`. `model` needs a string that is a `value` of `menu_from(initialize(cwd))["models"]`, then `type_setting`, then `live.model` = that entry's `model`. `effort` needs a string in `EFFORTS`, then `type_setting`, then `live.effort`. Any other `what` is `"not supported"`. Values are checked before any key.
  - `ClaudeEngine::menu` also captures the pane and sets `live.plan = Some(mode == "plan")` when the footer is recognised.
- [ ] **Step 4: Run**; expect all passed. Run clippy.
- [ ] **Step 5: Commit** `feat(chat): Claude model, effort and plan pickers`.

### Task 8: Codex question cards, plan switch, pickers and menu

**Files:**
- Modify: `crates/agent/src/collect/codex-remote.mjs` (`nativeLauncher`, and helpers next to `chatMessages`), `test/codex-native.test.mjs`

**Interfaces:**
- Consumes: `ChatFeed` `ops`, `putAsk`, `dropAsk`, `setSettings`; the `Codex` class's `call`, `send`, `models()`; Part 4's main-thread filter.
- Produces: ops `answer`, `set`, `menu` on the native feed; module functions `questionAsk(request) -> ask` and `codexAnswers(questions, answers) -> {[id]: {answers: string[]}}`; `CODEX_COMMANDS` (`compact` "Summarize the conversation to free context", `review` "Review uncommitted changes", `plan` "Turn plan mode on", `model` "Choose the model", `effort` "Choose the reasoning effort").

- [ ] **Step 1: Write the failing tests.** Extend the fixture's fake proxy: `thread/settings/update`, `thread/compact/start` and `review/start` answer `{}`; `model/list` answers `{data:[{model:'gpt-check',displayName:'GPT Check',hidden:false,supportedReasoningEfforts:[{reasoningEffort:'low'},{reasoningEffort:'high'}],defaultReasoningEffort:'low'}],nextCursor:null}`; `skills/list` answers `{data:[{cwd:'/p',errors:[],skills:[{name:'lint',description:'Run lint',enabled:true,path:'/p/.codex/skills/lint/SKILL.md',scope:'repo'}]}]}`. `SID` is the fixture's thread id; `res(c, rid)` waits for and returns that `res` line; `asksNow(c)` is the last `asks` line's array.

```js
test('questions from any thread become cards; an answer is the server-request response; resolution drops them', { timeout: 20000 }, async t => {
  const f = await fixture(t);
  await f.wait(() => f.native()?.chat_sock);
  const c = await client(f.native().chat_sock, { after: 0, epoch: null });
  await until(() => c.lines.some(l => l.t === 'ready'));
  const q = { id: 'q_file', header: 'File', question: 'Which file?', isOther: true, isSecret: false, options: [{ label: 'a.txt', description: 'A' }, { label: 'b.txt', description: 'B' }] };
  await f.inject({ id: 41, method: 'item/tool/requestUserInput', params: { threadId: 'child-thread', turnId: 't9', itemId: 'i9', isBlocking: true, questions: [q] } });
  await until(() => asksNow(c).length === 1);
  assert.deepEqual(asksNow(c)[0], { id: '41', kind: 'question', tool: 'codex', questions: [{ id: 'q_file', header: 'File', question: 'Which file?', multi: false, other: true, secret: false, options: [{ label: 'a.txt', description: 'A' }, { label: 'b.txt', description: 'B' }] }] });
  c.write({ t: 'req', rid: 1, op: 'answer', id: '41', answer: { answers: { q_file: { choices: ['b.txt'], text: 'keep it short' } } } });
  assert.equal((await res(c, 1)).ok, true);
  const reply = (await f.messages()).find(m => m.rpc?.id === 41 && m.rpc.result);
  assert.deepEqual(reply.rpc.result, { answers: { q_file: { answers: ['b.txt', 'user_note: keep it short'] } } });
  await until(() => asksNow(c).length === 0);
  await f.inject({ id: 42, method: 'item/tool/requestUserInput', params: { threadId: SID, turnId: 't9', itemId: 'i10', isBlocking: true, questions: [{ ...q, isOther: false }] } });
  await until(() => asksNow(c).length === 1);
  await f.inject({ method: 'serverRequest/resolved', params: { threadId: SID, requestId: 42 } });
  await until(() => asksNow(c).length === 0);
  c.write({ t: 'req', rid: 2, op: 'answer', id: '42', answer: { answers: { q_file: { choices: ['a.txt'] } } } });
  assert.deepEqual(await res(c, 2), { t: 'res', rid: 2, ok: false, error: 'already answered' });
  c.close();
});
test('the plan switch, pickers, menu, slash commands and Implement this plan? drive the app server', { timeout: 20000 }, async t => {
  const f = await fixture(t);
  await f.wait(() => f.native()?.chat_sock);
  const c = await client(f.native().chat_sock, { after: 0, epoch: null });
  await until(() => c.lines.some(l => l.t === 'ready'));
  c.write({ t: 'req', rid: 1, op: 'set', what: 'plan', value: true });
  assert.equal((await res(c, 1)).ok, true);
  assert.deepEqual(f.rpc('thread/settings/update').params, { threadId: SID, collaborationMode: { mode: 'plan', settings: { model: 'gpt-check', reasoning_effort: 'low', developer_instructions: null } } });
  await until(() => c.lines.some(l => l.t === 'settings' && l.settings.plan === true));
  c.write({ t: 'req', rid: 2, op: 'set', what: 'effort', value: 'high' });
  await res(c, 2);
  assert.deepEqual(f.rpc('thread/settings/update').params, { threadId: SID, effort: 'high' });
  c.write({ t: 'req', rid: 3, op: 'set', what: 'effort', value: 'ultra' });
  assert.deepEqual(await res(c, 3), { t: 'res', rid: 3, ok: false, error: 'unknown effort' });
  c.write({ t: 'req', rid: 4, op: 'menu' });
  const menu = (await res(c, 4)).data;
  assert.deepEqual(menu.commands.map(x => x.name), ['compact', 'review', 'plan', 'model', 'effort', 'lint']);
  assert.deepEqual([menu.models, menu.efforts, menu.agents], [[{ value: 'gpt-check', label: 'GPT Check', efforts: ['low', 'high'] }], ['low', 'high'], []]);
  await f.inject({ method: 'turn/started', params: { threadId: SID, turn: { id: 't5' } } });
  await f.inject({ method: 'item/completed', params: { threadId: SID, turnId: 't5', item: { id: 'p1', type: 'plan', text: '# Plan\n1. x' } } });
  await f.inject({ method: 'turn/completed', params: { threadId: SID, turn: { id: 't5', status: 'completed' } } });
  await until(() => asksNow(c).length === 1);
  assert.deepEqual(asksNow(c)[0], { id: 'implement:t5', kind: 'plan', tool: 'codex', plan: '# Plan\n1. x' });
  c.write({ t: 'req', rid: 5, op: 'answer', id: 'implement:t5', answer: { choice: 'yes' } });
  await res(c, 5);
  assert.equal(f.rpc('thread/settings/update').params.collaborationMode.mode, 'default');
  assert.equal(f.rpc('turn/start').params.input[0].text, 'Implement the plan.');
  for (const [cid, text] of [['c1', '/compact'], ['c2', '/review'], ['c3', '/lint fix it'], ['c4', '/diff']]) c.write({ t: 'send', cid, text, steer: false });
  await until(() => c.lines.filter(l => l.t === 'ack').length === 4);
  assert.ok(f.rpc('thread/compact/start'));
  assert.deepEqual(f.rpc('review/start').params, { threadId: SID, target: { type: 'uncommittedChanges' } });
  assert.deepEqual(f.rpc('turn/start').params.input, [{ type: 'skill', name: 'lint', path: '/p/.codex/skills/lint/SKILL.md' }, { type: 'text', text: 'fix it', text_elements: [] }]);
  assert.deepEqual(c.lines.find(l => l.t === 'ack' && l.cid === 'c4'), { t: 'ack', cid: 'c4', ok: false, error: 'Use Terminal for this command' });
  c.close();
});
```

- [ ] **Step 2: Run** `node --test test/codex-native.test.mjs`; expect the two tests to fail.
- [ ] **Step 3: Implement in `nativeLauncher`.** Keep `asks = new Map()` (ask id to `{ rpcId, questions }` or `{ implement: true }`), `lastPlan` and `settings` (`{model, effort, plan: null, agent: null, variant: null}` after `thread/start` or `thread/resume`, published with `setSettings`).
  - In the control handler, for every thread (in Part 4's block that handles server requests and `serverRequest/resolved` before `if (!mainThread(m, state.sid)) return;`): `item/tool/requestUserInput` puts `questionAsk(m)` (questions mapped as in the protocol reference: `multi: false`, `other: isOther || no options`, `secret: isSecret`, `options ?? []`); `serverRequest/resolved` drops `String(requestId)`. Other server requests only set `waiting`, as today, so they keep the banner.
  - Main thread only (after that line): `item/completed` with `item.type === 'plan'` sets `lastPlan = {turnId, text}`; `turn/started` drops asks with ids starting `implement:` and clears `lastPlan`; `turn/completed` with status `completed` whose turn id equals `lastPlan.turnId` puts `{id: 'implement:' + turnId, kind: 'plan', tool: 'codex', plan: text}`; `thread/settings/updated` also publishes `settings` with `plan: threadSettings.collaborationMode?.mode === 'plan'`.
  - `answer`: an unknown id throws `'already answered'`. An `implement:` ask is dropped; on `yes`, `thread/settings/update` with `collaborationMode` `default` (`settings: {model, reasoning_effort: state.effort, developer_instructions: null}`), then `turn/start` with input `[{type:'text', text:'Implement the plan.', text_elements:[]}]`, `effort: state.effort` and a new `clientUserMessageId`. A question: `control.send({ id: rpcId, result: { answers: codexAnswers(questions, answer.answers) } })` (the protocol reference's Codex column), then drop it.
  - `set`: `plan` (a boolean, else `'plan takes true or false'`) is `thread/settings/update` with `collaborationMode {mode: 'plan' | 'default', settings: {model: state.model, reasoning_effort: state.effort, developer_instructions: null}}`. `model` must be in `control.models()` (`'unknown model'`), sent as `{threadId, model, effort}` with the current effort if that model supports it, else its default. `effort` must be one of the current model's efforts (`'unknown effort'`), sent as `{threadId, effort}`. Anything else throws `'not supported'`. Update and publish `settings` after success; the `thread/settings/updated` notification confirms it.
  - `menu`: `CODEX_COMMANDS` plus enabled skills from `skills/list {cwds: [state.dir]}` as `{name, description}` (`shortDescription || description`, first 100 characters), at most 150 commands; models `{value: model, label, efforts}`; `efforts` of the current model; `agents: []`. Cache `control.models()` once it succeeds.
  - The feed's `send`, for text starting with `/<name>` and optional rest: `compact` is `thread/compact/start {threadId}`; `review` is `review/start {threadId, target: {type: 'uncommittedChanges'}}`; bare `plan` is the `set` op with `true`; `model` or `effort` with an argument is the `set` op; bare `model` or `effort` throws `'Use the model chip at the top of the chat'`; a skill name (from `skills/list`, read when needed) sends input `[{type:'skill', name, path}]` plus the rest as a text block, through the existing steer-or-start call; any other name throws `'Use Terminal for this command'`.
- [ ] **Step 4: Run** `node --test`; expect all passed, including the untouched worker tests.
- [ ] **Step 5: Commit** `feat(chat): Codex question cards, plan switch, pickers and menu`.

### Task 9: OpenCode question forms, pickers and menu

**Files:**
- Modify: `crates/agent/src/collect/opencode-remote.mjs` (`supervise` and `ChatBridge` only), `test/opencode-native.test.mjs`

**Interfaces:**
- Consumes: `ChatFeed` `ops`, `putAsk`, `dropAsk`, `setSettings`; `normalizeModels`, `modelRef`, `submitPrompt`.
- Produces: ops `answer`, `set`, `menu` on the OpenCode feed; module functions `formAsk(form) -> ask | null` and `formAnswer(form, answers) -> object`.

- [ ] **Step 1: Write the failing tests.** Extend the chat `fixture()`'s fake server (`GET …/form` keeps answering `{data: []}`): `POST …/form/<id>/reply` answers 204, or 409 when the id is `frm_done`; `GET /api/command` answers `{data:[{name:'review',description:'Review changes'}]}`; `GET /api/agent` answers `{data:[{id:'build',name:'build',mode:'primary',hidden:false},{id:'plan',name:'plan',mode:'primary',hidden:false},{id:'explore',name:'explore',mode:'subagent',hidden:false},{id:'title',name:'title',mode:'primary',hidden:true}]}`; `GET /api/model` answers `{data:[{providerID:'fixture',id:'model',name:'Model',variants:[{id:'high'}]},{providerID:'fixture',id:'other',name:'Other',variants:[]}]}`; `POST …/agent`, `…/model` and `…/command` answer 204. `form(id)` builds `{id, sessionID: f.native().sid, title: 'Questions', metadata: {kind: 'question'}, fields: [{key:'q0', type:'string', title:'Color', description:'Pick a color', options:[{value:'Red',label:'Red'},{value:'Blue',label:'Blue'}], custom:true}, {key:'q1', type:'multiselect', title:'Fruits', description:'Pick fruits', options:[{value:'Apple',label:'Apple'},{value:'Pear',label:'Pear'}], custom:true}]}`. Define `res` and `asksNow` in this file as in Task 8.

```js
test('question forms become cards, answers post the form reply, and settled forms drop', { timeout: 20000 }, async t => {
  const f = await fixture(t);
  await f.wait(() => f.native()?.chat_sock);
  const c = await client(f.native().chat_sock, { after: 0, epoch: null });
  await until(() => c.lines.some(l => l.t === 'ready'));
  await f.emit({ type: 'form.created', data: { form: form('frm_1') } });
  await until(() => asksNow(c).length === 1);
  assert.deepEqual(asksNow(c)[0], { id: 'frm_1', kind: 'question', tool: 'opencode', questions: [
    { id: 'q0', header: 'Color', question: 'Pick a color', multi: false, other: true, secret: false, options: [{ label: 'Red', description: '' }, { label: 'Blue', description: '' }] },
    { id: 'q1', header: 'Fruits', question: 'Pick fruits', multi: true, other: true, secret: false, options: [{ label: 'Apple', description: '' }, { label: 'Pear', description: '' }] }] });
  assert.ok(c.lines.some(l => l.t === 'state' && l.waiting === true));
  c.write({ t: 'req', rid: 1, op: 'answer', id: 'frm_1', answer: { answers: { q0: { choices: ['Blue'] }, q1: { choices: ['Apple'], text: 'Kiwi' } } } });
  assert.equal((await res(c, 1)).ok, true);
  assert.deepEqual(f.posted('/form/frm_1/reply'), { answer: { q0: 'Blue', q1: ['Apple', 'Kiwi'] } });
  await f.emit({ type: 'form.replied', data: { id: 'frm_1', sessionID: f.native().sid, answer: {} } });
  await until(() => asksNow(c).length === 0);
  await f.emit({ type: 'form.created', data: { form: form('frm_done') } });
  await until(() => asksNow(c).length === 1);
  c.write({ t: 'req', rid: 2, op: 'answer', id: 'frm_done', answer: { answers: { q0: { choices: ['Red'] }, q1: { choices: ['Pear'] } } } });
  assert.deepEqual(await res(c, 2), { t: 'res', rid: 2, ok: false, error: 'already answered' });
  await until(() => asksNow(c).length === 0);
  c.close();
});
test('pickers and the menu use the session API, and listed slash commands run as commands', { timeout: 20000 }, async t => {
  const f = await fixture(t);
  await f.wait(() => f.native()?.chat_sock);
  const c = await client(f.native().chat_sock, { after: 0, epoch: null });
  await until(() => c.lines.some(l => l.t === 'ready'));
  c.write({ t: 'req', rid: 1, op: 'menu' });
  assert.deepEqual((await res(c, 1)).data, { commands: [{ name: 'review', description: 'Review changes' }],
    models: [{ value: 'fixture/model', label: 'Model', variants: ['high'] }, { value: 'fixture/other', label: 'Other', variants: [] }],
    efforts: [], agents: [{ value: 'build', label: 'build' }, { value: 'plan', label: 'plan' }] });
  c.write({ t: 'req', rid: 2, op: 'set', what: 'agent', value: 'plan' }); await res(c, 2);
  assert.deepEqual(f.posted('/agent'), { agent: 'plan' });
  c.write({ t: 'req', rid: 3, op: 'set', what: 'agent', value: 'explore' });
  assert.deepEqual(await res(c, 3), { t: 'res', rid: 3, ok: false, error: 'unknown agent' });
  c.write({ t: 'req', rid: 4, op: 'set', what: 'variant', value: 'high' }); await res(c, 4);
  assert.deepEqual(f.posted('/model'), { model: { providerID: 'fixture', id: 'model', variant: 'high' } });
  c.write({ t: 'req', rid: 5, op: 'set', what: 'model', value: 'fixture/other' }); await res(c, 5);
  assert.deepEqual(f.posted('/model'), { model: { providerID: 'fixture', id: 'other' } });
  c.write({ t: 'send', cid: 'c1', text: '/review src', steer: false });
  await until(() => f.posted('/command'));
  assert.deepEqual(f.posted('/command'), { name: 'review', text: 'src' });
  c.write({ t: 'send', cid: 'c2', text: '/unknown x', steer: false });
  await until(() => f.posted('/prompt')?.text === '/unknown x');
  await f.emit({ type: 'session.agent.selected', data: { sessionID: f.native().sid, agent: 'plan' } });
  await until(() => c.lines.some(l => l.t === 'settings' && l.settings.agent === 'plan'));
  c.close();
});
```

- [ ] **Step 2: Run** `node --test test/opencode-native.test.mjs`; expect the two tests to fail.
- [ ] **Step 3: Implement in `supervise`.**
  - `formAsk(form)`: only when every field is `string` or `multiselect` without `when`; each field becomes `{id: key, header: title ?? '', question: description ?? title ?? '', multi: type === 'multiselect', other: custom === true || no options, secret: false, options: [{label, description: description ?? ''}]}`. Otherwise `null`, and the form keeps the banner.
  - `supervise` keeps the forms shown as cards in one `Map` (form id to form), shared by every `ChatBridge` it starts (a failed bridge is replaced) and by the `answer` op. `ChatBridge`'s `nativeEvent`: `form.created` for this session calls `addForm(data.form)` and returns; `form.replied` and `form.cancelled` for this session drop the ask before `super.nativeEvent`. `syncForms` reads `GET …/form`, drops asks of forms no longer listed, and calls `addForm` for each. `addForm` adds the id to `this.forms` (so `syncPermissions` keeps `requires_action`), puts `formAsk(form)` when not `null`, and calls `this.remote.state('requires_action', {tool_name: 'Terminal', action_description: 'Answer the pending form.'})`. It never emits the "Use Terminal" text message `RemoteBridge.showForm` sends.
  - `answer`: an unknown id throws `'already answered'`; post `{answer: formAnswer(form, answer.answers)}` (the protocol reference's OpenCode column); a 409 or 404 drops the ask and throws `'already answered'`; success drops it.
  - `set`: `agent` must be an agent from `GET /api/agent` whose `mode` is not `subagent` and that is not hidden, as the TUI's Tab cycle keeps them (`'unknown agent'`), posted to `…/agent`. `model` and `variant` are checked against `normalizeModels(GET /api/model)` (`'unknown model'`, `'unknown variant'`; an empty variant means none) and posted to `…/model` as `modelRef(model, variant)`; a model change keeps the current variant only if the new model lists it. Anything else throws `'not supported'`.
  - `menu`: as the test expects: commands with descriptions cut to 100 characters, at most 150; models from `normalizeModels` with `OpenCode: ` removed from labels; `efforts: []`; the agents `set` accepts, as `{value: id, label: name ?? id}`.
  - `send`: text starting with `/<name>` where `name` is in `GET /api/command` posts `…/command {name, text: rest}`; anything else goes to `submitPrompt` as today.
  - Settings `{model: state.model, effort: null, plan: null, agent: state.agent ?? null, variant: state.variant ?? null}` are published when the chat starts and after every `session.model.selected`, `session.agent.selected` (which sets `state.agent`) and reconnect read (which sets `state.agent` from the session's `agent`).
- [ ] **Step 4: Run** `node --test`; expect all passed, including the untouched `opencode-remote.test.mjs`.
- [ ] **Step 5: Commit** `feat(chat): OpenCode question cards, pickers and menu`.

### Task 10: The store keeps asks, settings and replies

**Files:**
- Modify: `public/chat/store.js`, `test/chat-store.test.mjs`

**Interfaces:**
- Produces: state fields `asks` (`[]`), `settings` (`{}`), `hooks` (`true`), `waiting_for` (`null`; if Part 4 already keeps it under another name, use that name), `menu` (`null`, then `{commands, models, efforts, agents}`), `answered` (`Map` id → `{ok, error}`), `setReply` (`null`, then `{ok, error}`); `cdashChatStore.needsTerminal(state, waitedMs) -> boolean`; `cdashChatStore.chipText(state) -> string`.

- [ ] **Step 1: Write the failing tests**

```js
const st = o => ({ t: 'state', working: false, waiting: false, ended: false, unreadable: false, held: [], ...o });
test('asks, settings and hooks come from state frames; menu, answered and set replies are kept', () => {
  let s = S.initial();
  assert.deepEqual([s.asks, s.settings, s.hooks, s.waiting_for, s.menu, s.setReply], [[], {}, true, null, null, null]);
  s = S.apply(s, st({ waiting: true, waiting_for: 'input needed', asks: [{ id: 'a1', kind: 'plan', tool: 'claude', plan: '# P' }], settings: { model: 'claude-opus-5-5' }, hooks: false }));
  assert.deepEqual([s.asks[0].id, s.settings.model, s.hooks, s.waiting_for], ['a1', 'claude-opus-5-5', false, 'input needed']);
  s = S.apply(s, { t: 'answered', id: 'a1', ok: false, error: 'already answered' });
  assert.deepEqual(s.answered.get('a1'), { ok: false, error: 'already answered' });
  s = S.apply(s, st({ asks: [] }));
  assert.equal(s.answered.size, 0, 'replies about asks that are gone are forgotten');
  s = S.apply(s, { t: 'menu', commands: [{ name: 'compact' }] });
  assert.deepEqual(s.menu, { commands: [{ name: 'compact' }], models: [], efforts: [], agents: [] });
  s = S.apply(s, { t: 'set', ok: false, error: 'Stop the turn or wait for it to finish' });
  assert.deepEqual(s.setReply, { ok: false, error: 'Stop the turn or wait for it to finish' });
});
test('the Terminal banner waits 500 ms for a card, except where no card can come', () => {
  const w = o => ({ waiting: true, asks: [], hooks: true, waiting_for: null, ...o });
  assert.equal(S.needsTerminal(w({}), 100), false);
  assert.equal(S.needsTerminal(w({}), 600), true);
  assert.equal(S.needsTerminal(w({ waiting_for: 'dialog open' }), 0), true);
  assert.equal(S.needsTerminal(w({ hooks: false }), 0), true);
  assert.equal(S.needsTerminal(w({ asks: [{ id: 'a' }] }), 9999), false);
  assert.equal(S.needsTerminal(w({ waiting: false }), 9999), false);
});
test('the header chip reads model, effort, Plan and a non-default agent', () => {
  assert.equal(S.chipText({ tool: 'claude', settings: { model: 'claude-opus-5-5', effort: 'high', plan: true } }), 'Opus · high · Plan');
  assert.equal(S.chipText({ tool: 'codex', settings: { model: 'gpt-5.4', effort: 'low', plan: false } }), 'gpt-5.4 · low');
  assert.equal(S.chipText({ tool: 'opencode', settings: { model: 'anthropic/claude-sonnet-5', variant: 'max', agent: 'plan' } }), 'claude-sonnet-5 · max · plan');
  assert.equal(S.chipText({ tool: 'opencode', settings: { model: 'a/b', variant: null, agent: 'build' } }), 'b');
  assert.equal(S.chipText({ tool: 'claude', settings: {} }), '');
});
```

- [ ] **Step 2: Run** `node --test test/chat-store.test.mjs`; expect the new tests to fail.
- [ ] **Step 3: Implement.** The `state` case also copies `asks`, `settings`, `hooks` (`f.hooks !== false`) and `waiting_for`, then deletes `answered` entries whose id is not in `asks`. New cases `answered`, `menu` (missing lists become `[]`) and `set`. `needsTerminal`: `waiting && asks.length === 0 && (waiting_for === 'dialog open' || !hooks || waitedMs >= 500)`. `chipText`: the model label is, for Claude, the word after `claude-` capitalised; for OpenCode, the part after the last `/`; otherwise the id. Then `effort ?? variant`, then `Plan` when `plan === true`, then for OpenCode the agent when it is not `build`, joined with ` · `, skipping empty parts.
- [ ] **Step 4: Run**; expect all passed.
- [ ] **Step 5: Commit** `feat(chat): store keeps asks, settings and replies`.

### Task 11: Question and plan cards in the chat view

**Files:**
- Create: `public/chat/cards.js`, `test/chat-cards.test.mjs`
- Modify: `public/chat/view.js`, `public/index.html`, `public/style.css`, `crates/agent/src/http/serve.rs` (asset arm), `crates/agent/src/http/routes.rs` (asset test), `test/frontend-guards.test.mjs`, `README.md`

**Interfaces:**
- Consumes: `cdashChatStore.needsTerminal`, state `asks`, `answered`, `hooks`; `cdashMd.render`.
- Produces: `globalThis.cdashCards = { render(box, state, send), complete(ask, picks), frame(ask, picks) }`, where `picks` is `{[questionId]: {choices: string[], text: string}}`; markup id `#chat-cards`.

- [ ] **Step 1: Write the failing tests.** `test/chat-cards.test.mjs`:

```js
import test from 'node:test';
import assert from 'node:assert/strict';
await import('../public/chat/cards.js');
const C = globalThis.cdashCards;
const ask = { id: 'a1', kind: 'question', tool: 'claude', questions: [
  { id: 'q0', header: 'Color', question: 'Pick a color', multi: false, other: true, secret: false, options: [{ label: 'Red', description: '' }, { label: 'Blue', description: '' }] },
  { id: 'q1', header: 'Fruits', question: 'Pick fruits', multi: true, other: false, secret: false, options: [{ label: 'Apple', description: '' }, { label: 'Pear', description: '' }] }] };
test('Submit waits for every question, and the frame carries choices and trimmed text', () => {
  assert.equal(C.complete(ask, {}), false);
  assert.equal(C.complete(ask, { q0: { choices: ['Red'], text: '' } }), false);
  assert.equal(C.complete(ask, { q0: { choices: [], text: '  ' }, q1: { choices: ['Pear'], text: '' } }), false);
  const picks = { q0: { choices: [], text: ' Green ' }, q1: { choices: ['Apple', 'Pear'], text: '' } };
  assert.equal(C.complete(ask, picks), true);
  assert.deepEqual(C.frame(ask, picks), { t: 'answer', id: 'a1', answers: { q0: { choices: [], text: 'Green' }, q1: { choices: ['Apple', 'Pear'], text: '' } } });
});
```

In `frontend-guards.test.mjs`: add `chat/cards.js` before `chat/view.js` to the script-order test and `chat-cards` to its id list; extend the markup guard to read `view.js` and `cards.js`, each passing the same `innerHTML` rule. In `routes.rs` `chat_assets_are_embedded`, add `("/chat/cards.js", "cdashCards")`.

- [ ] **Step 2: Run** `node --test` and `cargo test -p cdash-agent chat_assets`; expect failures.
- [ ] **Step 3: Implement `cards.js`** in the IIFE shape of `store.js`, touching `document` only inside functions.
  - `complete`: every question has a choice or trimmed text. `frame`: the protocol reference's question answer with trimmed text.
  - `render`: one `.chat-card` per ask, in order (a stack), rebuilt only when the asks or their `answered` entries change; picks live in a module `Map` by ask id, pruned when asks go. Question cards show each question's header and text, options as `aria-pressed` buttons (single choice) or checkboxes (multiple), and, when `other`, a text input (`type="password"` when `secret`) with placeholder "Other answer"; **Submit** is enabled only when `complete` (inputs update it in place). Plan cards render `cdashMd.render(ask.plan)` into a scrollable `.chat-card-plan`; Claude shows **Approve** (`{t:'answer', id, choice:'yes'}`) and a text box with **Keep planning** (`choice:'no'`, `text`); Codex shows "Implement this plan?" with **Yes** and **No**. After sending, the card says "Sending…" with its buttons disabled; an `answered` error of `already answered` shows "Already answered" and hides the buttons; any other error is shown and the buttons come back.
- [ ] **Step 4: Wire the view.** `index.html`: `<div id="chat-cards" class="chat-cards"></div>` before `#chat-held`, and `<script src="chat/cards.js"></script>` before `chat/view.js`. `serve.rs`: a `chat/cards.js` arm. `view.js`:
  - `render()` calls `cdashCards.render($('#chat-cards'), x.state, sendFrame)`; `close()` empties `#chat-cards`.
  - `onmessage` records `x.waitSince = Date.now()` when `waiting` turns true and schedules a render 550 ms later.
  - `renderBanner()` shows the Terminal banner when `cdashChatStore.needsTerminal(s, Date.now() - c.waitSince)`, with " Restart the session to answer from chat." appended when `!s.hooks`.
  - The composer placeholder while waiting is "Answer the card first" when asks exist, else "Answer in Terminal first".
- [ ] **Step 5: Add styles** under the chat section, with the chat tokens (`--chat-pad`, `--surface`, `--border`, `--accent`, `--r-md`, `--text-muted`): `.chat-cards` stacked with 8 px gaps above the held list; `.chat-card` bordered; `.chat-card-plan` with `max-height: 40vh; overflow: auto`; option buttons wrap.
- [ ] **Step 6: Update `README.md`'s Chat section.** Replace the bullet "When the session waits for an answer (a permission, a question, a plan approval), chat says so and points to Terminal." with: "Questions and plan approvals appear as cards above the composer and can be answered from chat; the first answer from any door wins and the card closes everywhere. Permission prompts and other dialogs still point to Terminal, as do Claude Code sessions started before upgrading until they are restarted."
- [ ] **Step 7: Run** `node --test` and `cargo test -p cdash-agent`; expect all passed.
- [ ] **Step 8: Commit** `feat(chat): question and plan cards`.

### Task 12: Slash menu, header chip and pickers sheet

**Files:**
- Create: `public/chat/menu.js`, `test/chat-menu.test.mjs`
- Modify: `public/chat/view.js`, `public/index.html`, `public/style.css`, `crates/agent/src/http/serve.rs`, `crates/agent/src/http/routes.rs`, `test/frontend-guards.test.mjs`, `README.md`

**Interfaces:**
- Consumes: state `menu`, `settings`, `tool`, `working`, `waiting`, `setReply`; `cdashChatStore.chipText`.
- Produces: `globalThis.cdashMenu = { wire(send, getState), render(state), reset(), filter(commands, text) }`; markup ids `#chat-chip`, `#chat-menu`, `#chat-settings`, `#chat-settings-body`, `#chat-settings-close`.

- [ ] **Step 1: Write the failing tests.** `test/chat-menu.test.mjs`:

```js
import test from 'node:test';
import assert from 'node:assert/strict';
await import('../public/chat/menu.js');
const { filter } = globalThis.cdashMenu;
const cmds = [{ name: 'compact' }, { name: 'clear' }, { name: 'config' }];
test('the slash menu filters by the typed prefix and closes once arguments start', () => {
  assert.deepEqual(filter(cmds, '/').map(c => c.name), ['compact', 'clear', 'config']);
  assert.deepEqual(filter(cmds, '/CO').map(c => c.name), ['compact', 'config']);
  assert.deepEqual(filter(cmds, '/compact x'), []);
  assert.deepEqual(filter(cmds, 'hello'), []);
  assert.equal(filter(Array.from({ length: 80 }, (_, i) => ({ name: 'c' + i })), '/c').length, 50);
});
```

In `frontend-guards.test.mjs`: add `chat/menu.js` before `chat/view.js` and the ids `chat-chip`, `chat-menu`, `chat-settings`; include `menu.js` in the markup guard. In `routes.rs`, add `("/chat/menu.js", "cdashMenu")`.

- [ ] **Step 2: Run** `node --test` and `cargo test -p cdash-agent chat_assets`; expect failures.
- [ ] **Step 3: Implement `menu.js`.**
  - `filter`: text must match `^\/(\S*)$`; case-insensitive name prefix; at most 50, in the given order.
  - Slash list in `#chat-menu` (`role="listbox"`): on composer `input`, show `filter(state.menu?.commands ?? [], value)` with each command's name, hint and description; typing `/` when `state.menu` is `null` sends `{t:'menu'}` once per open chat and shows "Loading…". A capture-phase `keydown` on `#chat-input` handles the open list: ArrowUp and ArrowDown move, Enter and Tab complete to `/<name> ` with the cursor at the end, Escape closes; each calls `preventDefault` and `stopImmediatePropagation`. A click completes too.
  - `#chat-chip` shows `chipText(state)` and is hidden when empty. Tapping it opens `#chat-settings` and sends `{t:'menu'}`.
  - The sheet, rebuilt from `state.menu`, `state.settings` and `state.tool`: a model `<select>` (current when `value` or `model` equals `settings.model`); an effort `<select>` (Claude `menu.efforts`, Codex the chosen model's `efforts`) or, for OpenCode, a variant `<select>` from the current model's `variants` plus "default" (`""`); a Plan switch (`role="switch"` checkbox) for Claude and Codex; an agent `<select>` for OpenCode. Claude adds "Also becomes your default for new Claude sessions" under model and effort and, while `working || waiting`, disables everything and says "Stop the turn or wait for it to finish". Codex adds "Changes apply from the next turn." A change sends `{t:'set', what, value}` and shows "Saving…" until the next `setReply`, whose error is shown in the sheet.
  - `reset` hides the list and closes the sheet.
- [ ] **Step 4: Wire the view.** `index.html`: `<button id="chat-chip" class="chat-chip" type="button" hidden></button>` after `.chat-heading`; `<div id="chat-menu" class="chat-menu" role="listbox" hidden></div>` just before `#chat-form`; a `<dialog id="chat-settings" class="dialog" aria-label="Session settings">` shaped like `#chat-sheet`, holding `#chat-settings-body` and the close button `#chat-settings-close`; `<script src="chat/menu.js"></script>` before `chat/view.js`. `serve.rs`: a `chat/menu.js` arm. `view.js`: `wireOnce` calls `cdashMenu.wire(sendFrame, () => c?.state ?? null)`; `render()` calls `cdashMenu.render(x.state)`; `close()` calls `cdashMenu.reset()`.
- [ ] **Step 5: Add styles:** `.chat-chip` as a small pill in the bar; `.chat-menu` floating above the composer at most 40vh tall and scrolling, `.chat-menu-item[aria-selected="true"]` highlighted with `--accent`; the sheet's rows and notes in `--text-muted`.
- [ ] **Step 6: Update `README.md`'s Chat section** with a bullet: "Type `/` for the session's commands. The chip at the top shows the model, effort and plan mode (for OpenCode, the variant and agent); tap it to change them. Claude Code's pickers work while the session is idle, and a model or effort change there also becomes the default for new Claude sessions; Codex applies changes from the next turn."
- [ ] **Step 7: Run** `node --test` and `cargo test -p cdash-agent`; expect all passed.
- [ ] **Step 8: Commit** `feat(chat): slash menu and session pickers`.

### Task 13: Real sessions, findings and the full gates

**Files:**
- Modify: `CLAUDE_FINDINGS.md`; `crates/agent/src/chat/picker.rs` and `crates/agent/src/collect/spawn.rs` only if a check below says so

This task needs the owner's machine and accounts; run it with them. Every command runs from `/mnt/d/git/claude-dashboard/.worktrees/chat-parts-2-5`. Record each result for the pull request description. Plan mode switches Claude to Sonnet (E1c), so the plan steps cost a little more than haiku.

- [ ] **Step 1: Run the gates.** `node --test`, `CARGO_TARGET_DIR=/home/p/.cache/chat-parts-2-5-target cargo test --all --locked`, the clippy command. Expected: all pass.
- [ ] **Step 2: Start a private server and a test agent.** The agent's plain `tmux` calls follow `$TMUX`, so they all reach `cdash-test`. Both start from a minimal environment so no `CLAUDE*` variable leaks into the sessions (E1 setup).

```bash
CARGO_TARGET_DIR=/home/p/.cache/chat-parts-2-5-target cargo build -p cdash-agent
CLEAN=(env -i "HOME=$HOME" "USER=$USER" "LOGNAME=$USER" "PATH=$PATH" "SHELL=$SHELL" LANG=C.UTF-8 TERM=xterm-256color "XDG_RUNTIME_DIR=$XDG_RUNTIME_DIR")
"${CLEAN[@]}" tmux -L cdash-test -f /dev/null new-session -d -s cdash-hold
SOCK=$(tmux -L cdash-test display-message -p '#{socket_path}')
mkdir -p /tmp/cdash-p2 && cp -p ~/.claude/settings.json /tmp/cdash-p2/settings.json.bak
"${CLEAN[@]}" "TMUX=$SOCK,0,0" PORT=23290 CDASH_HOOK_PORT=23291 /home/p/.cache/chat-parts-2-5-target/debug/cdash-agent > /tmp/cdash-p2/agent.log 2>&1 &
```

Open `http://127.0.0.1:23290` in the Windows browser. Expected: the dashboard loads and `agent.log` has no `hook listener off`.
- [ ] **Step 3: Claude launch.** Launch Claude Code in `/tmp/cdash-p2` with model `haiku`, effort `low`. Then `NAME=$(tmux -L cdash-test ls -F '#{session_name}' | grep '^cdash-cdash-p2-' | tail -1)` and `PID=$(tmux -L cdash-test display-message -p -t "=$NAME:" '#{pane_pid}')`; repeat both after each later launch. Expected: `tmux -L cdash-test show-options -v -t "=$NAME" @cdash_hook` prints 32 hex characters; `ps -o args= -p $PID` shows `"matcher":"AskUserQuestion|ExitPlanMode"`, `Bearer $CDASH_HOOK_TOKEN` literally, and `--remote-control`, and does not contain the token.
- [ ] **Step 4: A question answered from chat (also verifies the matcher admits AskUserQuestion).** Open Chat and send: "Use the AskUserQuestion tool once with two questions: 'Pick a color' (Red or Blue, single choice) and 'Pick fruits' (Apple, Pear; multiple choice). Then tell me my answers." Expected: a card with both questions and no Terminal banner; `agent.log` has `hook cdash-… AskUserQuestion: card`. Answer Blue, Apple and Pear, then Submit. Expected: the reply names Blue and "Apple, Pear". **Fallback:** if the TUI shows the dialog but `agent.log` has no hook line, the matcher is not honoured: remove `"matcher":"AskUserQuestion|ExitPlanMode",` from `hook_settings` and from its test, rerun `cargo test -p cdash-agent spawn`, rebuild, restart the agent, launch a new session, repeat this step, and record it.
- [ ] **Step 5: Answered in Terminal.** Send: "Use AskUserQuestion to ask me 'Pick a size' with options Small and Large." When the card shows, run `tmux -L cdash-test send-keys -t "=$NAME:" Enter`. Expected: the card disappears from chat within a second; the reply says Small. Ask again, then press `tmux -L cdash-test send-keys -t "=$NAME:" Escape`. Expected: the card disappears within 2 s. In both cases record whether Claude closed the held connection: `ss -tn state established '( sport = :23291 )'` before and after.
- [ ] **Step 6: The Plan switch.** Tap the chip, switch Plan on. Expected: `tmux -L cdash-test capture-pane -p -t "=$NAME:" | grep '⏸ plan mode on'` matches and the chip ends in "· Plan". Switch it off. Expected: the footer reads `⏵⏵ bypass permissions on` and "Plan" leaves the chip.
- [ ] **Step 7: A plan approved from chat (verifies the matcher admits ExitPlanMode).** Plan on; send "Make a 2-step plan to create hello.txt containing hi, write it to the plan file and call ExitPlanMode." Expected: a plan card rendered as Markdown. **Approve.** Expected: the footer shows bypass, the chip loses "Plan", `hello.txt` exists.
- [ ] **Step 8: Keep planning.** Plan on; ask for a 2-step plan for `bye.txt`; on the card type "Add a third step" and tap **Keep planning**. Expected: a new card whose plan has three steps; Approve it.
- [ ] **Step 9: A plan answered in Terminal, written in the same message as its ExitPlanMode.** Plan on; send "In ONE message, write a new 2-step plan for cya.txt to the plan file AND call ExitPlanMode in that same message." When the card shows, `tmux -L cdash-test send-keys -t "=$NAME:" Enter`. Expected: the card disappears within a second. Record whether the transcript's ExitPlanMode input held the previous plan (E1c).
- [ ] **Step 10: A permission dialog keeps the banner (verifies the matcher filters).** `tmux -L cdash-test send-keys -t "=$NAME:" BTab BTab` until the footer reads `⏸ manual mode on`; send "Run: echo spike > out.txt". Expected: the TUI shows a Bash permission dialog; chat shows the Terminal banner after half a second and no card; `agent.log` has no hook line for Bash (with the fallback: `Bash: empty`). Answer it with `send-keys … Enter`, then press `BTab` until the footer reads bypass.
- [ ] **Step 11: Model and effort, with the settings file backed up (Step 2).** First record each confirmation by hand: `tmux -L cdash-test send-keys -t "=$NAME:" '/model sonnet' Enter`, then `capture-pane -p` and `cat ~/.claude/sessions/$PID.json`. Record the exact dialog text, the option under `❯` and `waitingFor`, then press Escape. Repeat with `/effort high`; the dialog appears only when the cache may be warm, so send a short prompt first. If the title, the `Yes` line or `waitingFor` (`dialog open`) differ from `CONFIRMS` and `confirm_ready`, update them and their test, rerun `cargo test -p cdash-agent chat::picker`, rebuild and restart the agent. Then pick `sonnet` and later `haiku` in the sheet, and effort `medium`. Expected: chat confirms each dialog by itself, the chip follows after the next reply, and `agent.log` has no picker error. Restore: `cp -p /tmp/cdash-p2/settings.json.bak ~/.claude/settings.json && cmp ~/.claude/settings.json /tmp/cdash-p2/settings.json.bak`.
- [ ] **Step 12: The slash menu.** Type `/`. Expected: a list including `compact` and `clear`, without `model`, `config` or `usage`, within a second; no extra session card appears on the dashboard; `ls ~/.claude/projects/*cdash-p2*/` gains no transcript. Type `/co` and press Tab. Expected: `/compact ` in the box. Send bare `/config`. Expected: the row says "Not sent: Use Terminal for /config".
- [ ] **Step 13: Open in Claude.** Open the card's **Open in Claude** link in the Claude app. Expected: it connects. Ask a question as in Step 5 and answer it in the Claude app. Expected: the chat card disappears within a second.
- [ ] **Step 14: Codex.** Launch Codex in `/tmp/cdash-p2` with its cheapest model and effort `low`. Open Chat; switch Plan on; send "Before planning, ask me one question with request_user_input: which file name, a.txt or b.txt? Then plan creating it." Expected: a question card while the TUI shows its prompt. Answer `b.txt`. Expected: `tmux -L cdash-test capture-pane` shows the TUI's prompt closed; the turn ends with a plan, and the card "Implement this plan?" appears. **Yes.** Expected: a new turn starts with "Implement the plan." and `b.txt` is created. Send `/diff`. Expected: "Use Terminal for this command".
- [ ] **Step 15: OpenCode.** Launch OpenCode in `/tmp/cdash-p2` (a haiku model if one is configured). Expected: `ps -o args=` of its TUI shows `--auto`. Send "Use the question tool to ask me to pick a color: Red or Blue." Expected: a card; answer Blue; the reply names Blue. Send "Run `touch oc.txt`". Expected: it runs without a prompt. Pick agent `plan` in the sheet. Expected: the chip shows "plan"; switch back to `build`.
- [ ] **Step 16: Clean up.** `pkill -f 'chat-parts-2-5-target/debug/cdash-agent'`, `tmux -L cdash-test kill-server`, and `cmp ~/.claude/settings.json /tmp/cdash-p2/settings.json.bak` once more.
- [ ] **Step 17: Record the findings.** In `CLAUDE_FINDINGS.md`, add a section "Dashboard chat prompts and menus (Claude Code 2.1.323)" with what was observed: whether the `PermissionRequest` matcher filtered, the two confirmation dialogs' text, key and `waitingFor`, whether Claude closes a held hook on Esc or on a TUI answer, the stale-plan result of Step 9, each marked as observed and not a supported contract.
- [ ] **Step 18: Rerun the gates** if anything changed; then commit `docs: chat prompts and menus findings` (with `picker.rs`, `spawn.rs` and their tests if Steps 4 or 11 changed them).
