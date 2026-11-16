# Dashboard-native chat, part 5: rewind and the later tier

Status: approved by the owner, 2026-11-13.
Code references are to `master` at `e3385c4` (Part 1 merged).

## Intent

With Parts 1 to 4, chat can hold a whole working session. Part 5 adds the
things the owner reaches for during review and course correction: going back
to an earlier prompt, seeing what changed on disk, knowing how much context
and plan allowance is left, asking a quick question without derailing the
session, and seeing what is still running in the background. It also fixes
a Part 1 bug: after a rewind in Claude's TUI, chat can hide an answered
prompt while its reply stays visible.

## Program context

Part 5 of the program in `2026-11-12-chat-core-design.md`, shipped in one
pull request with Parts 2 to 4 (owner decision, 2026-11-13). Sibling specs:
`2026-11-13-chat-prompts-menus-design.md`,
`2026-11-13-chat-attachments-design.md`,
`2026-11-13-chat-push-reliability-design.md`.

## Decisions made by the owner

- **Scope (2026-11-13).** In: rewind, diff sheet, message polish (copy,
  syntax colours, thinking, checklist, timestamps, drafts), usage and context
  sheet, a list of running shells and subagents, side chat. The running list
  is a list only: "No need for subagent transcripts, just list of running
  shells/agents is fine". Out: file viewer, fork, share links, feedback,
  cloud PR and CI screens, live port preview, dictation, prompt suggestions.
- **Rewind can be started from chat in all three tools**, Claude included,
  where chat drives Claude's own rewind dialog with key presses checked
  against the screen. The owner accepted that this breaks, safely, when
  Claude changes that dialog.
- **Rewinds done in any door show correctly in chat:** Claude's `parentUuid`
  chain, Codex's double-Esc, OpenCode's revert and undo.
- **Claude side questions run headless and ask first when they are big.**
  A side question re-sends the whole conversation (the test saw no cache
  hit), so above 100k tokens chat shows the size and asks for confirmation.
  Hooks and MCP servers are off for that run. Chosen over driving `/btw` in
  Claude's TUI and over no side chat on Claude (2026-11-13).

## Goals

1. Chat always shows the conversation the tool will continue from, after a
   rewind in any door.
2. "Rewind to here" on a past prompt, for all three tools, with the prompt
   returned to the composer and, where the tool can, files restored.
3. A diff sheet with the session folder's uncommitted changes, and inline
   diffs on edit rows.
4. Copy, syntax colours, thinking on tap, a checklist, timestamps and saved
   drafts.
5. A usage sheet: context fill, session tokens, cost where the tool reports
   it, and plan limits.
6. A running list of background shells and subagents.
7. Side chat: a question about the session that is answered aside and never
   enters the conversation.

## Non-goals

- Subagent transcripts, and stopping a background task from the list.
- A per-turn diff history, staging or committing from the diff sheet.
- Multi-turn side conversations. Each side question stands alone.
- Cost for Claude and Codex sessions, which their records do not carry.
- A checklist for OpenCode (its `todowrite` tool is off in release builds).
- Rewinding from a prompt that is still running, or to a Codex prompt that
  steered a turn (Codex refuses both).

## Anti-goals

- Key presses into a Claude dialog that chat has not confirmed on screen.
- A false rewind: chat hiding part of a conversation that the tool still
  continues from.
- A side question changing files or the session's history.

## User-visible behaviour

### Rewind

- Every prompt bubble on the current branch has **Rewind to here** in its
  menu, while the session is idle. A sheet explains what happens and, where
  the tool supports it, offers **Also restore files**:

  | Tool | Conversation | Files |
  |---|---|---|
  | Claude Code | back to before the prompt | optional, when Claude has checkpoints for that prompt |
  | Codex | back to before the prompt | never restored (Codex leaves files alone) |
  | OpenCode | back to before the prompt | optional |

- Afterwards the prompt's text is in the composer, ready to edit and send,
  as each TUI does. Attachments are not put back. When the prompt had any,
  the composer says "This prompt had N attachments; attach them again".
- OpenCode keeps a rewind reversible until the next prompt; chat shows "N
  messages rewound · Undo" until then.
