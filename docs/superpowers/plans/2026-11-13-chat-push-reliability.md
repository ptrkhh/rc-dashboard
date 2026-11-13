# Chat Push Notifications and Reliability Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** The owner's phone, browser and desktop say when a chat session finishes, fails or needs an answer, with no third party reading anything; and the reliability debts Part 1 left (deferred findings, Stop timing, flaky tests, guessed performance shortcuts) are paid.

**Architecture:** Reliability fixes land first, because Parts 2, 3 and 5 build on them; performance shortcuts are measured last and fixed only past the spec's thresholds. In between, every hub publishes `Note`s on `Ctx.notes` from live events only, dropping them while any chat or Terminal socket of that session reports focus. A notifier task keeps hubs on every running session while anyone listens and sends each note as RFC 8291 encrypted Web Push signed with a VAPID JWT (`ring`, `jsonwebtoken`), while `/api/notify` streams the same notes to the desktop app (toasts through `tauri-plugin-notification`) and the Android app (a Kotlin foreground service in a small local Tauri plugin).

**Tech Stack:** Rust (axum 0.8, tokio, ring 0.17.14, jsonwebtoken 9, reqwest 0.13), Tauri 2 with `tauri-plugin-notification`, Kotlin (Android foreground service), vanilla JavaScript, `node --test`, `cargo test`.

**Spec:** `docs/superpowers/specs/2026-11-13-chat-push-reliability-design.md`, evidence in `docs/superpowers/specs/2026-11-13-chat-evidence/` (`review-checks.md`, `claude-2.1.323-live-spike.md`). Cross-plan names come from `docs/superpowers/plans/2026-11-13-chat-parts-2-5-shared.md`. This plan runs first of the four.

## Global Constraints

- Work in `/mnt/d/git/claude-dashboard/.worktrees/chat-parts-2-5` on branch `feat/chat-parts-2-5` (Task 1 creates it). Every cargo command runs with `CARGO_TARGET_DIR=/home/p/.cache/chat-parts-2-5-target` exported (the D: drive is nearly full).
- CI commands: `node --test`, `cargo test --all --locked`, `cargo clippy --all-targets --locked -- -D warnings -D clippy::disallowed_types`.
- Never `git add -A` or `git add .`. `claude-1-261102-19.xapk` in the repo root stays untracked.
- No new npm packages. New Rust crates only where a spec names them: `ring` as a direct dependency of `cdash-agent` at the locked 0.17.14, and `tauri-plugin-notification` in `crates/tauri-app`.
- Agent subprocesses go through `Runner` (`crates/agent/src/host/cmd.rs`). Supervisors never write to stdout: it is the native TUI's terminal.
- Remote Control and Open in Claude keep working. The Remote Control worker code paths (`worker()` in `codex-remote.mjs`, `worker()` and `RemoteBridge` in `opencode-remote.mjs`, `claude-remote.mjs`) change only where a spec says so. The Claude launch command does not change in this plan.
- Every new file under `public/` is added to the embedded file list in `crates/agent/src/http/serve.rs` (this plan adds none).
- CSS class names and ids must not match `/\.send[\s{.:-]/` or contain `send-input`; `app.js` must not contain `/api/keys`.
- A line on the supervisor socket is at most 64 KiB in either direction (`LINE_MAX` in `chat-feed.mjs`).
- The agent logs chat activity without message text; push logs name only the push service's host.
- Mark deliberate shortcuts with a `ponytail:` comment naming the ceiling.
- Real-session checks run on a private tmux server, with `--model haiku` where the tool allows it. They never touch the default tmux server or the running agent on 127.0.0.1:23274. `/model` and `/effort` run only with `~/.claude/settings.json` backed up first and restored after.
- Commit messages use Conventional Commits and end with `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>` and `Claude-Session: https://claude.ai/code/session_0121cbef6XAzH3a3m9iNVdAr`. The commit steps below show the subject only.
- Contract names, exactly: `ClientFrame::Focus { on: bool }` (`{"t":"focus","on":…}`); `EngineMsg::Status { working: bool, waiting: bool, waiting_for: Option<String> }`; state frame key `waiting_for`, always present (`null` when unknown); `Ctx.focus: Mutex<HashMap<String, usize>>`; `Ctx.notes: broadcast::Sender<Note>`.
- Owner-only. No Firebase, UnifiedPush, Termux:API, developer key or third-party account. A push payload is exactly `{"s":<session>,"p":<project folder>,"k":"done"|"error"|"ask","t":<unix seconds>}`: no message text, tool names or file names. No e-mail or personal data in VAPID claims or headers; the VAPID `sub` is `https://github.com/ptrkhh/rc-dashboard`.
- Push state lives in `<claude_dir>/cdash-push.json`, mode 0600, next to `cdash-pins.json`: the VAPID P-256 key (made on first use) and subscriptions `{endpoint, p256dh, auth, added}`.
- Web Push request: `POST` with `Content-Encoding: aes128gcm`, `TTL: 86400`, `Urgency` (`high` for ask, `normal` for done; error: Decision 11), `Topic` = base64url SHA-256 of the session name cut to 32 characters. 404 or 410 removes the subscription; other failures are logged and dropped.
- Texts: title = project folder; body `Finished`, `Stopped with an error`, `Needs your answer`. Android's own notification: `Watching for replies`. The switch: `Notifications on this device`; on an insecure page it is disabled with `needs HTTPS or 127.0.0.1`.
- Timings: needs-an-answer after 5 s of waiting; focus lapses after 2 minutes without input; the notifier lists sessions every 5 s; `/api/notify` sends a comment every 25 s; Stop settles after 500 ms without a new event, 2.5 s at most; the apps reconnect after 5 s, doubling to 5 minutes.
- Windows writes only `HKCU\Software\Classes\AppUserModelId\dev.cdash.app` (`DisplayName` `RC Dashboard`, `IconUri`), needs no admin rights, and removes the key when switched off. Android: foreground service type `specialUse`, `START_STICKY`, not started at boot.
- A device that leaves notifications off sees no change.

## Review Focus

Each line names a condition no acceptance check in the spec exercises. The owning task adds the test that pins it.

1. A phone whose socket drops without saying `focus off` (sleep, network change, killed tab) must stop silencing that session at once. Task 12.
2. Tapping a notification for a session that has ended since must say so, not open a chat that shows "Reconnecting…" forever. Task 20.
3. One device whose subscription is gone (404 or 410) or unreachable must not stop the note reaching the others; the gone one is removed. Task 17.
4. A corrupt `cdash-push.json` must be reported and left untouched: minting a new key over it would silently orphan every subscribed device. Task 16.
5. A push whose payload cannot be read must still show a notification: browsers penalize pushes that show nothing, and the owner should still hear that something happened. Task 19.

## Decisions this plan makes

Places where the spec, the contract and the code disagree, or the spec leaves a choice open:

