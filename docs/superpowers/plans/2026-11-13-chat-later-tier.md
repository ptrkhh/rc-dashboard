# Chat Part 5: Rewind and the Later Tier Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Chat shows the branch each tool continues from after a rewind in any door, can rewind to a prompt itself, and gains the Changes sheet, message polish, the Usage sheet, the running list and side questions, for Claude Code, Codex and OpenCode.

**Architecture:** The Claude tap follows the transcript's `parentUuid` chain and emits a `cut` event when a prompt branches; the hub truncates its log there and reloads its clients. Rewinds started from chat go through new `Engine` methods: the Claude engine drives the TUI's own rewind dialog, checking the screen before every key, and the Codex and OpenCode supervisors call their servers' revert APIs and reset their feeds. Usage and running tasks are engine snapshots kept in hub state. Side questions run as a separate one-shot process or server call and stream only to the socket that asked. The browser gets a highlighter, store helpers for the new rows, and one sheets module.

**Tech Stack:** Rust (axum 0.8, tokio, serde_json), Node 22 ES modules for the supervisors, vanilla JavaScript and CSS, `node --test`, `cargo test`, git and tmux on the agent host.

**Spec:** `docs/superpowers/specs/2026-11-13-chat-later-tier-design.md`. Shared contract: `docs/superpowers/plans/2026-11-13-chat-parts-2-5-shared.md`. Evidence: `docs/superpowers/specs/2026-11-13-chat-evidence/` (E4, E6 and E7 of `claude-2.1.323-live-spike.md`; `review-checks.md`; sections 1 to 7 of `codex-opencode-source-notes.md`). Part 1's plan, `2026-11-12-chat-core.md`, describes the code this builds on.

This plan runs last, after Parts 4, 2 and 3 exactly as their plans describe them (`2026-11-13-chat-push-reliability.md`, `2026-11-13-chat-prompts-menus.md`, `2026-11-13-chat-attachments.md`). It calls their names as built:
- from the contract's "Names the plans added": `NativeEngine::request`, `ChatFeed`'s `ops` table and setters, `Runner::spawn_piped`, `Ctx.own_pids`;
- from Part 2's plan: `tmux_io::capture`, `tmux_io::key`, `claude::Live`, `picker::route`;
- from Part 3's plan: image prompts with `files`, and the OpenCode supervisor's `chatPrompt` and `rowFiles`;
- from Part 4's plan: `waiting_for`, `install_stub`, `mainThread`.

## Global Constraints

- Work in `/mnt/d/git/claude-dashboard/.worktrees/chat-parts-2-5` on `feat/chat-parts-2-5`. Export `CARGO_TARGET_DIR=/home/p/.cache/chat-parts-2-5-target` in every shell that runs cargo.
- CI commands: `node --test`, `cargo test --all --locked`, `cargo clippy --all-targets --locked -- -D warnings -D clippy::disallowed_types`.
- Never `git add -A` or `git add .`: each commit adds its task's listed files by name. `claude-1-261102-19.xapk` stays untracked.
- No new npm packages and no new Rust crates.
- Agent subprocesses go through `Runner` (`crates/agent/src/host/cmd.rs`): the side question uses Part 2's `Runner::spawn_piped`, and Task 7 adds `Runner::run_capped`.
- Supervisors never write to stdout: it is the native TUI's terminal.
- Remote Control and Open in Claude keep working. The worker paths (`worker()` in `codex-remote.mjs`, `worker()` and `RemoteBridge` in `opencode-remote.mjs`, `claude-remote.mjs`) produce exactly today's output: the shared helpers this plan extends (`replayHistory`, `historyMessages`, `RemoteBridge.emitHistory`) gain options whose defaults keep it, and only chat passes them.
- The Claude launch command does not change.
- Every new file under `public/` goes into `embedded_asset` in `crates/agent/src/http/serve.rs`.
- CSS class names and ids must not match `/\.send[\s{.:-]/` or contain `send-input`; `app.js` must not contain `/api/keys`.
- A supervisor socket line is at most 64 KiB in either direction.
- The agent logs chat activity without message text: `chat rewind <name>`, `chat aside <name>`, `chat diff <name>`.
- Mark deliberate shortcuts with a `ponytail:` comment naming the ceiling.
- Real-session steps use the private server `tmux -L cdash-test` and `--model haiku` where the tool allows it. They never touch the default tmux server or the agent on 127.0.0.1:23274. No step runs `/model` or `/effort`.
- Commits use Conventional Commits and end with `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>` and `Claude-Session: https://claude.ai/code/session_0121cbef6XAzH3a3m9iNVdAr`.
- Values from the spec:
  - Claude's rewind selector opens with Esc, then Esc 250 ms later; after the restore key the engine waits up to 2 s for `idle`.
  - Rewinds only while the session is idle and not waiting.
  - Claude side questions above 100,000 context tokens ask first.
  - The diff stops at 2 MB (2 × 1024 × 1024 bytes) of output.
  - Git runs with `-c core.fsmonitor=false`, and every diff with `--no-color --no-ext-diff --no-textconv`.

## Review Focus

1. **Claude rewound in Terminal, then "Rewind to here" in chat before any new prompt.** The transcript's branch is now longer than the TUI's, so a step check must fail: chat backs out with Escape and refuses, and never presses a restore key. Task 5.
2. **Short prompts that repeat ("yes", "yes").** The target is found by position. One Up press too many or too few must fail the step check, not rewind to the other "yes". Task 5.
3. **A cut whose anchor is not in the hub's log** (a log built another way, or capped by Part 4's measured fix). Nothing is removed, so chat never hides what the tool continues from. Task 2.
4. **File names with spaces, quotes or non-ASCII characters, and a repository before its first commit,** in the Changes sheet. Task 7.
5. **Closing the side sheet, or losing the socket, mid-answer.** The `claude -p` process dies and its pid leaves `Ctx.own_pids`. Task 6.

## Protocol reference

Decided once here; tasks refer to it.

**Events.** Every event may carry `ts` (ISO 8601). New or changed shapes:

```
{"type":"cdash","subtype":"cut","after":U|null}                  Claude tap; the hub acts on it and never stores it
{"type":"cdash","subtype":"todo","uuid":U?,"items":[{"text":T,"status":"pending"|"in_progress"|"completed"}]}
{"type":"cdash","subtype":"staged","messages":N}                   OpenCode, while a revert is staged
{"type":"assistant","uuid":U,"message":{"id":M,"role":"assistant","content":[{"type":"thinking","thinking":T}]}}
prompt events:  "raw":T when the recorded text differs from the display text (Claude)
                "src":"steer" on a Codex prompt that was not the first of its turn
tool_result blocks: "diff":[{"file":P,"additions":N,"deletions":N,"patch":T?}] (OpenCode)
Claude results and cdash events now carry their record's "uuid"
```

**Snapshots** (hub state, not events):

```
usage: {"cwd":P|null,"model":M|null,"context":{"used":N,"window":N|null}|null,
        "tokens":{"input":N,"output":N,"cache_read":N,"cache_write":N}|null,"cost":USD|null}
tasks: [{"id":S,"kind":"shell"|"agent","title":T,"started":ISO-8601}]
```

**Browser and agent.** `state` gains `usage` and `tasks`.

```
client: {"t":"rewind","uuid":U,"files":B}  {"t":"rewind_undo"}  {"t":"aside","id":I,"text":T}  {"t":"aside_cancel","id":I}
server: {"t":"rewound","uuid":U|null,"ok":B,"text":T?,"attachments":N?,"error":E?}     uuid is null for an undo
        {"t":"aside","id":I,"text":T,"done":B,"error":E?}                               text is appended, not replaced
GET /api/chat/diff?session=<name> -> {"folder":P,"repo":B,"files":[{"path","status","additions","deletions","patch"}],"truncated":B}
        status: added | deleted | modified | untracked; additions, deletions and patch may be null
```