- A rewind made in Terminal or the Claude app shows in chat as the abandoned
  messages disappearing, once the next prompt is sent.
- After a rewind made in Claude's Terminal, the rewound prompt sits in the
  TUI's input box. A message sent from chat before that box is emptied joins
  that text. This is Part 1's accepted "half-typed text" limitation.

### Diff sheet

- A **Changes** button in the chat header opens a sheet with the session
  folder's uncommitted changes against `HEAD`, untracked files included:
  one section per file with `+N −N`, collapsible, lines coloured.
- It refreshes when opened and at the end of each turn while open.
- Not a git repository: the sheet says so. Over 2 MB of diff: it shows the
  file list with counts and says to use Terminal for the rest.
- Edit rows in the conversation show `+N −N` in their label, and their
  detail sheet shows the edit as a diff.

### Message polish

- **Copy:** a copy action on every message, and a copy button on every code
  block.
- **Syntax colours** in code blocks: comments, strings, numbers and keywords
  for common languages.
- **Thinking:** when a reply carried readable thinking, a collapsed
  "Thinking" row precedes it; tap to read. (Most Claude thinking blocks are
  stored as signatures only: of 2,726 in the owner's recent transcripts, 285
  had text. Those without text show nothing, as today.)
- **Checklist:** when the agent keeps a todo list or plan checklist, it is
  pinned at the top of the chat as "3 of 5 done", expandable.
- **Timestamps:** tap a message to see its time; day separators between
  days.
- **Drafts:** text typed in a session's composer survives closing chat and
  reloading; it is cleared when sent. Attachments are not kept.

### Usage sheet

A **Usage** button opens a sheet with:

- context: how full the context window is, in percent and tokens;
- this session's tokens (input, output, cache) and, for OpenCode, its cost;
- plan limits, the same numbers as the dashboard's meters (Claude 5-hour,
  weekly, and per-model weekly; Codex primary and secondary windows).

### Running list

When the session has background shells or subagents running, the header
shows "2 running". Tapping it lists each one: shell or agent, its command or
description, and how long it has run. Finished ones leave the list.

### Side chat

- **Ask aside** in the composer menu, or typing `/btw <question>`, opens a
  side sheet. The question is answered there, from the conversation so far,
  without tools. The answer never enters the conversation, and Terminal and
  the Claude app do not see it.
- **Ask another** clears the sheet for a new, independent question.
- For Claude, a side question re-reads the whole conversation. Above 100k
  tokens the sheet shows the size and asks "This question re-reads about N
  tokens of the conversation. Ask anyway?" before sending.

## How it works

### Following Claude's chain

Claude writes each record with a `parentUuid`. A rewind writes nothing until
the next prompt, which then names the kept record as its parent. The owner's
own transcripts show this (for example two prompts in one session, hundreds
of records apart, both parented to the same earlier `turn_duration` record).

- Every event the tap emits carries its record's `uuid`, including `result`
  events (from `turn_duration`) and `cdash` events.
- The tap tracks, for every main-chain record with a `uuid` (emitted or not):
  the last main-chain `uuid` seen, and an anchor, which is the `uuid` of the
  last event emitted at or before that record.
- A prompt record (rule 7, or a Part 3 image prompt) whose `parentUuid` is
  known but is not the last main-chain `uuid` is a branch: the tap emits
  `{"type":"cdash","subtype":"cut","after":<anchor of the parent, or null>}`
  before the prompt.
- The hub applies a cut by truncating its log after the last event whose
  `uuid` equals `after` (all of it if `null`), starting a new epoch and
  broadcasting `reset`, so clients reload the backlog.
- A cut that removes no event is ignored: no new epoch, no `reset`. After a
  rewind started from chat the hub has already cut, and the next prompt
  makes the tap cut again at the same place.
- A cut never touches pending or held messages, so the prompt that follows
  it still confirms the message chat sent.
- Checked on the owner's 253 latest transcripts (2026-11-13): this rule
  finds 17 cuts in 922 prompts, and every one skips replies or prompts. No
  record written between turns caused a cut.
