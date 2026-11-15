# Claude Remote Control findings

Recovered from Claude session `3737d133-bd46-4507-a0d2-dba9076a6fad` and checked
against installed Claude Code 2.1.296 on 2026-10-14. This describes observed
behavior, not a supported third-party integration contract.

## Recovered session

The prior session extracted the Remote Control environment client, work loop,
headless child launcher, and CCR v2 transport. It discovered the process wrapper
hook but did not demonstrate a replacement worker or implement a Codex bridge.

Its transcript remains in
`~/.claude/projects/-mnt-d-git-claude-dashboard/3737d133-bd46-4507-a0d2-dba9076a6fad.jsonl`.
Temporary extraction artifacts remain under
`/tmp/claude-1000/-mnt-d-git-claude-dashboard/3737d133-bd46-4507-a0d2-dba9076a6fad/scratchpad/`.

| Artifact | Purpose |
| --- | --- |
| `pretty/0480.js` | Environment registration and work API |
| `pretty/0481.js` | Polling, dispatch, resume pointers, shutdown |
| `pretty/0460.js` | Work-secret validation, session URL, worker registration |
| `pretty/0474.js` | Headless child arguments, environment, token refresh |
| `pretty/0461.js` | CCR v2 SSE, events, acknowledgements, worker state |
| `claude.strings` | Includes the self-invocation/process-wrapper resolver |

These temporary paths are evidence locations, not runtime dependencies. No
extracted Claude implementation is included in the dashboard.

## Observed transport

The authenticated runner registers an environment and polls for work. Its
versioned work secret carries a session ingress token and API base URL. Worker
registration obtains an epoch. The child receives its session URL, ID, ingress
credential, and epoch from that runner.

Incoming events arrive as SSE `client_event` frames with a payload, event ID,
and sequence number. The service can encode sequence numbers as strings;
convert them to numbers before comparing. Reconnect uses `from_sequence_num`.

The session's worker API accepts event batches, delivery acknowledgements,
heartbeats, and state updates. Event batches contain `{payload}` envelopes;
stable payload UUIDs allow upload retries to be deduplicated. Approval requests
also need pending-action metadata for the remote interface.

`CLAUDE_CODE_PROCESS_WRAPPER` accepts an absolute executable path or a JSON argv
array. Self-invocation inserts the original Claude executable after that prefix.
Remote Control's headless child receives `--sdk-url`, `--session-id`, stream JSON
arguments, `CLAUDE_CODE_SESSION_ACCESS_TOKEN`, and `CLAUDE_CODE_WORKER_EPOCH`.
Credential rotation arrives on stdin as `update_environment_variables`.

Standalone Remote Control can reuse a recently saved environment and session
for its directory even without an explicit `--continue`. Replacing its child
while starting a fresh Codex thread must therefore isolate the runner directory.

The bridge uses the installed runner's login and eligibility checks. It does
not replace authentication or change organization policy.

## Checks in this session

- A temporary wrapper was actually invoked by the authenticated runner and
  received the expected credential and epoch. Only their presence was logged.
- The Codex worker initialized a real app-server thread, registered worker
  state, published its initialization event, and obtained a Claude RC link.
- Two text turns returned the requested marker replies; Claude accepted their
  durable reply events.
- Launching two sessions through the dashboard API in one project produced
  distinct tmux panes, Codex thread IDs, and Claude RC links.
- Dashboard send reached Codex and produced `APP_MANAGED_CODEX_OK`.
- Dashboard kill and resume retained the same Codex thread ID.
- The offline process integration check exercises split SSE frames, numeric
  string sequences, persisted resume cursors, credential isolation/rotation,
  and refusal of unsupported requests.

The Claude mobile/web interface's exact rendering has not been manually checked.
Protocol acceptance and session registration were checked against the real
service. The bridge currently accepts text and completed assistant messages;
it does not expose the native Codex terminal interface.

## Dashboard chat tap (Claude Code 2.1.322)

Observed on Claude Code 2.1.322 on 2026-11-12 while designing dashboard chat
(`docs/superpowers/specs/2026-11-12-chat-core-design.md`). These are internal
formats and behaviours, not a supported contract; an update can change them.
Chat reads them in `crates/agent/src/chat/tap.rs` and `claude.rs`.

- **Transcript records.** `user` with string content is a prompt; with
  `tool_result` blocks it is a tool result; a text block starting
  `[Request interrupted by user` marks a stopped turn. `assistant` writes one
  line per content block (`thinking`, `text`, `tool_use`), several lines
  sharing one `message.id`. `system` with subtype `turn_duration` (carrying
  `durationMs`) ends a turn. `isSidechain: true` is subagent traffic and
  `isMeta: true` is injected text. `<bash-input>` and
  `<bash-stdout>`/`<bash-stderr>` user records are `!` shell commands.
  `promptSource` is `typed`, `queued` or `system`.
