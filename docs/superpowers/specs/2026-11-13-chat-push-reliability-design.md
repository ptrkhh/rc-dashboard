# Dashboard-native chat, part 4: push notifications and reliability

Status: approved by the owner, 2026-11-13.
Code references are to `master` at `e3385c4` (Part 1 merged).

## Intent

Part 1 made chat work. The owner now wants to stop watching it: when a turn
finishes or a session needs an answer, the device in their pocket should say
so, whichever client they use (the PWA, the Android app, the desktop app).
This must not route message content through anyone else's servers.

The same part also clears what Part 1's review deferred, measures the timings
Part 1 guessed, fixes the performance shortcuts that turn out to matter, and
makes the flaky tests stop flaking.

## Program context

This is Part 4 of the program described in
`2026-11-12-chat-core-design.md`. The owner asked for Parts 2 to 5 in **one
pull request**, with a spec per part (2026-11-13). The sibling specs are:

- `2026-11-13-chat-prompts-menus-design.md` (Part 2)
- `2026-11-13-chat-attachments-design.md` (Part 3)
- `2026-11-13-chat-later-tier-design.md` (Part 5)

Part 4 depends on Part 2 only for the notification text of a "needs an
answer" event; everything else stands alone.

## Decisions made by the owner

- **Owner-only; no third-party relay for message content.** (Part 4 request.)
- **Android: a background service** in the app that keeps a connection to
  the agent, chosen over Termux:API notifications and over "PWA only". The
  owner accepted its costs: a permanent "connected" notification, some
  battery, and that the Kotlin code is built here but tested by the owner on
  a phone. (The option was worded "one connection to each saved host"; the
  app has no host list in practice, see "Author's calls" 11.)
- **Windows desktop app: it registers itself with Windows** when
  notifications are switched on (2026-11-13), chosen over building an
  installer and over leaving Windows to the PWA. The app ships as a portable
  `.exe`, and Tauri's notification plugin shows toasts only for an app ID
  Windows knows. The owner checks it on their machine.
- **Performance shortcuts: fix only those that measurably matter.**
- Everything else in this document is the author's call; see "Author's
  calls".

## Goals

1. A notification when a turn finishes on its own, or when a session has
   been waiting for an answer, on:
   - the PWA and any browser with Web Push (Chrome, Edge, Firefox, Safari
     16.4+ home-screen apps);
   - the desktop app, while it runs;
   - the Android app, in the background, through its own service.
2. Tapping a notification opens that session's chat.
3. No notification for a session that some device is showing right now.
4. The deferred Part 1 findings, plus two new ones found while preparing
   this part, each fixed with a test (list below).
5. Status timings measured on a real session, and `STOP_SETTLE` and the
   release rule set from them.
6. The four `ponytail:` performance shortcuts measured; the ones over their
   threshold fixed.
7. The 5 s test timeouts and the stub-tmux `ETXTBSY` race gone.

## Non-goals

- Message text, tool names or file names in a notification.
- Answering from the notification itself (Allow / Deny buttons on the
  notification). Tapping opens chat, where Part 2's card answers.
- Firebase Cloud Messaging, UnifiedPush, Termux:API, or any push path that
  needs a developer key or a third-party account.
- Notifications for sessions chat cannot open (external sessions, bare
  Terminal sessions, legacy Codex panes) and for resumable sessions that are
  not running.
- Starting the Android service at phone boot. The owner opens the app once.
- A notification history or inbox screen.

## Anti-goals

- Anything readable by a push service. Browser push services carry only an
  encrypted payload, and that payload holds no conversation content.
- The owner's e-mail address or any personal data in VAPID claims or headers.
- A notification storm: one notification per session at a time, replaced,
  never stacked.
- Changing anything for a device that leaves notifications off.

## User-visible behaviour

### Turning notifications on

The dashboard's menu gets **Notifications on this device**, a switch. It is
per device. Turning it on:

- **Browser or PWA:** asks the browser for notification permission,
  subscribes with the agent's public key, and registers the subscription with
  the agent. Turning it off unsubscribes and tells the agent. If the page is
  not a secure context (plain HTTP to a LAN address), the switch is disabled
  with the reason ("needs HTTPS or 127.0.0.1").