**Agent and supervisor** (Part 2's `req`/`res` envelope):

```
req ops: rewind {uuid, files} -> data {text}   rewind_undo   aside {id, text}   aside_cancel {id}
sup -> agent: {"t":"usage","usage":{…}}  {"t":"tasks","tasks":[…]}   both replayed after every hello
              {"t":"aside","id","text","done","error"?}                not replayed
              a new {"t":"hello",…} on a live connection: the supervisor reset its log (native.rs reads it as Reset)
```

## File structure

| File | Responsibility |
|---|---|
| `crates/agent/src/chat/rewind.rs` (new) | Claude rewind dialog: screen parsers and the key driver |
| `crates/agent/src/chat/diff.rs` (new) | The Changes sheet: git diff of the session's folder |
| `crates/agent/src/chat/testdata/{rewind,compact,facts}.jsonl` (new) | Tap fixtures |
| `public/chat/highlight.js` (new) | `cdashHighlight`: code to escaped, coloured HTML |
| `public/chat/sheets.js` (new) | `cdashSheets`: Changes, Usage, running list and side sheets |
| `test/chat-highlight.test.mjs`, `test/chat-sheets.test.mjs` (new) | Their tests |

Modified: `chat/{tap,hub,wire,claude,native,ws,mod}.rs`, `host/cmd.rs`, `http/{routes,serve}.rs`, `collect/{chat-feed,codex-remote,opencode-remote,opencode-api}.mjs`, `public/chat/{store,view,md,menu}.js`, `public/{app.js,index.html,style.css}`, `test/{chat-feed,codex-native,opencode-native,opencode-remote,chat-store,frontend-guards}.test.mjs`, `CLAUDE_FINDINGS.md`.

---

## Phase A: Claude's chain and the hub

### Task 1: The Claude tap follows the chain and emits cuts

**Files:**
- Modify: `crates/agent/src/chat/tap.rs`, `crates/agent/src/chat/testdata/claude.jsonl` (one parent)
- Create: `crates/agent/src/chat/testdata/rewind.jsonl`, `crates/agent/src/chat/testdata/compact.jsonl`

**Interfaces:**
- Consumes: Part 3's image-prompt path in `Tap::feed`.
- Produces:
  - the `cut` event and the `uuid`, `ts` and `raw` fields of the protocol reference;
  - private chain state on `Tap`: `nodes: HashMap<String, Node>` with `struct Node { parent: Option<String>, anchor: Option<String> }`, `last_main: Option<String>`, `anchor: Option<String>`.
  - Part 1's tap retract rule (`Typed`, `Tap.last`) is deleted. The hub's own retract stays.

- [ ] **Step 1: Create the fixtures.** `rewind.jsonl` holds 14 records of the spike's session `83f627fd` (E4: three prompts, a rewind to the second in Terminal, then a new prompt). They are cut to the fields the tap reads, with the scratch path replaced by `/tmp/spike`:

```
{"type":"attachment","uuid":"b2d9530c-cfa3-452a-8335-e1f1ef9c5d5a","parentUuid":"13e175dc-91a7-4edc-ad40-805e318e7202","isSidechain":false,"timestamp":"2026-11-13T04:32:13.019Z","attachment":{"type":"hook_additional_context"}}
{"type":"user","uuid":"96de607e-9a66-4b25-b198-2ff36a049aa1","parentUuid":"b2d9530c-cfa3-452a-8335-e1f1ef9c5d5a","isSidechain":false,"promptSource":"typed","timestamp":"2026-11-13T04:32:30.863Z","message":{"role":"user","content":[{"type":"text","text":"[Image #1] What color is this image? Answer in one word."},{"type":"image","source":{"type":"base64","media_type":"image/png","data":"iVBORw0KGgoAAAANSUhEUgAAAEAAAABACAIAAAAlC+aJAAAAb0lEQVR4nO3PAQkAAAyEwO9feoshgnABdLep8QUNyPEFDcjxBQ3I8QUNyPEFDcjxBQ3I8QUNyPEFDcjxBQ3I8QUNyPEFDcjxBQ3I8QUNyPEFDcjxBQ3I8QUNyPEFDcjxBQ3I8QUNyPEFDcjxBQ3IPanc8OLDQitxAAAAAElFTkSuQmCC"}}]}}
{"type":"assistant","uuid":"2b5570a2-c1a6-4d0e-8055-f93acbe70d91","parentUuid":"8bedbc38-97dc-4f65-afdb-f6028e0e988e","isSidechain":false,"timestamp":"2026-11-13T04:32:32.232Z","message":{"id":"msg_011CgzJSqFt5Bqe1nDCz7D5A","role":"assistant","model":"claude-haiku-5-5","content":[{"type":"thinking","thinking":"","signature":"sig"}],"usage":{"input_tokens":2,"cache_creation_input_tokens":25488,"cache_read_input_tokens":17977,"output_tokens":92}}}
{"type":"assistant","uuid":"93b39bbc-1450-4286-bdf2-0897106f4a76","parentUuid":"2b5570a2-c1a6-4d0e-8055-f93acbe70d91","isSidechain":false,"timestamp":"2026-11-13T04:32:32.346Z","message":{"id":"msg_011CgzJSqFt5Bqe1nDCz7D5A","role":"assistant","model":"claude-haiku-5-5","content":[{"type":"text","text":"Red."}],"usage":{"input_tokens":2,"cache_creation_input_tokens":25488,"cache_read_input_tokens":17977,"output_tokens":92}}}
{"type":"system","subtype":"turn_duration","uuid":"37929adb-63f0-4e64-b002-3f3c2f2a84b4","parentUuid":"cc860028-ea84-4aff-a194-b01a1bc77a9c","isSidechain":false,"isMeta":false,"timestamp":"2026-11-13T04:32:32.403Z","durationMs":1591}
{"type":"user","uuid":"dd5d85d6-4fed-4e5a-bd06-4aaf58dea9b9","parentUuid":"37929adb-63f0-4e64-b002-3f3c2f2a84b4","isSidechain":false,"promptSource":"typed","timestamp":"2026-11-13T04:32:47.676Z","message":{"role":"user","content":"/tmp/spike/red.png Reply with only: ok"}}
{"type":"assistant","uuid":"2180fb2f-e0df-41f8-ae49-5d68a6732f3b","parentUuid":"2ddfe528-538a-4428-baaf-5c1aae6428d7","isSidechain":false,"timestamp":"2026-11-13T04:32:48.653Z","message":{"id":"msg_011CgzJU4XdQ35MKgia2w5D7","role":"assistant","model":"claude-haiku-5-5","content":[{"type":"text","text":"ok"}],"usage":{"input_tokens":2,"cache_creation_input_tokens":298,"cache_read_input_tokens":43465,"output_tokens":4}}}
{"type":"system","subtype":"turn_duration","uuid":"8a67cba9-8aaf-4c97-b616-5a73ade75388","parentUuid":"b421db3e-739f-436a-806e-0c39024b9798","isSidechain":false,"isMeta":false,"timestamp":"2026-11-13T04:32:48.694Z","durationMs":1022}
{"type":"user","uuid":"ea31531f-5d95-4d92-a42e-39428c1ed6eb","parentUuid":"8a67cba9-8aaf-4c97-b616-5a73ade75388","isSidechain":false,"promptSource":"typed","timestamp":"2026-11-13T04:33:12.814Z","message":{"role":"user","content":[{"type":"text","text":"[Image #2] Reply with only: ok3"},{"type":"image","source":{"type":"base64","media_type":"image/png","data":"iVBORw0KGgoAAAANSUhEUgAAAEAAAABACAIAAAAlC+aJAAAAb0lEQVR4nO3PAQkAAAyEwO9feoshgnABdLep8QUNyPEFDcjxBQ3I8QUNyPEFDcjxBQ3I8QUNyPEFDcjxBQ3I8QUNyPEFDcjxBQ3I8QUNyPEFDcjxBQ3I8QUNyPEFDcjxBQ3I8QUNyPEFDcjxBQ3IPanc8OLDQitxAAAAAElFTkSuQmCC"}}]}}
{"type":"assistant","uuid":"e137e03c-50a7-4c6c-8b09-5b0089a95ee4","parentUuid":"11806c77-527a-4835-84c0-166f3d976ffc","isSidechain":false,"timestamp":"2026-11-13T04:33:14.021Z","message":{"id":"msg_011CgzJVuvkuwBHPnZDH8qUD","role":"assistant","model":"claude-haiku-5-5","content":[{"type":"text","text":"ok3"}],"usage":{"input_tokens":2,"cache_creation_input_tokens":256,"cache_read_input_tokens":43767,"output_tokens":5}}}
{"type":"system","subtype":"turn_duration","uuid":"44ede06f-c882-46dc-8efa-42a4079e39fe","parentUuid":"7b99b459-cdd4-4183-8d84-1a271babebaf","isSidechain":false,"isMeta":false,"timestamp":"2026-11-13T04:33:14.065Z","durationMs":1286}
{"type":"user","uuid":"6fd3884f-3237-4929-aab5-9ea3f714f4ad","parentUuid":"37929adb-63f0-4e64-b002-3f3c2f2a84b4","isSidechain":false,"promptSource":"typed","timestamp":"2026-11-13T04:34:10.043Z","message":{"role":"user","content":"Reply with only: new4"}}
{"type":"assistant","uuid":"59c78581-de4e-4f3b-a493-656098e41e09","parentUuid":"7288e018-a661-4c30-bd76-a1ad264cdf98","isSidechain":false,"timestamp":"2026-11-13T04:34:11.063Z","message":{"id":"msg_011CgzJa9QDyegVYHfAKiRid","role":"assistant","model":"claude-haiku-5-5","content":[{"type":"text","text":"new4"}],"usage":{"input_tokens":2,"cache_creation_input_tokens":254,"cache_read_input_tokens":43465,"output_tokens":4}}}
{"type":"system","subtype":"turn_duration","uuid":"db5a7c16-64a2-4664-8f9a-652a769dbbce","parentUuid":"5defa36b-4aad-4575-ba89-d9504f3d2b29","isSidechain":false,"isMeta":false,"timestamp":"2026-11-13T04:34:11.100Z","durationMs":1066}
```

`compact.jsonl` is an auto-compaction from one of the owner's transcripts, kept to its structure; both texts are replaced:

```
{"type":"system","subtype":"turn_duration","uuid":"1f2a686a-6106-4dc8-8612-3ab8910a20f8","parentUuid":"03a82745-6f94-4455-a312-d25ef099b24f","isSidechain":false}
{"type":"system","subtype":"compact_boundary","uuid":"2fed1735-26ac-4cdf-9015-359a74d55d90","parentUuid":null,"logicalParentUuid":"1f2a686a-6106-4dc8-8612-3ab8910a20f8","isSidechain":false}
{"type":"user","uuid":"fd277824-f7a9-494c-96f0-90723428e000","parentUuid":"2fed1735-26ac-4cdf-9015-359a74d55d90","isSidechain":false,"isCompactSummary":true,"isVisibleInTranscriptOnly":true,"message":{"role":"user","content":"This session is being continued from a previous conversation that ran out of context."}}
{"type":"attachment","uuid":"63c28e42-9102-4443-9bff-2305a35cdeb9","parentUuid":"fd277824-f7a9-494c-96f0-90723428e000","isSidechain":false,"attachment":{"type":"file"}}
{"type":"system","subtype":"informational","uuid":"75928b5b-9212-4e7e-a93b-ab9933e90693","parentUuid":"7645452b-6009-4deb-9800-422ca6b37e27","isSidechain":false}
{"type":"user","uuid":"a7e885f8-2a43-43d9-afd3-e119d30842f2","parentUuid":"75928b5b-9212-4e7e-a93b-ab9933e90693","isSidechain":false,"promptSource":"typed","message":{"role":"user","content":"next step"}}
```

- [ ] **Step 2: Write the failing tests** in `tap.rs`.
  - In `claude.jsonl`, Part 3's appended line `g1` gets `"parentUuid":"cs1"` instead of `"q1"`. A real prompt continues from the record before it, here the compact summary; left on `q1` it is a branch the new rule would cut.
  - In `the_fixture_becomes_exactly_these_events_in_this_order`, the tag `"cdash:retract"` becomes `"cdash:cut"`.
  - In `details_chat_depends_on`, the retract assertion becomes the two lines below (index 16: Part 3 appended its lines at the end).

```rust
assert_eq!(evs[16]["after"], "s3", "a handed-back prompt is cut back to its turn's end");
assert_eq!((evs[4]["uuid"].as_str(), evs[12]["uuid"].as_str()), (Some("s2"), Some("i1")), "results carry their record's uuid");
```

```rust
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
```

- [ ] **Step 3: Run** `cargo test -p cdash-agent chat::tap`; expect the new and changed tests to fail.
- [ ] **Step 4: Implement in `Tap::feed`.**
  - **Main chain:** every record that is not `isSidechain: true` and has a `uuid`. This includes `isMeta`, `isCompactSummary` and `isVisibleInTranscriptOnly` records, attachments and system records. Those three flags still produce no events, so their early return moves below the bookkeeping.
  - **Cut:** just before a prompt event is emitted (rule 7's prompt and slash-command path, and Part 3's image prompt), check the record's `parentUuid`. If it is `Some(p)`, `nodes` holds `p`, and `Some(p) != last_main`, first emit `{"type":"cdash","subtype":"cut","after":nodes[p].anchor}`, with `null` when that anchor is `None`.
  - **Stamping:** every event from the record except a cut gets `uuid` = the record's `uuid` unless it already has one, and `ts` = the record's `timestamp` when present.
  - **Bookkeeping, after the events:** if the record emitted an event that carries a uuid, `anchor` becomes that uuid. Then `nodes.insert(uuid, Node { parent, anchor: anchor.clone() })` and `last_main = Some(uuid)`.
  - **`raw`:** the recorded text, set when it differs from the display text. That is the string content as written, or the text blocks joined, before paste unwrapping, `[Image #N]` removal and `Attached:` line removal.
  - Delete rule 8 (retract) and `Typed`.
  - `// ponytail: one map entry per record for the conversation's life (~150 bytes each); forget entries before the last compaction if memory ever shows it.`
- [ ] **Step 5: Run**; expect every `chat::tap` test to pass. Run clippy.
- [ ] **Step 6: Commit** `feat(chat): Claude tap follows the parentUuid chain and emits cuts` (`tap.rs`, `claude.jsonl` and the two new fixtures).

### Task 2: The hub applies cuts and keeps usage and tasks

**Files:**
- Modify: `crates/agent/src/chat/hub.rs`

**Interfaces:**
- Consumes: `cut` events (Task 1).
- Produces:
  - `EngineMsg::Usage(Value)` and `EngineMsg::Tasks(Vec<Value>)`;
  - state keys `usage` (the latest snapshot, `null` until one arrives) and `tasks` (`[]`);
  - `Hub::folder(&self) -> Option<String>`: the latest snapshot's `cwd`;
  - private `Actor::cut_to(&mut self, len: usize)` (Task 4 reuses it), and `Actor::backlog(&self, after: Option<u64>, epoch: Option<String>) -> Vec<Frame>`, which holds the frames `subscription` returns, factored out.

- [ ] **Step 1: Write the failing tests.** New helpers sit next to the existing ones: `result(uuid)` = `{"type":"result","subtype":"success","uuid":uuid}`, and `cut(after: Option<&str>)`. `Status` takes Part 4's `waiting_for: None`.

```rust
async fn log6(tx: &mpsc::Sender<EngineMsg>) {
    for e in [prompt("u1", "a"), answer("k1"), result("s1"), prompt("u2", "b"), answer("k2"), result("s2")] { tx.send(EngineMsg::Ev(e)).await.unwrap(); }
    settle().await;
}
fn last_state(s: &Subscription) -> Value { serde_json::from_str(s.backlog.last().unwrap()).unwrap() }
#[tokio::test]
async fn a_cut_truncates_after_its_anchor_and_reloads_every_client() {
    let (hub, tx, _) = rig();
    log6(&tx).await;
    let mut sub = hub.subscribe(None, None).await.unwrap();
    let old: Value = serde_json::from_str(&sub.backlog[0]).unwrap();
    tx.send(EngineMsg::Ev(cut(Some("s1")))).await.unwrap();
    tx.send(EngineMsg::Ev(prompt("u3", "c"))).await.unwrap();
    settle().await;
    let f = frames(&mut sub);
    assert_eq!((f[0]["t"].as_str(), f[0]["reset"].as_bool(), f[0]["head"].as_u64()), (Some("hello"), Some(true), Some(3)));
    assert_ne!(f[0]["epoch"], old["epoch"]);
    assert_eq!(f[1..4].iter().map(|x| x["ev"]["uuid"].as_str().unwrap()).collect::<Vec<_>>(), ["u1", "k1", "s1"]);
    assert_eq!(f[4]["t"], "state");
    assert_eq!((f[5]["seq"].as_u64(), f[5]["ev"]["uuid"].as_str()), (Some(4), Some("u3")), "the cut itself is not stored");
}
#[tokio::test]
async fn a_cut_that_removes_nothing_or_cannot_be_placed_changes_nothing() {
    let (hub, tx, _) = rig();
    log6(&tx).await;
    let mut sub = hub.subscribe(None, None).await.unwrap();
    for after in [Some("s2"), Some("not-in-this-log")] { tx.send(EngineMsg::Ev(cut(after))).await.unwrap(); }
    settle().await;
    assert!(frames(&mut sub).is_empty(), "no new epoch, no reload, nothing hidden");
}
#[tokio::test]
async fn a_cut_keeps_pending_and_held_messages() {
    let (hub, tx, _) = rig();
    log6(&tx).await;
    hub.command(send("c1", "next")).await;
    tx.send(EngineMsg::Status { working: true, waiting: false, waiting_for: None }).await.unwrap();
    settle().await;
    hub.command(send("c2", "later")).await;
    let mut sub = hub.subscribe(None, None).await.unwrap();
    tx.send(EngineMsg::Ev(cut(Some("s1")))).await.unwrap();
    tx.send(EngineMsg::Ev(prompt("u3", "next"))).await.unwrap();
    settle().await;
    let f = frames(&mut sub);
    assert_eq!(f.iter().find(|x| x["ev"]["uuid"] == "u3").unwrap()["ev"]["cid"], "c1", "the prompt after a cut still confirms it");
    assert_eq!(f.iter().rev().find(|x| x["t"] == "state").unwrap()["held"][0]["text"], "later");
}
#[tokio::test]
async fn usage_and_tasks_are_state_not_history_and_a_reset_clears_them() {
    let (hub, tx, _) = rig();
    let st = last_state(&hub.subscribe(None, None).await.unwrap());
    assert_eq!((st["usage"].clone(), st["tasks"].clone()), (Value::Null, json!([])), "keys always present");
    tx.send(EngineMsg::Usage(json!({"cwd":"/w","context":{"used":5,"window":null}}))).await.unwrap();
    tx.send(EngineMsg::Tasks(vec![json!({"id":"b1","kind":"shell","title":"sleep 60","started":"2026-11-13T10:00:00Z"})])).await.unwrap();
    settle().await;
    let s = hub.subscribe(None, None).await.unwrap();
    assert_eq!((last_state(&s)["usage"]["cwd"].as_str(), last_state(&s)["tasks"][0]["id"].as_str()), (Some("/w"), Some("b1")));
    assert_eq!(serde_json::from_str::<Value>(&s.backlog[0]).unwrap()["head"], 0, "snapshots are not events");
    assert_eq!(hub.folder().await.as_deref(), Some("/w"));
    tx.send(EngineMsg::Reset).await.unwrap();
    settle().await;
    let st = last_state(&hub.subscribe(None, None).await.unwrap());
    assert_eq!((st["usage"].clone(), st["tasks"].clone(), hub.folder().await), (Value::Null, json!([]), None));
}
```

- [ ] **Step 2: Run** `cargo test -p cdash-agent chat::hub`; expect a compile failure.
- [ ] **Step 3: Implement.**
  - **Placing a cut** in `event()` (a `cdash/cut` event is never stored): `after: null` places it at 0; otherwise one past the last event whose `uuid` equals `after`, skipping `cdash/retract` events, whose `uuid` names the prompt they withdraw. An `after` that is not in the log places nothing.
  - **`cut_to(len)`:** return when `len >= log.len()`. Otherwise truncate the log, take a new epoch, and broadcast every frame of `backlog(None, None)`: a `hello` with `reset: true`, the last `BACKLOG` events and `state`, so every client reloads exactly as a new one would. Pending entries and the queue are left untouched.
  - **Snapshots:** `Usage` and `Tasks` replace the stored value. `Reset` sets `usage` to `null` and `tasks` to `[]`. Both keys go into every `state` frame.
  - **`Hub::folder`:** a new `Cmd::Folder { reply: oneshot::Sender<Option<String>> }`.
  - **If Part 4's conditional Task 24 ran** (a 2,000-event window, older pages from `Engine::history`): placing a cut becomes one function, used by the hub and by Claude's `history` replay. Applying cuts to the replayed events keeps replayed positions equal to the hub's.
- [ ] **Step 4: Run**; expect every hub test to pass. Run clippy.
- [ ] **Step 5: Commit** `feat(chat): hub applies branch cuts and keeps usage and tasks in state`.

### Task 3: Claude facts: thinking, checklist, usage, running tasks

**Files:**
- Modify: `crates/agent/src/chat/tap.rs`, `crates/agent/src/chat/claude.rs`
- Create: `crates/agent/src/chat/testdata/facts.jsonl`

**Interfaces:**
- Consumes: `EngineMsg::Usage`, `EngineMsg::Tasks` (Task 2).
- Produces:
  - thinking blocks and `cdash/todo` events (protocol reference);
  - `Tap::usage(&self) -> Value` and `Tap::tasks(&self) -> Vec<Value>` (snapshot shapes; `cost` is always `null` for Claude);
  - `pub fn context_window(model: &str) -> Option<u64>` in `tap.rs`;
  - the Claude engine sends `Usage` and `Tasks` after `Ready`, whenever they differ from what it last sent. The first `Usage` always goes out, and an empty task list is never sent first.

- [ ] **Step 1: Read each model family's context window from a status-line sample.** No prompt is sent, so no model is called:

```bash
mkdir -p /tmp/cdash-p5/sl && cd /tmp/cdash-p5/sl
for m in opus sonnet fable haiku; do
  tmux -L cdash-test -f /dev/null new-session -d -s "sl-$m" -x 160 -y 50 -c /tmp/cdash-p5/sl \
    "claude --model $m --settings '{\"statusLine\":{\"type\":\"command\",\"command\":\"cat > /tmp/cdash-p5/sl/$m.json\"}}'"
done
sleep 4; for m in opus sonnet fable haiku; do tmux -L cdash-test send-keys -t "=sl-$m:" Down Enter; done   # folder-trust dialog
sleep 8; for m in opus sonnet fable haiku; do jq -r '[.model.id, .context_window.context_window_size] | @tsv' "$m.json"; done
tmux -L cdash-test kill-server
```

Expected: one id and window per family; 1000000 for opus-5-5, sonnet-5-5 and fable-5-2 (the spec expects 1M). The table in Step 4 holds exactly the families sampled here. A family with no sample stays out, so it shows tokens only.

- [ ] **Step 2: Create `facts.jsonl`** (synthetic, in the shapes E7 recorded):

```
{"type":"assistant","uuid":"f1","cwd":"/proj","message":{"id":"m1","model":"claude-opus-5-5","role":"assistant","content":[{"type":"thinking","thinking":"Check the tests first.","signature":"s"}],"usage":{"input_tokens":3,"cache_creation_input_tokens":100,"cache_read_input_tokens":900,"output_tokens":20}}}
{"type":"assistant","uuid":"f2","parentUuid":"f1","cwd":"/proj","message":{"id":"m1","model":"claude-opus-5-5","role":"assistant","content":[{"type":"tool_use","id":"toolu_t","name":"TodoWrite","input":{"todos":[{"content":"Read","status":"completed","activeForm":"Reading"},{"content":"Fix","status":"in_progress","activeForm":"Fixing"},{"content":"Test","status":"pending","activeForm":"Testing"}]}}],"usage":{"input_tokens":3,"cache_creation_input_tokens":100,"cache_read_input_tokens":900,"output_tokens":20}}}
{"type":"assistant","uuid":"f3","parentUuid":"f2","message":{"id":"m2","model":"claude-opus-5-5","role":"assistant","content":[{"type":"tool_use","id":"toolu_b","name":"Bash","input":{"command":"sleep 60","description":"Sleep a minute","run_in_background":true}}],"usage":{"input_tokens":5,"cache_creation_input_tokens":50,"cache_read_input_tokens":1000,"output_tokens":10}}}
{"type":"user","uuid":"f4","parentUuid":"f3","timestamp":"2026-11-13T10:00:05.000Z","toolUseResult":{"stdout":"","stderr":"","interrupted":false,"backgroundTaskId":"bo29v19cm"},"message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"toolu_b","content":"Command running in background with ID: bo29v19cm."}]}}
{"type":"assistant","uuid":"f5","parentUuid":"f4","message":{"id":"m3","model":"claude-opus-5-5","role":"assistant","content":[{"type":"tool_use","id":"toolu_a","name":"Agent","input":{"description":"Scan the repo","prompt":"List the modules.","subagent_type":"Explore"}}]}}
{"type":"user","uuid":"f6","parentUuid":"f5","timestamp":"2026-11-13T10:00:07.000Z","toolUseResult":{"isAsync":true,"status":"async_launched","agentId":"a6cc68d2a048556bd","description":"Scan the repo"},"message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"toolu_a","content":"Async agent launched successfully."}]}}
{"type":"queue-operation","operation":"enqueue","content":"<task-notification>\n<task-id>bo29v19cm</task-id>\n<tool-use-id>toolu_b</tool-use-id>\n<status>completed</status>\n<summary>Background command \"Sleep a minute\" completed (exit code 0)</summary>\n</task-notification>"}
{"type":"assistant","uuid":"f8","parentUuid":"f6","message":{"id":"m4","model":"claude-opus-5-5","role":"assistant","content":[{"type":"tool_use","id":"toolu_s","name":"TaskStop","input":{"task_id":"a6cc68d2a048556bd"}}]}}
{"type":"user","uuid":"f9","parentUuid":"f8","toolUseResult":{"message":"Successfully stopped task: a6cc68d2a048556bd (Scan the repo)","task_id":"a6cc68d2a048556bd","task_type":"local_agent"},"message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"toolu_s","content":"Successfully stopped task"}]}}
{"type":"attachment","uuid":"f10","parentUuid":"f9","timestamp":"2026-11-13T11:00:00.000Z","attachment":{"type":"task_status","taskId":"bw2ckxil8","taskType":"local_bash","description":"npm run dev","status":"running","shell":{"command":"npm run dev"}}}
{"type":"assistant","uuid":"f11","parentUuid":"f10","cwd":"/proj/.worktrees/a","message":{"id":"m5","model":"claude-opus-5-5","role":"assistant","content":[{"type":"tool_use","id":"toolu_c","name":"TaskCreate","input":{"subject":"Ship it","description":"Release the build"}}]}}
{"type":"user","uuid":"f12","parentUuid":"f11","toolUseResult":{"task":{"id":"1","subject":"Ship it"}},"message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"toolu_c","content":"Task #1 created successfully: Ship it"}]}}
{"type":"assistant","uuid":"f13","parentUuid":"f12","message":{"id":"m6","model":"claude-opus-5-5","role":"assistant","content":[{"type":"tool_use","id":"toolu_u","name":"TaskUpdate","input":{"taskId":"1","status":"completed"}}]}}
{"type":"user","uuid":"f14","parentUuid":"f13","toolUseResult":{"success":true},"message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"toolu_u","content":"Updated task #1 status"}]}}
{"type":"attachment","uuid":"f15","parentUuid":"f14","attachment":{"type":"queued_command","prompt":"<task-notification>\n<task-id>bw2ckxil8</task-id>\n<status>killed</status>\n</task-notification>","commandMode":"task-notification"}}
```

- [ ] **Step 3: Write the failing tests** in `tap.rs`, plus the engine test in `claude.rs`:

```rust
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
// claude.rs
#[tokio::test]
async fn the_engine_reports_usage_once_ready_with_the_session_folder_as_fallback() {
    let (ctx, _dir) = fixture_ctx("facts");
    let (_engine, mut rx) = start(ctx, "cdash-a".into(), 4242);
    collect_until_ready(&mut rx).await;
    let EngineMsg::Usage(u) = next_of(&mut rx, |m| matches!(m, EngineMsg::Usage(_))).await else { unreachable!() };
    assert_eq!(u["cwd"], "/proj", "no record carries a cwd: the session file's");
}
```

- [ ] **Step 4: Run** `cargo test -p cdash-agent chat::tap chat::claude`; expect failures.
- [ ] **Step 5: Implement.**
  - `Rec` gains `cwd`, `timestamp` and `tool_use_result` (`toolUseResult`).
  - **Thinking:** a `thinking` block with non-empty text becomes `{"type":"thinking","thinking":text}`.
  - **Remembered calls:** a private `calls: HashMap<String, (String, Value)>` maps a tool-use id to its name and input, for `Bash`, `Agent`, `TaskCreate`, `TaskUpdate` and `TaskStop`. An entry goes when its result arrives.
  - **Checklist:** `TodoWrite` replaces the list with `input.todos` (`content` → `text`) and emits a `todo` event after its tool_use event. A `TaskCreate` result that is not an error appends `(toolUseResult.task.id, input.subject, "pending")`; a `TaskUpdate` result sets that task's `status` and `subject`, and status `deleted` removes it. Each of these emits a `todo` event carrying the result record's uuid.
  - **Usage:** assistant records with a `message.usage`, except `message.model == "<synthetic>"`, count once per `message.id` for `tokens` (`cache_creation` → `cache_write`). `context.used` is the latest such record's `input + cache_read + cache_creation`, and `context.window` is `context_window(model)`. `cwd` is the latest main-chain record's `cwd`.
  - **Running tasks** (E7):
    - A `Bash` result whose `toolUseResult.backgroundTaskId` is set starts a `shell`, titled `input.description`, else `input.command`, cut to 80 characters. An `Agent` result with `isAsync: true` and `agentId` starts an `agent` titled `input.description`. `started` is the result record's `timestamp`.
    - Ends come from `<task-notification>` text whose `<status>` is `completed`, `failed`, `killed` or `stopped`, naming its `<task-id>`. Read it from a `user` string record (before rule 7 drops it), from an `attachment` of type `queued_command` (`prompt`), or from a `queue-operation` `enqueue` (`content`).
    - A `TaskStop` result ends `toolUseResult.task_id`.
    - A `task_status` attachment with `status: running` adds a task that is not already listed: a `shell` for `local_bash`, else an `agent`, titled `description`, else `shell.command`.
  - **Context windows:** `context_window` matches by prefix against `const WINDOWS: &[(&str, u64)]`, which holds Step 1's samples, with `// ponytail: one entry per model family, each read from a status-line sample; unknown models show tokens only.`
  - **Engine (`run`):** beside Part 2's `Asks` and `Settings`, after each tick once ready, compare `tap.usage()` and `tap.tasks()` with the last values sent and send `Usage` or `Tasks` when they differ. A `null` `cwd` in the usage is replaced by the session file's `cwd`.
- [ ] **Step 6: Run**; expect all to pass. Run clippy.
- [ ] **Step 7: Commit** `feat(chat): Claude thinking, checklist, usage and background tasks`.

---

## Phase B: Rewind, side questions and the Changes route

### Task 4: Wire frames, Engine methods and the hub's rewind

**Files:**
- Modify: `crates/agent/src/chat/wire.rs`, `crates/agent/src/chat/hub.rs`

**Interfaces:**
- Consumes: `Actor::cut_to` (Task 2).
- Produces:
  - `ClientFrame::Rewind { uuid: String, #[serde(default)] files: bool }`, `ClientFrame::RewindUndo` and `ClientFrame::AsideCancel { id: String }`, the last two with `#[serde(rename = "rewind_undo")]` and `#[serde(rename = "aside_cancel")]` (`rename_all = "lowercase"` would make them `rewindundo` and `asidecancel`); `ClientFrame::Aside { id: String, text: String }`.
  - `pub struct AsideChunk { pub text: String, pub done: bool, pub error: Option<String> }` in `hub.rs`.
  - `Engine` methods, in the trait's `impl Future + Send` style:
    - `rewind(&self, uuid: &str, files: bool) -> Result<String, String>`, default `Err("not supported")`;
    - `rewind_undo(&self) -> Result<(), String>`, default `Err`;
    - `aside(&self, id: &str, text: &str) -> Result<tokio::sync::mpsc::Receiver<AsideChunk>, String>`, default `Err`;
    - `aside_cancel(&self, id: &str)`, default no-op.
  - The hub answers `Rewind` and `RewindUndo` with `rewound`. `Hub::command` returns `None` for `Aside` and `AsideCancel`, which `ws.rs` routes itself (Task 6).

- [ ] **Step 1: Write the failing tests.**
  - `wire.rs`: `part_five_frames_parse` asserts that `{"t":"rewind","uuid":"u"}` parses with `files: false`, that `rewind` with `files:true`, `rewind_undo`, `aside` and `aside_cancel` parse, and that `{"t":"rewindundo"}` does not.
  - In `hub.rs`, `FakeEngine` gains `fail_rewind: bool`, set through `rig_with` by `rig_failing_rewind()`. `rewind` records `rewind:<uuid>:<files>` and returns `Ok(format!("text of {uuid}"))`, or `Err("rewind failed; use Terminal")` when `fail_rewind` is set; `rewind_undo` records `undo` and returns `Ok(())`. The tests use Part 2's `reply(f)`, which parses a reply frame.

```rust
fn rewind(uuid: &str) -> ClientFrame { ClientFrame::Rewind { uuid: uuid.into(), files: true } }
#[tokio::test]
async fn rewind_runs_only_while_idle_cuts_at_the_prompt_and_reloads_clients_once() {
    let (hub, tx, calls) = rig();
    log6(&tx).await;
    tx.send(EngineMsg::Status { working: true, waiting: false, waiting_for: None }).await.unwrap();
    settle().await;
    let busy = reply(hub.command(rewind("u2")).await);
    assert_eq!((busy["ok"].as_bool(), busy["error"].as_str()), (Some(false), Some("Stop the turn or wait for it to finish")));
    tx.send(EngineMsg::Status { working: false, waiting: false, waiting_for: None }).await.unwrap();
    settle().await;
    let mut sub = hub.subscribe(None, None).await.unwrap();
    assert_eq!(reply(hub.command(rewind("u2")).await), json!({"t":"rewound","uuid":"u2","ok":true,"text":"text of u2","attachments":0}));
    assert_eq!(*calls.lock().unwrap(), ["rewind:u2:true"], "nothing reached the engine while busy");
    let f = frames(&mut sub);
    assert_eq!((f[0]["t"].as_str(), f[0]["head"].as_u64()), (Some("hello"), Some(3)), "cut before the prompt");
    tx.send(EngineMsg::Ev(cut(Some("s1")))).await.unwrap();             // the next prompt makes the tap cut at the same place
    tx.send(EngineMsg::Ev(prompt("u3", "two, edited"))).await.unwrap();
    settle().await;
    let g = frames(&mut sub);
    assert!(!g.iter().any(|x| x["t"] == "hello"), "a chat rewind followed by its prompt reloads clients once");
    assert_eq!(g.iter().find(|x| x["t"] == "ev").unwrap()["seq"], 4);
}
#[tokio::test]
async fn a_failed_unknown_or_steered_rewind_leaves_the_conversation_alone() {
    let (hub, tx, calls) = rig_failing_rewind();
    log6(&tx).await;
    let failed = reply(hub.command(rewind("u2")).await);
    assert_eq!((failed["ok"].as_bool(), failed["error"].as_str()), (Some(false), Some("rewind failed; use Terminal")));
    assert_eq!(reply(hub.command(rewind("zz")).await)["error"], "not in this conversation");
    let mut steered = prompt("s9", "steered");
    steered["src"] = json!("steer");
    tx.send(EngineMsg::Ev(steered)).await.unwrap();
    settle().await;
    assert_eq!(reply(hub.command(rewind("s9")).await)["error"], "this prompt cannot be rewound");
    assert_eq!(*calls.lock().unwrap(), ["rewind:u2:true"]);
    assert_eq!(serde_json::from_str::<Value>(&hub.subscribe(None, None).await.unwrap().backlog[0]).unwrap()["head"], 7);
    let (hub, tx, _) = rig();
    let mut p = prompt("u1", "a");
    p["files"] = json!([{"kind":"file","name":"a.pdf"},{"kind":"image","ref":"ab","mime":"image/png"}]);
    tx.send(EngineMsg::Ev(p)).await.unwrap();
    settle().await;
    assert_eq!(reply(hub.command(rewind("u1")).await)["attachments"], 2);
}
#[tokio::test]
async fn undo_reaches_the_engine_only_while_idle() {
    let (hub, tx, calls) = rig();
    tx.send(EngineMsg::Status { working: true, waiting: false, waiting_for: None }).await.unwrap();
    settle().await;
    assert_eq!(reply(hub.command(ClientFrame::RewindUndo).await)["ok"], false);
    tx.send(EngineMsg::Status { working: false, waiting: false, waiting_for: None }).await.unwrap();
    settle().await;
    assert_eq!(reply(hub.command(ClientFrame::RewindUndo).await), json!({"t":"rewound","uuid":null,"ok":true}));
    assert_eq!(*calls.lock().unwrap(), ["undo"]);
}
```

- [ ] **Step 2: Run** `cargo test -p cdash-agent chat::wire chat::hub`; expect a compile failure.
- [ ] **Step 3: Implement** the variants, `AsideChunk` and the four default methods. Then the hub:
  - **Rewind refusals,** in this order: `working` gives "Stop the turn or wait for it to finish" (Part 2's wording); then `refusal()`'s message; a uuid that matches no prompt event gives "not in this conversation"; a prompt with `src: "steer"` gives "this prompt cannot be rewound". No refusal reaches the engine.
  - **Success:** `attachments` is the length of the target's `files`. Call the engine; on `Ok(text)`, `cut_to(len)`, where `len` is the target's index minus the uuid-less events just before it (a `cdash/queued` row belongs to the prompt it announced). Log `chat rewind <name>`.
  - The rewind runs inside the actor, so no paste, release or other key can interleave with it.
  - **`RewindUndo`** follows the same idle rule and replies with `"uuid": null`.
- [ ] **Step 4: Run**; expect all to pass. Run clippy.
- [ ] **Step 5: Commit** `feat(chat): rewind frames, engine methods and the hub's rewind`.

### Task 5: Driving Claude's rewind dialog

**Files:**
- Create: `crates/agent/src/chat/rewind.rs`
- Modify: `crates/agent/src/chat/mod.rs` (`pub mod rewind;`), `crates/agent/src/chat/tap.rs`, `crates/agent/src/chat/claude.rs`, `CLAUDE_FINDINGS.md`

**Interfaces:**
- Consumes: Task 1's `nodes` and `last_main`; `Engine::rewind` (Task 4); Part 2's `tmux_io::capture(runner, name)`, `tmux_io::key(runner, name, key)`, `claude::Live` and the engine's `ctx` and `pid`; Part 1's `tmux_io::clear_input`.
- Produces:
  - **`tap.rs`:** `pub struct Listed { pub uuid: String, pub text: String, pub raw: String, pub pasted: bool, pub images: bool }` and `Tap::listed(&self) -> Vec<Listed>`. These are the current branch's prompts that the selector lists, oldest first. Walk from `last_main` through `nodes` parents and stop at an unknown or `null` parent, so pre-compaction prompts are not listed.
  - **`rewind.rs`:**
    - `pub trait Tui: Sync` with `screen(&self) -> String`, `press(&self, key: &str) -> Result<(), String>`, `clear(&self, text: &str)` and `status(&self) -> (Option<String>, Option<String>)` (the status file's `status` and `waitingFor`), each in the `impl Future + Send` style. It is called `Tui` because Part 2 already has a `tmux_io::Pane` struct.
    - `selector_entry(screen: &str) -> Option<String>`;
    - `confirm(screen: &str) -> Option<Confirm>` with `pub struct Confirm { pub quoted: String, pub options: Vec<(String, String)> }` (digit, label);
    - `entry_matches(entry: &str, p: &Listed) -> bool`;
    - `drive(tui: &impl Tui, listed: &[Listed], target: usize, files: bool) -> Result<(), String>`.
  - **`claude.rs`:** `Live` gains `branch: Vec<Listed>`, `sid: Option<String>` and `cwd: Option<String>` (the session file's). `ClaudeEngine` implements `rewind`, returning `Ok(listed[target].text)`.

- [ ] **Step 1: Record the dialog on a real session.** The spec leaves three facts to the builder.

```bash
mkdir -p /tmp/cdash-p5/rw && cd /tmp/cdash-p5/rw && git init -q && printf 'a\n' > f.txt && git add f.txt && git -c user.name=t -c user.email=t@t commit -qm init
tmux -L cdash-test -f /dev/null new-session -d -s rw -x 160 -y 50 -c /tmp/cdash-p5/rw claude --dangerously-skip-permissions --model haiku
sleep 4; tmux -L cdash-test send-keys -t =rw: Down Enter            # folder trust
# Send each line (send-keys '<text>' Enter), waiting for its reply (capture-pane) before the next:
#   Reply with only: one        |  !echo two        |  /rename spike-rewind
#   Append the line b to f.txt, then reply with only: done
#   Run sleep 3 with run_in_background, then reply with only: started   (wait for the notification's turn)
#   Run sleep 8 in the foreground, then reply ok   -- and while it runs send: Reply with only: queued
tmux -L cdash-test send-keys -t =rw: Escape; sleep 0.25; tmux -L cdash-test send-keys -t =rw: Escape; sleep 0.5
tmux -L cdash-test capture-pane -p -t =rw:      # (a) which of the six kinds are listed, and how
# Up to the "Append the line b" prompt, Enter, capture-pane   # (b) the options and their numbers with code changes
# press the digit of "Restore conversation" alone, capture-pane 300 ms later   # (c) does the digit alone confirm?
tmux -L cdash-test kill-server
```

  Add (a), (b) and (c), with exact screen text, to a new `CLAUDE_FINDINGS.md` section, "Dashboard chat rewind (Claude Code 2.1.323)". They set three things in Step 4: which records `listed()` includes; the label `drive` looks for when files are wanted (the plan assumes `Restore code and conversation`); and whether `drive` sends Enter after the digit.
- [ ] **Step 2: Write the failing tests.** `SELECTOR` and `CONFIRM` are E4's two screens, verbatim, with `/tmp/…/spike` written as `/tmp/spike`. `FakeTui` is a TUI that knows only this dialog:
  - It holds `entries` (the selector's prompt lines, oldest first), `options` (default: E4's four), an optional `quote` override, a `status` (default `idle`), and every press it receives; `clear` is recorded as `"clear"`.
  - Its view is closed, the selector at a cursor (`entries.len()` is `(current)`), or the confirm screen for one entry.
  - A second `Escape` while closed opens the selector, and the status becomes `waiting` with `dialog open`. `Escape` on the confirm screen goes back to the selector; on the selector it closes the dialog.
  - `Up` moves the cursor. `Enter` on an entry opens the confirm screen. An option's digit closes everything and sets `idle`.
  - `screen()` renders the current view the way E4 shows it, with `❯` on the cursor line.

```rust
fn listed(lines: &[&str]) -> Vec<Listed> { /* uuid "p<i>", text = raw = the line, pasted/images = false */ }
#[test]
fn the_selector_and_confirm_screens_parse() {
    assert_eq!(selector_entry(SELECTOR).as_deref(), Some("(current)"));
    let moved = SELECTOR.replace("   ❯ (current)", "     (current)").replace("     /tmp/spike/red.png", "   ❯ /tmp/spike/red.png");
    assert_eq!(selector_entry(&moved).as_deref(), Some("/tmp/spike/red.png Reply with only: ok"));
    let c = confirm(CONFIRM).unwrap();
    assert_eq!(c.quoted, "/tmp/spike/red.png Reply with only: ok");
    assert_eq!(c.options, [("1", "Restore conversation"), ("2", "Summarize from here"), ("3", "Summarize up to here"), ("4", "Never mind")].map(|(d, l)| (d.to_string(), l.to_string())));
    assert!(selector_entry("❯ hello").is_none() && confirm(SELECTOR).is_none(), "an input box or the other screen is not mistaken");
}
#[test]
fn entries_match_by_first_line_paste_chip_or_image_token() {
    let p = |raw: &str| Listed { uuid: "u".into(), text: raw.into(), raw: raw.into(), pasted: raw.contains("<pasted_content"), images: raw.contains("[Image #") };
    assert!(entry_matches("yes", &p("yes")) && !entry_matches("yes", &p("yes please")));
    assert!(entry_matches("fix the parser so it…", &p("fix the parser so it stops at the end of the line\nand more")));
    assert!(entry_matches("[Pasted text #2 +40 lines]", &p("<pasted_content id=\"2\">\nlog\n</pasted_content id=\"2\">")) && !entry_matches("[Pasted text #2 +40 lines]", &p("log")));
    assert!(entry_matches("[Image #3] what is this", &p("[Image #3] what is this")) && !entry_matches("[Image #3]", &p("no image")));
}
#[tokio::test]
async fn rewinding_two_back_checks_every_step_restores_the_conversation_and_clears_the_box() {
    let lines = ["[Image #1] What color is this image? Answer in one word.", "/tmp/spike/red.png Reply with only: ok", "[Image #2] Reply with only: ok3"];
    let tui = FakeTui::new(&lines);
    drive(&tui, &listed(&lines), 1, false).await.unwrap();
    assert_eq!(tui.presses(), ["Escape", "Escape", "Up", "Up", "Enter", "1", "clear"]);
}
#[tokio::test]
async fn repeated_prompts_are_told_apart_by_position() {
    let tui = FakeTui::new(&["yes", "no", "yes"]);
    drive(&tui, &listed(&["yes", "no", "yes"]), 0, false).await.unwrap();
    assert_eq!(tui.presses()[..6], ["Escape", "Escape", "Up", "Up", "Up", "Enter"]);
}
#[tokio::test]
async fn a_dialog_that_disagrees_with_the_transcript_is_escaped_without_a_restore_key() {
    let tui = FakeTui::new(&["one", "two"]);                  // rewound in Terminal: the TUI lists fewer prompts
    assert_eq!(drive(&tui, &listed(&["one", "two", "three"]), 0, false).await.unwrap_err(), "rewind failed; use Terminal");
    assert!(!tui.presses().iter().any(|k| k == "clear" || k.chars().all(|c| c.is_ascii_digit())) && tui.closed());
    let quoting = FakeTui::new(&["one", "two"]).quoting("something else");
    assert!(drive(&quoting, &listed(&["one", "two"]), 1, false).await.is_err());
    assert_eq!(quoting.presses().iter().rev().take(2).collect::<Vec<_>>(), ["Escape", "Escape"], "confirm, then selector");
}
#[tokio::test]
async fn files_pick_restore_code_and_conversation_only_when_offered() {
    let both = || FakeTui::new(&["edit it"]).options(&[("1", "Restore code and conversation"), ("2", "Restore conversation"), ("3", "Never mind")]);
    let (a, b) = (both(), both());
    drive(&a, &listed(&["edit it"]), 0, true).await.unwrap();
    drive(&b, &listed(&["edit it"]), 0, false).await.unwrap();
    assert_eq!((a.presses()[4].as_str(), b.presses()[4].as_str()), ("1", "2"));
    let plain = FakeTui::new(&["no edits"]);                   // E4's options: conversation only
    drive(&plain, &listed(&["no edits"]), 0, true).await.unwrap();
    assert_eq!(plain.presses()[4], "1");
}
#[tokio::test]
async fn a_busy_or_waiting_session_gets_no_key() {
    let busy = FakeTui::new(&["one"]).status("busy");
    assert_eq!(drive(&busy, &listed(&["one"]), 0, false).await.unwrap_err(), "Stop the turn or wait for it to finish");
    let asking = FakeTui::new(&["one"]).status("waiting");
    assert_eq!(drive(&asking, &listed(&["one"]), 0, false).await.unwrap_err(), "Answer the open question first");
    assert!(busy.presses().is_empty() && asking.presses().is_empty());
}
// tap.rs
#[test]
fn the_branch_lists_its_prompts_oldest_first_and_skips_abandoned_ones() {
    let mut tap = Tap::default();
    for l in include_str!("testdata/claude.jsonl").lines() { tap.feed(l); }
    assert_eq!(tap.listed().iter().map(|p| p.uuid.as_str()).collect::<Vec<_>>(), ["u1", "u2", "u5", "g1"]);
}
```

  In the last test, add `b1` and/or `n1` in chain order if Step 1 found that `!` commands or task notifications are listed. `u4`, the handed-back prompt, is never on the branch; `g1` is Part 3's image prompt.
- [ ] **Step 3: Run** `cargo test -p cdash-agent chat::rewind chat::tap`; expect a compile failure.
- [ ] **Step 4: Implement.**
  - **`listed()` entries:** the prompts the selector lists according to Step 1. Without a record, that is rule-7 prompts (typed, queued, slash commands) and Part 3's image prompts, recorded at emission: `text` is the display text, `raw` the recorded text, `pasted` whether `raw` contains `<pasted_content`, and `images` whether the prompt had image blocks.
  - **`selector_entry`:** the screen must hold a line whose trimmed text is `Rewind`, a line containing `Restore the code and/or conversation to the point before`, and a later line containing `Enter to continue`. The entry is the line between them whose trimmed text starts with `❯`, with the `❯` and surrounding spaces removed.
  - **`confirm`:** the screen must hold a line containing `Confirm you want to restore to the point before you sent this message:`. `quoted` is the first later line whose trimmed text starts with `│`, without it. `options` are the later lines that read `[❯] <digit>. <label>`.
  - **`entry_matches`:** with a trailing `…` (the TUI cut the line), the rest must be a prefix of the first line of `raw`, or of `text`. Otherwise it must equal one of them. Failing both, it matches when it starts with `[Pasted text #` and `pasted`, or with `[Image #` and `images`.
  - **`drive`,** with `POLL = 50 ms`, `ESC_GAP = 250 ms`, `OPEN_WAIT = 2 s`, `STEP_WAIT = 1 s` and `IDLE_WAIT = 2 s`:
    1. Use Part 2's gate wording. `idle` and `shell` (Part 4: idle with a background shell) pass; `waiting` refuses with "Answer the open question first", and anything else with "Stop the turn or wait for it to finish".
    2. Press `Escape`, wait `ESC_GAP`, press `Escape`. Wait up to `OPEN_WAIT` for `waiting` / `dialog open` together with `selector_entry == Some("(current)")`.
    3. For `j` in `1..=k`, where `k = listed.len() - target`: press `Up`. Wait up to `STEP_WAIT` for a highlighted entry different from the last, and require `entry_matches(entry, &listed[listed.len() - j])`.
    4. Press `Enter`. Wait up to `STEP_WAIT` for `confirm(screen)`, and require `entry_matches(quoted, &listed[target])`.
    5. Find the label: `Restore code and conversation` (Step 1's text) when `files` is set and offered, else `Restore conversation`. Press its digit. If Step 1 found that the digit alone does not confirm, press `Enter` once the screen shows `❯` on that option.
    - Any failure or timeout in steps 2 to 5, up to the digit, presses `Escape` until neither screen shows (at most three times) and returns "rewind failed; use Terminal".
    6. Once the digit is pressed the rewind counts as done. Wait up to `IDLE_WAIT` for `idle`, then `tui.clear(&listed[target].raw)`, whatever the box shows.
  - **`ClaudeEngine`:**
    - `run` sets `live.sid` and `live.cwd` from each session-file read, and `live.branch = tap.listed()` after every tick that read lines.
    - `rewind` finds the target by uuid in `live.branch`, else "this prompt cannot be rewound from chat; use Terminal". It then calls `drive` with `struct TmuxTui<'a> { runner: &'a Runner, name: &'a str, claude_dir: &'a Path, pid: i32 }`, which implements `Tui` with `tmux_io::capture`, `tmux_io::key`, `tmux_io::clear_input` and `session_file_for`.
- [ ] **Step 5: Run**; expect all to pass. Run clippy.
- [ ] **Step 6: Commit** `feat(chat): drive Claude's rewind dialog from chat`.

### Task 6: Side questions: the Claude process, the hub and the socket

**Files:**
- Modify: `crates/agent/src/chat/claude.rs`, `crates/agent/src/chat/hub.rs`, `crates/agent/src/chat/ws.rs`

**Interfaces:**
- Consumes: `AsideChunk`, `Engine::aside` and `aside_cancel` (Task 4); `Live.sid` and `Live.cwd` (Task 5); Part 2's `Live.model`, `Runner::spawn_piped(program, args, cwd) -> io::Result<Child>` and `Ctx.own_pids`; Part 4's `install_stub`.
- Produces:
  - In `claude.rs`: `aside_args(sid: &str, model: Option<&str>, question: &str) -> Vec<String>`, `aside_chunk(line: &str) -> Option<AsideChunk>`, `ASIDE_MAX: Duration = 300 s`; `ClaudeEngine` implements `aside`.
  - `Hub::aside(&self, id: String, text: String) -> Result<mpsc::Receiver<AsideChunk>, String>` and `Hub::aside_cancel(&self, id: String)`.
  - `ws.rs` sends `aside` frames to the socket that asked.

- [ ] **Step 1: Check the flags** the spec relies on (no model call):

```bash
claude --help > /tmp/cdash-p5/help.txt
for f in --resume --fork-session --no-session-persistence --tools --strict-mcp-config --mcp-config --settings --model --output-format --verbose; do grep -q -- "$f" /tmp/cdash-p5/help.txt || echo "MISSING $f"; done
```

  Expected: no output. A flag E6 already used (`--fork-session`, `--no-session-persistence`, `--tools`) may be absent from `--help`; any other missing flag stops the task and is reported.
- [ ] **Step 2: Write the failing tests.**
  - `stub_claude(dir, script)` is `install_stub(dir, "claude", …)`: it appends `$PWD|$*` to `<dir>/claude-args`, then runs `script`.
  - `print_lines(text)` is a script that prints `text` with `printf '%s\n'`. `eventually` is Part 1's helper, which Part 4 made `pub(crate)`.
  - `ANSWER` is three lines a script prints: `{"type":"system","subtype":"init"}`, `{"type":"assistant","message":{"content":[{"type":"text","text":"You asked about the tests."}]}}` and `{"type":"result","subtype":"success","is_error":false,"result":"You asked about the tests."}`.

```rust
#[test]
fn side_question_arguments_follow_the_spec_and_put_the_question_last() {
    let a = aside_args("sid-1", Some("claude-opus-5-5"), "--what did I ask?");
    assert_eq!(a, ["-p", "--resume", "sid-1", "--fork-session", "--no-session-persistence", "--tools", "", "--strict-mcp-config",
        "--mcp-config", r#"{"mcpServers":{}}"#, "--settings", r#"{"disableAllHooks":true}"#, "--model", "claude-opus-5-5",
        "--output-format", "stream-json", "--verbose", "--", "--what did I ask?"]);
    assert!(!aside_args("sid-1", None, "q").iter().any(|x| x == "--model"));
}
#[test]
fn stream_json_lines_become_chunks() {
    let c = aside_chunk(r#"{"type":"assistant","message":{"content":[{"type":"text","text":"Hi"}]}}"#).unwrap();
    assert_eq!((c.text.as_str(), c.done, c.error), ("Hi", false, None));
    assert!(aside_chunk(r#"{"type":"result","subtype":"success","is_error":false,"result":"Hi"}"#).is_some_and(|c| c.done && c.text.is_empty() && c.error.is_none()));
    assert!(aside_chunk(r#"{"type":"result","subtype":"error_during_execution","is_error":true,"result":"overloaded"}"#).is_some_and(|c| c.done && c.error.as_deref() == Some("overloaded")));
    assert!(aside_chunk(r#"{"type":"system","subtype":"init"}"#).is_none() && aside_chunk("garbage").is_none());
}
#[tokio::test]
async fn a_side_question_runs_claude_in_the_session_folder_and_streams_its_answer() {
    let (ctx, dir) = fixture_ctx("aside");
    write_session(&dir, &format!(r#"{{"sessionId":"sid-1","cwd":"{}","status":"busy"}}"#, dir.display()));
    stub_claude(&dir, &print_lines(ANSWER));
    let (engine, mut rx) = start(Arc::clone(&ctx), "cdash-a".into(), 4242);
    collect_until_ready(&mut rx).await;
    let mut answer = engine.aside("q1", "what did I ask first?").await.unwrap();
    let first = answer.recv().await.unwrap();
    assert_eq!((first.text.as_str(), first.done), ("You asked about the tests.", false));
    assert!(answer.recv().await.unwrap().done);
    let args = std::fs::read_to_string(dir.join("claude-args")).unwrap();
    assert!(args.starts_with(&format!("{}|-p --resume sid-1 ", dir.display())) && args.trim_end().ends_with("-- what did I ask first?"));
    eventually("the pid is forgotten once it exits", || ctx.own_pids.lock().unwrap().is_empty()).await;
}
#[tokio::test]
async fn closing_the_answer_kills_the_process_and_forgets_its_pid() {
    // stub_claude whose script loops appending to <dir>/tick every 50 ms; while it runs, own_pids holds one pid
    // drop(answer) -> 300 ms later the tick file stops growing, and own_pids is empty
}
// hub.rs: FakeEngine::aside hands out the receiver stored in its `aside_rx`
#[tokio::test]
async fn a_side_question_never_touches_the_log_or_the_queue() {
    // log6 and one held message; Hub::aside("q1","why?") passes the fake's chunks through; head and held are unchanged
    // after EngineMsg::Ended, Hub::aside returns Err("session ended")
}
```

  In `ws.rs`: `a_side_answer_reaches_the_asking_socket_and_never_the_log` uses `chat_server` with `stub_claude` printing `ANSWER`. It rewrites the session file so `cwd` is the test dir, then sends `{"t":"aside","id":"q1","text":"hi"}`. Expected: frames `aside` with the text and `done: false`, then `done: true`; no `ev` frame; a second socket receives no `aside` frame.
- [ ] **Step 3: Run** `cargo test -p cdash-agent chat::claude chat::hub chat::ws`; expect a compile failure.
- [ ] **Step 4: Implement.**
  - **`ClaudeEngine::aside`:**
    - Requires `live.sid` and `live.cwd` (the session file's `cwd`, where the transcript lives), else "no session yet". Runs `spawn_piped("claude", &aside_args(sid, live.model, text), cwd)`; the question is one argument and never passes through a shell.
    - Takes and drops `child.stdin` at once, so Claude reads end of input as from `/dev/null` (E6: an open, silent stdin costs a 3 s wait). Inserts the child's pid into `ctx.own_pids`.
    - A task reads stdout with `BufReader::lines` inside `tokio::time::timeout(ASIDE_MAX, …)`, `select!`ing on the next line and on the outgoing sender's `closed()`. Lines go through `aside_chunk`. A stream that ends without a result sends `done` with error "no answer"; the timeout sends `done` with "no answer in 5 minutes".
    - The task then drops the child (`kill_on_drop` ends it if it still runs) and removes the pid. Log `chat aside <name>`.
  - **`aside_chunk`:** an `assistant` line's text blocks, joined, give a chunk. A `result` line gives `done`, with `error` = `result` text when `is_error` (else "side question failed"). Every other line gives `None`.
  - **Hub:** `Cmd::Aside` sanitises the text with `wire::sanitize` and refuses an ended session with "session ended"; it then returns `engine.aside(…)`. `Cmd::AsideCancel` calls `engine.aside_cancel`.
  - **`ws.rs`:** keep one `Option<(String, Receiver<AsideChunk>)>` per socket and add it to the `select!` (a helper awaits `pending()` when it is `None`).
    - `Aside` cancels any running one through `hub.aside_cancel`, then stores the new receiver; on `Err(e)` it sends `{"t":"aside","id":…,"text":"","done":true,"error":e}`. `AsideCancel` with the current id cancels it, and so does leaving `pump`.
    - A chunk becomes `{"t":"aside","id","text","done","error"?}`; `done` clears the slot.
  - **`/btw`:** the composer routes it here before any frame leaves the browser (Task 17), which replaces Part 2's refusal as chat's answer to `/btw`. Part 2's `/btw` arm in `picker::route` stays as the guard that keeps `/btw` out of Claude's TUI for any client that still sends it.
- [ ] **Step 5: Run**; expect all to pass. Run clippy.
- [ ] **Step 6: Commit** `feat(chat): side questions on Claude, streamed to the asking socket`.

### Task 7: The Changes route

**Files:**
- Create: `crates/agent/src/chat/diff.rs`
- Modify: `crates/agent/src/chat/mod.rs` (`pub mod diff;`), `crates/agent/src/host/cmd.rs`, `crates/agent/src/http/routes.rs`, `crates/agent/src/http/serve.rs`

**Interfaces:**
- Consumes: `Hub::folder` (Task 2), `hub_for`.
- Produces:
  - `Runner::run_capped(&self, program: &str, args: &[&str], key: &str, timeout: Duration, cap: usize) -> Result<Capped, String>`, with `pub struct Capped { pub out: String, pub truncated: bool, pub code: Option<i32> }`. It reads at most `cap` bytes of stdout, kills the child when more arrive, and errs only on spawn failure or timeout.
  - `diff::DIFF_MAX: usize = 2 * 1024 * 1024` and `diff::diff(runner: &Runner, folder: &str) -> Value` (protocol reference).
  - Route `get "/api/chat/diff" => routes::get_chat_diff`.

- [ ] **Step 1: Write the failing tests.** The diff tests use real git through `Runner`, as `collect/git.rs` tests do.

```rust
// cmd.rs
#[tokio::test]
async fn run_capped_stops_at_the_cap_and_reports_the_exit_code() {
    let (r, _) = runner();
    let c = r.run_capped("sh", &["-c", "yes x | head -c 100000"], "cap", Duration::from_secs(5), 10).await.unwrap();
    assert_eq!((c.out.as_str(), c.truncated), ("x\nx\nx\nx\nx\n", true));
    let c = r.run_capped("sh", &["-c", "printf ab; exit 1"], "code", Duration::from_secs(5), 10).await.unwrap();
    assert_eq!((c.out.as_str(), c.truncated, c.code), ("ab", false, Some(1)));
    assert!(r.run_capped("sleep", &["5"], "slow", Duration::from_millis(100), 10).await.is_err());
}
// diff.rs; repo(tag) = a temp dir with `git init`, user.name/email set, committing a.txt "one\ntwo\n", b.bin (bytes with NUL) and gone.txt
#[tokio::test]
async fn changes_list_every_file_with_status_counts_and_patch() {
    let d = repo("changes");
    write(&d, "a.txt", "one\n2\n"); write(&d, "b.bin", "\0changed"); git(&d, &["rm", "-q", "gone.txt"]);
    write(&d, "new file.txt", "n\n"); git(&d, &["add", "new file.txt"]); write(&d, "naïve \"q\".md", "x\ny\n");
    let v = diff(&runner(), d.to_str().unwrap()).await;
    let row = |f: &Value| (f["path"].as_str().unwrap().to_string(), f["status"].as_str().unwrap().to_string(), f["additions"].clone(), f["deletions"].clone());
    assert_eq!(v["files"].as_array().unwrap().iter().map(row).collect::<Vec<_>>(), [
        ("a.txt".into(), "modified".into(), json!(1), json!(1)), ("b.bin".into(), "modified".into(), Value::Null, Value::Null),
        ("gone.txt".into(), "deleted".into(), json!(0), json!(1)), ("new file.txt".into(), "added".into(), json!(1), json!(0)),
        ("naïve \"q\".md".into(), "untracked".into(), json!(2), json!(0)),
    ]);
    assert!(v["files"][0]["patch"].as_str().unwrap().contains("-two\n+2"));
    assert!(v["files"][4]["patch"].as_str().unwrap().contains("+x"));
    assert_eq!((v["repo"].as_bool(), v["truncated"].as_bool(), v["folder"].as_str()), (Some(true), Some(false), d.to_str()));
}
#[tokio::test]
async fn outside_git_and_before_the_first_commit() {
    // a plain temp dir -> {"repo": false, "files": [], "truncated": false}
    // `git init` with x.txt staged and y.txt untracked, no commit -> [("x.txt","added",1,0), ("y.txt","untracked",1,0)]
}
#[tokio::test]
async fn over_two_megabytes_keeps_the_list_and_counts_but_drops_the_lines() {
    // a.txt rewritten with 3 MB of lines, plus an untracked small.txt
    // -> truncated true; a.txt keeps its counts with patch null; small.txt has patch, additions and deletions null
}
#[tokio::test]
async fn every_git_call_has_fsmonitor_off_and_no_external_tools() {
    // stub `git` (install_stub) that records "$*" and prints "true" for rev-parse --is-inside-work-tree
    // -> every recorded line contains "-c core.fsmonitor=false"; every line containing " diff " also contains
    //    "--no-color", "--no-ext-diff" and "--no-textconv"
}
// routes.rs
#[tokio::test]
async fn chat_diff_refuses_bad_names_and_sessions_without_chat() {
    let b = serve(cfg_for(tempdir("chat-diff"))).await.unwrap();
    assert_eq!(http_get(&format!("http://{}/api/chat/diff?session=other", b.addr)).await.0, 400);
    assert_eq!(http_get(&format!("http://{}/api/chat/diff?session=cdash-gone", b.addr)).await.0, 409);
}
```

- [ ] **Step 2: Run** `cargo test -p cdash-agent host::cmd chat::diff chat_diff`; expect a compile failure.
- [ ] **Step 3: Implement.** Each call below is `git -C <folder> -c core.fsmonitor=false …` with `GIT_TIMEOUT`, and `D` stands for `diff --no-color --no-ext-diff --no-textconv --no-renames --relative`:
  1. `rev-parse --is-inside-work-tree` failing gives `{folder, repo: false, files: [], truncated: false}`.
  2. `rev-parse --verify -q HEAD` sets the base to `HEAD`. If it fails (no commits yet), the base is the empty tree, `const EMPTY_TREE: &str = "4b825dc642cb6eb9a060e54bf8d69288fbee4904"`.
  3. `D --name-status -z <base>` gives the status (A added, D deleted, else modified).
  4. `D --numstat -z <base>` gives the counts; `-` means binary, so `null`.
  5. `D <base>` through `run_capped(DIFF_MAX)` gives the patch. Split it at lines starting with `diff --git `. The sections come in the same order as steps 3 and 4, so the k-th section belongs to the k-th file; no path is parsed from a header. When truncated, the last, partial section is dropped, and every file without a section has `patch: null`.
  6. `ls-files --others --exclude-standard -z` lists the untracked files.
  7. For each untracked path, while budget remains, `diff --no-color --no-ext-diff --no-textconv --no-index -- /dev/null <path>` through `run_capped(remaining)`, where exit code 1 means "differs". Count `+` lines (not `+++`). Binary gives `null`. Files after the budget have `null` counts and patch.
  - Tracked files come first, in git's order, then untracked ones. `truncated` is true when any patch was dropped. `// ponytail: one git per untracked file; batch them if a folder with thousands of untracked files makes the sheet slow.`
  - **`get_chat_diff`:** `assert_kill_name(session)` gives 400; `hub_for` failing gives 409 with its message; `hub.folder()` being `None` gives 409 "this session's folder is not known yet". Log `chat diff <name>` and answer `Json(diff(&ctx.runner, &folder).await)`. No client value but the session name reaches git.
- [ ] **Step 4: Run**; expect all to pass. Run clippy.
- [ ] **Step 5: Commit** `feat(chat): Changes route with the session folder's diff against HEAD`.

---

## Phase C: Codex and OpenCode

### Task 8: The supervisor feed and the native engine learn Part 5

**Files:**
- Modify: `crates/agent/src/collect/chat-feed.mjs`, `test/chat-feed.test.mjs`, `crates/agent/src/chat/native.rs`

**Interfaces:**
- Consumes: Part 2's `ChatFeed` (the `ops` table, `putAsk`, `setSettings`, and the lines `#attach` replays) and `NativeEngine::request(op, fields)`; `AsideChunk` and the Engine methods (Task 4).
- Produces:
  - **`ChatFeed`:**
    - `reset()`: new epoch, empty log and uuid set, not ready. Every connected client gets the lines a new connection gets, starting with the new `hello`.
    - `setUsage(u)` and `setTasks(list)`, next to Part 2's setters: each stores, broadcasts, and replays after every `hello`. `tasks` is always replayed, so a reconnecting agent drops tasks that ended meanwhile; `usage` once known.
    - `pushAside(id, text, done, error)` broadcasts an `aside` line and stores nothing.
    - `accept` also keeps `type: "cdash"` events.
    - Supervisors put `rewind`, `rewind_undo`, `aside` and `aside_cancel` in the constructor's `ops` table (Tasks 9, 11 and 13); a missing one already answers "not supported".
  - **`native.rs`:** `usage` lines become `EngineMsg::Usage`, and `tasks` lines `EngineMsg::Tasks`; `aside` lines go to the receiver registered for their id, which `done` removes. `NativeEngine` implements:
    - `rewind`: `request("rewind", {uuid, files})`, returning `data.text`; `rewind_undo`: `request("rewind_undo", {})`;
    - `aside`: register a channel of 64, then `request("aside", {id, text})`, unregistering on an error; `aside_cancel`: unregister, then `request("aside_cancel", {id})`, ignoring the result.

- [ ] **Step 1: Write the failing tests** in `test/chat-feed.test.mjs`, with the file's `client` and `until`:

```js
test('a reset gives every client a new hello and the history after it, then ready again', async t => {
  const dir = await mkdtemp(join(tmpdir(), 'cdash-feed-')); t.after(() => rm(dir, { recursive: true, force: true }));
  const feed = new ChatFeed({ send: async () => {}, stop: async () => {} }); t.after(() => feed.close());
  feed.accept(user('u1', 'one')); feed.ready(); feed.setUsage({ cwd: '/w' });
  const a = await client(await feed.listen(dir), { after: 0, epoch: null });
  await until(() => a.lines.some(l => l.t === 'ready'));
  const before = a.lines.length;
  feed.reset(); feed.accept(user('u1', 'one again')); feed.ready();
  await until(() => a.lines.slice(before).some(l => l.t === 'ready'));
  const after = a.lines.slice(before);
  assert.deepEqual([after[0].t, after[0].head], ['hello', 0]);
  assert.notEqual(after[0].epoch, a.lines[0].epoch);
  assert.deepEqual(after.filter(l => l.t === 'ev').map(l => [l.seq, l.ev.message.content[0].text]), [[1, 'one again']], 'a uuid from before the reset is accepted again');
  assert.ok(after.some(l => l.t === 'usage' && l.usage.cwd === '/w') && after.some(l => l.t === 'asks'));
});
test('usage and tasks are replayed after hello; aside lines are not; cdash events are kept', async t => {
  // setUsage({cwd:'/w'}), setTasks([{id:'b1'}]), accept({type:'cdash',subtype:'todo',items:[]}), pushAside('q1','Hi',false), then a client connects:
  // it gets usage and tasks lines, the todo ev, and no aside line; pushAside('q1','lo',true) then reaches it as
  // {t:'aside',id:'q1',text:'lo',done:true}; setTasks([]) is broadcast as a second tasks line
});
```

  Part 1's test `a client gets the log, state, ready and then live events; reconnecting resumes`, as Part 2 left it, gets `'tasks'` after `'asks'` in its first expected sequence. It waits for 6 lines instead of 5 and starts its second comparison at `slice(6)`.

  In `native.rs`, with the existing `fake_supervisor`:

```rust
#[tokio::test]
async fn usage_tasks_and_side_answers_reach_the_hub_and_the_asker() {
    let (sock, mut sup) = fake_supervisor("p5");
    let (engine, mut rx) = start(sock, log(), "cdash-a".into());
    sup.read_line().await;
    sup.write_line(r#"{"t":"hello","epoch":"e1","head":0}"#).await;
    sup.write_line(r#"{"t":"usage","usage":{"cwd":"/w"}}"#).await;
    sup.write_line(r#"{"t":"tasks","tasks":[{"id":"b1"}]}"#).await;
    assert!(matches!(rx.recv().await, Some(EngineMsg::Usage(u)) if u["cwd"] == "/w"));
    assert!(matches!(rx.recv().await, Some(EngineMsg::Tasks(t)) if t.len() == 1));
    let asking = tokio::spawn(async move { let r = engine.aside("q1", "why?").await; (r, engine) });
    let req: Value = serde_json::from_str(&sup.read_line().await).unwrap();
    assert_eq!((req["op"].as_str(), req["id"].as_str(), req["text"].as_str()), (Some("aside"), Some("q1"), Some("why?")));
    sup.write_line(&json!({"t":"res","rid":req["rid"],"ok":true}).to_string()).await;
    let (answer, _engine) = asking.await.unwrap();
    let mut answer = answer.unwrap();
    sup.write_line(r#"{"t":"aside","id":"q1","text":"Be","done":false}"#).await;
    sup.write_line(r#"{"t":"aside","id":"q1","text":"cause.","done":true}"#).await;
    assert_eq!(answer.recv().await.unwrap().text, "Be");
    let last = answer.recv().await.unwrap();
    assert!(last.done && last.text == "cause." && last.error.is_none());
}
#[tokio::test]
async fn rewind_and_undo_are_requests_whose_results_come_back() {
    // engine.rewind("u2", true) sends a req with op "rewind", uuid "u2", files true; res ok data {text:"two"} -> Ok("two")
    // engine.rewind_undo() sends op "rewind_undo"; res ok false error "nothing to undo" -> Err("nothing to undo")
}
```

- [ ] **Step 2: Run** `node --test test/chat-feed.test.mjs` and `cargo test -p cdash-agent chat::native`; expect failures.
- [ ] **Step 3: Implement.** `reset()` and a new connection share one function that builds the fresh-connection lines: `hello`, `state`, `asks`, `settings` (when set), `tasks`, `usage` (when set), and `ready` only once `ready()` has been called again. `native.rs` already treats a `hello` with a new epoch as `Reset` and resumes from 0, so it needs no change for resets.
- [ ] **Step 4: Run**; expect all to pass, including Part 1's and Part 2's feed tests. Run clippy.
- [ ] **Step 5: Commit** `feat(chat): supervisor feed resets, usage, tasks and side answers`.

### Task 9: Codex: rewinds in any door

**Files:**
- Modify: `crates/agent/src/collect/codex-remote.mjs` (`replayHistory`, `nativeLauncher`), `test/codex-native.test.mjs`

**Interfaces:**
- Consumes: `ChatFeed.reset` (Task 8); the feed's `ops` table (Part 2); Part 3's `chatPrompt`; Part 4's `mainThread`.
- Produces:
  - `replayHistory(codex, threadId, remoteSid, emit, submitted = new Set(), { reasoning = false } = {})` calls `emit(message, turn)` (the worker ignores the second argument).
  - In `nativeLauncher`: `firstOf: Map<turnId, uuid>` and `prompts: Map<uuid, {turnId, text}>`, filled wherever chat messages reach the feed, live or replayed. A user message that is not its turn's first gets `src: 'steer'`. `thread/reverted` for the main thread re-reads the thread and resets the feed. `ops.rewind` calls `thread/revert` and restarts the native TUI.

- [ ] **Step 1: Write the failing test.** `fixture` gains a fifth argument `{ history }`, passed to the fake as `CDASH_CHECK_HISTORY` (JSON); when set, the fake answers `thread/turns/list` with it. The fake answers `thread/revert` with `{thread:{id:<threadId>,turns:[]}}`.

```js
const steered = { data: [
  { id: 't1', startedAt: 1789000000, items: [{ id: 'u1', type: 'userMessage', content: [{ type: 'text', text: 'hello' }] }, { id: 'a1', type: 'agentMessage', text: 'hi', phase: 'final' }] },
  { id: 't2', startedAt: 1789000100, items: [{ id: 'u2', type: 'userMessage', content: [{ type: 'text', text: 'second' }] },
    { id: 'u3', type: 'userMessage', content: [{ type: 'text', text: 'steer me' }] }, { id: 'a2', type: 'agentMessage', text: 'ok', phase: 'final' }] },
], nextCursor: null };
test('Codex rewinds: steers are marked, chat reverts and restarts the TUI, any revert resets chat', { timeout: 20000 }, async t => {
  const f = await fixture(t, '', process.platform, true, { history: steered });
  await f.wait(() => f.native()?.chat_sock);
  const c = await client(f.native().chat_sock, { after: 0, epoch: null });
  await until(() => c.lines.some(l => l.t === 'ready'));
  const ev = id => c.lines.find(l => l.t === 'ev' && l.ev.message?.id === id).ev;
  assert.deepEqual([ev('u2').src, ev('u3').src], [undefined, 'steer']);
  c.write({ t: 'req', rid: 1, op: 'rewind', uuid: ev('u3').uuid, files: false });
  c.write({ t: 'req', rid: 2, op: 'rewind', uuid: ev('u2').uuid, files: true });
  await until(() => c.lines.some(l => l.rid === 2));
  assert.equal(c.lines.find(l => l.rid === 1).ok, false, 'a steer cannot be rewound');
  assert.equal(f.rpc('thread/revert').params.beforeTurnId, 't2');
  assert.deepEqual(c.lines.find(l => l.rid === 2).data, { text: 'second' });
  await f.wait(async () => (await f.messages()).filter(m => m.kind === 'tui').length === 2);
  const [old] = (await f.messages()).filter(m => m.kind === 'tui');
  assert.equal(f.alive(old.pid), false);
  assert.equal(f.child.exitCode, null, 'the supervisor keeps running');
  const before = c.lines.length;
  await f.inject({ method: 'thread/reverted', params: { threadId: '01234567-1234-1234-1234-0123456789ab' } });
  await until(() => c.lines.slice(before).some(l => l.t === 'ready'));
  const after = c.lines.slice(before);
  assert.equal(after[0].t, 'hello');
  assert.notEqual(after[0].epoch, c.lines[0].epoch);
  assert.equal(after.find(l => l.t === 'ev').seq, 1);
});
```

- [ ] **Step 2: Run** `node --test test/codex-native.test.mjs`; expect the new test to fail.
- [ ] **Step 3: Implement** in `nativeLauncher`.
  - **One accept function** feeds the feed, replacing the direct `feed.accept` calls after Part 3's `chatPrompt` rewrite, so live and replayed messages share the bookkeeping. For a user prompt (text content, not a tool result) in turn `T` with uuid `U`: `if (!firstOf.has(T)) firstOf.set(T, U)`; `src = 'steer'` when `firstOf.get(T) !== U`; `prompts.set(U, {turnId: T, text})`.
  - **`ops.rewind`**, in the feed's `ops` table: `firstOf.get(prompts.get(uuid)?.turnId) === uuid`, else throw "this prompt cannot be rewound". Then `await control.call('thread/revert', { threadId: state.sid, beforeTurnId })`, call `restartTui()`, and return `{ text }`. Codex never restores files.
  - **`thread/reverted`** with `p.threadId === state.sid` calls `reread()`: `feed.reset()`; clear `firstOf` and `prompts`; drop shell tasks (Task 10; a revert kills background terminals); set `replayed = []` and rerun the replay as at start, flushing `replayed` and calling `feed.ready()` when it settles. This also runs after chat's own revert, since `thread/reverted` reaches every connection.
  - **`startTui()`** spawns the TUI as today. Its exit handler calls `startTui()` again when `restarting` is set (clearing it), and otherwise keeps today's stop. `restartTui()` sets `restarting` and sends `SIGTERM`.
- [ ] **Step 4: Run** `node --test`; expect all to pass, including the worker tests in `test/codex-remote.test.mjs`.
- [ ] **Step 5: Commit** `feat(chat): Codex rewinds from chat and from Terminal`.

### Task 10: Codex: usage, running list, thinking, checklist, times

**Files:**
- Modify: `crates/agent/src/collect/codex-remote.mjs` (`chatMessages`, `nativeLauncher`), `test/codex-native.test.mjs`

**Interfaces:**
- Consumes: `ChatFeed.setUsage`, `ChatFeed.setTasks` (Task 8); `replayHistory`'s `reasoning` option and `turn` argument (Task 9).
- Produces: from the supervisor, the snapshots of the protocol reference, thinking messages, `cdash/todo` events, and `ts` on every chat message.

- [ ] **Step 1: Write the failing test.** The fake's `thread/resume` result gains `thread.cwd` (the test dir), and its `thread/read` answers `{thread:{id:<threadId>,parentThreadId:'01234567-1234-1234-1234-0123456789ab',agentNickname:'Scout'}}`.

```js
test('Codex facts: usage, running shells and agents, thinking, checklist and times', { timeout: 20000 }, async t => {
  const sid = '01234567-1234-1234-1234-0123456789ab';
  const f = await fixture(t, '', process.platform, true, { history: { data: [{ id: 't1', startedAt: 1789000000, items: [
    { id: 'u1', type: 'userMessage', content: [{ type: 'text', text: 'hello' }] }, { id: 'r1', type: 'reasoning', summary: ['Thought about it'], content: [] }] }], nextCursor: null } });
  await f.wait(() => f.native()?.chat_sock);
  const c = await client(f.native().chat_sock, { after: 0, epoch: null });
  await until(() => c.lines.some(l => l.t === 'ready'));
  const evs = () => c.lines.filter(l => l.t === 'ev').map(l => l.ev);
  assert.equal(evs()[0].ts, new Date(1789000000 * 1000).toISOString());
  assert.deepEqual(evs()[1].message.content[0], { type: 'thinking', thinking: 'Thought about it' });
  await f.inject({ method: 'thread/tokenUsage/updated', params: { threadId: sid, turnId: 't1', tokenUsage: {
    total: { totalTokens: 900, inputTokens: 700, cachedInputTokens: 100, cacheWriteInputTokens: 0, outputTokens: 200, reasoningOutputTokens: 0 },
    last: { totalTokens: 300, inputTokens: 250, cachedInputTokens: 0, cacheWriteInputTokens: 0, outputTokens: 50, reasoningOutputTokens: 0 }, modelContextWindow: 272000 } } });
  await until(() => c.lines.some(l => l.t === 'usage' && l.usage.context));
  assert.deepEqual(c.lines.findLast(l => l.t === 'usage').usage, { cwd: f.dir, model: 'gpt-check', context: { used: 300, window: 272000 },
    tokens: { input: 700, output: 200, cache_read: 100, cache_write: 0 }, cost: null });
  await f.inject({ method: 'item/started', params: { threadId: sid, turnId: 't2', item: { id: 'x1', type: 'commandExecution', source: 'unifiedExecStartup', command: 'npm run dev', status: 'inProgress' } } });
  await f.inject({ method: 'thread/status/changed', params: { threadId: 'child-1', status: { type: 'active', activeFlags: [] } } });
  await until(() => c.lines.findLast(l => l.t === 'tasks')?.tasks.length === 2);
  assert.deepEqual(c.lines.findLast(l => l.t === 'tasks').tasks.map(x => [x.id, x.kind, x.title]), [['x1', 'shell', 'npm run dev'], ['child-1', 'agent', 'Scout']]);
  await f.inject({ method: 'item/completed', params: { threadId: sid, turnId: 't2', item: { id: 'x1', type: 'commandExecution', source: 'unifiedExecStartup', command: 'npm run dev', status: 'completed', exitCode: 0 } } });
  await f.inject({ method: 'thread/status/changed', params: { threadId: 'child-1', status: { type: 'idle' } } });
  await until(() => c.lines.findLast(l => l.t === 'tasks').tasks.length === 0);
  await f.inject({ method: 'turn/plan/updated', params: { threadId: sid, turnId: 't2', explanation: null, plan: [{ step: 'Read', status: 'completed' }, { step: 'Fix', status: 'inProgress' }] } });
  await until(() => evs().some(e => e.subtype === 'todo'));
  assert.deepEqual(evs().find(e => e.subtype === 'todo').items, [{ text: 'Read', status: 'completed' }, { text: 'Fix', status: 'in_progress' }]);
});
```

- [ ] **Step 2: Run** `node --test test/codex-native.test.mjs`; expect the new test to fail.
- [ ] **Step 3: Implement.** Only main-thread notifications feed the conversation and the usage (Part 4's filter); child threads feed the running list.
  - **Usage:** `usage = { cwd: result.thread.cwd ?? state.dir, model: state.model, context: null, tokens: null, cost: null }`. It goes to `feed.setUsage` once after `thread/resume` or `thread/start`, and again after each update:
    - `thread/tokenUsage/updated`: `context = { used: last.totalTokens, window: modelContextWindow ?? null }` and `tokens = { input: total.inputTokens, output: total.outputTokens, cache_read: total.cachedInputTokens, cache_write: total.cacheWriteInputTokens ?? 0 }`;
    - `thread/settings/updated`: `cwd` and `model`.
  - **Running list:**
    - `item/started` for a `commandExecution` with `source: 'unifiedExecStartup'` adds `{id: item.id, kind: 'shell', title: item.command, started: now}`; `item/completed` for that id removes it.
    - `thread/status/changed` for a thread other than the main one (handled before Part 4's `if (!mainThread(m, state.sid)) return;`), never seen before, calls `thread/read {threadId, includeTurns: false}` once and caches whether `parentThreadId === state.sid`, and its title (`agentNickname || agentRole || name || preview || 'agent'`). For such a child, `status.type === 'active'` adds `{id, kind: 'agent', title, started: now}`; any other status removes it.
    - Every change goes to `feed.setTasks`.
  - **Thinking:** `chatMessages` turns a completed `reasoning` item into `{type:'assistant', message:{id: item.id, role:'assistant', content:[{type:'thinking', thinking: summary.join('\n\n')}]}}` when the text is not empty. The replay passes `{ reasoning: true }`, and `replayHistory` then shapes reasoning items the same way.
  - **Checklist:** `turn/plan/updated` becomes `{type:'cdash', subtype:'todo', items: plan.map(s => ({text: s.step, status: s.status === 'inProgress' ? 'in_progress' : s.status}))}`. It is live only, so the feed's log is what replays it.
  - **Times:** the accept function sets a missing `ts`: the replay's `turn.startedAt` × 1000 as ISO, else the time of arrival.
- [ ] **Step 4: Run** `node --test`; expect all to pass.
- [ ] **Step 5: Commit** `feat(chat): Codex usage, running list, thinking, checklist and times`.

### Task 11: OpenCode: reverts in any door, rewind and Undo

**Files:**
- Modify: `crates/agent/src/collect/opencode-remote.mjs` (`supervise`), `test/opencode-native.test.mjs`

**Interfaces:**
- Consumes: `ChatFeed.reset` (Task 8); the feed's `ops` table (Part 2); Part 3's `chatPrompt` and `rowFiles`.
- Produces:
  - `rebuild()`, run on `session.revert.staged`, `session.revert.cleared` and `session.revert.committed` for the session;
  - the `cdash/staged` event;
  - `promptRows: Map<uuid, {id, text}>`;
  - `ops.rewind` (`POST …/revert/stage {messageID, files}`) and `ops.rewind_undo` (`DELETE …/revert`), in the feed's `ops` table.

- [ ] **Step 1: Write the failing test.** The fake server keeps `revert` in memory: `POST …/revert/stage` sets it to `{messageID}` and answers `{data: revert}`; `DELETE …/revert` clears it and answers 204; the session object carries it. `CDASH_TEST_ROWS` (JSON) replaces the message list when set.

```js
const four = [{ id: 'msg_u', type: 'user', text: 'hello' }, { id: 'msg_a', type: 'assistant', time: { completed: 1 }, content: [{ type: 'text', text: 'hi there' }] },
  { id: 'msg_u2', type: 'user', text: 'second' }, { id: 'msg_a2', type: 'assistant', time: { completed: 2 }, content: [{ type: 'text', text: 'ok' }] }];
test('OpenCode rewinds: stage from chat, show it as staged with Undo, and follow reverts made anywhere', { timeout: 20000 }, async t => {
  const f = await fixture(t, { rows: four });
  await f.wait(() => f.native()?.chat_sock);
  const c = await client(f.native().chat_sock, { after: 0, epoch: null });
  await until(() => c.lines.some(l => l.t === 'ready'));
  const u2 = c.lines.find(l => l.t === 'ev' && l.ev.message?.id === 'msg_u2').ev.uuid;
  c.write({ t: 'req', rid: 1, op: 'rewind', uuid: u2, files: true });
  await until(() => c.lines.some(l => l.rid === 1));
  assert.deepEqual(f.posted('/revert/stage'), { messageID: 'msg_u2', files: true });
  assert.deepEqual(c.lines.find(l => l.rid === 1).data, { text: 'second' });
  let mark = c.lines.length;
  await f.emit({ type: 'session.revert.staged', data: { sessionID: 'ses_fixture', revert: { messageID: 'msg_u2' } } });
  await until(() => c.lines.slice(mark).some(l => l.t === 'ready'));
  const staged = c.lines.slice(mark);
  assert.equal(staged[0].t, 'hello');
  assert.deepEqual(staged.filter(l => l.t === 'ev').map(l => l.ev.subtype ?? l.ev.message.content[0].text), ['hello', 'hi there', 'staged']);
  assert.equal(staged.find(l => l.ev?.subtype === 'staged').ev.messages, 2);
  c.write({ t: 'req', rid: 2, op: 'rewind_undo' });
  await until(() => f.deleted('/revert'));
  mark = c.lines.length;
  await f.emit({ type: 'session.revert.cleared', data: { sessionID: 'ses_fixture' } });
  await until(() => c.lines.slice(mark).some(l => l.t === 'ready'));
  assert.equal(c.lines.slice(mark).filter(l => l.t === 'ev').length, 4, 'everything is back; no staged row');
});
```

  `f.deleted(suffix)` is like `posted`, for `DELETE`.
- [ ] **Step 2: Run** `node --test test/opencode-native.test.mjs`; expect the new test to fail.
- [ ] **Step 3: Implement** in `supervise`.
  - **`sink.event`** fills `promptRows` from user prompt messages: `uuid → {id: message.id, text}`.
  - **`ops.rewind`:** look up `promptRows.get(uuid)`, else throw "this prompt cannot be rewound". Then `POST /api/session/<sid>/revert/stage {messageID, files}`, where a 409 becomes "the session is busy", and return `{ text }`.
  - **`ops.rewind_undo`:** `DELETE /api/session/<sid>/revert`.
  - **`rebuild()`,** for the three revert events of `state.sid` from the existing event subscription:
    1. `feed.reset()`, then hold `sink.event` calls in a buffer;
    2. read the session (`revert?.messageID`) and `client.messages(sid)`;
    3. pass every row whose `id < messageID` (ids ascend), or every row when nothing is staged, through `historyMessages` and `sink.event`, with `rowFiles.set(row.id, row.files)` around a row that has files, as Part 3's `ChatBridge.emitHistory` does, so Part 3's `chatPrompt` applies;
    4. when staged, `feed.accept({type:'cdash', subtype:'staged', messages: <rows hidden>})`; flush the buffer, then `feed.ready()`.
    - It does not use `RemoteBridge.emitHistory`, whose `sent` set would skip rows already shown once.
- [ ] **Step 4: Run** `node --test`; expect all to pass, including `test/opencode-remote.test.mjs`.
- [ ] **Step 5: Commit** `feat(chat): OpenCode rewinds, staged reverts and Undo`.

### Task 12: OpenCode: usage, running list, thinking, times, edit patches

**Files:**
- Modify: `crates/agent/src/collect/opencode-remote.mjs` (`historyMessages`, `RemoteBridge`, `supervise`), `test/opencode-native.test.mjs`, `test/opencode-remote.test.mjs`

**Interfaces:**
- Consumes: `ChatFeed.setUsage` and `ChatFeed.setTasks` (Task 8); `rebuild()` (Task 11).
- Produces:
  - `historyMessages(row, nativeSid, remoteSid, submitted = new Set(), { chat = false } = {})`. With `chat`:
    - reasoning parts become thinking messages, with part key `reasoning:<index>` and id `<row>-reasoning-<index>`;
    - every message gets `ts` from `row.time.created`;
    - tool results gain `diff` from `state.metadata.files`.
  - `RemoteBridge` gets `this.chat = false`; `emitHistory` passes `{ chat: this.chat }`, and `ChatBridge` sets `chat = true`. `rebuild()` (Task 11) passes `{ chat: true }` too.
  - The supervisor reports usage and tasks through `feed.setUsage` and `feed.setTasks`.

- [ ] **Step 1: Write the failing tests.**
  - `opencode-remote.test.mjs`: `historyMessages without chat is unchanged`. A row with a reasoning part, `time.created` and a tool part carrying `metadata.files` gives exactly today's messages without `{chat}`. With `{chat:true}` it adds the thinking message, `ts` and `diff`.
  - The fake server in `opencode-native.test.mjs` adds:
    - `GET /api/session` (with `parentID`): `{data:[{id:'ses_child',parentID:'ses_fixture',title:'Explore the repo'}]}`; `GET /api/session/active`: `{data:{ses_child:{type:'running'}}}` when `CDASH_TEST_ACTIVE` is set;
    - `GET /api/shell`: `{data:[{id:'sh_1',status:'running',command:'sleep 60',metadata:{sessionID:'ses_fixture'},time:{started:1789000000000}}]}`; `GET /api/model`: `{data:[{providerID:'fixture',id:'model',limit:{context:200000}}]}`;
    - on the session object `tokens:{input:10,output:5,reasoning:1,cache:{read:100,write:20}}`, `cost:0.42` and `location:{directory:<cwd>}`; an assistant row with `tokens:{input:3,output:2,reasoning:0,cache:{read:40,write:5}}`.

```js
test('OpenCode facts: usage, running shells and subagents', { timeout: 20000 }, async t => {
  const f = await fixture(t, { active: true });
  await f.wait(() => f.native()?.chat_sock);
  const c = await client(f.native().chat_sock, { after: 0, epoch: null });
  await until(() => c.lines.some(l => l.t === 'usage' && l.usage.context) && c.lines.some(l => l.t === 'tasks'));
  assert.deepEqual(c.lines.findLast(l => l.t === 'usage').usage, { cwd: f.dir, model: 'fixture/model', context: { used: 50, window: 200000 },
    tokens: { input: 10, output: 6, cache_read: 100, cache_write: 20 }, cost: 0.42 });
  assert.deepEqual(c.lines.findLast(l => l.t === 'tasks').tasks.map(x => [x.id, x.kind, x.title]), [['sh_1', 'shell', 'sleep 60'], ['ses_child', 'agent', 'Explore the repo']]);
  await f.emit({ type: 'shell.exited', data: { id: 'sh_1', status: 'exited' } });
  await f.emit({ type: 'session.execution.succeeded', data: { sessionID: 'ses_child' } });
  await until(() => c.lines.findLast(l => l.t === 'tasks').tasks.length === 0);
  await f.emit({ type: 'session.usage.updated', data: { sessionID: 'ses_fixture', cost: 0.5, tokens: { input: 20, output: 5, reasoning: 0, cache: { read: 100, write: 20 } } } });
  await until(() => c.lines.findLast(l => l.t === 'usage').usage.cost === 0.5);
});
```

- [ ] **Step 2: Run** `node --test test/opencode-remote.test.mjs test/opencode-native.test.mjs`; expect failures.
- [ ] **Step 3: Implement.**
  - **`diff` entries:** `{file, additions, deletions, patch}`. `// ponytail: patches are cut so one event's patch text stays under 16 KiB (the socket's line limit is 64 KiB); the rest keep counts only.`
  - **Usage** starts as `{cwd: state.dir, model: state.model, context: null, tokens: null, cost: null}`. At chat start and after every `rebuild()`, read the session (`location.directory`, `tokens`, `cost`), the last visible assistant row with `tokens` (for `context.used`), and `GET /api/model` once (`limit.context` of the entry matching `providerID` and `id`). Then follow `session.usage.updated`, `session.step.ended` (its `tokens` give `context.used`) and `session.model.selected`.
    - Tokens map as `{input, output: output + reasoning, cache_read: cache.read, cache_write: cache.write}`, and `context.used` is `input + output + reasoning + cache.read + cache.write`, as OpenCode's TUI computes it.
  - **Running list,** handled before the existing `data.sessionID !== state.sid` return:
    - The family is the session plus its children, from `GET /api/session?parentID=<sid>` and later `session.created` with that `parentID`.
    - Shells come from `GET /api/shell` and `shell.created`, filtered on `metadata.sessionID` in the family; `shell.exited` and `shell.deleted` remove them. `started` is `time.started` as ISO.
    - Agents are the children in `GET /api/session/active`; `session.execution.started` for a child adds one, and `.succeeded`, `.failed` and `.interrupted` remove it.
- [ ] **Step 4: Run** `node --test`; expect all to pass.
- [ ] **Step 5: Commit** `feat(chat): OpenCode usage, running list, thinking, times and edit diffs`.

### Task 13: Side chat on Codex and OpenCode

**Files:**
- Modify: `crates/agent/src/collect/codex-remote.mjs`, `crates/agent/src/collect/opencode-remote.mjs`, `crates/agent/src/collect/opencode-api.mjs`, `test/codex-native.test.mjs`, `test/opencode-native.test.mjs`

**Interfaces:**
- Consumes: the feed's `ops` table (Part 2); `ChatFeed.pushAside` (Task 8); Part 4's `mainThread`.
- Produces:
  - Codex: `sides: Map<childThreadId, {id, turnId}>`, with the child thread's notifications routed to `feed.pushAside` and never into the conversation.
  - OpenCode: `OpenCode.request(path, { method, body, signal, timeout = 15000 })`; `sides: Map<id, AbortController>`.

- [ ] **Step 1: Write the failing tests.** The Codex fake answers `thread/fork` with `{thread:{id:'side-1'}}`, `thread/inject_items` and `thread/unsubscribe` with `{}`, and `turn/start` on `side-1` with `{turn:{id:'st1',status:'inProgress'}}`. The OpenCode fake answers `POST …/generate` with `{data:{text:'You asked hello.'}}`.

```js
test('a Codex side question forks read-only, streams the answer aside, and cleans up', { timeout: 20000 }, async t => {
  // connect; write {t:'req',rid:1,op:'aside',id:'q1',text:'what did I ask?'} -> res ok
  const fork = f.rpc('thread/fork').params;
  assert.deepEqual([fork.ephemeral, fork.excludeTurns, fork.sandbox, fork.approvalPolicy, fork.model], [true, true, 'read-only', 'never', 'gpt-check']);
  assert.ok(f.rpc('thread/inject_items').params.items[0].content[0].text.startsWith('Side conversation boundary.'));
  assert.equal(f.rpc('turn/start').params.input[0].text, 'what did I ask?');
  // inject item/agentMessage/delta {threadId:'side-1', itemId:'s', delta:'You asked'} and turn/completed {threadId:'side-1', turn:{id:'st1',status:'completed'}}
  // -> lines {t:'aside',id:'q1',text:'You asked',done:false} then done:true; no ev and no delta line for side-1; thread/unsubscribe called
  // a second question cancelled mid-turn ({op:'aside_cancel',id}) -> turn/interrupt {threadId:'side-1',turnId:'st1'} and thread/unsubscribe
});
test('an OpenCode side question is one generate call with the /btw preamble', { timeout: 20000 }, async t => {
  // {op:'aside',id:'q1',text:'what did I ask?'} -> posted('/generate').prompt equals
  // 'The user is asking a quick side question about the conversation so far. Answer directly and concisely in markdown from what you already know. Do not call any tools and do not take any actions.\n\nwhat did I ask?'
  // and the client gets {t:'aside',id:'q1',text:'You asked hello.',done:true}
});
```

- [ ] **Step 2: Run** both files; expect the new tests to fail.
- [ ] **Step 3: Implement.**
  - **Codex `ops.aside`:**
    1. `thread/fork {threadId: state.sid, ephemeral: true, excludeTurns: true, model: state.model, config: {model_reasoning_effort: state.effort}, sandbox: 'read-only', approvalPolicy: 'never'}`;
    2. `thread/inject_items {threadId: child, items: [{type:'message', role:'user', content:[{type:'input_text', text: SIDE_BOUNDARY}]}]}`;
    3. `turn/start {threadId: child, input: [{type:'text', text, text_elements: []}]}`. Return once it is started.
    - `SIDE_BOUNDARY` is the text of `SIDE_BOUNDARY_PROMPT` in Codex 0.167's `codex-rs/tui/src/app/side.rs` (lines 28-42), verbatim; its first line is `Side conversation boundary.`
    - **Routing,** before Part 4's `mainThread` return: for a child in `sides`, `item/agentMessage/delta` becomes `feed.pushAside(id, delta, false)`. `turn/completed` becomes `feed.pushAside(id, '', true, status === 'completed' ? undefined : turn.error?.message || 'side question failed')`, followed by `thread/unsubscribe`.
    - **`ops.aside_cancel`:** `turn/interrupt {threadId: child, turnId}` while the turn runs, then `thread/unsubscribe`.
  - **OpenCode `ops.aside`:** `client.request('/api/session/<sid>/generate', {method:'POST', body:{prompt: BTW + '\n\n' + text}, signal, timeout: 300000})`, not awaited. It ends in `feed.pushAside(id, data.text || '', true)`, or `done` with the error unless aborted. `BTW` is OpenCode's preamble, as in the test. `ops.aside_cancel` aborts the request.
  - **`opencode-api.mjs`:** `request` takes `timeout` in place of the fixed `15000`.
- [ ] **Step 4: Run** `node --test`; expect all to pass.
- [ ] **Step 5: Commit** `feat(chat): side questions on Codex and OpenCode`.

---

## Phase D: The chat screen

### Task 14: The store: times, days, thinking, checklist, staged reverts, edit diffs, rewind rule

**Files:**
- Modify: `public/chat/store.js`, `test/chat-store.test.mjs`

**Interfaces:**
- Consumes: the event and snapshot shapes of the protocol reference.
- Produces:
  - Rows carry new fields: user rows `uuid`, `src`, `ts`; text rows `ts`; tools rows `ts`; and each call `diff`, from its tool result's `diff`.
  - New row kinds: `{kind:'thinking', key, text, ts}`, `{kind:'day', key, ts}` (before the first row of each later local day), and `{kind:'staged', key, count}`.
  - A `state` frame copies `usage` (or `null`) and `tasks` (or `[]`).
  - Exports added to `cdashChatStore`:
    - `todo(s) -> {done, total, items} | null`, from the latest `todo` event;
    - `lineDiff(a: string, b: string) -> [{t: ' '|'-'|'+', s}]`;
    - `editDiff(call) -> {files: [{path, lines}], add, del} | null`;
    - `canRewind(s, row) -> bool`.
  - Edit-tool labels end in ` +<add> −<del>`.

- [ ] **Step 1: Write the failing tests** with the file's helpers (`prompt` takes an `extra` object):

```js
test('rows carry time, id and source; days are separated; thinking is its own row', () => {
  const s = feed([hello(), ev(1, prompt('u1', 'hi', { ts: '2026-11-12T12:00:00Z' })),
    ev(2, { type: 'assistant', uuid: 'k1', ts: '2026-11-12T12:00:05Z', message: { id: 'm1', role: 'assistant', content: [{ type: 'thinking', thinking: 'Plan it.' }] } }),
    ev(3, { ...say('k2', 'm1', 'hello'), ts: '2026-11-12T12:00:06Z' }), ev(4, prompt('u2', 'again', { ts: '2026-11-13T12:00:00Z', src: 'steer' }))]);
  assert.deepEqual(S.rows(s).map(r => r.kind), ['user', 'thinking', 'text', 'day', 'user']);
  const [u1, th, , day, u2] = S.rows(s);
  assert.deepEqual([u1.uuid, u1.src, u1.ts, th.text, day.ts, u2.src], ['u1', 'typed', '2026-11-12T12:00:00Z', 'Plan it.', '2026-11-13T12:00:00Z', 'steer']);
});
test('the checklist is the latest todo event; a staged revert is a row', () => {
  const todo = items => ({ type: 'cdash', subtype: 'todo', items });
  const s = feed([hello(), ev(1, todo([{ text: 'a', status: 'pending' }])), ev(2, todo([{ text: 'a', status: 'completed' }, { text: 'b', status: 'in_progress' }])),
    ev(3, { type: 'cdash', subtype: 'staged', messages: 2 })]);
  assert.deepEqual(S.todo(s), { done: 1, total: 2, items: [{ text: 'a', status: 'completed' }, { text: 'b', status: 'in_progress' }] });
  assert.equal(S.todo(feed([hello()])), null);
  assert.deepEqual(S.rows(s).map(r => [r.kind, r.count]), [['staged', 2]], 'todo events are not rows');
});
test('edit diffs and their counts come from each tool\'s own record', () => {
  assert.deepEqual(S.lineDiff('a\nb\nc', 'a\nB\nc'), [{ t: ' ', s: 'a' }, { t: '-', s: 'b' }, { t: '+', s: 'B' }, { t: ' ', s: 'c' }]);
  const n = d => [d.add, d.del];
  assert.deepEqual(n(S.editDiff({ name: 'Edit', input: { file_path: '/p/x.js', old_string: 'a\nb\nc', new_string: 'a\nB\nc' } })), [1, 1]);
  assert.deepEqual(n(S.editDiff({ name: 'MultiEdit', input: { file_path: '/p/x.js', edits: [{ old_string: 'a', new_string: 'b' }, { old_string: 'c', new_string: 'd\ne' }] } })), [3, 2]);
  assert.deepEqual(n(S.editDiff({ name: 'Write', input: { file_path: '/p/n.txt', content: 'x\ny' } })), [2, 0]);
  const codex = S.editDiff({ name: 'Edit', input: { changes: [{ path: 'a.rs', kind: { type: 'update' }, diff: '@@ -1 +1 @@\n-old\n+new\n' }, { path: 'b.rs', kind: { type: 'add' }, diff: 'one\ntwo\n' }] } });
  assert.deepEqual([...n(codex), codex.files.length], [3, 1, 2]);
  assert.deepEqual(n(S.editDiff({ name: 'edit', input: { path: 'a.ts' }, diff: [{ file: 'a.ts', additions: 1, deletions: 1, patch: '--- a.ts\n+++ a.ts\n@@ -1 +1 @@\n-x\n+y\n' }] })), [1, 1]);
  assert.equal(S.editDiff({ name: 'Read', input: { file_path: '/p/x' } }), null);
  const rows = S.rows(feed([hello(), ev(1, use('a', 't1', 'Edit', { file_path: '/p/x.js', old_string: 'a\nb', new_string: 'a\nB' })), ev(2, done('b', 't1', 'ok'))]));
  assert.equal(rows[0].label, 'Edited x.js +1 −1');
});
test('rewind is offered on idle prompts backed by an event that did not steer; state carries usage and tasks', () => {
  const idle = { working: false, waiting: false, ended: false, unreadable: false };
  assert.equal(S.canRewind(idle, { kind: 'user', uuid: 'u1', src: 'typed' }), true);
  assert.equal(S.canRewind(idle, { kind: 'user', uuid: 'u1' }), true, 'Codex and OpenCode prompts have no src');
  for (const [st, row] of [[{ ...idle, working: true }, { kind: 'user', uuid: 'u1' }], [idle, { kind: 'user', uuid: 'u1', src: 'steer' }],
    [idle, { kind: 'user', cid: 'c1' }], [idle, { kind: 'text', uuid: 'k1' }]]) assert.equal(S.canRewind(st, row), false);
  const s = feed([hello(), { t: 'state', working: false, waiting: false, ended: false, held: [], usage: { cwd: '/w' }, tasks: [{ id: 'b1' }] }]);
  assert.deepEqual([s.usage, s.tasks], [{ cwd: '/w' }, [{ id: 'b1' }]]);
});
```

- [ ] **Step 2: Run** `node --test test/chat-store.test.mjs`; expect failures.
- [ ] **Step 3: Implement.**
  - **`lineDiff`** is an LCS over lines. When `lines(a) × lines(b)` exceeds 250,000 it falls back to all `-` lines, then all `+` lines (`// ponytail:` note).
  - **`editDiff` sources:** Claude `Edit` (`old_string`/`new_string`), `MultiEdit` (`edits`) and `Write` (`content`, all `+`); Codex `input.changes` (`update`: the unified `diff` lines; `add`: all `+`; `delete`: all `-`); OpenCode `call.diff` patches. Counts skip `+++` and `---` lines.
  - **Day rows** compare `new Date(ts).toDateString()` with the previous dated row.
  - **`canRewind`:** `row.kind === 'user'`, a `uuid`, `src !== 'steer'`, and the session neither working, waiting, ended nor unreadable.
  - **If Part 4's conditional Task 26 ran** (rows carry `sig`): the new kinds get one too: thinking (text length), day (its date), staged (count). User rows add `uuid` and `src`.
- [ ] **Step 4: Run** `node --test test/chat-store.test.mjs`; expect all to pass.
- [ ] **Step 5: Commit** `feat(chat): store rows for times, thinking, checklist, staged reverts and edit diffs`.

### Task 15: Syntax colours

**Files:**
- Create: `public/chat/highlight.js`, `test/chat-highlight.test.mjs`
- Modify: `public/chat/md.js`

**Interfaces:**
- Produces: `globalThis.cdashHighlight = { html(code, lang) }`, returning escaped HTML whose only markup is `<span class="hl-c|hl-s|hl-n|hl-k">`. The file has the same IIFE shape as `md.js`. `md.js` renders a fenced block's body with `g.cdashHighlight?.html(unesc(body), lang) ?? body`.

- [ ] **Step 1: Write the failing tests**

```js
import test from 'node:test';
import assert from 'node:assert/strict';
await import('../public/chat/highlight.js');
await import('../public/chat/md.js');
const H = globalThis.cdashHighlight;
test('tokens get classes and everything stays escaped', () => {
  const html = H.html('const s = "<script>alert(1)</script>"; // done\nlet n = 42;', 'js');
  assert.ok(!html.includes('<script>'));
  assert.match(html, /<span class="hl-k">const<\/span>/);
  assert.match(html, /<span class="hl-s">&quot;&lt;script&gt;alert\(1\)&lt;\/script&gt;&quot;<\/span>/);
  assert.match(html, /<span class="hl-c">\/\/ done<\/span>/);
  assert.match(html, /<span class="hl-n">42<\/span>/);
});
test('python, rust, go and shell', () => {
  assert.match(H.html('def f(x):  # note\n    return "a"', 'python'), /<span class="hl-k">def<\/span>.*<span class="hl-c"># note<\/span>.*<span class="hl-k">return<\/span>/s);
  assert.match(H.html('fn main() { let x = 0x1f; }', 'rust'), /<span class="hl-k">fn<\/span>.*<span class="hl-n">0x1f<\/span>/s);
  assert.match(H.html('func f() string { return "go" }', 'go'), /<span class="hl-k">func<\/span>/);
  assert.match(H.html('if [ -f x ]; then echo "y"; fi # end', 'bash'), /<span class="hl-k">if<\/span>.*<span class="hl-c"># end<\/span>/s);
});
test('unknown languages are only escaped, and markdown code blocks use the highlighter', () => {
  assert.equal(H.html('<b>&', 'cobol'), '&lt;b&gt;&amp;');
  assert.match(globalThis.cdashMd.render('```js\nconst a = "<b>";\n```'), /<code class="lang-js"><span class="hl-k">const<\/span> a = <span class="hl-s">&quot;&lt;b&gt;&quot;<\/span>;/);
});
test('unterminated strings and comments neither swallow the rest wrongly nor hang', () => {
  assert.match(H.html('x = "open\ny = 1', 'python'), /<span class="hl-n">1<\/span>/, 'a string stops at the line end');
  const t = Date.now(); H.html('/*'.repeat(20000) + '"'.repeat(20000), 'js'); assert.ok(Date.now() - t < 1000);
});
```

- [ ] **Step 2: Run** `node --test test/chat-highlight.test.mjs`; expect failure.
- [ ] **Step 3: Implement.**
  - **Families:** C-like (`js javascript mjs jsx ts typescript tsx java c h cpp cc cs go rust rs swift kotlin kt php css`) has `//` line comments, `/* */` block comments, `"…"` and `'…'` strings, and `` `…` `` for js and ts. Hash (`py python sh bash shell zsh rb ruby yaml yml toml`) has `#` comments and `"`/`'` strings. `json` gets strings, numbers, `true`, `false` and `null`.
  - **Tokens:** a quoted string ends at its closing quote or the line end; a block comment ends at `*/` or the end. Numbers are `0x…` or digits with `_`, `.` and an exponent. Words in the language's keyword set get `hl-k`.
  - **Scan:** one sticky regex alternation per family, in the order comment, string, number, word. Text between matches is escaped plainly. An unknown or missing `lang` is escaped only.
  - Keyword lists are the builder's (spec); start from each language's reserved words.
- [ ] **Step 4: Run** `node --test`; expect all to pass, including `test/chat-md.test.mjs`, which does not load the highlighter.
- [ ] **Step 5: Commit** `feat(chat): syntax colours in code blocks`.

### Task 16: The chat view: message actions, rewind, drafts, checklist

**Files:**
- Modify: `public/chat/view.js`, `public/app.js`, `public/index.html`, `public/style.css`, `test/frontend-guards.test.mjs`

**Interfaces:**
- Consumes: Task 14's rows and helpers, `cdashHighlight` (through `md.js`), and the `rewind`, `rewind_undo` and `rewound` frames.
- Produces:
  - `cdashChat.open(opts)` takes four more options from `openChat`: `store` (app.js's guarded `store`), `copy` (`text => copyText(text, 'Text')`), `api` (app.js's `api`) and `limits` (`() => ({ claude: lastStats?.claudeUsage || [], codex: lastStats?.codexUsage || [] })`, with a new `let lastStats` set in the render).
  - `copyText(text, label = 'Session ID')` toasts `<label> copied`, or `<label>: <text>`.
  - New markup ids: `#chat-todo` (above `#chat-log`) and `#chat-hint` (above the composer).

- [ ] **Step 1: Write the failing tests** in `test/frontend-guards.test.mjs`:
  - **Copy:** the existing copy test still gets `Session ID copied`. Add `copyText('x', 'Text')` giving `Text copied`.
  - **Options:** `chat gets the store, copy, api and limits` checks the `openChat` source contains `store,`, `copy:`, `api,` and `limits:`.
  - **Markup guard:** extend `the chat view never builds markup from conversation text`. The `innerHTML` assignments must still all go through `cdashMd.render(` or `''`, and `view.js` must not contain `localStorage`.
  - **Ids:** `chat-todo` and `chat-hint` join the id list of `chat scripts load before app.js and the chat markup exists`.
- [ ] **Step 2: Run** `node --test test/frontend-guards.test.mjs`; expect failures.
- [ ] **Step 3: Implement in `view.js`.**
  - **Message actions:** tapping a `user`, `text` or `thinking` row toggles an action line under it: the row's time (`toLocaleTimeString`, when `ts` is set), **Copy** (`opts.copy(row.text)`), and **Rewind to here** when `cdashChatStore.canRewind(state, row)`.
  - **Code blocks:** every `pre` rendered from Markdown gets a **Copy** button with its `textContent`.
  - **New rows:** a `thinking` row is a collapsed **Thinking** button that shows `cdashMd.render(text)` when tapped. A `day` row shows `toLocaleDateString(undefined, {weekday: 'short', day: 'numeric', month: 'short'})`. A `staged` row reads "N messages rewound" with **Undo**, which sends `{t: 'rewind_undo'}`.
  - **Rewind sheet** (in `#chat-sheet`): the title "Rewind to here?" and "The conversation goes back to before this message. Its text returns to the composer."; for Codex, "Files stay as they are."; for Claude and OpenCode, an **Also restore files** checkbox. **Rewind** sends `{t: 'rewind', uuid, files}`; **Cancel** closes the sheet.
  - **`rewound`:** when `ok`, close the sheet and put `text` into the box the way `toBox` does; when `attachments > 0`, `#chat-hint` reads "This prompt had N attachments; attach them again" until the next send. Otherwise `opts.toast(error)`.
  - **Checklist:** `#chat-todo` shows `cdashChatStore.todo(state)` as "D of T done". Tapping toggles a list with one mark per item (✓ completed, • in progress, ○ pending). It is hidden when `todo` is `null`.
  - **Drafts:** `open` restores the box from `opts.store.get('cdash-draft:' + name)`; each `input` saves it, or removes it when empty; a successful submit removes it.
  - **`app.js`:** the `copyText` label, `lastStats`, and the four options.
  - **`style.css`:** the `hl-*` colours (comments muted, strings and numbers in two accent tones, keywords bold) for both themes, from the existing tokens; the action line, thinking, day, staged, checklist and hint styles.
- [ ] **Step 4: Run** `node --test`; expect all to pass.
- [ ] **Step 5: Commit** `feat(chat): message actions, rewind, drafts, thinking, days and checklist`.

### Task 17: Sheets: Changes, Usage, running list, side chat

**Files:**
- Create: `public/chat/sheets.js`, `test/chat-sheets.test.mjs`
- Modify: `public/chat/view.js`, `public/chat/menu.js`, `public/index.html`, `public/style.css`, `crates/agent/src/http/serve.rs`, `crates/agent/src/http/routes.rs`, `test/frontend-guards.test.mjs`

**Interfaces:**
- Consumes: `opts.api`, `opts.limits`, the chat context, `aside` frames and `GET /api/chat/diff`.
- Produces:
  - `globalThis.cdashSheets`. Pure helpers: `diffLines(patch) -> [[cls, text]]` (`cls` is `meta`, `hunk`, `add`, `del` or `ctx`), `elapsed(startedIso, nowMs) -> string`, `asideConfirm(state) -> number` (the context tokens when Claude is over 100,000, else 0) and `btw(text) -> string | null`. DOM: `openDiff(c)`, `openUsage(c)`, `openRunning(c)`, `openAside(c, question)`, `onFrame(c, frame)` and `refresh(c)`.
  - Header buttons `#chat-changes`, `#chat-usage` and `#chat-running` (hidden at 0 tasks).
  - Script tags for `chat/highlight.js` and `chat/sheets.js` before `app.js`.

- [ ] **Step 1: Write the failing tests**

```js
import test from 'node:test';
import assert from 'node:assert/strict';
await import('../public/chat/sheets.js');
const C = globalThis.cdashSheets;
test('diff lines get classes by how they start', () => {
  assert.deepEqual(C.diffLines('diff --git a/x b/x\nindex 1..2\n--- a/x\n+++ b/x\n@@ -1 +1 @@\n-a\n+b\n c\n\\ No newline at end of file'),
    [['meta', 'diff --git a/x b/x'], ['meta', 'index 1..2'], ['meta', '--- a/x'], ['meta', '+++ b/x'], ['hunk', '@@ -1 +1 @@'], ['del', '-a'], ['add', '+b'], ['ctx', ' c'], ['ctx', '\\ No newline at end of file']]);
});
test('elapsed time reads as seconds, minutes or hours', () => {
  const at = s => Date.parse('2026-11-13T10:00:00Z') + s * 1000;
  assert.deepEqual([45, 185, 3720].map(s => C.elapsed('2026-11-13T10:00:00Z', at(s))), ['45s', '3m 05s', '1h 02m']);
});
test('a Claude side question over 100,000 context tokens asks first', () => {
  assert.equal(C.asideConfirm({ tool: 'claude', usage: { context: { used: 120000 } } }), 120000);
  for (const s of [{ tool: 'claude', usage: { context: { used: 100000 } } }, { tool: 'codex', usage: { context: { used: 500000 } } }, { tool: 'claude', usage: null }])
    assert.equal(C.asideConfirm(s), 0);
});
test('/btw becomes a side question', () => {
  assert.deepEqual(['/btw what changed?', '/btw', '/btwx', 'hi /btw x'].map(C.btw), ['what changed?', '', null, null]);
});
```

  In `test/frontend-guards.test.mjs`:
  - `sheets.js` builds markup only through `cdashMd.render(` or `''`;
  - `index.html` loads `chat/highlight.js` and `chat/sheets.js` before `app.js` and has the three header ids.
  In `routes.rs`, `chat_assets_are_embedded` adds `/chat/highlight.js` (`cdashHighlight`) and `/chat/sheets.js` (`cdashSheets`).
- [ ] **Step 2: Run** `node --test test/chat-sheets.test.mjs test/frontend-guards.test.mjs` and `cargo test -p cdash-agent chat_assets`; expect failures.
- [ ] **Step 3: Implement.** All sheets render into `#chat-sheet` with `createElement` and `textContent`; only the side answer uses `cdashMd.render`.
  - **Changes:** `opts.api('/api/chat/diff?session=' + name)`, with `folder` in the header, or "Not a git repository." when `!repo`. One `details` per file: its summary "path +A −D" (or "binary"), its body one `div.diff-<cls>` per `diffLines` row. When `truncated`: "Over 2 MB of changes: the rest is listed without its lines. Use Terminal to see them." Fetched on open, and again whenever `working` turns false while the sheet is open (`refresh`).
  - **Usage:** context "N of W tokens (P%)", or "N tokens" without a window; session tokens as input, output, cache read and cache write; cost when not `null`. Plan limits come from `opts.limits()`: `claude` for Claude, `codex` for Codex, none for OpenCode, each as "short: pct% used · resets …", like the dashboard tiles.
  - **Running list:** "Shell" or "Agent", the title, and `elapsed(started, Date.now())`, refreshed every second while open (the view's 1 s clock calls `refresh`). The header button reads "N running".
  - **Side chat:** `openAside(c, question)` shows a question box and an answer area. Before sending, when `asideConfirm(state)` is greater than 0, it asks "This question re-reads about N tokens of the conversation. Ask anyway?". It sends `{t: 'aside', id: crypto.randomUUID(), text}`. `onFrame` appends each `aside` frame's `text` to the answer and renders it with `cdashMd.render`; `error` is shown as text. **Ask another** clears the sheet. Closing the dialog while an answer runs sends `{t: 'aside_cancel', id}`.
  - **`view.js`:** it wires the header buttons and passes every frame to `cdashSheets.onFrame`. In `showTools`, a call whose `cdashChatStore.editDiff(call)` is not `null` shows each file's lines as `div.diff-add`, `div.diff-del` or `div.diff-ctx` (by `t`), with "path +A −D" above them, instead of its input JSON. Submitting text for which `cdashSheets.btw(text) !== null` opens the side sheet with that question, sends no message, and clears the box. This is the contract's "Part 5 replaces Part 2's `/btw` refusal": chat never sends `/btw`, and Part 2's `picker::route` arm stays only as the guard that keeps it out of Claude's TUI.
  - **`menu.js`:** where Part 2's slash list calls `filter`, a fixed first entry joins `state.menu?.commands` for every tool: `{name: 'btw', description: 'Ask aside: answered in a side sheet, never added to the conversation', hint: '<question>'}`.
  - **`serve.rs`:** two `embedded_asset` arms. **`style.css`:** the diff line classes and the sheet sections.
- [ ] **Step 4: Run** `node --test` and `cargo test -p cdash-agent`; expect all to pass. Run clippy.
- [ ] **Step 5: Commit** `feat(chat): Changes, Usage, running list and side chat sheets`.

---

## Phase E: Finish

### Task 18: Real sessions and findings

**Files:**
- Modify: `CLAUDE_FINDINGS.md`

- [ ] **Step 1: Run the gates.** `node --test`; `cargo test --all --locked`; `cargo clippy --all-targets --locked -- -D warnings -D clippy::disallowed_types`. Expected: all pass.
- [ ] **Step 2: Start a test agent on the private tmux server.** It uses its own ports, and its hook listener is moved off 23275, Part 2's default, which the running agent may hold.

```bash
cargo build -p cdash-agent
mkdir -p /tmp/cdash-p5/bin /tmp/cdash-p5/{claude,codex,opencode}
printf '#!/bin/sh\nexec /usr/bin/tmux -L cdash-test "$@"\n' > /tmp/cdash-p5/bin/tmux && chmod 755 /tmp/cdash-p5/bin/tmux
for d in claude codex opencode; do git -C /tmp/cdash-p5/$d init -q; printf 'one\n' > /tmp/cdash-p5/$d/a.txt
  git -C /tmp/cdash-p5/$d add a.txt; git -C /tmp/cdash-p5/$d -c user.name=t -c user.email=t@t commit -qm init; done
PATH=/tmp/cdash-p5/bin:$PATH PORT=23299 CDASH_HOOK_PORT=23298 "$CARGO_TARGET_DIR/debug/cdash-agent" > /tmp/cdash-p5/agent.log 2>&1 &
cat > /tmp/cdash-p5/chat.mjs <<'EOF'
// node /tmp/cdash-p5/chat.mjs <session> ['<frame json>' …]: prints frames for WAIT ms (default 20000), sending the frames after the first state
const [name, ...out] = process.argv.slice(2);
const ws = new WebSocket(`ws://127.0.0.1:23299/api/chat?name=${encodeURIComponent(name)}`);
let sent = false;
ws.onmessage = e => { const f = JSON.parse(e.data); console.log(JSON.stringify(f).slice(0, 400));
  if (f.t === 'state' && !sent) { sent = true; for (const s of out) ws.send(s); } };
setTimeout(() => process.exit(0), Number(process.env.WAIT || 20000));
EOF
```

  Launch from the test dashboard at http://127.0.0.1:23299: Claude Haiku in `/tmp/cdash-p5/claude`, and Codex and OpenCode in their folders, each with its smallest listed model. Answer Claude's folder-trust dialog in Terminal if it shows. Session names come from `curl -s 127.0.0.1:23299/api/sessions | jq -r '.running[].name'`. Pane keys go through `tmux -L cdash-test send-keys -t =<name>: …`.
- [ ] **Step 3: Claude.** Send three short prompts from chat; the second asks to append `two` to `a.txt`.
  1. **Rewind in Terminal:** Esc, Esc, Up, Enter, digit of "Restore conversation"; then Ctrl+U and a new prompt in Terminal. `chat.mjs` shows a `hello` with `reset: true` without the abandoned prompts. The new record's `parentUuid` (from `jq` on the transcript) equals the abandoned prompt's.
  2. **Rewind from chat, without files:** `chat.mjs <name> '{"t":"rewind","uuid":"<a prompt uuid>","files":false}'` answers `rewound` with `ok` and the text. `capture-pane` shows an empty input box. A new prompt from chat is parented as in 1, so it lands on the state Terminal's rewind gives.
  3. **Rewind from chat, with files,** to the prompt that edited `a.txt`: the answer is `ok`, and `a.txt` reads `one` again.
  4. **Side question:** `md5sum` the transcript. Send `{"t":"aside","id":"q1","text":"In one line, what did I ask you first?"}`: `aside` frames end in `done: true` with a correct answer. The transcript's md5 is unchanged.
  5. **Running list:** prompt "Run `sleep 60` in the background, then reply started". `state.tasks` lists one shell, and about 60 s later it is empty.
  6. **Checks:** `state.usage` has the model, a context and tokens; `curl -s '127.0.0.1:23299/api/chat/diff?session=<name>'` lists the change to `a.txt`.
- [ ] **Step 4: Codex.**
  1. **Double-Esc in Terminal:** Escape, Escape, Enter. Chat gets a reset with the shorter history.
  2. **Rewind from chat:** the answer is `ok` with the text; `capture-pane` shows the restarted TUI without the reverted turn.
  3. **Side question:** an answer arrives as `aside` frames, and a fresh `chat.mjs` connection shows the same number of `ev` frames as before.
  4. **Running list:** the `sleep 60` check as for Claude.
- [ ] **Step 5: OpenCode.**
  1. **Undo in Terminal:** type `/undo` and Enter. Chat shows the `staged` row.
  2. **Rewind and Undo from chat:** rewind to a prompt, then send `{"t":"rewind_undo"}`. The staged row comes, then goes, and the history is whole again.
  3. **Side question:** as for Codex.
  4. **Running list:** the `sleep 60` check.
- [ ] **Step 6: Look at it.** Open http://127.0.0.1:23299 and check, with the owner on a phone where possible: copy on a message and on a code block; colours in a code block; a Thinking row; the checklist (ask Claude to keep a three-item todo list); times and a day separator; a draft kept across closing chat; the Changes, Usage and running sheets; and `/btw` from the composer.
- [ ] **Step 7: Clean up.** Stop the test agent (`kill %1`), `tmux -L cdash-test kill-server`, `rm -rf /tmp/cdash-p5`.
- [ ] **Step 8: Record findings.** Add to `CLAUDE_FINDINGS.md`'s Part 5 section, each marked as observed on 2.1.323 and not a supported contract: the side-question flags, and that a `-p` run writes no transcript and registers an `sdk-cli` session file; the `TaskCreate` and `TaskUpdate` record shapes, if Step 6 produced them; the task-notification carriers the tap reads.
- [ ] **Step 9: Commit** `docs: chat later tier findings` (`git add CLAUDE_FINDINGS.md`). Record each Step 3 to 6 result in the pull request description.