1. The Android Kotlin lives in a local plugin crate, `crates/tauri-plugin-cdash-watch` (its `android/` Gradle library), not in `crates/tauri-app/gen/android`: that folder is gitignored and `scripts/release.sh` and CI regenerate it with `cargo tauri android init`. The crate's build-dependency `tauri-plugin` is already in Cargo.lock.
2. `tauri-plugin-notification` reports clicks from 2.6.0, and every release from 2.6.0 requires `tauri ^2.12`, so Cargo.lock moves `tauri` from 2.11.5 to 2.12.x. The pinned Tauri CLI is already 2.12.1.
3. `Hub::spawn` keeps its signature for Parts 2, 3 and 5; `Hub::spawn_with(…, Option<notify::Sink>)` carries notes.
4. A Claude turn whose last assistant record has `isApiErrorMessage: true` gets its `result` reported as `error_during_execution` by the tap, so one hub rule covers all three tools. Chat shows the existing "Turn ended with an error" note for it.
5. "The transcript has not grown for 500 ms" is read as "no engine event for 500 ms": `EngineMsg` is fixed by the contract, and every record that decides a hand-back is an event.
6. `Tail::poll` keeps its return type; `Tail::at_end()` reports whether the last poll reached the end of the file.
7. Finding 2: Send again checks the socket before it removes the row. Edit already loses nothing offline (it moves the text into the box), so it is unchanged.
8. Finding 6: a held row missing from `state.held` is dropped, except when that frame says the session ended: then it becomes "Not sent: session ended", as an online client is told. A client that reconnects after its session ended usually gets a 409, not a final state frame; Task 20's tap check covers that case for notifications.
9. Finding 3: a reset acks `unconfirmed` only pending messages no event showed, as `CONFIRM_AFTER` does; messages the old conversation showed leave with it.
10. The dashboard has no menu; the switch is a top-bar icon button with `role="switch"`, beside the theme toggle.
11. `Urgency: high` for `error` as well (the spec names ask and done only), matching the Android channel choice.
12. Android notification importance is per channel, so "Replies" (high: ask, error) and "Finished turns" (default: done) are two channels; "Background" holds the service's own notification at minimum importance.
13. The PWA clears a session's notification from the page (`registration.getNotifications({tag})`), not by messaging the worker.
14. The 30 s test ceilings also cover `chat::native` and `collect::term` and both `eventually` helpers, and every in-process stub executable (not only tmux) goes through the helper: both are needed for the stress acceptance check.
15. The conditional fixes in Tasks 24 and 25 need `Engine::history` and `Engine::watchers`, which the contract does not list. If either task runs, it adds the method with a default body and a row to the contract's `Engine` table.
16. Measurements use a private tmux server through `TMUX_TMPDIR` (the agent's tmux calls take no `-L`), with `TMUX` unset.
17. Windows' fallback if toasts still do not show (a Start-menu shortcut carrying the app ID) is not built; Task 29 reports it to the owner if needed.

## File structure

| File | Responsibility |
|---|---|
| `crates/agent/src/chat/notify.rs` | `Note`, `NoteKind`, `Sink` (focus check, publish), the notifier task |
| `crates/agent/src/chat/ece.rs` | RFC 8291 `aes128gcm` encryption (pure) |
| `crates/agent/src/chat/push.rs` | VAPID key and JWT, `cdash-push.json`, sending a note to every subscription |
| `crates/agent/src/chat/perf.rs` | Ignored measurement tests for the performance table |
| `crates/tauri-app/src/notify.rs` | Desktop stream reader, toasts, taps, Windows app ID |
| `crates/tauri-plugin-cdash-watch/` | Android bridge: Rust handle, Kotlin plugin and foreground service |
| `test/perf/chat-frame.html` | Frame-time harness for 2,000 chat rows (opened in Chrome) |

Modified: `chat/{hub,claude,native,tap,wire,ws,mod,tmux_io}.rs`, `collect/{ctx,term,fsio,codex,opencode,sessions,spawn,summaries}.rs`, `collect/codex-remote.mjs`, `http/{routes,serve}.rs`, `crates/agent/Cargo.toml`, `crates/agent/tests/auth_integration.rs`, `crates/tauri-app/{Cargo.toml,src/lib.rs}`, root `Cargo.toml` and `Cargo.lock`, `public/{app.js,sw.js,index.html,style.css}`, `public/chat/{view,store}.js`, `test/{chat-store,frontend-guards,sw,codex-native,opencode-native}.test.mjs`, `README.md`, `CLAUDE_FINDINGS.md`, the spec's performance table.

---

## Phase A: reliability

### Task 1: Branch, worktree and the documents

**Files:**
- Create (copies): `docs/superpowers/specs/2026-11-13-chat-{prompts-menus,attachments,push-reliability,later-tier}-design.md`, `docs/superpowers/specs/2026-11-13-chat-evidence/`, `docs/superpowers/plans/2026-11-13-chat-{parts-2-5-shared,push-reliability,prompts-menus,attachments,later-tier}.md`

**Interfaces:** none.

- [ ] **Step 1: Create the worktree on a new branch off master**

```bash
cd /mnt/d/git/claude-dashboard
git worktree add -b feat/chat-parts-2-5 .worktrees/chat-parts-2-5 master
```

Expected: `Preparing worktree (new branch 'feat/chat-parts-2-5')`, HEAD at `e3385c4` or later.

- [ ] **Step 2: Copy the documents in.** All five plan files must exist; if one is missing, stop and ask.

```bash
S=/mnt/d/git/claude-dashboard/docs/superpowers
W=/mnt/d/git/claude-dashboard/.worktrees/chat-parts-2-5/docs/superpowers
cp $S/specs/2026-11-13-chat-{prompts-menus,attachments,push-reliability,later-tier}-design.md $W/specs/
cp -r $S/specs/2026-11-13-chat-evidence $W/specs/
cp $S/plans/2026-11-13-chat-{parts-2-5-shared,push-reliability,prompts-menus,attachments,later-tier}.md $W/plans/
```

- [ ] **Step 3: Mark the four specs approved**

```bash
cd /mnt/d/git/claude-dashboard/.worktrees/chat-parts-2-5
sed -i 's/^Status: draft for owner review, 2026-11-13\. No product code has changed\.$/Status: approved by the owner, 2026-11-13./' docs/superpowers/specs/2026-11-13-chat-*-design.md
grep -c '^Status: approved by the owner, 2026-11-13\.$' docs/superpowers/specs/2026-11-13-chat-*-design.md
```

Expected: each of the four files reports `1`.

- [ ] **Step 4: Commit**

```bash
git add docs/superpowers/specs/2026-11-13-chat-*-design.md docs/superpowers/specs/2026-11-13-chat-evidence docs/superpowers/plans/2026-11-13-chat-*.md
git status --short   # only the documents above
git commit -m "docs: chat parts 2 to 5 specs, evidence, shared contract and plans"
```

From here on every task runs in `/mnt/d/git/claude-dashboard/.worktrees/chat-parts-2-5` with `export CARGO_TARGET_DIR=/home/p/.cache/chat-parts-2-5-target`.

### Task 2: Chat client fixes (findings 1, 2 and 6)

**Files:**
- Modify: `public/chat/view.js` (scroll handler 31-39, row buttons 51-61, `sendFrame` 164-168, `render` 262-303, exports 397-403), `public/chat/store.js` (`state` case 75-83)
- Test: `test/frontend-guards.test.mjs`, `test/chat-store.test.mjs`

**Interfaces:**
- Produces: `cdashChat.needPage(more: boolean, paging: boolean, top: number, height: number, view: number) -> boolean`; `cdashChat.resend(state, cid: string, send: (text: string) => boolean) -> boolean`. Store `state` frames reconcile held rows on every frame.

- [ ] **Step 1: Write the failing tests.** In `test/frontend-guards.test.mjs`:

```js
test('older history is asked for while the list cannot scroll, and Send again keeps its row offline', async () => {
  globalThis.document = undefined;
  await import('../public/chat/store.js');
  await import('../public/chat/view.js');
  const { needPage, resend } = globalThis.cdashChat;
  assert.equal(needPage(true, false, 0, 500, 600), true, 'a first screen that does not scroll still pages');
  assert.equal(needPage(true, false, 10, 3000, 600), true, 'scrolled to the top');
  assert.equal(needPage(true, false, 900, 3000, 600), false);
  assert.equal(needPage(false, false, 0, 500, 600), false, 'nothing older');
  assert.equal(needPage(true, true, 0, 500, 600), false, 'one page at a time');
  const s = { mine: [{ cid: 'c1', text: 'hi', status: 'unconfirmed', seq: null }] };
  assert.equal(resend(s, 'c1', () => false), false);
  assert.deepEqual(s.mine.map(m => m.cid), ['c1'], 'offline: the row and its text stay');
  assert.equal(resend(s, 'c1', text => text === 'hi'), true);
  assert.deepEqual(s.mine, []);
});
```

In `test/chat-store.test.mjs`:

```js
test('held rows the latest state no longer lists are dropped, or kept as not sent when the session ended', () => {
  let s = feed([hello()]);
  S.addMine(s, 'c1', 'one'); s = S.apply(s, { t: 'ack', cid: 'c1', status: 'held', id: 'h1' });
  S.addMine(s, 'c2', 'two'); s = S.apply(s, { t: 'ack', cid: 'c2', status: 'held', id: 'h2' });
  s = S.apply(s, { t: 'state', working: true, waiting: false, ended: false, held: [{ id: 'h2', text: 'two' }] });
  assert.deepEqual(s.mine.map(m => m.cid), ['c2'], 'h1 left the queue while this client was away');
  s = S.apply(s, { t: 'state', working: false, waiting: false, ended: true, held: [] });
  assert.deepEqual(S.rows(s).map(r => [r.text, r.status, r.error]), [['two', 'refused', 'session ended']]);
});
```

- [ ] **Step 2: Run** `node --test test/frontend-guards.test.mjs test/chat-store.test.mjs`. Expected: the two new tests fail (`needPage` is not a function; `c1` is still there).
- [ ] **Step 3: Implement.**
  - `needPage` returns `more && !paging && (top < 40 || height <= view)`. The scroll handler and the end of `render()` (after the scroll position is set) both call it with `log.scrollTop, log.scrollHeight, log.clientHeight`, and when it is true and the socket is open they set `paging` and send `{t:'page', before: state.first}`. Render's call is what pages after the first screen and after each page arrives.
  - `resend` finds the `mine` row by `cid`, calls `send(text)`, and removes the row only when that returned true. The log's Send again button uses it with `submitText`. Edit and Dismiss are unchanged.
  - `sendFrame` toasts `Offline` instead of `Not connected`.
  - Store `state`: after assigning the fields, each `mine` row with status `held` whose `heldId` is not in `state.held` becomes `refused` with error `session ended` when the frame has `ended`, `refused` with `not sent` when `forgetHeld` is set (the existing reset rule), and is removed otherwise (it left the queue elsewhere; its prompt event shows it if it was sent). Then clear `forgetHeld`.
- [ ] **Step 4: Run** `node --test`. Expected: all pass.
- [ ] **Step 5: Commit**

```bash
git add public/chat/view.js public/chat/store.js test/frontend-guards.test.mjs test/chat-store.test.mjs
git commit -m "fix(chat): page short histories, keep rows offline, drop stale held rows"
```

### Task 3: Hub acks on reset and confirmed messages (findings 3 and 4)

**Files:**
- Modify: `crates/agent/src/chat/hub.rs` (`Reset` arm 281-286, `event` 299-307, test rig 501-515)

**Interfaces:**
- Produces (hub tests): `rig_accepting()` (a rig whose `FakeEngine` has `accepts: true`; `rig_with` keeps its signature, since Parts 2, 3 and 5 may call it) and `engine_status(working: bool, waiting: bool) -> EngineMsg`. Later tasks build their hub tests on these; the names are specific so the other plans' helpers in this module cannot collide with them.

- [ ] **Step 1: Write the failing tests** in `hub.rs` tests, adding the two helpers (`rig_with` and `rig_accepting` share one private builder taking `slow` and `accepts`).

```rust
#[tokio::test]
async fn a_reset_reports_what_the_old_conversation_never_showed_as_unconfirmed() {
    let (hub, tx, _) = rig();
    let mut sub = hub.subscribe(None, None).await.unwrap();
    hub.command(send("c1", "never shown")).await;
    hub.command(send("c2", "shown")).await;
    tx.send(EngineMsg::Ev(prompt("u2", "shown"))).await.unwrap();
    tx.send(EngineMsg::Reset).await.unwrap();
    settle().await;
    let acks: Vec<(String, String)> = frames(&mut sub).iter().filter(|f| f["t"] == "ack")
        .map(|f| (f["cid"].as_str().unwrap().into(), f["status"].as_str().unwrap().into())).collect();
    assert_eq!(acks, [("c1".to_string(), "unconfirmed".to_string())], "c2 was shown; it leaves with the old conversation");
}
#[tokio::test]
async fn a_message_the_tool_confirmed_is_forgotten_at_the_next_turn_end() {
    let (hub, tx, calls) = rig_accepting();                   // Codex and OpenCode confirm what they accept
    hub.command(send("c1", "the tool reworded this")).await;
    tx.send(EngineMsg::Ev(json!({"type":"result","subtype":"success"}))).await.unwrap();
    settle().await;
    hub.command(ClientFrame::Stop).await;
    assert_eq!(*calls.lock().unwrap(), ["send:the tool reworded this:false"], "idle with nothing pending: Stop interrupts nothing");
}
```

- [ ] **Step 2: Run** `cargo test -p cdash-agent chat::hub`. Expected: both new tests fail.
- [ ] **Step 3: Implement.** On `Reset`, after broadcasting `reset`, ack `unconfirmed` each pending entry that has no `uuid`, then clear `pending`. On a `result` event, drop every exempt pending entry as well as every entry that has a `uuid`; assistant and tool-result events keep today's rule.
- [ ] **Step 4: Run** `cargo test -p cdash-agent chat::hub`. Expected: all pass.
- [ ] **Step 5: Commit** `git add crates/agent/src/chat/hub.rs` then `git commit -m "fix(chat): ack pending messages on reset and forget confirmed ones at turn end"`.

### Task 4: Ready waits for the end of the transcript (finding 5)

**Files:**
- Modify: `crates/agent/src/chat/claude.rs` (`Tail` 25-60, `Ready` at 139-144)

**Interfaces:**
- Produces: `Tail::at_end(&self) -> bool`: true when the last `poll` read up to the file's length as seen by that poll, or found no file; false before the first poll and after a poll that stopped at `READ_MAX`.

- [ ] **Step 1: Write the failing tests** in `claude.rs` tests.

```rust
#[tokio::test]
async fn a_poll_that_stops_inside_a_huge_line_is_not_the_end_of_the_file() {
    let f = tempfile_path("tail-end");
    std::fs::write(&f, format!("{}\nsmall\n", "x".repeat(9 * 1024 * 1024))).unwrap();
    let mut t = Tail::new(f.clone());
    assert!(t.poll().await.unwrap().is_empty());
    assert!(!t.at_end(), "4 MB read, 5 MB to go");
    let mut got = Vec::new();
    for _ in 0..8 { if t.at_end() { break; } got.extend(t.poll().await.unwrap()); }
    assert!(t.at_end());
    assert_eq!(got.last().map(String::as_str), Some("small"));
}
#[tokio::test]
async fn ready_waits_for_the_end_of_a_transcript_with_a_huge_line() {
    let (ctx, dir) = fixture_ctx("huge");
    let blob = format!(r#"{{"type":"api-request-blob","body":"{}"}}"#, "x".repeat(9 * 1024 * 1024));
    write_transcript(&dir, "sid-1", &format!("{}{blob}\n{}", fixture_lines(1..10), fixture_lines(13..14)));
    let (_engine, mut rx) = start(ctx, "cdash-a".into(), 4242);
    let got = collect_until_ready(&mut rx).await;
    assert_eq!(got.iter().filter(|m| matches!(m, EngineMsg::Ev(_))).count(), 6, "the prompt after the blob is history");
}
```

- [ ] **Step 2: Run** `cargo test -p cdash-agent chat::claude`. Expected: compile error (`at_end`); after a stub returning `true`, the second test fails with 5.
- [ ] **Step 3: Implement.** `Tail` gains `at_end: bool`, set by every `poll`: true for a missing file or `len == offset`, else `offset == len` after the read. The engine sends `Ready` only after a poll that returned no lines and left `at_end()` true.
- [ ] **Step 4: Run** `cargo test -p cdash-agent chat::claude`. Expected: all pass.
- [ ] **Step 5: Commit** `git commit -m "fix(chat): Ready waits for the end of the transcript"` (adding `crates/agent/src/chat/claude.rs`).

### Task 5: A test for OpenCode's interrupted turns (finding 7)

**Files:**
- Test: `test/opencode-native.test.mjs` (after the test at 238)

**Interfaces:**
- Consumes: the file's `fixture` (`emit`, `native`), `client`, `until`.

- [ ] **Step 1: Write the test.**

```js
test('a stop from any door reaches chat as interrupted, and a failed turn as an error', { timeout: 30000 }, async t => {
  const f = await fixture(t);
  await f.wait(() => f.native()?.chat_sock);
  const c = await client(f.native().chat_sock, { after: 0, epoch: null });
  await until(() => c.lines.some(l => l.t === 'ready'));
  const sid = f.native().sid;
  const results = () => c.lines.filter(l => l.t === 'ev' && l.ev.type === 'result').map(l => l.ev.subtype);
  await f.emit({ type: 'session.execution.started', data: { sessionID: sid } });
  await f.emit({ type: 'session.execution.interrupted', data: { sessionID: sid } });
  await until(() => results().length === 1);
  await f.emit({ type: 'session.execution.started', data: { sessionID: sid } });
  await f.emit({ type: 'session.execution.failed', data: { sessionID: sid } });
  await until(() => results().length === 2);
  assert.deepEqual(results(), ['interrupted', 'error_during_execution']);
  c.close();
});
```

- [ ] **Step 2: Run** `node --test test/opencode-native.test.mjs`. Expected: PASS. This finding is a missing test, not a bug; if it fails, fix `sink.event` or `ChatBridge` in `supervise` (`opencode-remote.mjs` 400-416) until it passes.
- [ ] **Step 3: Commit** `git commit -m "test(chat): pin OpenCode's interrupted and failed turn conversion"`.

### Task 6: Codex subagent threads stay out of chat (finding 8)

**Files:**
- Modify: `crates/agent/src/collect/codex-remote.mjs` (`Codex` notification branch at 66-70; the control handler at 532-559; after `state.sid = result.thread.id`)
- Test: `test/codex-native.test.mjs`

**Interfaces:**
- Produces: `mainThread(m, sid) -> boolean` in `codex-remote.mjs`: true when `sid` is unset, the notification has no `params.threadId`, or it equals `sid`. `Codex#thread` (unset by default): when set, `turn/started` and `turn/completed` of other threads leave `turnId` and `turnEpoch` alone. Only `nativeLauncher` sets it, so `worker()` behaves as before.

- [ ] **Step 1: Write the failing test** in `test/codex-native.test.mjs`.

```js
test('a subagent thread never reaches the conversation, the working state or the turn id', { timeout: 30000 }, async t => {
  const f = await fixture(t, undefined, process.platform, true);
  await f.wait(() => f.native()?.chat_sock);
  const c = await client(f.native().chat_sock, { after: 0, epoch: null });
  await until(() => c.lines.some(l => l.t === 'ready'));
  const main = '01234567-1234-1234-1234-0123456789ab', child = '89abcdef-0000-4000-8000-000000000000';
  await f.inject({ method: 'turn/started', params: { threadId: main, turn: { id: 't2' } } });
  await f.inject({ method: 'turn/started', params: { threadId: child, turn: { id: 'c1' } } });
  await f.inject({ method: 'item/completed', params: { threadId: child, turnId: 'c1', item: { id: 'ci', type: 'agentMessage', text: 'child says', phase: 'final' } } });
  await f.inject({ method: 'turn/completed', params: { threadId: child, turn: { id: 'c1', status: 'completed' } } });
  await f.inject({ id: 77, method: 'item/tool/requestUserInput', params: { threadId: child, turnId: 'c1', itemId: 'q', questions: [] } });
  await until(() => c.lines.some(l => l.t === 'state' && l.waiting === true));   // a subagent's question still needs an answer
  assert.ok(!c.lines.some(l => l.t === 'ev' && JSON.stringify(l.ev).includes('child says')));
  assert.ok(!c.lines.some(l => l.t === 'ev' && l.ev.type === 'result'), "the child's turn end is not the main turn's");
  assert.equal(c.lines.findLast(l => l.t === 'state').working, true);
  c.write({ t: 'send', cid: 'c1', text: 'steer me', steer: true });
  await until(() => f.rpc('turn/steer'));
  assert.equal(f.rpc('turn/steer').params.expectedTurnId, 't2');
  c.close();
});
```

- [ ] **Step 2: Run** `node --test test/codex-native.test.mjs`. Expected: the new test fails on `child says`.
- [ ] **Step 3: Implement.**
  - In `Codex`, the `turn/started` and `turn/completed` lines run only when `mainThread(m, this.thread)`.
  - In `nativeLauncher`, set `control.thread = state.sid` right after `state.sid = result.thread.id`.
  - In the control handler, handle server requests (`m.id != null && m.method`: waiting on) and `serverRequest/resolved` (waiting off) first, for any thread; then `if (!mainThread(m, state.sid)) return;` with the comment `// a subagent's turns and items are not this conversation; its questions (above) still are`. Everything after it (chat messages, deltas, working state, settings, the `.native` file) is unchanged.
- [ ] **Step 4: Run** `node --test`. Expected: all pass, including the untouched `test/codex-remote.test.mjs`.
- [ ] **Step 5: Commit** `git commit -m "fix(chat): keep Codex subagent threads out of the conversation"`.

### Task 7: `shell` counts as idle (finding 9)

**Files:**
- Modify: `crates/agent/src/chat/claude.rs` (the `working` computation at 146-149)

- [ ] **Step 1: Write the failing test** in `claude.rs` tests.

```rust
#[tokio::test]
async fn a_background_shell_counts_as_idle() {
    let (ctx, dir) = fixture_ctx("shell");
    let (_engine, mut rx) = start(ctx, "cdash-a".into(), 4242);
    collect_until_ready(&mut rx).await;
    assert!(matches!(next_status(&mut rx).await, EngineMsg::Status { working: true, .. }));
    write_session(&dir, r#"{"sessionId":"sid-1","cwd":"/proj","status":"shell"}"#);
    assert!(matches!(next_status(&mut rx).await, EngineMsg::Status { working: false, waiting: false, .. }), "\"· 1 shell\": idle with background work");
}
```

- [ ] **Step 2: Run** `cargo test -p cdash-agent chat::claude`. Expected: the new test fails.
- [ ] **Step 3: Implement:** `working` is `status == "busy"`; the transcript fallback for a missing `status` is unchanged.
- [ ] **Step 4: Run** the same command. Expected: all pass.
- [ ] **Step 5: Commit** `git commit -m "fix(chat): a background shell no longer holds chat messages"`.

### Task 8: Stop settles on a quiet record

**Files:**
- Modify: `crates/agent/src/chat/hub.rs` (constants 21-25, `Actor`, the `Ev` arm, `tick` 433-440)

**Interfaces:**
- Produces: `hub::STOP_QUIET: Duration = 500 ms`. `STOP_SETTLE` stays 2.5 s and is now the ceiling.

- [ ] **Step 1: Write the failing tests** in `hub.rs` tests.

```rust
#[tokio::test(start_paused = true)]
async fn a_stop_settles_half_a_second_after_the_record_goes_quiet() {
    let (hub, tx, calls) = rig();
    hub.command(send("c1", "first")).await;
    tx.send(engine_status(true, false)).await.unwrap();
    tx.send(EngineMsg::Ev(prompt("u1", "first"))).await.unwrap();
    settle().await;
    hub.command(ClientFrame::Stop).await;
    tx.send(engine_status(false, false)).await.unwrap();
    tokio::time::sleep(Duration::from_millis(400)).await;
    assert_eq!(calls.lock().unwrap().len(), 2, "not before 500 ms of quiet");
    tokio::time::sleep(Duration::from_millis(400)).await;
    assert_eq!(*calls.lock().unwrap(), ["send:first:false", "stop", "clear:first"]);
}
#[tokio::test(start_paused = true)]
async fn a_record_that_keeps_growing_settles_at_the_ceiling() {
    let (hub, tx, calls) = rig();
    hub.command(send("c1", "first")).await;
    tx.send(engine_status(true, false)).await.unwrap();
    settle().await;
    hub.command(ClientFrame::Stop).await;
    tx.send(engine_status(false, false)).await.unwrap();
    for i in 1..=14 {
        tokio::time::sleep(Duration::from_millis(200)).await;
        tx.send(EngineMsg::Ev(json!({"type":"cdash","subtype":"queued","text":"other"}))).await.unwrap();
        if i == 12 { assert_eq!(calls.lock().unwrap().len(), 2, "2.4 s and still growing"); }
    }
    assert_eq!(calls.lock().unwrap().last().map(String::as_str), Some("clear:first"), "settled at the 2.5 s ceiling");
}
```

- [ ] **Step 2: Run** `cargo test -p cdash-agent chat::hub`. Expected: the first new test fails (the hand-back waits 2.5 s).
- [ ] **Step 3: Implement.** The actor keeps `last_ev: Instant`, set on every `EngineMsg::Ev`. A Stop has settled when `!working` and either `now - max(stopped_at, last_ev) >= STOP_QUIET` or `now - stopped_at >= STOP_SETTLE`; the rest of the hand-back rule and the `CONFIRM_AFTER` give-up are unchanged. Update the doc comment on `STOP_SETTLE` (now the ceiling) and keep the existing `nothing_is_pasted…` test as it is.
- [ ] **Step 4: Run** the same command. Expected: all pass.
- [ ] **Step 5: Commit** `git commit -m "fix(chat): Stop settles 500 ms after the record goes quiet"`.

### Task 9: `waiting_for` in Status and the state frame

**Files:**
- Modify: `crates/agent/src/chat/hub.rs` (`EngineMsg`, `Actor`, `state()`, the `Status` arm, tests), `crates/agent/src/chat/claude.rs` (status at 145-157, tests at 303-305), `crates/agent/src/chat/native.rs` (`state` line at 143-146, test at 233)

**Interfaces:**
- Produces: `EngineMsg::Status { working: bool, waiting: bool, waiting_for: Option<String> }`: Claude's `waitingFor` while `status` is `waiting`, else `None`; always `None` for Codex and OpenCode. State frame key `waiting_for` (string or `null`, always present).

- [ ] **Step 1: Write the failing tests.** In `hub.rs`:

```rust
#[tokio::test]
async fn the_state_frame_says_what_the_session_waits_for() {
    let (hub, tx, _) = rig();
    let mut sub = hub.subscribe(None, None).await.unwrap();
    assert_eq!(serde_json::from_str::<Value>(sub.backlog.last().unwrap()).unwrap()["waiting_for"], Value::Null);
    tx.send(EngineMsg::Status { working: false, waiting: true, waiting_for: Some("dialog open".into()) }).await.unwrap();
    settle().await;
    assert_eq!(frames(&mut sub).last().unwrap()["waiting_for"], "dialog open");
}
```

In `claude.rs`, the existing step that writes `"waitingFor":"dialog open"` asserts `EngineMsg::Status { working: false, waiting: true, waiting_for: Some(ref w) } if w == "dialog open"`, and a new step writes `{"sessionId":"sid-1","cwd":"/proj","status":"busy","waitingFor":"dialog open"}` and asserts `EngineMsg::Status { working: true, waiting_for: None, .. }` (a stale field outside `waiting` is ignored). In `native.rs`, the `state` line asserts `waiting_for: None`.

- [ ] **Step 2: Run** `cargo test -p cdash-agent chat`. Expected: compile errors.
- [ ] **Step 3: Implement.** Add the field everywhere `Status` is built or matched; `engine_status()` in hub tests now builds `waiting_for: None`, and the older literal constructions in hub tests switch to it. The Claude engine's change detection compares the whole triple. The hub stores `waiting_for` and the state frame carries it.
- [ ] **Step 4: Run** `cargo test -p cdash-agent` and clippy. Expected: all pass.
- [ ] **Step 5: Commit** `git commit -m "feat(chat): carry Claude's waitingFor to chat clients"`.

### Task 10: Test ceilings of 30 s

**Files:**
- Modify (tests only): `crates/agent/src/chat/claude.rs` (`next_msg` 244), `crates/agent/src/chat/ws.rs` (`next_json` 151-161), `crates/agent/src/chat/native.rs` (193, 202), `crates/agent/src/chat/tmux_io.rs` (`eventually` 148-156), `crates/agent/src/collect/term.rs` (`read_until` 511, `close_code` 527, `eventually` 541-549)

- [ ] **Step 1: Raise every 5 s ceiling to 30 s**, messages included ("within 30 s"); both `eventually` helpers poll 300 times at 100 ms. They are ceilings for a failing test, not expectations of a passing one.
- [ ] **Step 2: Run** `cargo test -p cdash-agent`. Expected: all pass, in the same time as before.
- [ ] **Step 3: Commit** `git commit -m "test(chat): 30 s ceilings instead of 5 s"`.

### Task 11: Stub executables through Part 1's helper

**Files:**
- Modify (tests only): `crates/agent/src/chat/tmux_io.rs` (`stub_tmux` 83-105), `crates/agent/src/collect/codex.rs` (`host_in` 459-479, 608), `crates/agent/src/collect/opencode.rs` (`fixture` 468-481, 574), `crates/agent/src/collect/sessions.rs` (372-374, `fake_tmux` 616-626), `crates/agent/src/collect/spawn.rs` (`stub_tmux` 353-367), `crates/agent/src/collect/summaries.rs` (175-176)

**Interfaces:**
- Produces: `crate::chat::tmux_io::tests::install_stub(dir: &Path, name: &str, script: &str) -> String`: writes `script` to `<dir>/<name>.src`, has a child `sh` copy it to `<dir>/bin/<name>.new`, `chmod 755` it and `mv -f` it onto `<dir>/bin/<name>`, and returns `<dir>/bin`. `stub_tmux` becomes a call to it with today's script. `eventually` becomes `pub(crate)`.

- [ ] **Step 1: Convert** every test that writes an executable from the test process (the files above, including the in-place rewrites of `bin/codex` and `bin/opencode`) to `install_stub`. No thread of the test process may hold a write handle to a file another thread executes.
- [ ] **Step 2: Check** `grep -rn 'set_permissions' crates/agent/src`. Expected: only production code (`opencode.rs` `private_write` and `install`, `summaries.rs` root folder) and `host/probe.rs` `make_executable` (that file is never executed).
- [ ] **Step 3: Run the suites five times under load.**

```bash
stress -c $(nproc) & L=$!          # no stress installed: for i in $(seq $(nproc)); do (while :; do :; done) & done
for i in 1 2 3 4 5; do cargo test --all --locked -q && node --test || break; done
kill $L                            # or: kill $(jobs -p)
```

Expected: five passes. A test that times out under load gets its ceiling raised to 30 s the same way (node `until` defaults and per-test `timeout` included); a socket test closed by the 2 s `PING_EVERY` under test gets it raised to 10 s. Name each change in the commit body.
- [ ] **Step 4: Commit** `git commit -m "test: write stub executables through a child process"`.

---

## Phase B: notifications

### Task 12: Focus on the agent

**Files:**
- Modify: `crates/agent/src/collect/ctx.rs` (field, `focused`, `FocusGuard`, tests), `crates/agent/src/chat/wire.rs` (`Focus`), `crates/agent/src/chat/hub.rs` (`client` match), `crates/agent/src/chat/ws.rs` (`pump`), `crates/agent/src/collect/term.rs` (`pump`, Text arm at 260-264)

**Interfaces:**
- Produces: `Ctx.focus: Mutex<HashMap<String, usize>>` (std `Mutex`); `Ctx::focused(&self, name: &str) -> bool`; `pub struct FocusGuard<'a>` with `FocusGuard::new(ctx: &'a Ctx, name: &'a str) -> Self` and `set(&mut self, on: bool)` (counts this connection at most once; `Drop` sets it off; a count of 0 removes the key); `ClientFrame::Focus { on: bool }`. `ws::pump` gains `ctx: &Ctx, name: &str`.

- [ ] **Step 1: Write the failing tests.** In `ctx.rs` (new `tests` module, ctx from `crate::chat::claude::tests::ctx_with_path(tempfile_path("focus"), String::new())`):

```rust
#[test]
fn focus_counts_each_connection_once_and_a_dropped_one_counts_as_off() {
    let ctx = ctx();
    let (mut a, mut b) = (FocusGuard::new(&ctx, "cdash-a"), FocusGuard::new(&ctx, "cdash-a"));
    a.set(true); a.set(true); b.set(true);
    assert_eq!(ctx.focus.lock().unwrap().get("cdash-a"), Some(&2));
    a.set(false);
    assert!(ctx.focused("cdash-a"), "the other device still looks");
    drop(b);
    assert!(!ctx.focused("cdash-a") && !ctx.focus.lock().unwrap().contains_key("cdash-a"));
}
```

In `wire.rs`, `client_frames_parse_and_unknown_ones_do_not` gains `assert!(matches!(f(r#"{"t":"focus","on":true}"#), Ok(ClientFrame::Focus { on: true })));`. In `ws.rs` (Review Focus 1):

```rust
#[tokio::test]
async fn focus_frames_feed_the_count_and_a_socket_that_drops_counts_as_off() {
    let (b, _dir) = chat_server("focus");
    let mut ws = connect(b.addr, "name=cdash-a", None).await.unwrap();
    next_of(&mut ws, "state").await;
    ws.send(WsMsg::Text(r#"{"t":"focus","on":true}"#.into())).await.unwrap();
    eventually("chat focus counted", || b.ctx.focused("cdash-a")).await;
    drop(ws);                                              // no "focus off": the phone went to sleep
    eventually("dropped socket uncounted", || !b.ctx.focused("cdash-a")).await;
}
```

In `term.rs`, the same test against `/api/term?name=<s>&cols=80&rows=24` on a real tmux `Session::new("focus")`, skipped without tmux like its neighbours.
- [ ] **Step 2: Run** `cargo test -p cdash-agent ctx wire chat::ws term`. Expected: compile errors.
- [ ] **Step 3: Implement.** `Ctx::new` creates the empty map. `ws::pump` holds a `FocusGuard` for its connection, applies `ClientFrame::Focus` itself and never forwards it; the hub's `client` match answers `Focus` with `None`. The Terminal pump holds a guard too: a text frame that is not a resize and parses as `ClientFrame::Focus { on }` sets it.
- [ ] **Step 4: Run** the same command and clippy. Expected: all pass.
- [ ] **Step 5: Commit** `git commit -m "feat(chat): per-session focus count fed by chat and Terminal sockets"`.

### Task 13: Focus reporting in the browser

**Files:**
- Modify: `public/chat/view.js` (tracker and exports; `connect`, `open`, `close`), `public/app.js` (`termConnect` `onopen` at 641-647, `openTerm` 664-711, `closeTerm` 713-722)
- Test: `test/frontend-guards.test.mjs`

**Interfaces:**
- Produces: `cdashChat.focusActive({ visible, focused, lastInput, now }) -> boolean` (true when visible, focused and `now - lastInput < 120000`); `cdashChat.watchFocus(fn: (on: boolean) => void) -> () => void` (unsubscribe); `cdashChat.focusNow() -> boolean`.

- [ ] **Step 1: Write the failing test**

```js
test('focus counts only a visible, focused page with input in the last two minutes, and both sockets report it', async () => {
  globalThis.document = undefined;
  await import('../public/chat/store.js');
  await import('../public/chat/view.js');
  const at = o => globalThis.cdashChat.focusActive({ visible: true, focused: true, lastInput: 0, now: 0, ...o });
  assert.equal(at({ now: 119_999 }), true);
  assert.equal(at({ now: 120_000 }), false, 'a desk window left focused stops counting after two idle minutes');
  assert.equal(at({ visible: false }), false);
  assert.equal(at({ focused: false }), false);
  const src = f => readFileSync(new URL(f, import.meta.url), 'utf8');
  for (const f of ['../public/chat/view.js', '../public/app.js']) assert.match(src(f), /t: 'focus', on/, f);
});
```

- [ ] **Step 2: Run** `node --test test/frontend-guards.test.mjs`. Expected: the new test fails.
- [ ] **Step 3: Implement.** The tracker starts on the first `watchFocus`: `visibilitychange` on `document`; `focus` and `blur` on `window`; `keydown`, `pointerdown` and `touchstart` on `document` (capture, passive) set `lastInput`; a timer re-checks at `lastInput + 120000`; subscribers hear only changes. Chat sends `{ t: 'focus', on: focusNow() }` after each `hello` and on every change while its socket is open; it subscribes in `open` and unsubscribes in `close`. The Terminal does the same on `ws.onopen` and while `term` is open (`term.unwatch`, called in `closeTerm`).
- [ ] **Step 4: Run** `node --test`. Expected: all pass.
- [ ] **Step 5: Commit** `git commit -m "feat(chat): chat and Terminal report focus to the agent"`.

### Task 14: Notes from hubs

**Files:**
- Create: `crates/agent/src/chat/notify.rs`
- Modify: `crates/agent/src/collect/ctx.rs` (`notes`), `crates/agent/src/chat/hub.rs` (`spawn_with`, note rules), `crates/agent/src/chat/tap.rs` (`Rec`, `Tap`, `turn_duration` arm at 77-83), `crates/agent/src/chat/mod.rs` (`pub mod notify;`, `hub_for` at 36-56)

**Interfaces:**
- Consumes: `FocusGuard`, `Ctx::focused` (Task 12); `engine_status` (Task 3).
- Produces:
  - `notify::NoteKind { Done, Error, Ask }` with `as_str(self) -> &'static str` (`"done"`, `"error"`, `"ask"`);
  - `notify::Note { pub session: String, pub project: String, pub kind: NoteKind, pub at: u64 }` (`Clone`, `Debug`, `PartialEq`) with `payload(&self) -> String`: the JSON `{"s","p","k","t"}` in that order;
  - `notify::Sink { pub ctx: Arc<Ctx>, pub project: String }` with `note(&self, session: &str, kind: NoteKind)`: nothing while `ctx.focused(session)`, else sends a `Note` stamped with the current Unix time on `ctx.notes`;
  - `Ctx.notes: tokio::sync::broadcast::Sender<Note>` (capacity 64);
  - `Hub::spawn_with(tool, engine, events, log, name, notes: Option<Sink>) -> Hub`; `Hub::spawn` calls it with `None`;
  - `hub::ASK_AFTER: Duration = 5 s`.

- [ ] **Step 1: Write the failing tests.** In `hub.rs`, `rig_noting(ready: bool) -> (Hub, Sender<EngineMsg>, Arc<Ctx>, broadcast::Receiver<Note>)` builds the hub with `spawn_with(…, Some(Sink { ctx, project: "proj".into() }))` on a ctx from `ctx_with_path`, subscribes to `ctx.notes` first, and sends `Ready` only when asked; `note_kinds(&mut rx) -> Vec<&'static str>` drains it; `turn_end(subtype) -> EngineMsg` builds a result event.

```rust
#[tokio::test]
async fn history_never_notifies_only_live_turn_ends_do() {
    let (_hub, tx, _ctx, mut notes) = rig_noting(false);
    tx.send(turn_end("success")).await.unwrap();                    // replayed history
    tx.send(EngineMsg::Ready).await.unwrap();
    for s in ["success", "error_during_execution", "interrupted"] { tx.send(turn_end(s)).await.unwrap(); }
    tx.send(EngineMsg::Reset).await.unwrap();
    tx.send(turn_end("success")).await.unwrap();                    // the new conversation's history
    tx.send(EngineMsg::Ready).await.unwrap();
    tx.send(turn_end("success")).await.unwrap();
    settle().await;
    assert_eq!(note_kinds(&mut notes), ["done", "error", "done"], "a Stop or Esc (interrupted) never notifies");
}
#[tokio::test]
async fn finished_waits_for_the_queue_but_an_error_does_not() {
    let (hub, tx, _ctx, mut notes) = rig_noting(true);
    tx.send(engine_status(true, false)).await.unwrap();
    settle().await;
    hub.command(send("c1", "next")).await;                        // held; leaves on its own
    tx.send(turn_end("success")).await.unwrap();
    settle().await;
    assert!(note_kinds(&mut notes).is_empty(), "the held message is about to go");
    tx.send(turn_end("error_during_execution")).await.unwrap();
    settle().await;
    assert_eq!(note_kinds(&mut notes), ["error"]);
    hub.command(ClientFrame::Stop).await;                         // after a Stop nothing leaves on its own
    tx.send(turn_end("success")).await.unwrap();
    settle().await;
    assert_eq!(note_kinds(&mut notes), ["done"]);
}
#[tokio::test(start_paused = true)]
async fn needs_an_answer_after_five_seconds_once_per_spell_and_never_for_a_dialog() {
    let (_hub, tx, _ctx, mut notes) = rig_noting(true);
    let waiting = |w: &str| EngineMsg::Status { working: false, waiting: true, waiting_for: Some(w.into()) };
    tx.send(waiting("permission prompt")).await.unwrap();
    tokio::time::sleep(ASK_AFTER - Duration::from_millis(500)).await;
    assert!(note_kinds(&mut notes).is_empty(), "answered quickly in Terminal: no buzz");
    tokio::time::sleep(Duration::from_secs(6)).await;
    assert_eq!(note_kinds(&mut notes), ["ask"], "once per waiting spell");
    tx.send(engine_status(true, false)).await.unwrap();
    tx.send(waiting("dialog open")).await.unwrap();
    tokio::time::sleep(Duration::from_secs(10)).await;
    assert!(note_kinds(&mut notes).is_empty(), "/exit, rewind and model-switch dialogs never notify");
}
#[tokio::test]
async fn a_session_someone_is_looking_at_does_not_notify() {
    let (_hub, tx, ctx, mut notes) = rig_noting(true);
    let mut looking = FocusGuard::new(&ctx, "cdash-a");
    looking.set(true);
    tx.send(turn_end("success")).await.unwrap();
    settle().await;
    assert!(note_kinds(&mut notes).is_empty());
    drop(looking);
    tx.send(turn_end("success")).await.unwrap();
    settle().await;
    assert_eq!(note_kinds(&mut notes), ["done"]);
}
```

In `notify.rs`:

```rust
#[test]
fn the_payload_carries_session_project_kind_and_time_only() {
    let n = Note { session: "cdash-a".into(), project: "my \"proj\"".into(), kind: NoteKind::Ask, at: 1_789_000_000 };
    assert_eq!(serde_json::from_str::<Value>(&n.payload()).unwrap(), json!({"s":"cdash-a","p":"my \"proj\"","k":"ask","t":1_789_000_000u64}));
}
```

In `tap.rs`:

```rust
#[test]
fn a_turn_that_ended_on_an_api_error_ends_with_an_error_result() {
    let mut tap = Tap::default();
    let end = r#"{"type":"system","subtype":"turn_duration","durationMs":5,"uuid":"s9"}"#;
    tap.feed(r#"{"type":"user","uuid":"u9","promptSource":"typed","message":{"role":"user","content":"go"}}"#);
    tap.feed(r#"{"type":"assistant","uuid":"k9","isApiErrorMessage":true,"message":{"id":"m9","role":"assistant","content":[{"type":"text","text":"API Error: Rate limit reached"}]}}"#);
    assert_eq!(tap.feed(end)[0]["subtype"], "error_during_execution");
    tap.feed(r#"{"type":"assistant","uuid":"k10","message":{"id":"m10","role":"assistant","content":[{"type":"text","text":"ok"}]}}"#);
    assert_eq!(tap.feed(end)[0]["subtype"], "success");
}
```

- [ ] **Step 2: Run** `cargo test -p cdash-agent chat`. Expected: compile errors.
- [ ] **Step 3: Implement.**
  - The actor gains `notes: Option<Sink>`, `live: bool` (true on `Ready`, false on `Reset`), `waiting_since: Option<Instant>` and `asked: bool`. Nothing notifies unless `live`.
  - A `result` event: `success` sends `Done` unless the queue's head is `auto` (`queue.list().first().is_some_and(|h| h.auto)`); `error_during_execution` sends `Error`; anything else sends nothing.
  - `Status`: waiting turning true sets `waiting_since = now` and `asked = false`; waiting false clears `waiting_since`. Each tick sends `Ask` once when `live && waiting && !asked`, `ASK_AFTER` has passed since `waiting_since`, and `waiting_for` is not `dialog open`.
  - The tap reads `isApiErrorMessage` into `Rec`, remembers it per main-chain assistant record (`api_error`, reset by any other assistant record), and reports the next `turn_duration` as `error_during_execution` while it is set.
  - `hub_for` passes `Some(Sink { ctx, project })`. The project is the last path component of the native snapshot's `dir`, or of the Claude session file's `cwd` (`lookup::session_file_for`), else the session name.
- [ ] **Step 4: Run** `cargo test -p cdash-agent` and clippy. Expected: all pass.
- [ ] **Step 5: Commit** `git commit -m "feat(chat): hubs publish finished, failed and needs-an-answer notes"`.

### Task 15: Web Push crypto

**Files:**
- Create: `crates/agent/src/chat/ece.rs`, `crates/agent/src/chat/push.rs` (the VAPID part)
- Modify: `crates/agent/Cargo.toml` (`ring = "0.17.14"`), `Cargo.lock`, `crates/agent/src/chat/mod.rs`

**Interfaces:**
- Produces:
  - `ece::b64(bytes: &[u8]) -> String` and `ece::unb64(s: &str) -> Option<Vec<u8>>` (base64url, no padding);
  - `ece::encrypt(ua_public: &[u8], auth: &[u8], plaintext: &[u8]) -> Result<Vec<u8>, String>`: one `aes128gcm` record with a random salt and a fresh ECDH P-256 key, record size 4096;
  - `pub(crate) ece::encrypt_with(ecdh_secret: &[u8], auth: &[u8], ua_public: &[u8], as_public: &[u8], salt: &[u8; 16], plaintext: &[u8]) -> Vec<u8>` and `derive(…same first five…) -> ([u8; 16], [u8; 12])` (CEK, nonce);
  - `#[cfg(test)] pub(crate) ece::decrypt(ua_private: EphemeralPrivateKey, ua_public: &[u8], auth: &[u8], body: &[u8]) -> Vec<u8>`;
  - `push::SUBJECT = "https://github.com/ptrkhh/rc-dashboard"`; `push::Vapid` with `generate() -> Result<Vapid, String>`, `from_pkcs8_b64(&str) -> Result<Vapid, String>`, `pkcs8_b64(&self) -> String`, `public_b64(&self) -> String` (65-byte point), `jwt(&self, endpoint: &str, now: u64) -> Result<String, String>` (ES256 via `jsonwebtoken`, claims `aud` = the endpoint's origin, `exp` = `now + 12 h`, `sub` = `SUBJECT`), `authorization(&self, endpoint: &str, now: u64) -> Result<String, String>` (`vapid t=<jwt>, k=<public>`).

- [ ] **Step 1: Write the failing tests.** In `ece.rs`, the RFC 8291 Appendix A vector (`ring` cannot load a fixed ECDH key, so the vector's `ecdh_secret` feeds the steps after agreement) and a round trip that covers the agreement:

```rust
#[test]
fn rfc_8291_appendix_a() {
    let u = |s: &str| unb64(s).unwrap();
    let (ua, asp, auth) = (u("BCVxsr7N_eNgVRqvHtD0zTZsEc6-VV-JvLexhqUzORcxaOzi6-AYWXvTBHm4bjyPjs7Vd8pZGH6SRpkNtoIAiw4"),
        u("BP4z9KsN6nGRTbVYI_c7VJSPQTBtkgcy27mlmlMoZIIgDll6e3vCYLocInmYWAmS6TlzAC8wEqKK6PBru3jl7A8"), u("BTBZMqHH6r4Tts7J_aSIgg"));
    let salt: [u8; 16] = u("DGv6ra1nlYgDCS1FRnbzlw").try_into().unwrap();
    let ecdh = u("kyrL1jIIOHEzg3sM2ZWRHDRB62YACZhhSlknJ672kSs");
    let (cek, nonce) = derive(&ecdh, &auth, &ua, &asp, &salt);
    assert_eq!((b64(&cek), b64(&nonce)), ("oIhVW04MRdy2XN9CiKLxTg".to_string(), "4h_95klXJ5E_qnoN".to_string()));
    let body = encrypt_with(&ecdh, &auth, &ua, &asp, &salt, b"When I grow up, I want to be a watermelon");
    assert_eq!(b64(&body), "DGv6ra1nlYgDCS1FRnbzlwAAEABBBP4z9KsN6nGRTbVYI_c7VJSPQTBtkgcy27mlmlMoZIIgDll6e3vCYLocInmYWAmS6TlzAC8wEqKK6PBru3jl7A_yl95bQpu6cVPTpK4Mqgkf1CXztLVBSt2Ks3oZwbuwXPXLWyouBWLVWGNWQexSgSxsj_Qulcy4a-fN");
}
#[test]
fn a_message_encrypted_to_a_fresh_key_decrypts_with_it() {
    let rng = ring::rand::SystemRandom::new();
    let ua = EphemeralPrivateKey::generate(&ECDH_P256, &rng).unwrap();
    let ua_public = ua.compute_public_key().unwrap().as_ref().to_vec();
    let body = encrypt(&ua_public, &[7u8; 16], br#"{"s":"cdash-a"}"#).unwrap();
    assert_eq!(body.len(), 86 + 15 + 1 + 16, "one record: header, text, delimiter, tag");
    assert_eq!(decrypt(ua, &ua_public, &[7u8; 16], &body), br#"{"s":"cdash-a"}"#);
}
```

In `push.rs`:

```rust
#[test]
fn the_vapid_token_is_es256_for_the_push_origin_and_names_the_project() {
    let v = Vapid::generate().unwrap();
    assert_eq!(Vapid::from_pkcs8_b64(&v.pkcs8_b64()).unwrap().public_b64(), v.public_b64(), "the stored key reloads");
    let jwt = v.jwt("https://fcm.googleapis.com/fcm/send/abc:def", 1_800_000_000).unwrap();
    let part = |i: usize| -> Value { serde_json::from_slice(&unb64(jwt.split('.').nth(i).unwrap()).unwrap()).unwrap() };
    assert_eq!(part(0), json!({"typ":"JWT","alg":"ES256"}));
    assert_eq!(part(1), json!({"aud":"https://fcm.googleapis.com","exp":1_800_000_000u64 + 12 * 3600,"sub":SUBJECT}));
    let (signed, sig) = jwt.rsplit_once('.').unwrap();
    ring::signature::UnparsedPublicKey::new(&ring::signature::ECDSA_P256_SHA256_FIXED, unb64(&v.public_b64()).unwrap())
        .verify(signed.as_bytes(), &unb64(sig).unwrap()).expect("verifies against the advertised key");
    assert!(!jwt.contains('@'));
    assert!(v.authorization("https://fcm.googleapis.com/x", 1).unwrap().starts_with("vapid t="));
}
```

- [ ] **Step 2: Run** `cargo test -p cdash-agent chat::ece chat::push`. Expected: compile errors.
- [ ] **Step 3: Implement.** Add `ring = "0.17.14"` and let Cargo.lock record it (`cargo tree -i ring -p cdash-agent` still shows only 0.17.14).
  - `derive`: `PRK_key = HMAC(auth, ecdh)`; `IKM = HKDF-Expand(PRK_key, "WebPush: info\0" ‖ ua_public ‖ as_public, 32)`; `PRK = HMAC(salt, IKM)`; `CEK = Expand(PRK, "Content-Encoding: aes128gcm\0", 16)`; `NONCE = Expand(PRK, "Content-Encoding: nonce\0", 12)`. Use `ring::hkdf` (`Salt::new(HKDF_SHA256, …).extract(…).expand(&[info], Len(n))`, with a private `Len(usize)` implementing `KeyType`).
  - `encrypt_with`: header `salt ‖ 4096 as u32 big-endian ‖ 65 ‖ as_public` (86 bytes), then AES-128-GCM (`ring::aead`, `LessSafeKey`) over `plaintext ‖ 0x02`.
  - `encrypt`: `ring::rand::SystemRandom` fills the salt; `agreement::EphemeralPrivateKey::generate(&ECDH_P256, …)`; `agree_ephemeral` yields the secret.
  - `Vapid`: `signature::EcdsaKeyPair::generate_pkcs8(&ECDSA_P256_SHA256_FIXED_SIGNING, …)`; sign with `jsonwebtoken::encode(&Header::new(Algorithm::ES256), …, &EncodingKey::from_ec_der(pkcs8))`; `aud` from `reqwest::Url::parse(endpoint)?.origin().ascii_serialization()`.
- [ ] **Step 4: Run** the same command and clippy. Expected: all pass.
- [ ] **Step 5: Commit** `git add crates/agent/Cargo.toml Cargo.lock crates/agent/src/chat/{ece,push,mod}.rs` then `git commit -m "feat(push): RFC 8291 encryption and VAPID signing with ring"`.

### Task 16: Push state and routes

**Files:**
- Modify: `crates/agent/src/chat/push.rs` (store), `crates/agent/src/collect/fsio.rs`, `crates/agent/src/http/routes.rs`, `crates/agent/src/http/serve.rs` (`guarded_routes!` at 33-49)
- Test: `crates/agent/src/http/routes.rs`, `crates/agent/src/collect/fsio.rs`

**Interfaces:**
- Produces:
  - `fsio::write_atomic_private(file: &Path, contents: &str) -> io::Result<()>`: the temp file is created with mode 0600 (a stale one removed first), then renamed;
  - `push::Sub { pub endpoint: String, pub p256dh: String, pub auth: String, pub added: u64 }` (serde, `Clone`); the file is `{"vapid":<pkcs8 b64>,"subs":[Sub…]}` and every write holds a module `static LOCK: tokio::sync::Mutex<()>`;
  - `push::key(ctx: &Ctx) -> io::Result<String>`: the public key, making the key pair when the file is missing; an unreadable or unparsable file is an error and is never rewritten;
  - `push::add(ctx: &Ctx, sub: Sub) -> io::Result<()>` (upsert by endpoint), `push::remove(ctx: &Ctx, endpoint: &str) -> io::Result<()>`, `push::subscriptions(ctx: &Ctx) -> Vec<Sub>` (empty when missing or unreadable);
  - routes `get "/api/push/key" => get_push_key` (`{"key":…}`), `post "/api/push/subscribe" => post_push_subscribe` (body `{endpoint, keys:{p256dh, auth}}`, the shape of `PushSubscription.toJSON()`), `post "/api/push/unsubscribe" => post_push_unsubscribe` (`{endpoint}`); 200 `{"ok":true}`, 400 for a bad body, 500 `{"error":"Could not read cdash-push.json: …"}`.

- [ ] **Step 1: Write the failing tests** in `routes.rs` (`post(b, path, body) -> u16` is a two-line helper over `http_post`):

```rust
#[tokio::test]
async fn the_push_key_is_made_once_kept_private_and_names_no_one() {
    let dir = tempdir("push-key");
    let b = serve(cfg_for(dir.clone())).await.unwrap();
    let url = format!("http://{}/api/push/key", b.addr);
    let (k1, k2) = (reqwest_get(&url).await, reqwest_get(&url).await);
    assert_eq!(k1, k2);
    let key: Value = serde_json::from_str(&k1).unwrap();
    assert_eq!(crate::chat::ece::unb64(key["key"].as_str().unwrap()).unwrap().len(), 65);
    use std::os::unix::fs::PermissionsExt;
    assert_eq!(std::fs::metadata(dir.join("cdash-push.json")).unwrap().permissions().mode() & 0o777, 0o600);
    assert!(!std::fs::read_to_string(dir.join("cdash-push.json")).unwrap().contains('@'));
}
#[tokio::test]
async fn subscriptions_are_checked_kept_once_per_device_and_removed() {
    let b = serve(cfg_for(tempdir("push-subs"))).await.unwrap();
    let (p, a) = (crate::chat::ece::b64(&[4u8; 65]), crate::chat::ece::b64(&[1u8; 16]));
    let sub = |ep: &str, key: &str| format!(r#"{{"endpoint":"{ep}","keys":{{"p256dh":"{key}","auth":"{a}"}}}}"#);
    assert_eq!(post(&b, "/api/push/subscribe", &sub("http://push.example/x", &p)).await, 400, "push services are https");
    assert_eq!(post(&b, "/api/push/subscribe", &sub("https://push.example/x", "AAAA")).await, 400, "not a P-256 point");
    assert_eq!(post(&b, "/api/push/subscribe", &sub("https://push.example/x", &p)).await, 200);
    assert_eq!(post(&b, "/api/push/subscribe", &sub("https://push.example/x", &p)).await, 200);
    assert_eq!(crate::chat::push::subscriptions(&b.ctx).await.len(), 1);
    assert_eq!(post(&b, "/api/push/unsubscribe", r#"{"endpoint":"https://push.example/x"}"#).await, 200);
    assert!(crate::chat::push::subscriptions(&b.ctx).await.is_empty());
}
#[tokio::test]
async fn a_corrupt_push_file_is_reported_and_left_alone() {
    let dir = tempdir("push-corrupt");
    std::fs::write(dir.join("cdash-push.json"), "{not json").unwrap();
    let b = serve(cfg_for(dir.clone())).await.unwrap();
    let (status, body) = http_get(&format!("http://{}/api/push/key", b.addr)).await;
    assert_eq!(status, 500);
    assert!(body.contains("cdash-push.json"));
    assert_eq!(std::fs::read_to_string(dir.join("cdash-push.json")).unwrap(), "{not json", "a new key would orphan every device");
}
```

In `fsio.rs`, `write_atomic_private_leaves_a_0600_file` writes twice over an existing 0644 file and asserts the content and mode 0600.
- [ ] **Step 2: Run** `cargo test -p cdash-agent routes fsio`. Expected: failures (404, missing function).
- [ ] **Step 3: Implement.** Validation: the endpoint starts with `https://` and is at most 2,048 bytes; `p256dh` decodes to 65 bytes starting with `0x04`; `auth` decodes to 16 bytes. Handlers log nothing but failures.
- [ ] **Step 4: Run** `cargo test -p cdash-agent` (the auth suites pick the routes up from `GUARDED_PATHS`). Expected: all pass.
- [ ] **Step 5: Commit** `git commit -m "feat(push): subscription store and push routes"`.

### Task 17: The notifier and Web Push delivery

**Files:**
- Modify: `crates/agent/src/chat/push.rs` (`send`), `crates/agent/src/chat/notify.rs` (`spawn`, `ensure_hubs`), `crates/agent/src/http/serve.rs` (`serve()` at 313-333)

**Interfaces:**
- Consumes: `Note::payload`, `Vapid`, `ece::encrypt`, `push::{subscriptions, remove}`, `chat::hub_for`, `parse::tmux::{parse_tmux_panes, PANE_FORMAT}`.
- Produces:
  - `push::send(ctx: &Ctx, http: &reqwest::Client, note: &Note)`: one `POST` per subscription, in file order, with the headers of Global Constraints; 404 and 410 remove that subscription; other statuses and errors log `push failed <status> <host>`;
  - `notify::ensure_hubs(ctx: &Arc<Ctx>)`: `tmux list-panes -a -F PANE_FORMAT` through `Runner`, then `hub_for` for every pane that is not a bare terminal, ignoring refusals;
  - `notify::spawn(ctx: Arc<Ctx>, stop: tokio::sync::watch::Receiver<bool>)`: one task holding a `ctx.notes` receiver; every 5 s (first tick at once) it runs `ensure_hubs` when `!push::subscriptions(&ctx).await.is_empty() || ctx.notes.receiver_count() > 1`; each note goes to `push::send`; it ends when `stop` changes. `serve()` creates its stop channel first and calls `notify::spawn(Arc::clone(&ctx), stop_rx.clone())`; `router()` does not, so tests that build a router get no notifier.

- [ ] **Step 1: Write the failing tests.** Test helpers in `notify.rs`: `fake_push_service() -> Inbox` (an axum router on 127.0.0.1:0, `POST /push/{id}` recording the id, headers and body, answering 410 for id `gone` and 201 otherwise; `Inbox::take()` drains, `Inbox::wait(n)` waits up to 30 s for `n` requests then drains); `device() -> (EphemeralPrivateKey, Vec<u8>, [u8; 16])`; `sub(&inbox, id, key, auth) -> Sub`.

```rust
#[tokio::test]
async fn a_note_reaches_every_device_encrypted_and_a_gone_device_is_dropped() {
    let dir = tempfile_path("push-send");
    std::fs::create_dir_all(&dir).unwrap();
    let ctx = ctx_with_path(dir, String::new());
    let inbox = fake_push_service().await;
    let (ka, pa, aa) = device();
    let dead = Sub { endpoint: "http://127.0.0.1:1/push/dead".into(), p256dh: ece::b64(&pa), auth: ece::b64(&aa), added: 1 };
    push::add(&ctx, dead).await.unwrap();                                    // unreachable
    push::add(&ctx, sub(&inbox, "gone", &pa, &aa)).await.unwrap();
    push::add(&ctx, sub(&inbox, "a", &pa, &aa)).await.unwrap();
    push::send(&ctx, &reqwest::Client::new(), &Note { session: "cdash-a".into(), project: "proj".into(), kind: NoteKind::Ask, at: 7 }).await;
    let got = inbox.take();
    assert_eq!(got.iter().map(|r| r.id.as_str()).collect::<Vec<_>>(), ["gone", "a"], "a dead or gone device does not stop the next");
    let log = ctx.host.log.lines().join("\n");
    assert!(log.contains("push failed") && log.contains("127.0.0.1") && !log.contains("cdash-a"), "logs name the host only");
    let r = &got[1];
    assert_eq!((r.header("content-encoding"), r.header("ttl"), r.header("urgency")), ("aes128gcm", "86400", "high"));
    assert_eq!(r.header("topic").len(), 32);
    assert!(r.header("authorization").starts_with("vapid t="));
    assert_eq!(serde_json::from_slice::<Value>(&ece::decrypt(ka, &pa, &aa, &r.body)).unwrap(), json!({"s":"cdash-a","p":"proj","k":"ask","t":7}));
    assert_eq!(push::subscriptions(&ctx).await.len(), 2, "410 removed the gone one; an unreachable one stays");
}
#[tokio::test]
async fn a_live_turn_end_and_a_long_wait_reach_every_device_but_not_while_someone_looks() {
    // fixture_with("e2e", "") gives sessions/4242.json and a transcript whose history already holds a finished turn
    // (fixture lines 1..10); install_stub then replaces tmux with a case script: list-panes prints
    // 'cdash-a|4242|1785050000||/proj', display-message prints '4242|', everything exits 0.
    let (ctx, dir) = notifier_fixture("e2e");
    let transcript = dir.join("projects/-proj/sid-1.jsonl");
    let inbox = fake_push_service().await;
    let ((ka, pa, aa), (kb, pb, ab)) = (device(), device());
    push::add(&ctx, sub(&inbox, "a", &pa, &aa)).await.unwrap();
    push::add(&ctx, sub(&inbox, "b", &pb, &ab)).await.unwrap();
    let (_stop, stop) = tokio::sync::watch::channel(false);
    spawn(Arc::clone(&ctx), stop);
    eventually("the notifier opened a hub", || ctx.chats.try_lock().is_ok_and(|c| c.contains_key("cdash-a"))).await;
    tokio::time::sleep(Duration::from_secs(1)).await;
    assert!(inbox.take().is_empty(), "finished turns in history never notify");
    append(&transcript, &(fixture_lines(13..14) + &fixture_lines(9..10)));   // a prompt, then a turn end
    let done = inbox.wait(2).await;
    assert_eq!(serde_json::from_slice::<Value>(&ece::decrypt(ka, &pa, &aa, &done[0].body)).unwrap()["k"], "done");
    write_session(&dir, r#"{"sessionId":"sid-1","cwd":"/proj","status":"waiting","waitingFor":"permission prompt"}"#);
    let ask = inbox.wait(2).await;                                            // about 5 s later
    assert_eq!(serde_json::from_slice::<Value>(&ece::decrypt(kb, &pb, &ab, &ask[1].body)).unwrap()["k"], "ask");
    let mut looking = FocusGuard::new(&ctx, "cdash-a");
    looking.set(true);
    write_session(&dir, r#"{"sessionId":"sid-1","cwd":"/proj","status":"idle"}"#);
    append(&transcript, &(fixture_lines(13..14) + &fixture_lines(9..10)));
    tokio::time::sleep(Duration::from_secs(2)).await;
    assert!(inbox.take().is_empty(), "a focused chat: nothing is pushed");
}
```

- [ ] **Step 2: Run** `cargo test -p cdash-agent chat::notify`. Expected: compile errors.
- [ ] **Step 3: Implement.** `send` reads the subscriptions once, loads the key with `push::key`, encrypts `note.payload()` per subscription, and sets `Urgency: high` for ask and error, `normal` for done, and `Topic` from SHA-256 of the session. The task's `reqwest::Client` has a 10 s timeout and no redirects. `// ponytail: one push at a time; send concurrently if several slow devices make notes late.`
- [ ] **Step 4: Run** `cargo test -p cdash-agent` and clippy. Expected: all pass.
- [ ] **Step 5: Commit** `git commit -m "feat(push): notifier keeps hubs on running sessions and delivers Web Push"`.

### Task 18: The `/api/notify` stream

**Files:**
- Modify: `crates/agent/src/http/routes.rs` (`get_notify`), `crates/agent/src/http/serve.rs` (route), `crates/agent/tests/auth_integration.rs` (`under_none_no_route_is_rejected` at 172-189)
- Test: `crates/agent/src/chat/notify.rs`

**Interfaces:**
- Produces: `routes::get_notify(State(ctx)) -> Sse<impl Stream<Item = Result<Event, Infallible>>>` on `get "/api/notify"`: one `data:` line per note with `Note::payload()`, a comment every 25 s (`KeepAlive::new().interval(25 s)`), the stream built with `futures_util::stream::unfold` over `ctx.notes.subscribe()` (a lagged receiver skips ahead).

- [ ] **Step 1: Write the failing test** in `notify.rs`. It builds the router itself over a stub-tmux ctx (as `chat::ws` tests do): `serve()` would start the notifier on the real `PATH`, which lists the owner's real tmux panes.

```rust
#[tokio::test]
async fn the_stream_carries_each_note_as_one_data_line() {
    let (ctx, dir) = fixture_with("sse", "");
    let addr = serve_router(crate::http::serve::router(Arc::clone(&ctx), Arc::clone(&cfg_for(dir).auth), None, None)).await;
    let mut res = reqwest::get(format!("http://{addr}/api/notify")).await.unwrap();
    assert_eq!(res.headers()["content-type"], "text/event-stream");
    eventually("the stream subscribed", || ctx.notes.receiver_count() >= 1).await;
    for (s, k) in [("cdash-a", NoteKind::Done), ("cdash-b", NoteKind::Ask)] {
        ctx.notes.send(Note { session: s.into(), project: "proj".into(), kind: k, at: 1 }).unwrap();
    }
    let mut text = String::new();
    tokio::time::timeout(Duration::from_secs(30), async {
        while text.matches("data: ").count() < 2 { text.push_str(&String::from_utf8_lossy(&res.chunk().await.unwrap().unwrap())); }
    }).await.expect("two notes within 30 s");
    let notes: Vec<Value> = text.lines().filter_map(|l| l.strip_prefix("data: ")).map(|l| serde_json::from_str(l).unwrap()).collect();
    assert_eq!(notes, [json!({"s":"cdash-a","p":"proj","k":"done","t":1}), json!({"s":"cdash-b","p":"proj","k":"ask","t":1})]);
}
```

- [ ] **Step 2: Run** `cargo test -p cdash-agent chat::notify`. Expected: 404.
- [ ] **Step 3: Implement** the route. In `under_none_no_route_is_rejected`, read only the status (`common::send(…).await.status()`): reading the body of an endless stream never finishes. Every `/api/notify` stream also ends when the server's shutdown signal fires (select on it in the stream), so `Bound::stop` never waits on an open stream: the desktop app keeps one open to its own in-process agent. Add a test: start a server, open `/api/notify`, call `stop()`, and assert it returns within 2 s.
- [ ] **Step 4: Run** `cargo test --all --locked`. Expected: all pass.
- [ ] **Step 5: Commit** `git commit -m "feat(push): /api/notify event stream for the apps"`.

### Task 19: Service worker: push and taps

**Files:**
- Modify: `public/sw.js`
- Test: `test/sw.test.mjs`

**Interfaces:**
- Produces: `push` shows `registration.showNotification(p, { body, tag: s, data: { s, p }, icon: 'icon-192.png', renotify: true })`, or title `RC Dashboard` and body `Open the dashboard` (tag `cdash`) when the payload does not parse; `notificationclick` closes the notification, then focuses the first window client and posts `{ t: 'open-chat', s, p }` to it, or opens `/?chat=<s>&title=<p>`.

- [ ] **Step 1: Write the failing tests.** `worker()` runs `sw.js` in a vm context whose `self` has `registration.showNotification` (records), `clients.matchAll` (returns `w.clients`) and `clients.openWindow` (records); `pushed(w, text)` calls the `push` handler with `data` = `{ json: () => JSON.parse(text), text: () => text }` (or `null` for `undefined`) and returns its `waitUntil` promise.

```js
test('a push shows one notification per session, titled with the project', async () => {
  const w = worker();
  for (const [k, t] of [['done', 1], ['ask', 2]]) await pushed(w, JSON.stringify({ s: 'cdash-a', p: 'proj', k, t }));
  await pushed(w, JSON.stringify({ s: 'cdash-b', p: 'other', k: 'error', t: 3 }));
  assert.deepEqual(w.shown.map(n => [n.title, n.body, n.tag]),
    [['proj', 'Finished', 'cdash-a'], ['proj', 'Needs your answer', 'cdash-a'], ['other', 'Stopped with an error', 'cdash-b']]);
});
test('a push that cannot be read still shows a notification', async () => {
  const w = worker();
  await pushed(w, 'not json');
  await pushed(w, undefined);
  assert.deepEqual(w.shown.map(n => [n.title, n.body]), [['RC Dashboard', 'Open the dashboard'], ['RC Dashboard', 'Open the dashboard']]);
});
test('a tap asks an open dashboard to open the chat, or opens one that will', async () => {
  const w = worker();
  const note = { data: { s: 'cdash-a', p: 'proj' }, close() { this.closed = true; } };
  const tap = () => { let p; w.handlers.notificationclick({ notification: note, waitUntil: x => { p = x; } }); return p; };
  await tap();
  assert.deepEqual(w.opened, ['/?chat=cdash-a&title=proj']);
  const posted = [];
  w.clients.push({ focus: async () => {}, postMessage: m => posted.push(m) });
  await tap();
  assert.deepEqual(posted, [{ t: 'open-chat', s: 'cdash-a', p: 'proj' }]);
  assert.equal(note.closed, true);
});
```

- [ ] **Step 2: Run** `node --test test/sw.test.mjs`. Expected: the new tests fail.
- [ ] **Step 3: Implement** both handlers with the text table `{ done: 'Finished', error: 'Stopped with an error', ask: 'Needs your answer' }`; an unknown kind uses the fallback text. Reach the APIs through `self.registration` and `self.clients` (the test's context provides them on `self`), and encode both query values with `encodeURIComponent`.
- [ ] **Step 4: Run** `node --test`. Expected: all pass.
- [ ] **Step 5: Commit** `git commit -m "feat(push): service worker shows notes and opens their chat"`.

### Task 20: The switch, the subscription and taps in the page

**Files:**
- Modify: `public/index.html` (`.topbar-actions` at 40-44), `public/app.js` (new section after the Chat section, before `let modelDirTimer`; `listed` beside `lastRunning` at 363 and in `render` at 494; `tick()` after `render(data)`; `openChat`), `public/style.css` (switch state)
- Test: `test/frontend-guards.test.mjs`

**Interfaces:**
- Consumes: `api`, `invoke`, `isTauri`, `store`, `toast`, `openChat`, `lastRunning`; `/api/push/*` (Task 16); the worker's `open-chat` message (Task 19); app commands `notify_set`, `notify_clear`, `notify_take_open` (Tasks 21 and 22; until they exist the apps' calls fail into a toast).
- Produces: `<button id="notify-toggle" class="icon-btn" type="button" role="switch" aria-checked="false" aria-label="Notifications on this device">`; `ICONS.bell`, `ICONS.bellOff`; `let listed = false` beside `lastRunning`, set by `render()` (a session list has arrived; `polled` is set only at the end of `tick` and also after failures); in `app.js`, kept together before the line `/* notify wiring */` so a test can evaluate them alone: `notifyBlocked({ tauri, secure, push, permission }) -> string` (`''` when usable), `chatFromQuery(search: string) -> { s, p } | null`, `b64urlBytes(s: string) -> Uint8Array`, `syncPush(pm, key: string, post: (sub) => Promise) -> Promise<boolean>`, `let pendingOpen`, `noteOpen(s, p)`; `window.cdashOpenChat = noteOpen`.

- [ ] **Step 1: Write the failing tests.** `notifyHelpers(extra)` evaluates the slice from `function notifyBlocked(` to `/* notify wiring */` in a vm context holding `atob`, `URLSearchParams` (neither is a JavaScript built-in, so a new context lacks them), `listed: true`, `lastRunning: []`, an `openChat` and a `toast` that record into `opened` and `toasts`, plus `extra`.

```js
test('the switch says why it cannot work here', () => {
  const { notifyBlocked } = notifyHelpers();
  const env = o => ({ tauri: false, secure: true, push: true, permission: 'default', ...o });
  assert.equal(notifyBlocked(env({ secure: false })), 'needs HTTPS or 127.0.0.1');
  assert.equal(notifyBlocked(env({ push: false })), 'this browser cannot receive push notifications');
  assert.equal(notifyBlocked(env({ permission: 'denied' })), 'notifications are blocked for this site');
  assert.equal(notifyBlocked(env({})), '');
  assert.equal(notifyBlocked(env({ tauri: true, secure: false, push: false })), '', 'the apps use their own channel');
});
test('a tapped notification opens its chat, or says the session has ended', () => {
  const c = notifyHelpers({ lastRunning: [{ name: 'cdash-a', chat: true }] });
  assert.deepEqual(c.chatFromQuery('?chat=cdash-a&title=proj'), { s: 'cdash-a', p: 'proj' });
  assert.equal(c.chatFromQuery('?x=1'), null);
  c.noteOpen('cdash-a', 'proj');
  c.noteOpen('cdash-gone', 'old');
  assert.deepEqual(c.opened, [['cdash-a', 'proj']]);
  assert.deepEqual(c.toasts, ['That session has ended']);
});
test('a browser subscription follows the agent key and is registered again on each load', async () => {
  const { syncPush } = notifyHelpers();
  const posted = [];
  const sub = k => ({ options: { applicationServerKey: Uint8Array.from([k]).buffer }, toJSON: () => ({ endpoint: 'https://p/' + k }),
    unsubscribe: async () => { pm.current = null; return true; } });
  const pm = { current: null, getSubscription: async () => pm.current,
    subscribe: async o => (pm.current = sub(new Uint8Array(o.applicationServerKey)[0])) };
  assert.equal(await syncPush(pm, 'AQ', s => posted.push(s)), false, 'not subscribed: nothing to sync');
  pm.current = sub(1);
  assert.equal(await syncPush(pm, 'AQ', s => posted.push(s)), true);   // 'AQ' is [1]: registered again as it is
  assert.equal(await syncPush(pm, 'Ag', s => posted.push(s)), true);   // the agent's key is now [2]
  assert.deepEqual(posted.map(s => s.endpoint), ['https://p/1', 'https://p/2']);
});
```

Extend `chat scripts load before app.js and the chat markup exists` with `notify-toggle` in its id list.
- [ ] **Step 2: Run** `node --test test/frontend-guards.test.mjs`. Expected: the new tests fail.
- [ ] **Step 3: Implement.**
  - `syncPush`: no subscription gives `false`; a subscription whose `applicationServerKey` bytes differ from `key` is unsubscribed and replaced (`userVisibleOnly: true`); the subscription's `toJSON()` is posted either way; returns `true`.
  - `noteOpen`: until `listed` it stores `pendingOpen`; after that, a `lastRunning` entry with that name and `chat` opens it with `openChat(s, p || s)`, else it toasts `That session has ended`. `tick()` takes a stored `pendingOpen` and runs `noteOpen` on it right after `render(data)`. Under a full-screen chat the poll pauses, so a tap there uses the last list, which is at most one poll old.
  - Wiring, after `/* notify wiring */`: the switch shows `ICONS.bell` and `aria-checked="true"` when on, `ICONS.bellOff` otherwise, and is disabled with `title` = the reason from `notifyBlocked({ tauri: isTauri, secure: isSecureContext, push: 'PushManager' in window && 'serviceWorker' in navigator, permission: globalThis.Notification?.permission })`.
  - Browser on: `Notification.requestPermission()` must give `granted` (else toast and stay off); `GET /api/push/key`; `pushManager.subscribe`; `POST /api/push/subscribe`. Off: `unsubscribe()` and `POST /api/push/unsubscribe`. The state is "a subscription exists"; each load runs `syncPush` with the agent's key.
  - Apps: `invoke('notify_set', { on })`, remembered under the `store` key `cdash-notify`; each load re-sends `on: true` when remembered. On boot, `invoke('notify_take_open')` and `noteOpen` on a result. `window.cdashOpenChat = noteOpen`.
  - Taps: on load, `chatFromQuery(location.search)` is applied after `history.replaceState(null, '', location.pathname)`; the worker's `open-chat` message calls `noteOpen`.
  - `openChat` clears that session's notification: `navigator.serviceWorker.ready` then `getNotifications({ tag: name })`, closing each, in a browser; `invoke('notify_clear', { name })` in the apps; failures are ignored.
  - CSS: `#notify-toggle[aria-checked="true"]` uses `var(--accent)`.
- [ ] **Step 4: Run** `node --test`. Expected: all pass.
- [ ] **Step 5: Commit** `git commit -m "feat(push): notifications switch, Web Push subscription and taps"`.

### Task 21: Desktop app notifications

**Files:**
- Create: `crates/tauri-app/src/notify.rs`
- Modify: `crates/tauri-app/Cargo.toml`, `Cargo.lock`, `crates/tauri-app/src/lib.rs` (`agent_base` at 310-318, `run()` at 575-625)

**Interfaces:**
- Consumes: `/api/notify` (Task 18); `window.cdashOpenChat` (Task 20).
- Produces, in `notify.rs`:
  - `WireNote { s: String, p: String, k: String }` (`Deserialize`, `PartialEq`, `Debug`); `Lines::push(&mut self, chunk: &[u8]) -> Vec<WireNote>` (splits across chunks, accepts CRLF, skips comments and lines that do not parse);
  - `text_for(kind: &str) -> Option<&'static str>`; `FIRST_BACKOFF: Duration = 5 s`; `next_backoff(d: Duration) -> Duration` (doubles, at most 300 s); `note_id(session: &str) -> i32` (first 4 bytes of SHA-256, sign bit cleared);
  - `aumid_commands(on: bool, icon: &str) -> Vec<Vec<String>>` (`reg.exe` argument lists); `#[cfg(windows)] register_aumid(app: &AppHandle, on: bool) -> Result<(), String>`;
  - `NotifyState` (managed): the watcher's `JoinHandle`, and the shown notes by id plus the latest (session, project);
  - in `lib.rs`: `current_base(app, state) -> Result<String, String>` (today's `agent_base` body, which `agent_base` now calls); commands `notify_set(on: bool)` (async), `notify_clear(name: String)`, `notify_take_open() -> Option<serde_json::Value>` (desktop: always `None`).

- [ ] **Step 1: Write the failing tests** in `notify.rs`.

```rust
#[test]
fn stream_lines_become_notes_across_chunk_boundaries() {
    let mut l = Lines::default();
    let a = l.push(b"data: {\"s\":\"cdash-a\",\"p\":\"proj\",\"k\":\"done\",\"t\":1}\n\n: keep\n\ndata: {\"s\":\"cd");
    assert_eq!(a, [WireNote { s: "cdash-a".into(), p: "proj".into(), k: "done".into() }]);
    let b = l.push(b"ash-b\",\"p\":\"q\",\"k\":\"ask\",\"t\":2}\r\n\r\ndata: not json\n\n");
    assert_eq!(b.iter().map(|n| n.s.as_str()).collect::<Vec<_>>(), ["cdash-b"]);
}
#[test]
fn texts_backoff_and_ids() {
    assert_eq!(["done", "error", "ask", "later"].map(text_for), [Some("Finished"), Some("Stopped with an error"), Some("Needs your answer"), None]);
    assert_eq!((FIRST_BACKOFF, next_backoff(FIRST_BACKOFF), next_backoff(Duration::from_secs(200))),
        (Duration::from_secs(5), Duration::from_secs(10), Duration::from_secs(300)));
    assert_eq!(note_id("cdash-a"), note_id("cdash-a"));
    assert_ne!(note_id("cdash-a"), note_id("cdash-b"));
}
#[test]
fn windows_learns_the_app_id_when_on_and_forgets_it_when_off() {
    let s = |v: &[&str]| v.iter().map(|x| x.to_string()).collect::<Vec<_>>();
    let key = r"HKCU\Software\Classes\AppUserModelId\dev.cdash.app";
    let icon = r"C:\Users\o\AppData\Roaming\dev.cdash.app\notify-icon.png";
    assert_eq!(aumid_commands(true, icon), [s(&["add", key, "/v", "DisplayName", "/t", "REG_SZ", "/d", "RC Dashboard", "/f"]),
        s(&["add", key, "/v", "IconUri", "/t", "REG_SZ", "/d", icon, "/f"])]);
    assert_eq!(aumid_commands(false, ""), [s(&["delete", key, "/f"])]);
}
```

- [ ] **Step 2: Run** `cargo test -p cdash-tauri notify`. Expected: compile errors.
- [ ] **Step 3: Implement.**
  - `Cargo.toml`: `[target.'cfg(not(any(target_os = "android", target_os = "ios")))'.dependencies] tauri-plugin-notification = "2.6"`. Let Cargo update the lock: every release from 2.6.0 needs `tauri ^2.12`, so `tauri` and its family move to 2.12.x (Decisions 2). Check `cargo tree -i tauri` shows one `tauri`.
  - `run()` on desktop: `.plugin(tauri_plugin_notification::init())`, manage `NotifyState`, and in `setup` register `app.notification().on_action(…)`. A `tap` resolves its session from the reported notification's `extra["s"]`, else its id in `NotifyState`, else the latest note. Then on the `main` window: `unminimize`, `show`, `set_focus`, and `eval(&format!("window.cdashOpenChat&&window.cdashOpenChat({},{})", json!(s), json!(p)))`.
  - The watcher (`tauri::async_runtime::spawn`): its own `reqwest::Client` (no redirects, `connect_timeout` 3 s, `read_timeout` 60 s, no total timeout) reads `<base>/api/notify` with `chunk()`. Each note: record it, `remove_active(vec![id])` (replaces it on Linux; a no-op elsewhere), then show it with `.id(note_id(s)).title(p).body(text).extra("s", s).extra("p", p)`. After any end or error it sleeps the backoff, which resets to `FIRST_BACKOFF` after a 200.
  - `notify_set(true)`: on Windows `register_aumid(app, true)` first (writes `icons/icon.png`, through `include_bytes!`, to `app_data_dir()/notify-icon.png`, then runs the commands with `%SystemRoot%\System32\reg.exe` and `CREATE_NO_WINDOW`, under `#[allow(clippy::disallowed_types)] // reg.exe on the Windows client, which links no agent and so has no Runner`). Then `request_permission()`, abort any old watcher, and start one on `current_base`. `notify_set(false)`: abort the watcher; on Windows `register_aumid(app, false)`, ignoring "key not found".
  - `notify_clear(name)`: `remove_active(vec![note_id(&name)])`.
- [ ] **Step 4: Run** `cargo test -p cdash-tauri` and `cargo clippy --all-targets --locked -- -D warnings -D clippy::disallowed_types`. Expected: all pass. Cross-check Windows: `cargo xwin check -p cdash-tauri --target x86_64-pc-windows-msvc` with `. scripts/toolchain-env.sh` sourced.
- [ ] **Step 5: Commit** `git add crates/tauri-app/Cargo.toml Cargo.lock crates/tauri-app/src/{lib,notify}.rs` then `git commit -m "feat(desktop): show agent notes as system notifications"`.

### Task 22: Android background service

**Files:**
- Create: `crates/tauri-plugin-cdash-watch/{Cargo.toml,build.rs,src/lib.rs}`, `crates/tauri-plugin-cdash-watch/android/{build.gradle.kts,settings.gradle,consumer-rules.pro,.gitignore}`, `…/android/src/main/AndroidManifest.xml`, `…/android/src/main/java/dev/cdash/watch/{WatchPlugin,WatchService,Notes}.kt`, `…/android/src/test/java/dev/cdash/watch/NotesTest.kt`
- Modify: root `Cargo.toml` (`members`), `crates/tauri-app/Cargo.toml`, `Cargo.lock`, `crates/tauri-app/src/lib.rs` (Android branches of the three commands, the plugin in `run()`)

**Interfaces:**
- Consumes: `current_base`, `notify_set`, `notify_clear`, `notify_take_open` (Task 21); `window.cdashOpenChat` (Task 20).
- Produces:
  - Rust crate `tauri-plugin-cdash-watch` (`links = "tauri-plugin-cdash-watch"`, edition 2021; `tauri = "2"`, `serde_json`; build-dependency `tauri-plugin = { version = "2", features = ["build"] }`; `build.rs` = `tauri_plugin::Builder::new(&[]).android_path("android").build()`): `init<R: Runtime>() -> TauriPlugin<R>` (name `cdash-watch`, registering `dev.cdash.watch.WatchPlugin` on Android only); on Android, `WatchExt::watch(&self) -> &Watch<R>` with `start(&self, base: &str)`, `stop(&self)`, `clear(&self, session: &str)` (each `-> Result<(), String>`) and `take_open(&self) -> Result<Option<serde_json::Value>, String>`, through `run_mobile_plugin` with commands `start {base}`, `stop`, `clear {session}`, `takeOpen`. Nothing is exposed to JavaScript, so no capability file is needed.
  - Kotlin, package `dev.cdash.watch`. `Notes.kt`: `FIRST_DELAY_MS = 5_000L`, `nextDelay(ms: Long): Long` (doubles, at most 300 000), `dataOf(line: String): String?` (the text after `data:`, trimmed; `null` otherwise), `textFor(kind: String): String?`, `channelFor(kind: String): String` (`CHANNEL_REPLIES = "replies"` for ask and error, `CHANNEL_DONE = "done"` for done). `WatchPlugin` holds the commands. `WatchService` is the foreground service.

- [ ] **Step 1: Scaffold.** With `. scripts/toolchain-env.sh` sourced: `printf 'dev.cdash.watch\n' | script -q -c "cargo tauri plugin new cdash-watch --no-api --no-example --android --author 'RC Dashboard' --directory crates/tauri-plugin-cdash-watch" /dev/null`. Keep only the files listed above: delete the example command, `models.rs`, `commands.rs`, `desktop.rs`, `error.rs`, `permissions/` and `androidTest/`; set the namespace to `dev.cdash.watch`; keep `androidx.core:core-ktx` and `junit` and drop appcompat and material.
- [ ] **Step 2: Write the failing Kotlin test** `NotesTest.kt`.

```kotlin
class NotesTest {
    @Test fun backoffDoublesToFiveMinutes() {
        assertEquals(5_000L, FIRST_DELAY_MS); assertEquals(10_000L, nextDelay(5_000)); assertEquals(300_000L, nextDelay(200_000))
    }
    @Test fun onlyDataLinesCarryNotes() {
        assertEquals("{\"s\":\"cdash-a\"}", dataOf("data: {\"s\":\"cdash-a\"}")); assertNull(dataOf(": keep")); assertNull(dataOf(""))
    }
    @Test fun textsAndChannels() {
        assertEquals("Finished", textFor("done")); assertEquals("Stopped with an error", textFor("error"))
        assertEquals("Needs your answer", textFor("ask")); assertNull(textFor("later"))
        assertEquals(CHANNEL_REPLIES, channelFor("ask")); assertEquals(CHANNEL_REPLIES, channelFor("error")); assertEquals(CHANNEL_DONE, channelFor("done"))
    }
}
```

- [ ] **Step 3: Implement.**
  - `AndroidManifest.xml` (merged into the app): permissions `INTERNET`, `POST_NOTIFICATIONS`, `FOREGROUND_SERVICE`, `FOREGROUND_SERVICE_SPECIAL_USE`, `REQUEST_IGNORE_BATTERY_OPTIMIZATIONS`; `<service android:name=".WatchService" android:exported="false" android:foregroundServiceType="specialUse">` with `<property android:name="android.app.PROPERTY_SPECIAL_USE_FGS_SUBTYPE" android:value="Keeps one connection to the owner's own dashboard agent to show its notifications"/>`.
  - `WatchPlugin` (`@TauriPlugin`, with permission alias `notifications` for `POST_NOTIFICATIONS`):
    - `start {base}` asks for the notification permission on Android 13+ (`requestPermissionForAlias`). Unless already exempt (`PowerManager.isIgnoringBatteryOptimizations`), it shows an `AlertDialog` ("Keep watching while the phone sleeps", "Without this, Android cuts the connection when the phone sleeps and notifications stop.", "Continue") that opens `ACTION_REQUEST_IGNORE_BATTERY_OPTIMIZATIONS` for the package. It then saves `base` in SharedPreferences `cdash-watch`, calls `startForegroundService`, and resolves.
    - `stop` stops the service and clears the preference. `clear {session}` cancels tag `session`, id 1.
    - `load(webView)` keeps the WebView and records a pending open from `activity.intent` extras `cdash_chat` and `cdash_title`; `takeOpen` returns `{s, p}` once (else `{}`).
    - `onNewIntent` with those extras runs `window.cdashOpenChat&&window.cdashOpenChat(<JSONObject.quote(s)>,<JSONObject.quote(p)>)` on the WebView, on the UI thread.
  - `WatchService`:
    - `onStartCommand` takes `base` from the intent, or from the preference when Android restarts the service with a null intent.
    - It calls `startForeground(1, …)` with type `FOREGROUND_SERVICE_TYPE_SPECIAL_USE` on API 34+, on channel `background` ("Background", `IMPORTANCE_MIN`) with text `Watching for replies`. It returns `START_STICKY`.
    - One reader thread: `HttpURLConnection` to `<base>/api/notify` with `readTimeout` 60 s, reading lines; each `dataOf` line is parsed with `JSONObject` and posted on `channelFor(k)` (channels "Replies" `IMPORTANCE_HIGH` and "Finished turns" `IMPORTANCE_DEFAULT`) with tag `s`, id 1, title `p`, text `textFor(k)`, `setAutoCancel(true)`.
    - Its content intent is the package's launch intent with extras `cdash_chat`/`cdash_title`, `FLAG_IMMUTABLE`. It sleeps `nextDelay` between attempts, reset after an HTTP 200.
    - `onDestroy` stops the thread and disconnects. `// ponytail: no wake lock; incoming data wakes the phone — add a partial wake lock if notes arrive late with the screen off.`
  - `crates/tauri-app`: `[target.'cfg(target_os = "android")'.dependencies] tauri-plugin-cdash-watch = { path = "../tauri-plugin-cdash-watch" }`; `.plugin(tauri_plugin_cdash_watch::init())` on Android. The three commands' Android branches call `watch()` through `tauri::async_runtime::spawn_blocking` (the permission dialog must not block the main thread). Add the crate to the root `members`.
- [ ] **Step 4: Build and run what can run here.**

```bash
cargo test --all --locked && cargo clippy --all-targets --locked -- -D warnings -D clippy::disallowed_types
. scripts/toolchain-env.sh && cd crates/tauri-app && cargo tauri android init && cargo tauri android build --debug --apk --target aarch64
cd gen/android && ./gradlew :tauri-plugin-cdash-watch:testDebugUnitTest
```

Expected: the workspace passes; the debug APK builds (proof the Kotlin compiles and the manifests merge); `NotesTest` passes. The service itself is checked by the owner in Task 29.
- [ ] **Step 5: Commit**

```bash
git add Cargo.toml Cargo.lock crates/tauri-plugin-cdash-watch crates/tauri-app/Cargo.toml crates/tauri-app/src/lib.rs
git status --short   # no gen/, build/ or .tauri/ paths
git commit -m "feat(android): background service that shows agent notes"
```

---

## Phase C: performance

### Task 23: Measure the four shortcuts

**Files:**
- Create: `crates/agent/src/chat/perf.rs` (`#[cfg(test)] mod perf;` in `chat/mod.rs`), `test/perf/chat-frame.html`
- Modify: the spec's Performance table (`docs/superpowers/specs/2026-11-13-chat-push-reliability-design.md`)

**Interfaces:**
- Consumes: `claude::start`, `Hub::spawn`, `hub_for`, `notify::ensure_hubs`, `install_stub`.

- [ ] **Step 1: Write the measurements** as `#[ignore]` tests that print one line each.
  - `perf_memory_per_hub`: RSS (`VmRSS` in `/proc/self/status`) before and after hubs report Ready (`subscribe` returns). First one hub over a synthetic transcript of 10,000 turns × 5 events (prompt of 80 characters, a Bash `tool_use`, a 1,000-character tool result, 400 characters of text, `turn_duration`) = 50,000 events. Then hubs over the owner's 10 most recently modified `~/.claude/projects/*/*.jsonl`, read in place through symlinks under a temp `claude_dir` (`projects/-perf/<sid>.jsonl`, session files with `cwd` `/perf`). Prints the synthetic MB, the real total and the largest.
  - `perf_idle_cpu`: 10 Claude hubs on idle sessions (stub tmux, small transcripts); after 5 s of warm-up, the process's `utime + stime` (`/proc/self/stat`, divided by `sysconf(_SC_CLK_TCK)`) over 60 s, as a percentage of one core.
  - `perf_hub_for_latency`: on a private tmux server, 11 sessions `cdash-perf-0`…`10` running `sleep 600`, with session files under each pane pid; `hub_for` of session 0 first; then `ensure_hubs` for the rest while 50 `hub_for(cdash-perf-0)` calls run 10 ms apart. Prints the p95 in ms and kills that private server at the end.
  - `test/perf/chat-frame.html`: loads `../../public/style.css` and `../../public/chat/{md,store,view}.js`, copies the `#chat` markup from `index.html`, swaps `WebSocket` for a fake feeding `hello`, 2,000 `ev` frames of mixed rows and a `state`, then 200 more `state` frames 50 ms apart. It wraps `requestAnimationFrame` to time each callback and prints the median and p95 frame time on the page. All script inline (`node --test` would run any `.js` under `test/`).
- [ ] **Step 2: Run them**

```bash
env -u TMUX TMUX_TMPDIR=$(mktemp -d) cargo test -p cdash-agent --release chat::perf -- --ignored --nocapture --test-threads=1
python3 -m http.server 8765   # from the worktree root; open http://127.0.0.1:8765/test/perf/chat-frame.html in Chrome
```

In Chrome: DevTools, Performance, CPU 4× slowdown, reload, read the median.
- [ ] **Step 3: Record** the four numbers, the date and the machine in a new "Measured" column of the spec's table, and mark each row "fix" or "no fix" against its threshold.
- [ ] **Step 4: Commit** `git commit -m "perf(chat): measure the four shortcuts"`.

Tasks 24 to 27 each run **only if** their row in that table crossed its threshold. A row under its threshold means the task is skipped: write "skipped: <number> under <threshold>" in the pull request description and change nothing. A fix that runs finishes by re-running its measurement and recording the new number under the old one.

### Task 24: Keep the last 2,000 events per hub (only if memory is over 50 MB for one session or 200 MB in total)

**Run this task only if** Task 23 measured more than 50 MB for one session or more than 200 MB in total. Otherwise skip it.

**Files:**
- Modify: `crates/agent/src/chat/{hub,claude,native}.rs`, `docs/superpowers/plans/2026-11-13-chat-parts-2-5-shared.md` (the `Engine` table)

**Interfaces:**
- Produces: `hub::WINDOW: usize = 2000`; `Engine::history(&self, from: u64, to: u64) -> impl Future<Output = Result<Vec<Value>, String>> + Send` (default `Err("not supported")`): the engine's own events numbered `from..=to` since the last `Reset`. A contract row (owner Part 4).

- [ ] **Step 1: Write the failing tests.** In `hub.rs`, `rig_history()` uses a fake whose `history(from, to)` returns `{"type":"user","n":k}` for each `k`:

```rust
#[tokio::test]
async fn older_pages_come_from_the_source_and_a_position_older_than_the_window_starts_over() {
    let (hub, tx, _) = rig_history();
    for i in 0..2100 { tx.send(EngineMsg::Ev(prompt(&format!("u{i}"), "x"))).await.unwrap(); }
    settle().await;
    let page: Value = serde_json::from_str(&hub.command(ClientFrame::Page { before: 101, limit: Some(50) }).await.unwrap()).unwrap();
    assert_eq!((page["events"][0]["seq"].as_u64(), page["events"][0]["ev"]["n"].as_u64()), (Some(51), Some(51)), "re-read from the source");
    let epoch = serde_json::from_str::<Value>(&hub.subscribe(None, None).await.unwrap().backlog[0]).unwrap()["epoch"].as_str().unwrap().to_string();
    let old = hub.subscribe(Some(10), Some(epoch)).await.unwrap();
    assert_eq!(serde_json::from_str::<Value>(&old.backlog[0]).unwrap()["reset"], true);
}
```

Also: a hand-back `retract` the hub appended keeps its position in a later page. In `claude.rs`, `history(1, 3)` over the fixture equals the first three events the engine replayed. In `native.rs`, `history` opens a second connection with `{"after": from - 1, "epoch": <current>}`, returns the `ev`s up to `to`, and fails when the supervisor answers with another epoch.
- [ ] **Step 2: Run** `cargo test -p cdash-agent chat`; expect failures.
- [ ] **Step 3: Implement.**
  - The actor keeps positions `base+1..=head` in a `VecDeque` and pops the front beyond `WINDOW`. It also keeps `extra: Vec<(u64, Value)>`, the events it appended itself: engine number = position − (extras before it).
  - `page` serves the window from memory and awaits `history` for positions at or below `base`, merging `extra` back in. `// ponytail: an older page blocks this hub while the source is re-read; move it off the actor if it shows.`
  - Claude `history` replays the current conversation's transcript from the start through a fresh `Tail` and `Tap` (so Part 5's cuts apply again) and stops after `to`. Reading from an offset would bring back abandoned branches.
  - Add the contract row.
- [ ] **Step 4: Run** the agent suite and clippy, then re-measure (Task 23) and record.
- [ ] **Step 5: Commit** `git commit -m "perf(chat): keep 2,000 events per hub and page older ones from the source"`.

### Task 25: Claude engine idles (only if idle CPU is over 1% of one core)

**Run this task only if** Task 23 measured more than 1% of one core. Otherwise skip it.

**Files:**
- Modify: `crates/agent/src/chat/{hub,claude}.rs`, the contract's `Engine` table

**Interfaces:**
- Produces: `Engine::watchers(&self, n: usize) -> impl Future<Output = ()> + Send` (default no-op); the hub calls it on its tick when `out.receiver_count()` changed. A contract row (owner Part 4).

- [ ] **Step 1: Write the failing test** in `claude.rs`:

```rust
async fn next_ev(rx: &mut mpsc::Receiver<EngineMsg>) -> EngineMsg {
    next_of(rx, |m| matches!(m, EngineMsg::Ev(_))).await
}
#[tokio::test]
async fn an_idle_session_nobody_watches_is_read_through_its_status_file_only() {
    let (ctx, dir) = fixture_ctx("idle");
    write_session(&dir, r#"{"sessionId":"sid-1","cwd":"/proj","status":"idle"}"#);
    let (engine, mut rx) = start(ctx, "cdash-a".into(), 4242);
    collect_until_ready(&mut rx).await;
    let wait = |ms| Duration::from_millis(ms);
    append(&dir.join("projects/-proj/sid-1.jsonl"), &fixture_lines(13..14));
    assert!(tokio::time::timeout(wait(600), next_ev(&mut rx)).await.is_err(), "slow mode skips the transcript");
    write_session(&dir, r#"{"sessionId":"sid-1","cwd":"/proj","status":"busy"}"#);
    assert!(tokio::time::timeout(wait(1500), next_ev(&mut rx)).await.is_ok(), "busy: the prompt is read");
    append(&dir.join("projects/-proj/sid-1.jsonl"), &fixture_lines(9..10));    // the turn ends
    write_session(&dir, r#"{"sessionId":"sid-1","cwd":"/proj","status":"idle"}"#);
    next_ev(&mut rx).await;                                                   // its result: slow again
    engine.watchers(1).await;
    append(&dir.join("projects/-proj/sid-1.jsonl"), &fixture_lines(13..14));
    assert!(tokio::time::timeout(wait(300), next_ev(&mut rx)).await.is_ok(), "someone watches: polled as today");
}
```

- [ ] **Step 2: Run**; expect failure.
- [ ] **Step 3: Implement.** The engine keeps the count in an `Arc<AtomicUsize>` shared with its loop, beside a `tokio::sync::Notify` that `watchers` signals. Slow mode (`watchers == 0`, `status` `idle` or `shell`, and `!tap.turn_open()`) waits up to 1 s (or until notified) and reads only the session file. Anything else polls as today. `has-session` runs about every 2 s by elapsed time in both modes. `turn_open` keeps the fast poll until the turn's `turn_duration` arrives, so notes are not delayed.
- [ ] **Step 4: Run** the agent suite, re-measure and record.
- [ ] **Step 5: Commit** `git commit -m "perf(chat): Claude engine checks only the status file while nobody watches"`.

### Task 26: Row signatures in the view (only if the frame time is over 8 ms)

**Run this task only if** Task 23 measured more than 8 ms per frame. Otherwise skip it.

**Files:**
- Modify: `public/chat/store.js` (`rows`), `public/chat/view.js` (`render` at 276)
- Test: `test/chat-store.test.mjs`

- [ ] **Step 1: Write the failing test**

```js
test('every row carries a signature that changes when what the view shows changes', () => {
  let s = feed([hello(), ev(1, prompt('u1', 'hi')), ev(2, use('a', 't1', 'Bash', { command: 'ls' })), { t: 'delta', id: 'm9', text: 'Hel' }]);
  S.addMine(s, 'c1', 'mine');
  const sigs = st => S.rows(st).map(r => r.sig);
  const before = sigs(s);
  assert.deepEqual(sigs(s), before, 'stable for the same state');
  assert.ok(before.every(x => typeof x === 'string'));
  s = S.apply(s, { t: 'delta', id: 'm9', text: 'lo' });
  s = S.apply(s, ev(3, done('b', 't1', 'out')));
  s = S.apply(s, { t: 'ack', cid: 'c1', status: 'unconfirmed' });
  const after = sigs(s);                                   // rows: prompt, tools, streaming text, mine
  assert.equal(after[0], before[0], 'an unchanged row keeps its signature');
  for (const i of [1, 2, 3]) assert.notEqual(after[i], before[i], i);
});
```

- [ ] **Step 2: Run**; expect failure.
- [ ] **Step 3: Implement** `sig` from what `build` renders. User: status, error, text length. Text: text length, `streaming`. Tools: label, `running`, a `d`/`e`/`r` letter per call. Shell: command, stdout and stderr lengths. Note: its text. A row's text only grows under its key, so lengths suffice. `render` compares `r.sig` instead of `JSON.stringify(r)`.
- [ ] **Step 4: Run** `node --test`, re-measure in Chrome and record.
- [ ] **Step 5: Commit** `git commit -m "perf(chat): compare row signatures instead of serializing rows"`.

### Task 27: `hub_for` outside the lock (only if its p95 is over 100 ms)

**Run this task only if** Task 23 measured a p95 over 100 ms. Otherwise skip it.

**Files:**
- Modify: `crates/agent/src/collect/ctx.rs` (`chats`), `crates/agent/src/chat/mod.rs` (`hub_for`), the notifier test's `ctx.chats` check (Task 17)

**Interfaces:**
- Produces: `Ctx.chats: tokio::sync::Mutex<HashMap<String, Arc<tokio::sync::OnceCell<Hub>>>>`. Code outside `hub_for` must not touch `chats` directly; Parts 2, 3 and 5 go through `hub_for`.

- [ ] **Step 1: Write the failing test** in `chat/mod.rs`. Its tmux stub (`install_stub`) answers `display-message` for `=cdash-slow:` after `sleep 0.3` with `4243|`, for any other target with `4242|`, logs its arguments, and exits 0 for everything else; both pids have session files.

```rust
#[tokio::test]
async fn opening_one_chat_does_not_wait_for_another_hub_to_start() {
    let (ctx, dir) = slow_pane_fixture("lock");
    hub_for(&ctx, "cdash-fast").await.unwrap();
    let slow = tokio::spawn({ let ctx = Arc::clone(&ctx); async move { hub_for(&ctx, "cdash-slow").await.map(drop) } });
    tokio::time::sleep(Duration::from_millis(50)).await;     // cdash-slow is inside its tmux call now
    let started = std::time::Instant::now();
    hub_for(&ctx, "cdash-fast").await.unwrap();
    assert!(started.elapsed() < Duration::from_millis(50));
    slow.await.unwrap().unwrap();
    let (a, b) = tokio::join!(hub_for(&ctx, "cdash-new"), hub_for(&ctx, "cdash-new"));
    assert!(a.is_ok() && b.is_ok());
    let looked_up = std::fs::read_to_string(dir.join("tmux-args")).unwrap().lines().filter(|l| l.contains("display-message") && l.contains("=cdash-new:")).count();
    assert_eq!(looked_up, 1, "two first calls start one hub");
}
```

- [ ] **Step 2: Run**; expect failure.
- [ ] **Step 3: Implement:** under the lock, get or insert the name's cell (replacing one whose hub is closed); release the lock; `cell.get_or_try_init(|| start(ctx, name))`. A failed start leaves the cell empty for the next caller.
- [ ] **Step 4: Run** the agent suite, re-measure and record.
- [ ] **Step 5: Commit** `git commit -m "perf(chat): start hubs outside the global lock"`.

---

## Phase D: finish

### Task 28: Documentation and the final gates

**Files:**
- Modify: `README.md`, `CLAUDE_FINDINGS.md`

- [ ] **Step 1: Update the docs.** `README.md` gets a short "Notifications" section: the per-device switch; a browser needs HTTPS or 127.0.0.1; the Windows app registers its app ID while they are on; the Android app keeps a background service with a quiet permanent notification and asks to be exempt from battery optimisation; what notifies; push services see only an encrypted payload holding a session name, a project folder name, a kind and a time. `CLAUDE_FINDINGS.md` gets, each marked as observed on 2.1.323: `status: "shell"` is idle with background work; `waitingFor: "dialog open"` for the `/exit` confirmation and the rewind selector; `isApiErrorMessage` turns still end with `turn_duration`; the status file shows `idle` about 0.1 s after Esc.
- [ ] **Step 2: Run the gates five times under load** (Task 11, Step 3), then `cargo clippy --all-targets --locked -- -D warnings -D clippy::disallowed_types`. Expected: five passes and no warnings.
- [ ] **Step 3: Commit** `git commit -m "docs: notifications and the Claude status findings"`.

### Task 29: The owner's device checklist

This needs the owner, their machines and a build of this branch. Do it with them, and record each result in the pull request description. Real sessions run as Global Constraints say.

- [ ] **Chrome on Windows, the PWA at `http://127.0.0.1:23274`.** Switch on, allow. With the browser in the background, finish a turn and leave a permission prompt open for over 5 s: "Finished" and "Needs your answer" arrive, titled with the project folder, and tapping each opens its chat. With that chat (or its Terminal) open, focused and typed into, nothing arrives; after two idle minutes notes come again. A tap on a session that has since ended says "That session has ended".
- [ ] **The Windows desktop app, as the portable `.exe`.** Switch on: `reg query "HKCU\Software\Classes\AppUserModelId\dev.cdash.app"` shows `DisplayName` and `IconUri`. Both notifications arrive while the app runs, and a tap while the toast is on screen opens the chat. Note whether a second note for the same session replaced the first. Switch off: the key is gone. If no toast shows at all, stop and report: the spec's fallback (a per-user Start-menu shortcut with the same app ID) is not built.
- [ ] **The Android app** (APK from `scripts/release.sh`, the Termux agent on this build). Switch on: the notification permission prompt (Android 13+), the battery explanation and exemption, then a quiet "Watching for replies". Screen off for 10 minutes, then a permission prompt on a session: "Needs your answer" arrives, and tapping opens that chat. Switch off: the service and its notification are gone. Report late or missing notes (the next step is a wake lock).
- [ ] **Stop timing.** On a private tmux server with `--model haiku`: Stop during a tool, and a message held behind it is no longer refused after about 0.6 s, not 2.5 s.
- [ ] **No change with notifications off.** A device that never switched on sees no prompt, no service and no new notification.