- **Desktop app:** asks the OS for permission where the OS wants it. The app
  then shows notifications while it runs. Closing the app stops them (on
  Linux and macOS the app hosts the agent, so nothing runs anyway).
- **Android app:** asks for the notification permission (Android 13+), then
  asks to be exempted from battery optimisation, explaining that without it
  Android cuts the connection when the phone sleeps. It then starts the
  background service, which shows a quiet permanent notification ("Watching
  for replies"). Turning the switch off stops the service.

### What notifies

| Event | When | Text |
|---|---|---|
| Finished | a turn ends on its own (not by a Stop or Esc), and no held message is about to be sent | title: project folder; body: "Finished" |
| Stopped with an error | a turn ends on an error (API, rate limit) | title: project folder; body: "Stopped with an error" |
| Needs an answer | the session has been waiting for 5 s and still is, on a question, permission or plan | title: project folder; body: "Needs your answer" |

- One notification per session at a time; a newer event for the same session
  replaces the older one.
- No notification is sent while some device has that session's chat **or
  Terminal** open and in front (window focused, tab visible), with input in
  the last 2 minutes. This is decided by the agent, so no push is sent at
  all. A desk window left focused while the owner walks away stops counting
  after 2 idle minutes.
- Dialogs Claude reports as `dialog open` (the `/exit` confirmation, the
  rewind selector, the model-switch confirmation) never notify. They are
  opened from Terminal or by chat itself.
- Opening a session's chat on a device clears that session's notification on
  that device.

### Tapping

Tapping opens the dashboard (or brings the app to the front) on that
session's chat. If the session has ended since, chat says so as usual.

## How it works

### Focus reporting

- The chat view and the Terminal overlay both send
  `{"t":"focus","on":true|false}` on their sockets:
  - **on** when the document is visible and focused and there was input
    (key, pointer, touch) in the last 2 minutes;
  - **off** when that stops being true, with a 2-minute idle timer in the
    client.
- Part 1's `ClientFrame` gains `Focus`, and the Terminal socket
  (`collect/term.rs`) learns the same message.
- The agent keeps one focus count per session, fed by both kinds of
  connection; a closed connection counts as off. The count lives outside the
  hub, because Terminal has no hub.

### The notifier

A task in the agent, running while at least one Web Push subscription exists
or one notification stream is open.

1. Every 5 s it lists the running dashboard sessions the cheapest way the
   collector already knows, and calls `hub_for` for each one chat can open
   (errors mean "no chat for this session" and are skipped). Hubs therefore
   exist for every running session, not only for those opened in chat.
2. Each hub publishes notes on a shared broadcast channel, `Note { session,
   project, kind }`:
   - `done` when a `result` event with subtype `success` arrives and the
     hub's queue holds no message it is about to release;
   - `error` when a turn ends on an error: subtype `error_during_execution`
     for Codex and OpenCode, and for Claude a turn whose last assistant
     record has `isApiErrorMessage: true`;
   - `ask` when `waiting` has been true for 5 s, once per waiting spell,
     unless Claude's `waitingFor` is `dialog open`. `EngineMsg::Status`
     gains `waiting_for` to carry it.
   The note is dropped while the session's focus count is above zero. A
   Stop from chat, or Esc in Terminal, ends the turn `interrupted`, which
   never notifies.
   Notes come only from live events: none before the hub's `Ready`, and
   none between an engine `Reset` and the `Ready` after it. A new hub
   replays the whole history, so without this rule every running session
   would notify "Finished" at each agent start.
3. The notifier sends each note to every Web Push subscription and every open
   notification stream.

### Web Push

- **Keys and subscriptions** live in `~/.claude/cdash-push.json`, mode 0600,
  next to the existing `cdash-pins.json`. The VAPID key pair (P-256) is
  created on first use. Subscriptions are `{endpoint, p256dh, auth, added}`.
- **Crypto** uses `ring`, which the build already compiles through
  `jsonwebtoken` (rustls here uses aws-lc-rs), added as a direct dependency
  at the locked version, 0.17.14:
  - payload encryption is RFC 8291 `aes128gcm` (ECDH P-256, HKDF-SHA256,
    AES-128-GCM, one record);
  - the VAPID JWT (RFC 8292) is ES256, signed with `jsonwebtoken`, already a
    direct dependency, from a P-256 key `ring` made; `sub` is the project's
    GitHub URL, never an e-mail address.
- **Request:** `POST` to the endpoint with `Content-Encoding: aes128gcm`,
  `TTL: 86400`, `Urgency: high` for `ask` and `normal` for `done`, and
  `Topic:` set to a hash of the session name, so a push still waiting at the
  push service is replaced rather than queued behind. RFC 8030 allows at
  most 32 base64url characters, so the hash is cut to that length.
- **Payload:** `{"s":"<session>","p":"<project>","k":"done|error|ask","t":<unix>}`.
- **Failures:** 404 or 410 removes the subscription. Other failures are
  logged and dropped.
- **Routes,** behind the existing auth: `GET /api/push/key`,
  `POST /api/push/subscribe`, `POST /api/push/unsubscribe`.
- **Service worker** (`public/sw.js`): `push` always shows a notification
  (tag = session, so it replaces); `notificationclick` focuses an open
  dashboard window and tells it to open that chat, or opens a new window at
  a URL that opens it. Opening a chat tells the worker to close that tag's
  notification.

### Notification stream for the apps

`GET /api/notify` is a `text/event-stream`, behind the existing auth. Each
note is one `data:` line with the same JSON as the push payload. A comment
line every 25 s keeps idle connections open.

### Desktop app

- Adds `tauri-plugin-notification`.
- A Rust task reads `/api/notify` from the agent the app uses (`agent_base`:
  the in-process agent on Linux and macOS, the WSL agent on Windows) with
  reqwest, reconnecting with backoff, and shows each note through the
  plugin.
- Clicking a notification brings the window forward and asks the web view
  to open that chat. The plugin reports clicks from 2.6.0. On Windows a
  click counts only while the toast is on screen, not from the Action
  Center.
- **Windows:**
  - Switching notifications on writes the per-user key
    `HKCU\Software\Classes\AppUserModelId\dev.cdash.app`, with
    `DisplayName` "RC Dashboard" and an `IconUri` pointing at an icon the
    app writes next to its settings. Switching off removes the key.
  - Nothing else is written, and no admin rights are needed.
  - If toasts still do not show on the owner's machine, the fallback is a
    per-user Start-menu shortcut carrying the same app ID. That is the
    other way Windows associates an ID with an app that was not installed.

### Android app

- A Kotlin foreground service in `crates/tauri-app/gen/android`, started and
  stopped from the web view through a small Tauri mobile plugin (or a
  JavaScript interface; the builder's choice).
- At start it receives the agent's base URL from the web view (the app's
  `agent_base`, normally the Termux agent on 127.0.0.1). It holds
  `GET /api/notify` open with `HttpURLConnection`, reconnecting with backoff
  (5 s, doubling to 5 min).
- It posts each note on a "Replies" channel (high importance for `ask` and
  `error`, default for `done`), tagged with the session. Tapping opens
  `MainActivity` with the session as an extra; the web view opens that
  chat.
- Its own permanent notification is on a "Background" channel at minimum
  importance.
- `START_STICKY`. Foreground service type `specialUse`.

### Deferred Part 1 findings

Each gets a regression test that fails before the fix.

1. **Older history never loads when the first screen does not scroll.**
   After the first render, and after each page arrives, if the list is not
   scrollable and more history exists, request the next page.
2. **Edit and Send again lose the text offline.** Check the socket first; if
   it is not open, keep the row and say "Offline".
3. **A reset leaves pending messages stuck as "sent".** On `Reset`, ack each
   pending message `unconfirmed`, as `CONFIRM_AFTER` would have.
4. **Codex/OpenCode-confirmed pending entries that never match stay pending.**
   An entry the tool confirmed is dropped at the next `result` event, matched
   or not.
5. **Ready can fire mid-history** when a 4 MB read ends inside one huge line.
   `Tail::poll` reports whether it reached the end of the file; `Ready` needs
   an empty poll at the end of the file.
6. **A client that was offline when the session ended keeps its held rows.**
   The client drops held rows that are not in the latest `state.held`.
7. **No test for OpenCode's interrupted-subtype conversion.** Add one for
   `ChatBridge` in `opencode-remote.mjs`.
8. **New: Codex subagent events leak into chat.** Codex subscribes every
   client to a subagent's thread, and `codex-remote.mjs` never checks
   `params.threadId` (around lines 68-69 and 538-548). A subagent's turn can
   flip the working state, replace the turn ID used for steer and interrupt,
   and show its messages as the main conversation's. Only the main thread's
   notifications reach the conversation, the working state and the turn ID.
   Child threads are routed, not dropped: their `thread/status/changed`
   feeds Part 5's running list, and their questions still become cards
   (Part 2).
9. **New: Claude's `shell` status blocks chat.** See "Status timing" below.
   `claude.rs` maps `shell` to working; map it to idle.

The two Part 1 findings about the tap (images, rewind) are fixed in Parts 3
and 5.

### Status timing

Measured on 2026-11-13 on Claude Code 2.1.323, with the status file polled
every 50 ms over 10 turns and every dialog type:

| Event | Status file | Transcript |
|---|---|---|
| Enter on a prompt | `busy` after 19–58 ms | the prompt's line is readable after 199–322 ms |
| A turn ends | `idle` 17–29 ms after the `turn_duration` timestamp | that line is readable 75–173 ms after its timestamp; the status file changed first in 10 of 10 turns |
| Esc during a tool | `idle` after 111 ms | the rejected result and "[Request interrupted…]" after 98 ms; no `turn_duration` |
| Question, permission or plan dialog | `waiting` 8–10 ms before the dialog is drawn; `waitingFor` is `input needed` or `permission prompt` | — |
| `/exit` confirmation, rewind selector | `waiting` with `waitingFor: "dialog open"`, 46 ms after Enter | — |
| A turn ends with a background shell still running | **`shell`**, not `idle` | — |

What follows from them:

1. **`shell` means idle with background work.** The footer reads "· 1
   shell". Part 1 counts `shell` as working, so every chat message is held,
   and none is released, until the background shell exits; for a dev server
   that can be hours. `shell` now counts as idle. This is new finding 9.
2. **The `/exit` confirmation is visible after all.** It shows as `waiting`
   with `waitingFor: "dialog open"`, so Part 1's known gap closes: chat
   refuses to type while it is open. Part 2 uses `waitingFor` to tell a
   dialog chat can answer from one it cannot.
3. **Stop settles sooner.** The status file shows `idle` about 0.1 s after
   Esc, and the records follow within about 0.3 s. Part 1 waits a fixed
   2.5 s (`STOP_SETTLE`), sized from one 2.2 s outlier. The new rule: Stop
   has settled once the status file shows `idle` and the transcript has not
   grown for 500 ms, with 2.5 s as the ceiling. The typical wait drops from
   2.5 s to about 0.6 s.
4. **The release rule stays as it is.** It already waits for the
   `turn_duration` record (through `turn_ok`), which arrives about 0.1 s
   after the status file says `idle`.

### Performance

Measure first, on the owner's real sessions and on a synthetic 50,000-event
transcript. Fix only what crosses its threshold.

| Shortcut | Measure | Fix when | Measured (2026-11-14, WSL2 on this workstation) |
|---|---|---|---|
| Whole conversation in memory (`log_events`) | resident bytes per hub, with hubs on every running session (the notifier makes that the normal case) | over 50 MB for one session or 200 MB in total | **fix**: 121-132 MB for one hub over a synthetic 50,000-event transcript (limit 50 MB). The owner's 10 most recent transcripts (57.7 MB of JSONL): 47-51 MB in total, largest 13-23 MB, under both limits. Method: `perf_memory_per_hub`, `VmRSS` before and after each hub's `subscribe` returns, release build, 3 runs<br>**After Task 24** (2,000 events per hub, older pages re-read from the source): 41.7-49.4 MB for the synthetic hub (limit 50 MB); the owner's 10 most recent transcripts 13.3-20.5 MB in total, largest 6.1-10.3 MB. With `malloc_trim(0)` before the second reading (one-off check, not committed) the synthetic hub reads 23-30 MB: about 15 MB is memory the allocator kept after the replay, not events. Same method, 3 runs |
| Claude engine polls for the pane's whole life | CPU of the agent with 10 idle Claude panes | over 1% of one core | **fix**: 4.5-5.3% of one core (limit 1%), of which the `has-session` spawns are 0.84% (`perf_idle_cpu_spawns_only`: the same 10 liveness loops alone, 60 s) and the transcript and session-file polling about 3.6-4.5%. Polling dominates, so Task 25's fix must park the poll loop; moving the liveness check alone would not get under 1%. Method: `perf_idle_cpu`, 10 Claude hubs on idle sessions, stub tmux, process `utime + stime` over 60 s after 5 s warm-up; includes forking the stub `has-session` every 13th tick<br>**After Task 25** (the transcript is read only while a client is attached, the session is not idle, or a turn is open; otherwise the status file every 1 s, `has-session` every 2 s): 1.52-1.92% of one core, still over the 1% limit. One-off checks, not committed: status file every 10 s instead of 1 s, 0.97%; `has-session` every 10 s while parked, 1.37-1.45%; a synchronous status read instead of `tokio::fs`, 1.42-1.57%. What is left is the 1 Hz status reads, the 2 s liveness spawns (0.84% alone) and the hubs' own 250 ms ticks. Same method, 3 runs |
| View runs `JSON.stringify` on every row each frame | frame time with 2,000 rows, Chrome with 4× CPU throttling | over 8 ms per frame | **fix**: median 17.5 ms, p95 20.5 ms at 4x CPU throttling (limit 8 ms); at 1x median 9.6 ms, p95 15.3 ms. Method: `test/perf/chat-frame.html` in headless Chrome (Playwright `chrome-headless-shell`) over the DevTools protocol with `Emulation.setCPUThrottlingRate 4`, 2,000 rows (2,667 `ev` frames), 200 `state` frames 50 ms apart, `render` callback timed |

After Task 26 (row signatures instead of `JSON.stringify`), same harness, 4x throttling, two runs: median 12.2 and 11.8 ms, p95 15.3 and 14.8 ms (was 17.5 / 20.5). Still over the 8 ms limit: the remaining cost is `rows()` rebuilding all 2,000 rows from the events on every frame.
| `hub_for` holds the global lock across tmux calls | added latency of opening chat while the notifier creates hubs for 10 sessions | p95 over 100 ms | **no fix**: p95 40-50 ms (limit 100 ms; max 42-119 ms). Method: `perf_hub_for_latency`, private tmux server, 11 `sleep 600` sessions, 50 `hub_for` calls 10 ms apart while `ensure_hubs` creates the rest, 2 runs |

The fixes, if needed:

- keep the last 2,000 events per hub, and serve older pages by re-reading the
  source. For Claude that means replaying the transcript from its start
  through a fresh tap, so Part 5's cuts apply again; reading from an offset
  would bring back abandoned branches;
- the Claude engine polls the transcript only while a client is attached or
  the session is busy, and otherwise checks the status file every second;
- the view keeps a per-row signature from the store instead of serialising;
- `hub_for` reserves the name under the lock, does the tmux calls outside
  it, then fills the slot.

### Test robustness

- The 5 s timeouts in `chat::claude` and `chat::ws` tests become 30 s. They
  are ceilings for a failing test, not expectations of a passing one.
- Every test that writes a stub `tmux` uses Part 1's helper, which has a
  child process write it and rename it into place, so no thread of the test
  process ever holds a write handle to a file another thread executes.

## Security

- The push routes and `/api/notify` sit behind the existing auth. The
  desktop and Android apps reach only the hosts they already reach.
- The subscription file and VAPID private key are 0600 and never leave the
  host.
- The push payload holds a session name, a project folder name, an event kind
  and a time.
- The Android service connects only to the app's `agent_base` (see
  "Author's calls" 11).