- **Pasted prompts (seen on 2.1.323).** A long bracketed paste, which is how
  chat sends text, is recorded as
  `\n\n<pasted_content id="N">\n…\n</pasted_content id="N">\n`; short ones are
  recorded as typed. The tap unwraps the blocks before matching and display.
- **Handed-back prompt.** Escape before any reply puts the prompt back in the
  input box. Its `user` record stays in the file with no marker, and the next
  prompt is written with the same `parentUuid`.
- **The TUI's own queue.** `queue-operation` records: `enqueue` carries the
  text, `dequeue` is followed by an ordinary `user` record, and `remove` with
  reason `absorbed_mid_turn` means Claude took the message mid-turn; then
  there is no `user` record, only an `attachment` of type `queued_command`
  (`commandMode: "prompt"`) holding the text.
- **Session state file.** `~/.claude/sessions/<pid>.json` carries `status`
  (`busy`, `idle`, `waiting`, `shell`), `statusUpdatedAt` and `waitingFor`
  (`dialog open`, `input needed`). Older versions write no `status`; chat then
  falls back to the transcript. How quickly `status` follows a turn end, a
  stop or a dialog was not timed.
- **Keys.** A bracketed paste (`tmux paste-buffer -p`) followed by Enter about
  0.4 s later is accepted as one prompt, multi-line included. Escape
  interrupts a running tool in about 0.3 s; with a message in the TUI's own
  queue, Escape stops the tool and then runs that message at once. Ctrl+U
  clears to the start of a row, about two presses per line of a multi-line
  input, and is harmless on an empty box.
- **Key table (read from the installed program, not tested).** `ctrl+l` is
  `chat:clearInput`; `ctrl+x enter` is `chat:queueSubmit`; `ctrl+enter` and
  `ctrl+x ctrl+s` are `chat:sendNow`.
- **Status file details (observed on 2.1.323).** `status: "shell"` is idle
  with background work still running. `waitingFor: "dialog open"` is also
  set for the `/exit` confirmation and the rewind selector. About 0.1 s
  after Esc during a running tool the status file shows `idle`; that turn
  writes no `turn_duration`.
- **API error turns (observed on 2.1.323).** In 29 of 30 observed turns, a turn
  whose last message has `isApiErrorMessage` still ends with a
  `turn_duration` record.
- **Image paste (observed on 2.1.323).** A bracketed paste of a line holding
  only an image's absolute path attaches it as an `[Image #N]` chip within
  about 32 ms; spaces in the path are fine. `N` counts up across the session.
  A path followed by text on the same line stays text. An Enter sent before
  the chip is drawn is dropped. The prompt record's content is then a block
  list: a text block (`[Image #N] <text>`) followed by
  `{type:"image", source:{type:"base64", media_type, data}}`, with
  `imagePasteIds`, and an `isMeta` record `[Image: source: <path>]` follows.
  Chat pastes each path alone and waits for exactly one new chip in the input
  box (`tmux_io::attach_image`), then pastes a space and the text.

## Dashboard chat rewind (Claude Code 2.1.325)

Observed on Claude Code 2.1.325 on 2026-11-15 (haiku, 160x50 pane on the
private `tmux -L cdash-test` server, cwd a git repo holding `f.txt`). Chat
drives this dialog in `crates/agent/src/chat/rewind.rs`; `Tap::listed()` in
`tap.rs` predicts the list. Prompts sent in order: `Reply with only: one`,
`!echo two`, `/rename spike-rewind`, `Append the line b to f.txt, then reply
with only: done` (the model appended with a shell command, so no tracked code
change), `Run sleep 3 with run_in_background, then reply with only: started`
(a background-completion turn followed), `Run sleep 8 in the foreground, then
reply ok` with `Reply with only: queued` sent while it ran, and later `Use the
Edit tool (not bash) to change the line a to c in f.txt, then reply with only:
edited`. Twice an Enter sent right after typed text was swallowed and a second
Enter submitted it.

- **Opening.** Esc, Esc 250 ms later, while idle. The cursor starts on
  `(current)`. The list scrolls: four rows show (`(current)` included), with
  `↑ N more` / `↓ N more` markers. On 2.1.323 all rows showed.

  ```
     Rewind

     Restore the code and/or conversation to the point before…

       ↑ 3 more
       Run sleep 3 with run_in_background, then reply with only: started
       No code changes

       Run sleep 8 in the foreground, then reply ok
       No code changes

       Reply with only: queued
       ⚠ No code restore

     ❯ (current)

     Enter to continue · Esc to cancel
  ```

  After eight presses of Up:

  ```
     Rewind
     Restore the code and/or conversation to the point before…
     ❯ Reply with only: one
       No code changes
       ! echo two
       No code changes
       Append the line b to f.txt, then reply with only: done
       No code changes
       Run sleep 3 with run_in_background, then reply with only: started
       No code changes
       ↓ 3 more
     Enter to continue · Esc to cancel
  ```

  Once the Edit prompt was on the branch, the bottom of the list read:

  ```
       ↑ 4 more
       Run sleep 8 in the foreground, then reply ok
       No code changes
       Reply with only: queued
       ⚠ No code restore
       Use the Edit tool (not bash) to change the line a to c in f.txt, then reply with only: edited
       f.txt +1 -1
     ❯ (current)
     Enter to continue · Esc to cancel
  ```