- Part 1's tap retract rule (same `parentUuid` as the previous prompt) is the
  one-prompt case of this and is removed. The hub's own retract, for a prompt
  Claude handed back after Stop, stays.
- A record whose parent is unknown (for example the first record after
  compaction, which has a null `parentUuid`) is never a cut.

### Codex and OpenCode rewinds made elsewhere

- **Codex:** `thread/reverted` from any client makes the supervisor re-read
  the thread and send chat a reset with the new history.
- **OpenCode:** `session.revert.staged`, `session.revert.cleared` and
  `session.revert.committed` make the supervisor rebuild the visible history
  and send a reset. While a revert is staged, rows with an ID at or after
  `revert.messageID` are left out (OpenCode's history API still returns
  them; its own clients hide them the same way).

### Starting a rewind from chat

A new client frame, `{"t":"rewind","uuid":<prompt event uuid>,"files":bool}`.
The hub refuses it unless the session is idle and not waiting, then calls
`Engine::rewind`. On success it applies the cut itself (Claude writes
nothing until the next prompt) and returns the prompt's text, which the
client puts in the composer.

- **Claude Code:** the engine drives the TUI's rewind dialog. Measured on
  2.1.323:
  - Esc, then Esc about 250 ms later, while idle, opens the selector within
    about 130 ms. The status file shows `waiting` with
    `waitingFor: "dialog open"`. Nothing is written to the transcript.
  - The selector:
    - is titled `Rewind`, with the line "Restore the code and/or
      conversation to the point before…";
    - has two lines per prompt on the branch, oldest first: the prompt's
      first line, then "No code changes" or a summary of the changes;
    - ends with `❯ (current)`, where the cursor starts, and the footer
      "Enter to continue · Esc to cancel".
    Up and Down move the `❯`.
  - Enter opens a confirm screen that quotes the chosen prompt ("Confirm you
    want to restore to the point before you sent this message:"). Its
    options are numbered: `1. Restore conversation`,
    `2. Summarize from here`, `3. Summarize up to here`, `4. Never mind`.
    When the prompt has code changes, restore-code options appear too; the
    builder records their text on a real session.
  - After "Restore conversation" the status is `idle` about 60 ms later, and
    the chosen prompt's text sits in the input box verbatim.

  Matching by text alone is not enough:
  - short prompts such as "yes" repeat;
  - chat sends every prompt as a bracketed paste, and Claude may list a long
    paste as a `[Pasted text #N …]` chip rather than its text;
  - the tap removes `[Image #N]` tokens.

  So the target is found **by position**: the selector lists the branch's
  prompts oldest first, and the target is the k-th from the bottom.

  - The prompt events keep the raw recorded text (field `raw`, with paste
    and image tokens and Part 3's `Attached:` lines) next to the display
    text.
  - Each step is checked against the expected sequence. The highlighted
    entry must match the prompt expected at that position:
    - its first line equals the first line of `raw`; or
    - it is a paste chip, and that prompt was recorded as a paste; or
    - it is an image token, and that prompt had images.
  - Which prompts the selector lists (typed, queued, `!` commands, slash
    commands, task notifications) is recorded on a real session by the
    builder, and the expected sequence follows it.

  The engine therefore:
  1. checks the status is `idle`;
  2. presses Esc, Esc and waits for `dialog open` and the `Rewind` title;
  3. presses Up k times, reading the pane after each press and checking the
     highlighted entry against the expected sequence;
  4. presses Enter and checks that the confirm screen quotes the target in
     the same way;
  5. picks the restore option (conversation, or code and conversation when
     files are wanted and offered) by its number, after checking its label.

  Any mismatch up to step 5 presses Escape until the dialog is gone and
  refuses with "rewind failed; use Terminal". **Once step 5's key is
  pressed, the rewind counts as done:**
  6. the engine waits up to 2 s for `idle`;
  7. it clears the input box (Part 1's `clear_input`), whatever the box
     shows, because chat's composer now holds the text;
  8. the hub applies the cut.

  The input box is not verified, because it may hold a paste chip instead
  of the text.
- **Codex:** the supervisor calls `thread/revert {threadId, beforeTurnId}`,
  where `beforeTurnId` is the turn whose first user message is that prompt.
  Prompts that steered a turn have no rewind action. The native `--remote`
  TUI ignores reverts it did not make, so the supervisor restarts it after a
  successful revert, and Terminal then shows the reverted thread.
- **OpenCode:** the supervisor calls `POST /api/session/:id/revert/stage
  {messageID, files}`. The revert stays staged; the next prompt commits it,
  as in OpenCode's TUI. "Undo" in chat calls `DELETE /api/session/:id/revert`.

### Diff sheet

`GET /api/chat/diff?session=<name>`, behind the existing auth.

The folder is where the session works now, not where it was launched.
Claude often moves into `.worktrees/<name>`, which `git diff` in the launch
folder never shows (in 9 of the owner's last 40 transcripts for this repo).
So the folder comes from:

- Claude: the `cwd` of the latest main-chain record;
- Codex: the thread's `cwd`;
- OpenCode: the session's directory.

The sheet's header shows that path. In it the agent runs:

- `git -c core.fsmonitor=false diff --no-color --no-ext-diff --no-textconv HEAD`, and
- for each untracked file
  (`git -c core.fsmonitor=false ls-files --others --exclude-standard`), a
  diff against an empty file,

stopping at 2 MB of output, and returns `{files:[{path, status, additions,
deletions, patch}], truncated}`. The client renders the patch as escaped
text with line classes.

Edit rows get their diff from the tool's own record:

- Claude `Edit`, `MultiEdit` and `Write` inputs (old and new strings, or the
  whole new file);
- Codex `fileChange` items (`diff` per change);
- OpenCode edit tool metadata, when it carries a patch.

### Thinking, checklist, timestamps

- **Thinking:**
  - The Claude tap emits a `thinking` block when its text is not empty
    (today it drops all of them).
  - The Codex supervisor emits `reasoning` summaries. History needs
    `thread/turns/list` with `itemsView: "full"`.
  - The OpenCode supervisor emits `reasoning` parts.
- **Checklist:**
  - Claude: the latest `TodoWrite` input (the builder checks 2.1.323 for any
    newer task-list tool and follows it too).
  - Codex: `turn/plan/updated`. It is live only, so the supervisor keeps the
    last one per thread and replays it to new chat connections.
- **Timestamps:**
  - Claude records carry `timestamp`; the tap copies it to `ts` on each
    event.
  - Codex and OpenCode items carry their own times; the supervisors copy
    them.

### Usage

Engines send usage snapshots to the hub, which keeps the latest in its state
(not in the event log):

- **Claude:**
  - Context is the last main-chain reply's `input + cache_read +
    cache_creation` tokens. The agent has no live source for Claude's
    window: Claude sends it only to the status-line command, which the
    owner uses for their own status line. So the percentage comes from a
    small table of model to window. A family enters the table only after
    its `context_window` is read from one status-line sample. opus-5-5,
    sonnet-5-5 and fable-5-2 are expected at 1M: the owner's transcripts
    show contexts above 800k for each. haiku-5-5 is unconfirmed (its largest
    context seen is 160k). A model not in the table shows tokens only,
    never a guessed percentage. `ponytail:` the table needs an entry per
    new model family.
  - Session tokens are summed over replies, counting each `message.id` once.
- **Codex:**
  - Context is `last.totalTokens` against `modelContextWindow`, from
    `thread/tokenUsage/updated`.
  - Session tokens come from `total`.
- **OpenCode:**
  - The session's `tokens` and `cost`.
  - Context is the last reply's tokens against the model's `limit.context`,
    as its TUI computes it.

Plan limits come from the agent's existing usage collector
(`collect/usage.rs`), which already serves the dashboard's meters.

### Running list

Engines send a `tasks` snapshot to the hub state whenever it changes:
`[{id, kind: "shell"|"agent", title, started}]`.

- **Claude:** from the transcript, by the tap, with shapes seen in the
  owner's own transcripts:
  - **Start of a shell.** A `Bash` result whose `toolUseResult` has
    `backgroundTaskId`. This covers both `run_in_background` and a
    foreground command moved to the background after its timeout. The
    title is the call's `description`, else its `command`.
  - **Start of an agent.** An `Agent` result whose `toolUseResult` has
    `isAsync: true` and `agentId`; agents run in the background by default.
    The title is its `description`.
  - **End.** A `<task-notification>` naming the task's `<task-id>`, with
    status `completed`, `failed`, `killed` or `stopped`. It can arrive in a
    `user` record, an `attachment` of type `queued_command`, or a
    `queue-operation` `enqueue`; the tap reads all three, because the
    enqueue is the only one always written. A `TaskStop` result (`task_id`)
    also ends a task; no notification follows it.
  - **After compaction,** `attachment` records of type `task_status` with
    status `running` restate the tasks still running.

- **Codex:**
  - Shells: `item/started` for a `commandExecution` item with
    `source: "unifiedExecStartup"` adds an entry, and `item/completed` for
    the same item removes it. The end can arrive after the turn has
    finished.
  - Subagents: `thread/status/changed` for threads whose parent is the main
    thread; `active` means running.
  - This uses the subagent events that Part 4 stops from leaking into the
    conversation.
- **OpenCode:**
  - Shells: `GET /api/shell`, keeping shells whose `metadata.sessionID` is the
    session or one of its subagent sessions, then `shell.created` and
    `shell.exited` events.
  - Subagents: `GET /api/session?parentID=<sid>` intersected with
    `GET /api/session/active`, then `session.execution.*` events.

### Side chat

A new client frame `{"t":"aside","id":…,"text":…}`. The answer arrives as
`{"t":"aside","id":…,"text":…,"done":bool}` frames on the same socket. A
side question never touches the hub's log or queue.

- **Claude Code:** the agent runs, in the session's folder, with stdin from
  `/dev/null`:
  `claude -p --resume <sid> --fork-session --no-session-persistence --tools "" --strict-mcp-config --mcp-config '{"mcpServers":{}}' --settings '{"disableAllHooks":true}' --model <session model> --output-format stream-json --verbose "<question>"`.
  The builder checks each of these flags on 2.1.323 before relying on them.
  - The MCP and hook flags keep the run from loading the owner's MCP
    servers (whose tools `--tools ""` may not cover) and from running their
    hooks.
  - Before running, the sheet shows the conversation's size (the context
    tokens from the usage sheet). Above 100k tokens it asks "This question
    re-reads about N tokens of the conversation. Ask anyway?" (owner
    decision).
  Measured on 2.1.323 against a live, idle session:
  - it answered correctly from the conversation, in 6.2 s (about 4.3 s of
    that is start-up);
  - it wrote no transcript, and the session's transcript was byte-identical
    afterwards;
  - it got no prompt-cache hit: the whole conversation is sent and cached
    again (47–58k tokens for a tiny session), so the cost grows with the
    conversation;
  - for its few seconds of life it registers its own
    `~/.claude/sessions/<pid>.json` with the session's folder, so the
    collector must ignore session files of processes the agent started
    itself, or a phantom card would appear.
  The reply text streams into the sheet. Closing the sheet kills the
  process.
- **Codex:** the same calls as Codex's own `/side`:
  1. `thread/fork` with `ephemeral: true` and `excludeTurns: true`, the
     parent's model and effort, a read-only sandbox, and approval policy
     `never`;
  2. `thread/inject_items` with Codex's side boundary message;
  3. `turn/start` with the question.

  Agent-message deltas stream to the sheet. Closing the sheet sends
  `turn/interrupt` and `thread/unsubscribe`.
- **OpenCode:** `POST /api/session/:id/generate {prompt}` with OpenCode's own
  `/btw` preamble. One answer, no tools; OpenCode does not add its cost to
  the session.

`/btw` typed in chat is always routed here and never pasted into Claude's
TUI, where it would open an overlay chat cannot see.

## Security

- The diff route runs fixed `git` arguments in the session's current
  folder, taken from the session's own record. No client-supplied path or
  argument reaches the command line.
  - The folder is one the session's agent chose, and that agent already
    runs arbitrary commands there.
  - `core.fsmonitor` is forced off on both commands, and external diff and
    textconv drivers are disabled.
  - A clean filter defined in the repository's own `.git/config` still runs
    during `git diff` (checked 2026-11-13). The dashboard's existing
    `git status` already runs it too. Setting one needs write access to
    `.git`, which the session's agent has unless a sandbox stops it.
    Accepted risk.
  - Output is capped and rendered as text.
- The Claude side-question process gets the question as one argument (never
  through a shell), and runs with tools, MCP servers and hooks off.
- Rewind key presses follow the Part 1 rule: nothing is typed into a dialog
  chat has not seen on screen.

## Author's calls

1. **Cut-and-reset for branch changes,** rather than a finer "remove rows"
   event: rewinds are rare, and reset already exists end to end.
2. **Branch detection only on prompt records,** because a rewind always
   continues with a new prompt. That keeps other records (system,
   attachments) from ever causing a false cut.
3. **Rewind only while idle.** The Codex TUI refuses in-progress turns, and
   OpenCode's revert returns 409 while busy. For Claude, a key sequence into
   a working TUI is riskier still.
4. **The Codex native TUI is restarted after a chat revert;** otherwise
   Terminal shows stale turns.
5. **OpenCode rewinds stay staged** until the next prompt, matching its TUI,
   with Undo in chat.
6. **The diff is the working tree against `HEAD`, from `git`, for all three
   tools,** rather than each tool's own per-turn diff. It is the same for all
   three, it covers shell edits that Codex's `turn/diff` misses, and it is
   what the owner reviews before committing.
7. **A small built-in highlighter** (tokens for comments, strings, numbers
   and keywords) instead of a highlighting library.
8. **Codex side chat uses `/side`'s calls,** not `thread/sample`, because
   `thread/sample` needs an idle session and Claude's `/btw` works mid-turn.
   The forked thread gets a read-only sandbox, so it cannot change files.
9. **Usage and running tasks live in hub state,** not in the event log; they
   are snapshots, not history.
10. **Claude's rewind target is found by position,** checked step by step
    against the expected sequence, rather than by text. Short prompts
    repeat, and chat's own prompts may be listed as paste chips.
11. **A Claude rewind counts as done once the restore option is pressed.**
    Refusing after that point would leave chat on the old branch while
    Claude has already moved, with the old prompt sitting in the input box.
12. **The diff follows the session's current folder,** because the agents
    often work in a worktree that `git diff` in the launch folder cannot
    see.
13. **Claude's context percentage comes from a small model table,** and
    unknown models show tokens only. Claude reports its window only to the
    status-line command, which the owner already uses.
14. **Side questions on Claude run without MCP servers and hooks.** They
    answer from the conversation alone, and cannot start or change
    anything.

## Left to the builder

- Menu placement, sheet layouts and strings.
- The language keyword lists for the highlighter.
- Whether the side sheet keeps the last answer when reopened.

## Acceptance checks

- Unit tests:
  - the tap's cut, with fixtures cut from the owner's real transcript (a
    prompt parented to an older `turn_duration`), the one-prompt hand-back
    case, and a compaction boundary that must not cut;
  - the hub truncating at the right event and resetting; ignoring a cut
    that removes nothing (a chat rewind followed by a prompt resets once);
    and keeping a pending message across a cut;
  - diff route parsing and the 2 MB cap;
  - the highlighter on a few languages, with escaping kept (a string holding
    `<script>` renders as text).
- Real sessions:
  - Claude: rewind in Terminal, then send a new prompt: chat drops the
    abandoned branch. "Rewind to here" from chat, with and without files,
    lands on the same state as Terminal's rewind would.
  - Codex: double-Esc in Terminal shows in chat. "Rewind to here" from chat
    reverts, and Terminal shows the reverted thread after its restart.
  - OpenCode: `/undo` in Terminal shows in chat; "Rewind to here" and Undo
    from chat work.
  - A side question on each tool answers from the conversation and leaves
    the transcript unchanged.
  - The running list shows a background `sleep 60` on each tool and drops it
    when it ends.

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