## Author's calls

1. **Hubs for every running session** while notifications are on, rather
   than a second, lighter watcher. The hub already knows "finished on its
   own" and "waiting", for all three tools. The cost is measured in the
   performance table.
2. **Suppression is decided by the agent** from focus frames, so a push that
   would be shown on top of the open chat is never sent. The service worker
   therefore always shows what it receives, which keeps clear of browsers'
   silent-push penalties.
3. **5 s before "Needs your answer".** Claude's own permission notification
   waits about 6 s; an answer given quickly in Terminal should not buzz the
   phone.
4. **No minimum turn length for "Finished".** A short turn sent from the
   phone before locking it is exactly the case to notify.
5. **`ring` directly, not the `web-push` crate:** no new crate to build, and
   RFC 8291 has a test vector to pin it. The JWT goes through
   `jsonwebtoken`, which the agent already uses.
6. **State in `~/.claude/cdash-push.json`,** following `cdash-pins.json`.
7. **One switch per device** for all three client kinds.
8. **The desktop app does not keep running in the tray** when its window is
   closed.
9. **`specialUse` foreground service type:** `dataSync` is limited to six
   hours a day on Android 15.
10. **The Codex subagent leak is fixed here** because it corrupts chat state
   (working, turn ID), which is reliability work.
11. **The apps watch the one agent they already use.** `public/app.js` never
    calls the Tauri profile commands, and those accept only unauthenticated
    profiles, so no app has a host list today. Watching "each saved host"
    would first need host management and credentials (the unbuilt step 10
    of the multi-host design). The service and the desktop task follow the
    app's `agent_base`.