- **(a) What the selector lists.** A row is the prompt's first line, then
  `No code changes`, `⚠ No code restore` or `<file> +N -M`.
  - Plain prompts: listed, verbatim.
  - `!echo two`: listed as `! echo two` (a `<bash-input>` record).
  - `/rename spike-rewind`: not listed. It wrote only `system`
    `local_command` records and an `isMeta` reminder, no prompt record.
  - A prompt whose turn edited files: listed. With a shell append the row says
    `No code changes`; with the Edit tool, `f.txt +1 -1`.
  - Background-notification turns (`promptSource: "system"`,
    `<task-notification>`): not listed.
  - The message sent mid-turn: listed as its own row with `⚠ No code
    restore`. The transcript holds no `user` record for it, only the
    `attachment` of type `queued_command` (`commandMode: "prompt"`) that a
    message absorbed mid-turn writes.
- **(b) The confirm screen.** Enter on a row without code changes:

  ```
     Rewind
     Confirm you want to restore to the point before you sent this message:
     │ Append the line b to f.txt, then reply with only: done
     │ (2m ago)
     The conversation goes back to before this message.
     The code will be unchanged.
     ❯ 1. Restore conversation
       2. Summarize from here
       3. Summarize up to here
       4. Never mind
  ```

  Enter on the Edit-tool row:

  ```
     Rewind
     Confirm you want to restore to the point before you sent this message:
     │ Use the Edit tool (not bash) to change the line a to c in f.txt, then reply with only: edited
     │ (26s ago)
     The conversation goes back to before this message.
     The code will be restored +1 -1 in f.txt.
     ❯ 1. Restore code and conversation
       2. Restore conversation
       3. Restore code
       4. Summarize from here
     ↓ 5. Summarize up to here
     ⚠ Restored files go back whole, losing later changes by anyone. Bash changes to other files are not undone.
  ```

  The digits shift with code changes (`Restore conversation` is 1 without,
  2 with), and this list scrolls too (`6. Never mind` is below the fold).
  Chat therefore picks the option by its label: `Restore code and
  conversation` when files are wanted and it is offered, else `Restore
  conversation`. One Escape goes back from the confirm screen to the list; a
  second closes it.
- **(c) The digit alone confirms.** `1` with no Enter, on the Edit-tool row
  (`Restore code and conversation`): 300 ms later the dialog was gone, the
  Edit turn was removed and `f.txt` was restored. The digit of a plain
  `Restore conversation` was not pressed; it is the same menu. Chat sends no
  Enter after the digit.
- **After the rewind** (300 ms and 2 s later, the same): the conversation ends
  after the turn before the target, and the input box holds the rewound
  prompt verbatim, as on 2.1.323
  (`❯ Use the Edit tool (not bash) to change the line a to c in f.txt, then reply with only: edited`).
  Chat clears it with `tmux_io::clear_input`.
- **Later checks (2026-11-16, haiku, 80x24 pane).**
  - **Long and multi-line prompts.** The confirm screen quotes the whole
    prompt, wrapped to the pane, one `│` line each, then the age line:

    ```
       │ Reply with only the single word bravo, and do not add anything else at
       │ all to it, no punctuation, no quotes, nothing more
       │ (51s ago)
    ```

    Claude's code quotes the first 500 characters, at most four lines. The
    selector row still shows the first line cut with `…`. Chat compares the
    whole quote, whitespace aside (`rewind::quote_matches`).
  - **Slash commands.** The selector lists every `user` record that is not a
    tool result, `isMeta`, `isCompactSummary`, `isVisibleInTranscriptOnly` or
    command/shell output, so a slash command recorded as a prompt
    (`<command-name>/compact</command-name>`) is a row, shown as `/compact`.
    `/rename` and `/context` write `system` `local_command` records and are
    not.
  - **Compaction.** `/compact` writes `system`/`compact_boundary`
    (`parentUuid: null`, `logicalParentUuid`: the record before it), the
    `isCompactSummary` record, an `isMeta` caveat, the `/compact` record and
    its `<local-command-stdout>`. The Claude that compacted still lists the
    prompts before it, then `/compact`; a rewind there restores the
    uncompacted conversation (asked afterwards, it recalled a word given
    before the compaction). A Claude resumed after a compaction lists only
    the rows from the last compaction on (`/compact`, then later prompts).
  - **Dim text in the empty box.** A fresh session shows a placeholder and,
    after a turn, often a suggestion, both SGR 2 (`capture-pane -e`:
    `❯`, a no-break space, `ESC[2madd "two" to a.txtESC[0m`). Esc Esc still opens the
    selector over a suggestion. After a rewind to a slash command the box
    holds the command and its grey (`38;5;246`, not dim) argument hint:
    `❯ /compact  <optional custom summarization instructions>`.
