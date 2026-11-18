# Claude Code 2.1.323 TUI spike, 2026-11-13 (live sessions)

Facts only. Times are wall-clock deltas measured with 50 ms polling unless said otherwise.
Raw logs: `<scratchpad>/spike/`
(`hook.log.jsonl`, `req-<n>.json`, `resp/<n>.json`, `poll.jsonl` = status-file and transcript-line log,
`frames-A.jsonl` / `frames-B.jsonl` = every changed capture-pane frame, `actions.jsonl` = every key/paste with its time).

## Setup

- `claude --version` → `2.1.323 (Claude Code)`; `claude` on PATH symlinks to the npm `claude.exe`.
- Launch (session A), run under `env -i` with only HOME, USER, LOGNAME, PATH, SHELL, LANG=C.UTF-8,
  TERM=xterm-256color, XDG_RUNTIME_DIR. (The parent shell had CLAUDECODE=1, CLAUDE_EFFORT=max,
  CLAUDE_CODE_BRIDGE_SESSION_ID and others, which would have leaked into the child.)
  `tmux -L cdash-spike -f /dev/null new-session -d -s A -x 160 -y 50 -c <spike-cwd> claude --settings '<json>' --session-id <uuid> --dangerously-skip-permissions --model haiku`
- Settings for A: `{"tui":"fullscreen","env":{"CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC":""},"hooks":{"PermissionRequest":[{"hooks":[{"type":"http","url":"http://127.0.0.1:28771/hook","timeout":3600}]}]}}`.
  No matcher was needed. Session B used the same settings without `hooks`.
- No `--remote-control`. The user setting `remoteControlAtStartup` is `false`, and E5's initialize reply said `remote_control_auto_enable: false`.
- User-level config still applied: plugin SessionStart, UserPromptSubmit and Stop hooks, codebase-memory-mcp
  PreToolUse/SessionStart hooks, and a status line that shows `[CAVEMAN:ULTRA]` above the footer.
- Pasting followed production: `load-buffer -b spike <file>`, then `paste-buffer -p -d -b spike -t =A:`, then `send-keys Enter` 400 ms later.
- Model use: 11 TUI prompts and 2 `-p` side questions (13 total). E5 made no model call.
  `cost-state` totals: session A $0.191 ($0.017 Haiku plus **$0.174 Sonnet**, see E1c), session B $0.0086, E6 $0.0094 + $0.0116.
- Transcripts (left in place):
  - A: `~/.claude/projects/-tmp-claude-1000--mnt-d-git-claude-dashboard-ef25c560-4f1f-4004-94f4-defdef4aea0c-scratchpad-spike-cwd/6c0a105f-e3bf-450c-b88e-48368ad38d50.jsonl` (667 KB, 7 prompts)
  - B: `~/.claude/projects/-tmp-claude-1000--mnt-d-git-claude-dashboard-ef25c560-4f1f-4004-94f4-defdef4aea0c-scratchpad-spike-cwd/83f627fd-f24b-4e0c-bc62-4b7cf786b1b6.jsonl` (418 KB, 4 prompts)

### Folder-trust dialog (first start in the new folder)

Exact text. The default selection is **"No, exit"**, so the dashboard must press Down before Enter:
```
 Accessing workspace:
 <tmp>
 Quick safety check: Is this a project you created or one you trust? (Like your own code, a well-known open source project, or work from your team). If not,
 take a moment to review what's in this folder first.
 Claude Code'll be able to read, edit, and execute files here.
 Security guide
 ❯ No, exit
   Yes, I trust this folder
 Enter to confirm · Esc to cancel
```
`~/.claude/sessions/<pid>.json` did **not** exist while this dialog was open. It appeared 0.5 s after Enter, first
without `status`. `status: "idle"` was written 0.7 s after Enter. No bypass-mode warning appeared
(`skipDangerousModePermissionPrompt: true` is set in the user settings). The transcript file is created only at the first prompt.

## E1. PermissionRequest HTTP hook

### Request shape (all 7 requests)

`POST /hook` with headers `User-Agent: axios/1.15.2`, `Content-Type: application/json`,
`Accept: application/json, text/plain, */*`, `Accept-Encoding: gzip, compress, deflate, br`, `Connection: keep-alive`.
The body keys are: `session_id, transcript_path, cwd, scratchpad_dir, prompt_id, permission_mode, effort{level}, hook_event_name, tool_name, tool_input`.
`permission_suggestions` appears only for Write and Bash; it is absent for AskUserQuestion and ExitPlanMode.
There is **no tool_use_id**. `prompt_id` equals the `promptId` field on that prompt's transcript records.

### a. AskUserQuestion answered by the hook (bypass mode)