12. **Focus lapses after 2 minutes without input.** The main case is "send
    from the desk, walk away"; a desk window left focused must not silence
    the phone.
13. **Terminal counts as looking at the session,** and Claude's `dialog
    open` never notifies. The owner uses Terminal often, and those dialogs
    are opened from Terminal or by chat itself.
14. **Errors notify; "Finished" waits for the queue.** A turn that dies on a
    rate limit while the owner is away is exactly what they need to hear
    about. A queue of held messages would otherwise say "Finished" once per
    message while the session is busy again.

## Left to the builder

- Where the switch sits in the menu, and exact strings.
- Backoff constants, within the ranges above.
- Tauri mobile plugin versus JavaScript interface for the Android bridge.
- The URL form that opens a chat on load (add one if none exists).

## Acceptance checks

- The RFC 8291 Appendix A test vector passes in a unit test. `ring` cannot
  load a fixed ECDH private key, so the test feeds the vector's
  `ecdh_secret` to the steps after key agreement, and a round trip covers
  the agreement itself.
- Creating hubs for running sessions that already have finished turns sends
  no note.
- A unit test pins the VAPID JWT header, claims and signature verification.
- With a fake push endpoint on 127.0.0.1: one `done` and one `ask` arrive,
  decrypt to the payload above, and none arrives while a focused chat is
  connected.
- `/api/notify` streams the same notes; a test reads two.
- Each deferred finding has a test that fails before its fix.
- Real devices, checked by the owner:
  - Chrome on Windows, PWA at `http://127.0.0.1:23274`: both notifications
    arrive with the browser in the background; tapping opens the chat.
  - The Windows desktop app, run as the portable `.exe`: after switching
    notifications on, both arrive while it runs. After switching off, the
    registry key is gone.
  - The Android app with the service on, screen off for 10 minutes: an `ask`
    arrives; tapping opens the chat.
- The performance table filled in with measured numbers, and any fix shown
  to bring its number under the threshold.
- `cargo test` and the frontend tests pass five times in a row under
  `stress -c $(nproc)` without a timeout failure.

## Evidence

Collected on 2026-11-13 for Parts 2 to 5, in `2026-11-13-chat-evidence/`:

- `claude-2.1.323-live-spike.md`: live tests on Claude Code 2.1.323 in a
  private tmux server. Covers the permission hook (questions, plans,
  permissions, answering in the TUI first), image paste, status-file
  timing, the rewind dialog, the headless command list, side questions and
  background-task records.
- `claude-2.1.323-static-notes.md`: what the docs and the shipped CLI say
  about hooks, settings, commands, images, rewind and usage.
- `codex-opencode-source-notes.md`: Codex 0.167 app-server and OpenCode 2.2.6
  source and schema research (approvals, questions, settings, images,
  rewind, usage, diffs, side chat, background work), with file and line
  references.
- `review-checks.md`: checks run during the review (headless `initialize`
  with hooks off, image paths with spaces, branch cuts on real transcripts,
  clean filters, context sizes, API-error turns, build facts).