- **The hook fires in bypass mode.** Request 1 (trimmed):
```json
{"session_id":"6c0a105f-…","transcript_path":"~/.claude/projects/…/6c0a105f-….jsonl","cwd":"…/spike-cwd",
 "scratchpad_dir":"<tmp>",
 "prompt_id":"de75f374-5ceb-4909-8d34-548c5174681f","permission_mode":"bypassPermissions","effort":{"level":"high"},
 "hook_event_name":"PermissionRequest","tool_name":"AskUserQuestion",
 "tool_input":{"questions":[
   {"question":"Pick a color","header":"Color","options":[{"label":"Red","description":"Red"},{"label":"Blue","description":"Blue"}],"multiSelect":false},
   {"question":"Pick fruits","header":"Fruits","options":[{"label":"Apple","description":"Apple"},{"label":"Pear","description":"Pear"}],"multiSelect":true}]}}
```
- **Timing:** the hook request arrived 44 ms before the dialog was first seen in capture-pane (13 ms for request 2).
  The status file flipped to `waiting` 8–9 ms *before* the hook arrived. The tool_use transcript line was written 130 ms after the hook arrived.
- The dialog for two questions, exact text:
```
←  ☐ Color  ☐ Fruits  ✔ Submit  →
Pick a color
❯ 1. Red
     Red
  2. Blue
     Blue
  3. Type something.
  4. Chat about this
Enter to select · Tab/Arrow keys to navigate · Esc to cancel
```
- **Response sent** (the multi-select answer was a JSON array):
  `{"hookSpecificOutput":{"hookEventName":"PermissionRequest","decision":{"behavior":"allow","updatedInput":{"questions":<echo>,"answers":{"Pick a color":"Red","Pick fruits":["Apple","Pear"]}}}}}`
- **The dialog closed** 92 ms after the response was sent. The status went `waiting` → `busy` 65 ms after it. The screen then showed:
```
● User answered Claude's questions:
  ⎿  · Pick a color → Red
     · Pick fruits → Apple, Pear
  ⎿  Allowed by PermissionRequest hook
● Color: Red
  Fruits: Apple, Pear
```
- **The model received the answers.** The array was stored joined, as `"Apple, Pear"`. Transcript records, in order:
```json
{"type":"attachment","attachment":{"type":"hook_permission_decision","decision":"allow","toolUseID":"toolu_01G2j5ugLEa9jMM8bMLng3jP","hookEvent":"PermissionRequest"}}
{"type":"user","promptId":"de75f374-…","parentUuid":"<tool_use assistant uuid>",
 "message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"toolu_01G2j5ugLEa9jMM8bMLng3jP",
   "content":"Your questions have been answered: \"Pick a color\"=\"Red\", \"Pick fruits\"=\"Apple, Pear\". You can now continue with these answers in mind."}]},
 "toolUseResult":{"questions":[…echo…],"answers":{"Pick a color":"Red","Pick fruits":"Apple, Pear"}},
 "sourceToolAssistantUUID":"a3c1f897-…",
 "permissionDecision":{"decision":"accept","source":"hook","reasonType":"hook","reasonCode":"unknown"}}
```
- `turn_duration.durationMs` was **2447** for a turn that took 36.7 s of wall time, 34.2 s of it with the dialog open.
  **Dialog wait time is excluded**, and the footer said "✻ Churned for 2s".

### b. Answered in the TUI first while the hook was held

- The single-question dialog, exact text:
```
 ☐ Size
Pick a size
❯ 1. Small
     Small
  2. Large
     Large
  3. Type something.
  4. Chat about this
Enter to select · ↑/↓ to navigate · Esc to cancel
```
- Pressing Down, then Enter (150 ms apart) selected Large. The dialog closed 133 ms after Enter, the status was `busy` 76 ms after Enter, and the model replied "Large".
- **The held HTTP request was NOT closed.** The TCP connection was still ESTABLISHED after the turn ended, and the server logged no disconnect.
  It stayed open 18.4 s until I released it. (Research note: it lives until the hook's own timeout.)
- **A late response (answer "Small") had no effect.** It was written to the socket without error (HTTP 200). The transcript stayed at 70 lines, the screen did not change, and the answer stayed "Large".
- The TUI-answer record differs from the hook-answer record: `toolUseResult` gains `"annotations":{}`,
  `permissionDecision` is `{"decision":"accept","source":"user_temporary","grantScope":"once","reasonCode":"unknown"}`, and there is no `hook_permission_decision` attachment.

### c. Plan approval (ExitPlanMode)

- I entered plan mode with BTab (four presses from bypass; footer texts in d). The conversation then showed:
  `● Session was started with bypass permissions, so plan mode won't ask before most edits or commands. Start a new session without bypass permissions for prompts in plan mode.`
- **Surprise: plan mode switched the model to Sonnet.** In plan mode every main-thread request used `claude-sonnet-5-5`
  (`api-request.params.model`; the assistant records have `requestedModel:"claude-sonnet-5-5"`). A `model` attachment
  `{"type":"model","identity":{"modelId":"claude-sonnet-5-5",…}}` was written with each plan-mode prompt, and one with
  `claude-haiku-5-5` after approval. The session had been started with `--model haiku`, which is why session A cost $0.174 in Sonnet.
- The model wrote the plan with `Write` to `~/.claude/plans/make-a-2-step-plan-jazzy-patterson.md`, which was auto-allowed
  (`permissionDecision:{"decision":"accept","source":"config","reasonType":"mode"}`). It then ran `ToolSearch` `select:ExitPlanMode`
  (the tool is deferred) and called ExitPlanMode. **The hook fired.** Request 3 `tool_input`:
  `{"plan":"# Plan: create hello.txt\n\n## Context\n…\n## Steps\n1. Create `hello.txt` …\n2. Verify: `cat hello.txt` prints `hi`.\n","planFilePath":"~/.claude/plans/make-a-2-step-plan-jazzy-patterson.md"}`, with `permission_mode:"plan"`.
- The dialog, exact text (the plan body is elided here):
```
   Ready to code?
   Here is Claude's plan:
  ╌╌╌
   Plan: create hello.txt … (rendered markdown)
  ╌╌╌
   Claude has written up a plan and is ready to execute. Would you like to proceed?
   ❯ 1. Yes, and switch to BYPASS PERMISSIONS (no further prompts) for this session
     2. Yes, manually approve edits
     3. Tell Claude what to change
        shift+tab to approve with this feedback
   ctrl+g to edit in Vim · ~/.claude/plans/make-a-2-step-plan-jazzy-patterson.md
```
- **Allow** with `updatedInput:{"plan":<echo>}` and `updatedPermissions:[{"type":"setMode","mode":"acceptEdits","destination":"session"}]`.
  The dialog closed 113 ms later. Screen: `● User approved Claude's plan` / `⎿  Plan saved to: ~/.claude/plans/make-a-2-step-plan-jazzy-patterson.md · /plan to edit` / … / `⎿  Allowed by PermissionRequest hook`.
  The tool_result content is exactly:
  `"User has approved your plan. You can now start coding. Start with updating your todo list if applicable\n\nYour plan has been saved to: ~/.claude/plans/make-a-2-step-plan-jazzy-patterson.md\nYou can refer back to it if needed during implementation.\n\n## Approved Plan:\n<plan>"`,
  with `toolUseResult:{"plan":…,"isAgent":false,"filePath":…}` and `permissionDecision:{"decision":"accept","source":"hook","reasonType":"hook","reasonCode":"unknown"}`.
  Three attachments followed: `hook_permission_decision`, `model` (back to Haiku), and `plan_mode_exit {"planFilePath":…,"planExists":true}`.
- **The resulting mode was acceptEdits.** The next hook request carried `permission_mode:"acceptEdits"`, and the footer read `⏵⏵ accept edits on (shift+tab to cycle)`.
  The `permission-mode` record `acceptEdits` was written 2.3 s after the hook answer.
- Unplanned extra case: Haiku then wrote to a wrong absolute path outside the cwd. acceptEdits prompted for it (request 4, `tool_name:"Write"`,
  `permission_suggestions:[{"type":"addDirectories","directories":["<tmp>"],"destination":"session"}]`).
  That dialog read `Do you want to create hello.txt?` / `❯ 1. Yes` / `2. Yes, and always allow access to <dir> for this session (shift+tab)` / `3. No` / `Esc to cancel · Tab to amend`.
  I denied it with a message. The record was `{"type":"tool_result","content":"<message>","is_error":true}`, `toolUseResult:"Error: <message>"`,
  `permissionDecision:{"decision":"reject","source":"hook","reasonType":"hook","reasonCode":"hook:PermissionRequest"}`.
  The screen showed `⎿  Error: <message>` / `⎿  Denied by PermissionRequest hook`. The model retried with `hello.txt` (auto-allowed) and the file was created.
- **Second plan, deny "Add a third step":** the deny was sent 0.1 s after request 5. The model revised the plan, adding `3. Confirm file listed: \`ls bye.txt\`.`,
  and called ExitPlanMode again 4.5 s later (request 6). The deny result record was `{"type":"tool_result","content":"Add a third step","is_error":true}`,
  `toolUseResult:"Error: Add a third step"`, reject/hook `permissionDecision` as above. The plan file path was reused.
- **Stale plan in the transcript:** in the second plan, the model sent `Write(plan file)` and `ExitPlanMode` in **one** assistant message.
  The transcript `tool_use.input.plan` of that ExitPlanMode (`toolu_01KbDgeAv4D8AX9iwmFuJdMd`) holds the **previous plan (hello.txt)**,
  that is, the plan file before the Write ran. Hook request 5 got the correct new plan (bye.txt). The hook `tool_input`, or the plan file read at dialog time, is authoritative; the transcript tool_use is not.
- Request 6 was answered with deny plus `"interrupt":true`. The status went `waiting` → `idle` directly, 41 ms after the response, with no `busy` in between.
  Records: tool_result `"The user doesn't want to proceed with this tool use. The tool use was rejected (eg. if it was a file edit, the new_string was NOT written to the file). STOP what you are doing and wait for the user to tell you how to proceed."`
  (`is_error:true`, `toolUseResult:"User rejected tool use"`, **no** `permissionDecision`, **no** hook attachment), then a user text record
  `[Request interrupted by user for tool use]`, then `system turn_duration` (no `stop_hook_summary`). The mode stayed plan.
  This record set cannot be told apart from a user rejection.

### d. Mode cycle footers and the ordinary permission prompt

Footer text after each BTab, starting from `--dangerously-skip-permissions` (exact; the suffix was `· ← for agents` before the first prompt and `· ← 1 agent` after it):

| mode | footer |
|---|---|
| bypassPermissions | `⏵⏵ bypass permissions on (shift+tab to cycle) · ← 1 agent` |
| auto | `⏵⏵ auto mode on (shift+tab to cycle) · ← 1 agent` |
| default | `⏸ manual mode on · ← 1 agent` (no shift+tab hint) |
| acceptEdits | `⏵⏵ accept edits on (shift+tab to cycle) · ← 1 agent` |
| plan | `⏸ plan mode on (shift+tab to cycle) · ← 1 agent` |
| bypass, background shell running | `⏵⏵ bypass permissions on · 1 shell · ← 1 agent` |

The order was bypass → auto → default → acceptEdits → plan → bypass. The footer changed within one 50 ms frame of the key.

The first switch into auto mode wrote, about 0.9 s after BTab, `{"type":"system","subtype":"informational","level":"notice","content":"Auto mode lets Claude handle permission prompts automatically — Claude checks each tool call for risky actions and prompt injection before executing. Actions Claude identifies as safe are executed, while actions Claude identifies as risky are blocked and Claude may try a different approach. Ideal for long-running tasks. Claude can make mistakes that allow harmful commands to run, it's recommended to only use in isolated environments. Shift+Tab to change mode."}`.

**`permission-mode` records are never written when the mode changes.** They arrive in a metadata batch
(`last-prompt`, `mode`, `permission-mode`, `session-owner`, `atis-latch`), which is written at some prompt submissions or after the next assistant message:
- BTab → plan at +0 s; record at the next prompt, 28 s later.
- BTab → default; record at the next prompt, 12.9 s later.
- Hook setMode acceptEdits; record 2.3 s later, after the next assistant message.
- BTab → bypass; the next prompt ("sleep 20") got **no** batch. The record came with the prompt after that, 30 s after the BTab.
- Every typed-prompt `user` record carries `permissionMode` (e.g. `"plan"`, `"default"`), plus `promptSource:"typed"`, `origin:{"kind":"human"}`,
  `turnOrigin:"human"` and `turnPosition:{"promptIndex":n,"turnIndex":n}`. Prompts pasted with bracketed paste count as `typed`.

**Default-mode Bash write.** Request 7:
```json
{"prompt_id":"384b698f-…","permission_mode":"default","effort":{"level":"high"},"hook_event_name":"PermissionRequest","tool_name":"Bash",
 "tool_input":{"command":"echo spike > out.txt","description":"Write spike to out.txt"},
 "permission_suggestions":[{"type":"addDirectories","directories":["<scratchpad>/spike-cwd"],"destination":"session"}]}
```
The suggestion names the cwd itself. The dialog, exact text:
```
 Bash command
 Tip: auto mode handles these prompts for you — choose "switch to auto mode" below
 Write spike to out.txt
 echo spike > out.txt
 Do you want to proceed?
 ❯ 1. Yes
   2. Yes, and always allow access to <tmp>
      60-4f1f-4004-94f4-defdef4aea0c/scratchpad/spike-cwd from this project
   3. Yes, and switch to auto mode · auto mode handles these prompts for you
   4. No
 Esc to cancel · Tab to amend
```
A plain `{"behavior":"allow"}` with no updatedInput **ran the command**: `out.txt` contains `spike`. The screen showed `Ran 1 shell command` / `⎿  Allowed by PermissionRequest hook`.
The dialog closed 77 ms after the response. The status went `waiting` → `busy` 20 ms after the response, then `idle` 0.7 s later at turn end.

### e. Status-file values while dialogs were open, and hook timing per dialog

| n | dialog | `status` / `waitingFor` | hook − frame | status flip − hook | dialog closed after response |
|---|---|---|---|---|---|
| 1 | AskUserQuestion (2 questions) | `waiting` / `input needed` | hook 44 ms before frame | −8 ms | 92 ms |
| 2 | AskUserQuestion (TUI answered) | `waiting` / `input needed` | 13 ms before | −9 ms | (closed 133 ms after TUI Enter) |
| 3 | ExitPlanMode | `waiting` / `permission prompt` | 10 ms before | −10 ms | 113 ms |
| 4 | Write (acceptEdits) | `waiting` / `permission prompt` | 46 ms before | +17 ms | 159 ms |
| 5 | ExitPlanMode (deny) | `waiting` / `permission prompt` | 10 ms before | −8 ms | 69 ms |
| 6 | ExitPlanMode (deny + interrupt) | `waiting` / `permission prompt` | 42 ms before | +13 ms | 65 ms |
| 7 | Bash (default) | `waiting` / `permission prompt` | 5 ms before | −9 ms | 77 ms |
| – | `/exit` background-work confirmation | `waiting` / `dialog open` | – | – | – |
| – | Rewind selector (Esc Esc) | `waiting` / `dialog open` | – | – | – |
| – | idle with a background shell running | `shell` / – | – | – | – |
| – | folder-trust dialog | no session file yet | – | – | – |

Negative "status flip − hook" means the status file changed before the HTTP request arrived.

## E2. Image paste

- I pasted only the absolute path of a 64×64 solid-red PNG (168 bytes, made with zlib and struct), with bracketed paste and no Enter.
  **The `[Image #1]` chip was visible in the first frame, 32 ms after paste-buffer returned.** The raw path was never drawn. Input box: `❯ [Image #1]`.
- Then I pasted ` What color is this image? Answer in one word.` and pressed Enter 400 ms later. **The model saw the image** and answered `Red.`.
  The user record, with base64 trimmed:
```json
{"type":"user","promptId":"f3e0856b-…","parentUuid":"b2d9530c-…","uuid":"96de607e-…",
 "message":{"role":"user","content":[
   {"type":"text","text":"[Image #1] What color is this image? Answer in one word."},
   {"type":"image","source":{"type":"base64","media_type":"image/png","data":"iVBORw0KGgo…(224 chars)"}}]},
 "imagePasteIds":[1],"permissionMode":"bypassPermissions","origin":{"kind":"human"},"promptSource":"typed",
 "turnOrigin":"human","turnPosition":{"promptIndex":1,"turnIndex":1}}
```
  **The text block comes first, then the image.** Seven attachments later (environment, model, deltas, total_tokens_reminder), a second user record is chained:
  `{"type":"user","isMeta":true,"turnCompanion":true,"message":{"content":[{"type":"text","text":"[Image: source: /tmp/…/spike/red.png]"}]}}`.
  The screen shows `❯ [Image #1] What color…` / `⎿  [Image #1]`.
- **Path and text on one line** (`<path> Reply with only: ok`) stayed plain text. The input box showed the raw path, and the record is
  `"content":"/tmp/…/red.png Reply with only: ok"` (a string, no imagePasteIds). The model replied `ok`.
- **Enter with no wait** (paste-buffer, then Enter at 0 ms): **the Enter was dropped.** `❯ [Image #2]` stayed in the input box.
  Nothing was submitted: no transcript line and no status change in 3 s. Pasting ` Reply with only: ok3` and pressing Enter 400 ms later then sent
  `[{"type":"text","text":"[Image #2] Reply with only: ok3"},{"type":"image",…}]` with `imagePasteIds:[2]`. **Image numbers count up across the session.**
- Session B (started with `tmux -f /dev/null`) also showed the hint `tmux focus-events off · add 'set -g focus-events on' to ~/.tmux.conf and reattach for focus tracking`.

## E3. Status timing (`~/.claude/sessions/<pid>.json`, polled every 50 ms)

**Turn end.** The `status` → `idle` write (`statusUpdatedAt`) follows the `turn_duration` record's own `timestamp`
by **+17 to +29 ms** (10 turns). The file change was seen 4–48 ms after `statusUpdatedAt`. The `turn_duration` line became visible in the
transcript only 75–173 ms after its own timestamp (buffered write). **The status-file change was observed first in all 10 turns**, by 1–110 ms
(seven of the ten by 54–110 ms). `stop_hook_summary` precedes `turn_duration` by 3–6 ms.

| transcript | turn_duration ts | idle statusUpdatedAt − ts | line seen − ts |
|---|---|---|---|
| A | …729.419 | +27 ms | +105 ms |
| A | …777.025 | +26 ms | +75 ms |
| A | …089.930 | +24 ms | +97 ms |
| A | …122.854 (deny+interrupt) | +29 ms | +172 ms |
| A | …189.559 | +17 ms | +141 ms |
| A | …234.357 (bg shell left running) | +20 ms → status `shell` | +81 ms |
| B | 4 turns | +19 to +26 ms | +92 to +173 ms |

Prompt start (10 prompts): `busy` came **19–58 ms** after the Enter key (`statusUpdatedAt`). The prompt's `user` line was visible in the transcript 199–322 ms after Enter.
The Enter on `/exit` and the dropped Enter of E2 produced no `busy`.

**Esc during a running `sleep 20`** (Esc pressed 3.1 s after the tool_use line): status `idle` **111 ms** after the Esc key (`statusUpdatedAt`).
Records: tool_result `"The user doesn't want to proceed with this tool use. The tool use was rejected (eg. if it was a file edit, the new_string was NOT written to the file). STOP what you are doing and wait for the user to tell you how to proceed."`
(`is_error:true`, `toolUseResult:"User rejected tool use"`), then user text `[Request interrupted by user for tool use]`, both 98 ms after Esc.
**No `turn_duration` and no `stop_hook_summary` were written for this turn.** (The hook deny with `interrupt:true` in E1c *did* write `turn_duration`.)
Screen: `No result recorded for 1 call` / `⎿  $ sleep 20` / `⎿  Interrupted · What should Claude do instead?`.

**Question dialog** (from E1a/E1b): `busy` → `waiting`/`input needed` 8–9 ms before the hook request. → `busy` 65 ms after the hook answer, or 76 ms after the TUI Enter. → `idle` at turn end.

**Background shell and `/exit`.** After the model started `sleep 120` with `run_in_background`, the turn ended with status **`shell`** (not `idle`).
The footer gained `· 1 shell`, and the turn line read `✻ Crunched for 1s · done 11:30 AM · 1 shell still running`.
`/exit` was pasted and Enter pressed: status `waiting` / `dialog open` 46 ms after Enter, and the dialog visible at +80 ms. Exact text:
```
   Background work is running
   The following will stop when you exit:
   shell · sleep 120
   ❯ 1. Exit and stop tasks
     2. Move to background and exit
     3. Stay
   Enter to confirm · Esc to cancel
```
Choosing "1. Exit and stop tasks": the session file was deleted ≤48 ms after Enter, the `sleep 120` process was gone, and tmux closed the
session (the private server exited with it). Records appended at exit: `cost-state`, `last-prompt {"lastPrompt":…,"leafUuid":"<last turn_duration uuid>"}`,
`cost-state`, `file-history-snapshot`, and `queue-operation`
`{"operation":"enqueue","content":"<task-notification>\n<task-id>b6kul3og6</task-id>\n<tool-use-id>toolu_01DmzQNZq5znyVBJeuqoaukc</tool-use-id>\n<output-file>…/tasks/b6kul3og6.output</output-file>\n<status>killed</status>\n<summary>Background command \"Sleep 120 seconds in background\" was stopped</summary>\n</task-notification>"}`.
That notification is never delivered.

## E4. Rewind (session B, idle, after 3 prompts)

- Esc, then Esc 254 ms later. The selector was visible 127 ms after the second Esc, and the status went `waiting` / `dialog open` (+82 ms). No transcript record was written. Exact text:
```
   Rewind
   Restore the code and/or conversation to the point before…
     [Image #1] What color is this image? Answer in one word.
     No code changes
     <scratchpad>/spike/red.png Reply with only: ok
     No code changes
     [Image #2] Reply with only: ok3
     No code changes
   ❯ (current)
   Enter to continue · Esc to cancel
```
  Up/Down move the `❯` cursor, which starts on `(current)`. I pressed Up twice to reach the 2nd prompt, then Enter. The confirm screen, exact text:
```
   Rewind
   Confirm you want to restore to the point before you sent this message:
   │ /tmp/…/spike/red.png Reply with only: ok
   │ (51s ago)
   The conversation goes back to before this message.
   The code will be unchanged.
   ❯ 1. Restore conversation
     2. Summarize from here
     3. Summarize up to here
     4. Never mind
```
- "1. Restore conversation" + Enter: the status was `idle` 61 ms later. The conversation view kept only prompt 1 and its reply.
  **The input box held the 2nd prompt's text verbatim**: `❯ /tmp/…/spike/red.png Reply with only: ok`. **No transcript record was written** (66 lines before and after).
- Ctrl+U ×4 emptied the box. A hint appeared: `Ctrl+Y to paste deleted text`. The new prompt `Reply with only: new4` was answered `new4`.
- Records after the new prompt: `file-history-snapshot` (normal, `isSnapshotUpdate:false`), then the `user` record with
  **`parentUuid` = `37929adb-…`, the `turn_duration` record of turn 1** (the last record of the kept turn; the same parent the old prompt 2 had),
  then attachments, then `{"type":"last-prompt","leafUuid":"00000000-0000-0000-0000-000000000000","explicit":true}`, then the reply,
  `stop_hook_summary` and `turn_duration`. On `/exit`: `{"type":"last-prompt","lastPrompt":"Reply with only: new4","leafUuid":"db5a7c16-…"}`,
  the new turn's `turn_duration`. No `rewound` or `cutUuid` fields were written.
- **The abandoned branch stays in the file:** prompt 2, prompt 3, their image companion records and their replies (lines 47–66) are unchanged.
  Two `user` records now share parent `37929adb`, so only the chain from the newest leaf identifies the live branch.

## E5. Command list via `-p` stream-json initialize

`claude -p --input-format stream-json --output-format stream-json --verbose --model haiku` was started in spike-cwd. stdin received only
`{"type":"control_request","request_id":"1","request":{"subtype":"initialize"}}`, and was closed after the reply.
- **Reply after 2.61 s; process exit 3.83 s after start** (1.2 s after stdin closed), rc 0, empty stderr. Two runs gave 2.608 and 2.612 s.
- **SessionStart hooks ran.** Before the reply, stdout carried `{"type":"system","subtype":"hook_started","hook_name":"SessionStart:startup",…}`
  and `hook_response` lines (`output`, `stdout`, `stderr`, `exit_code`, `outcome`), 4 hooks from user and plugin config.
  The slowest finished at 2.54 s, and the control_response followed at 2.61 s, so the reply waits on SessionStart hooks.
  A second run with an extra SessionStart command hook in `--settings` wrote its marker file.
- The `control_response.response` has keys `commands, agents, output_style, available_output_styles, user_output_styles_dir, models, account, pid,
  claude_code_version, current_permission_mode, hook_on_failure_supported, other_hook_ask_outranks_host, feedback_mode, analytics_disabled,
  remote_control_auto_enable, remote_control_auto_connect_default, remote_control_available, remote_control_auto_on_by_default, ide_rc_auto_enable_gate,
  fast_mode_state, fast_mode_disabled_reason, session_state, capabilities`, plus `pending_permission_requests:[]` and `pending_user_dialog_requests:[]`
  next to it. (`account` holds the user's email and org; not copied here.)
- **153 commands.** Each has the keys `name, description, argumentHint, aliases?, builtin?`. Samples:
  `{"name":"model","description":"Set the AI model for Claude Code","argumentHint":"<model>","builtin":true}`,
  `{"name":"clear","description":"Start a new session with empty context; previous session stays on disk (resumable with /resume)","argumentHint":"[name]","aliases":["reset","new"],"builtin":true}`,
  `{"name":"usage",…,"aliases":["cost","stats"],"builtin":true}`, `{"name":"effort","argumentHint":"<low|medium|high|xhigh|max|auto|ultracode [on|off]>","builtin":true}`.
  Skills and plugin commands come without `builtin`, e.g. `superpowers:brainstorming`.
  Built-ins present: agents, mcp, config, init, doctor, model, effort, compact, clear, context, usage, fast, rename, recap, goal, color, focus and more.
  **Absent:** plan, permissions, rewind, resume, btw, exit, status, memory, hooks, tasks, export, diff, login, logout, help, review, add-dir, ide, vim, theme.
- `models`: default/opus → claude-opus-5-5, fable → claude-fable-5-2, sonnet → claude-sonnet-5-5, haiku → claude-haiku-5-5, plus 9 pinned IDs.
- **No transcript file was created.** The process did register `~/.claude/sessions/<pid>.json` (`kind:"interactive"`, `entrypoint:"sdk-cli"`) for its 2 s lifetime.

## E6. Side question (`--resume <sid> --fork-session --no-session-persistence`)

I ran it against the live, idle session A from spike-cwd, adding `--output-format json` to get the result message:
`claude -p --resume 6c0a105f-… --fork-session --no-session-persistence --model haiku --tools "" --output-format json "In one line, what did I ask you first?"`
- **No flag was rejected. It worked and the answer was correct:** `Pick color (Red/Blue) and fruits (Apple/Pear) via AskUserQuestion.`
  (second run: `Your first request: pick a color (Red or Blue) and pick fruits (Apple or Pear, multi-select) via AskUserQuestion.`).
- Wall time: **8.87 s** without stdin redirect, including a 3 s wait that printed to stderr
  `Warning: no stdin data received in 3s, proceeding without it. If piping from a slow command, redirect stdin explicitly: < /dev/null to skip, or wait longer.`
  **6.2 s** with `< /dev/null`. The result reported `duration_ms` 1632 / 1931 and `ttft_ms` 1486 / 1663, so startup is about 4.3 s.
- Usage: **no cache hit** in either run. Run 1: `input_tokens 2, cache_creation_input_tokens 46531 (all ephemeral_1h), cache_read_input_tokens 0, output_tokens 119`, $0.0094.
  Run 2: `2 / 57674 / 0 / 191`, $0.0116. The prefix differs from the TUI's (no tools) and differed between the two runs.
- Each run reported a new `session_id` (855ebf7f…, 6d91c35b…). **No transcript file was created**, and the original transcript was byte-identical (md5 unchanged).
  The process did register `~/.claude/sessions/<pid>.json` with the **same cwd** as the TUI session (`kind:"interactive"`, `entrypoint:"sdk-cli"`, no `tmux` field) for about 4 s.

## E7. Background tasks in existing transcripts (read-only, 26 files in `~/.claude/projects/-mnt-d-git-claude-dashboard/`)

**Background Bash** (`Bash` input `run_in_background:true`; 27 found). The tool_result text:
`"Command running in background with ID: bo29v19cm. Output is being written to: <tmp> You will be notified when it completes — do not poll that file or sleep while waiting. Read the file only if the user asks for progress, or if you need a line it prints before you can go on, such as a server's ready line or port."`
with `toolUseResult:{"stdout":"","stderr":"","interrupted":false,"isImage":false,"noOutputExpected":false,"backgroundTaskId":"bo29v19cm"}`.
A foreground Bash that times out is moved to the background. Its content ends with `Command did not complete within its 120s timeout and was moved to the background (ID: bn8eabt5a). Output is being written to: …`,
and `toolUseResult` has `backgroundTaskId` plus `"timedOutAfterMs":120000`. Shell task IDs are `b` + 8 chars.

**Background agents.** `Agent` input has no `run_in_background` key (`description, prompt, subagent_type`); agents launch async by default here. The tool_result text:
`"Async agent launched successfully. (This tool result is internal metadata — never quote or paste any part of it, including the agentId below, into a user-facing reply.)\nagentId: a6cc68d2a048556bd (internal ID - do not mention to user. Use SendMessage with to: 'a6cc68d2a048556bd', summary: '<5-10 word recap>' to continue this agent.)\nThe agent is working in the background. … output_file: <tmp> …"`
with `toolUseResult:{"isAsync":true,"status":"async_launched","agentId":"a6cc68d2a048556bd","description":"…","resolvedModel":"claude-fable-5-2","prompt":"…",…}`.
A foreground agent's `toolUseResult` is `{"status":"completed","prompt":…}`. Agent IDs are 17 hex chars.
`SendMessage` result: `{"success":true,"message":"Resuming agent a5af72a","resumedAgentId":"a5af72abfef0e75e7","pin":{…}}`.

**Notifications** are always the same XML in a string:
```
<task-notification>
<task-id>…</task-id>
<tool-use-id>toolu_…</tool-use-id>
<output-file>…/tasks/<id>.output</output-file>
<status>completed|failed|killed|stopped</status>
<summary>Background command "<description or command>" completed (exit code 0)</summary>
[<note>…</note>] [<result>…agent final report…</result>]   (agents only)
</task-notification>
```
Summary forms seen:
- `Background command "…" completed (exit code 0)`
- `Background command "…" was stopped` (killed)
- `Agent "…" finished`, with the note `A task-notification fires each time this agent stops with no live background children of its own. … the same task-id may notify more than once.`
- `Agent "…" failed: Agent terminated early due to an API error: …`
- On resume: `<status>stopped</status><summary>Background shell command didn't finish before the previous session ended</summary><note>No completion record was found …</note>` (no output-file)
- Monitor events: `<task-notification><task-id>…</task-id><summary>Monitor event: "…"</summary><event>node-test exit=0</event></task-notification>`.

Carriers (counts over all files):
1. A `queue-operation` `{"operation":"enqueue","timestamp":…,"content":"<task-notification>…"}` is always written first (108).
2. When it runs as its own turn: a `user` record with **string** content = the XML, `origin:{"kind":"task-notification","producer":"session-task","runId":"…"}`,
   `promptSource:"system"`, `turnOrigin:"task_notification"`, `queueSkipAttachments:true`, `permissionMode` (59 completed, 2 stopped, 1 failed).
3. When absorbed mid-turn: `queue-operation` `{"operation":"remove","reason":"absorbed_mid_turn","commandUuid":…,"deliveryId":…,"content":…}`, plus an
   `attachment` `{"type":"queued_command","prompt":"<task-notification>…","source_uuid":…,"delivery_id":…,"commandMode":"task-notification","origin":{"kind":"task-notification","producer":"session-task"},"runId":…}`, and no `user` record.

**Kill and finish:**
- `TaskStop` tool_use `{"task_id":"bdg5dpb84"}` gives `toolUseResult:{"message":"Successfully stopped task: bdg5dpb84 (<command>)","task_id":…,"task_type":"local_bash","command":…}`.
  **No task-notification followed a TaskStop** (4 cases).
- `killed` notifications came from session exit (4 enqueued in the same second, never delivered, matching the live `/exit` case in E3) and from one stop made outside TaskStop, which was delivered as a `user` record.
- `attachment` `{"type":"task_status","taskId":"bw2ckxil8","taskType":"local_bash","description":"…","status":"running","deltaSummary":null,"outputFilePath":"…","shell":{"command":…}}`
  appears only right after a compaction (`This session is being continued from a previous conversation…`), restating tasks that are still running (3 cases).

## Side effects left behind

- `~/.claude.json`: trust entry for spike-cwd (`hasTrustDialogAccepted: true`), written by Claude Code when the trust dialog was accepted.
- `~/.claude/plans/make-a-2-step-plan-jazzy-patterson.md`, the plan file written by the model.
- `~/.claude/file-history/6c0a105f-e3bf-450c-b88e-48368ad38d50/`, and the empty dir `~/.claude/projects/…-spike-cwd/memory/`.
- `<tmp>`, the background-task output dir.
- In spike-cwd: `hello.txt` (hi) and `out.txt` (spike).
- The private tmux server, hook server, poller and pane watchers are stopped. No spike entries remain in `~/.claude/sessions/`.
