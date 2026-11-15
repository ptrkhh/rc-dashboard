# Dashboard-native chat, part 2: prompts and menus

Status: approved by the owner, 2026-11-13.
Code references are to `master` at `e3385c4` (Part 1 merged).

## Intent

In Part 1, whenever a session stops to ask something (a question, a plan to
approve), chat shows "Answer in Terminal to continue" and goes quiet. On a
phone that means leaving chat for a terminal key bar at exactly the moment
the agent needs a decision. Part 2 lets chat answer questions and plan
approvals itself, and adds the menus the Claude app has: slash commands and
pickers for model, effort and plan mode.

## Program context

Part 2 of the program in `2026-11-12-chat-core-design.md`, shipped in one
pull request with Parts 3 to 5 (owner decision, 2026-11-13). Sibling specs:
`2026-11-13-chat-attachments-design.md`,
`2026-11-13-chat-push-reliability-design.md`,
`2026-11-13-chat-later-tier-design.md`.

## Decisions made by the owner

From the Part 2 request:

- Any launch change keeps `--remote-control` working and does not break Open
  in Claude.
- Chat never types into a dialog it cannot see (the hub's `refusal()` and
  waiting state stay the guard).
- Answers target the specific request.
- Hooks arrive in this part for Claude (as the Part 1 spec said).
- Native Codex runs with `approvalPolicy: "never"`.

From the brainstorm (2026-11-13):

- **Claude model and effort pickers type `/model` and `/effort`,** knowing
  that Claude then also saves the choice as the default for new Claude
  sessions (`~/.claude/settings.json`). The picker says so. If Claude asks
  for confirmation ("Switch model?"), chat confirms it after checking the
  screen shows exactly that dialog.

From the review (2026-11-13):

- **Every tool keeps running with every action allowed,** as today: Claude
  in bypass mode, Codex with `approvalPolicy: "never"` and
  `danger-full-access`, OpenCode with `--auto`. Chat offers no permission
  modes and no permission cards. Chosen over opt-in asking modes, for less
  code.

## Goals

1. A card in chat for each question and plan approval, for all three tools,
   answered from chat. The first answer from any door wins, and the card
   closes everywhere.
2. A slash-command menu in the composer.
3. Model, effort and plan pickers (for OpenCode: model, variant and agent).

## Non-goals

- Permission prompts and permission modes. A permission dialog that appears
  anyway (for example after shift+tab in Terminal) keeps the "Answer in
  Terminal" banner.
- MCP elicitation forms (Claude `Elicitation`, Codex
  `mcpServer/elicitation/request`). They keep the "Answer in Terminal"
  banner.
- Answering from a notification (Part 4).
- Claude dialogs that no hook reaches, such as the `/exit` confirmation. They
  keep the banner. (The rewind selector is driven by Part 5 itself.)
- Cards for Claude sessions started before this change. They keep the
  banner, with a hint to restart the session.

## Anti-goals

- A key press into any dialog that chat has not confirmed on screen.
- An answer applied to a request other than the one the card showed.
- Breaking Remote Control or Open in Claude.
- Writing the user's settings files, except the `/model` and `/effort`
  default the owner accepted.

## User-visible behaviour

### Cards

When the session waits on something chat can answer, the Part 1 banner is
replaced by a card above the composer:

- **Question:** each question with its options:
  - buttons for single choice, checkboxes for multiple choice;
  - "Other" with a text box, where the tool allows free text;
  - **Submit** once every question is answered.
- **Plan approval:** the plan, rendered as Markdown and scrollable, with:
  - for Claude, **Approve** (the session goes back to bypass mode) and
    **Keep planning** with a text box for what to change;
  - for Codex, **Implement this plan?** (Yes / No), the same as Codex's
    TUI.

More behaviour:

- Several pending requests show as a stack; each is answered on its own.
- A request answered elsewhere (Terminal, the Claude app, another device)
  disappears from chat, and the conversation shows the outcome as usual.
- Answering a request that was answered elsewhere a moment before shows
  "Already answered".
- When the session waits on something chat cannot answer, the Part 1 banner
  shows, as today.

### Slash menu

- Typing `/` at the start of the composer opens a filtered list: name,
  description and argument hint. Typing narrows it; Enter or Tab completes
  `/name ` and leaves the cursor for arguments.
- Commands that would open a TUI dialog are not listed. Typed anyway, chat
  refuses with a pointer to its own equivalent: the pickers, the usage
  sheet, rewind or side chat (Part 5).

### Pickers

- The chat header shows a chip with the model and effort, plus "Plan" while
  plan mode is on (for example "Opus · high · Plan"). Tapping it opens a
  sheet with the pickers.
- **Claude Code:**
  - model: the aliases Claude reports (default/opus, fable, sonnet,
    haiku);
  - effort: low, medium, high, xhigh, max, auto;
  - a Plan switch.
  - The model and effort pickers carry the note "Also becomes your default
    for new Claude sessions".
  - All three act only while the session is idle. During a turn the sheet
    says "Stop the turn or wait for it to finish". Shift+tab and Enter are
    answer keys inside Claude's dialogs, and a dialog can open between a
    check and a key press (see "Author's calls" 5).
- **Codex:**
  - model and effort: from `model/list`, with each model's own effort
    levels;
  - a Plan switch.
  - Changes apply from the next turn.
- **OpenCode:**
  - model, and variant (OpenCode's effort);
  - agent (build, plan, and any other visible primary agent).

## How it works

### Cards in the hub

- The hub's state gains `asks`: a list of
  `{id, kind: "question"|"plan", tool, …card content…}`. Asks are state, not
  events: they are transient and every client must see the current set.
- A new client frame carries the answer:
  `{"t":"answer","id":…,"choice":…,"text":…,"answers":…}`. The hub passes it
  to `Engine::answer(id, Answer)` and replies
  `{"t":"answered","id":…,"ok":bool,"error"?:…}`.
  - The id is the engine's own key for that one request, so an answer can
    only reach the request its card showed.
  - An id that is no longer pending gets "already answered".
- `refusal()` keeps refusing to type while the session waits. Its message is
  "answer the card first" when an ask is pending, and "answer in Terminal
  first" otherwise.
- The Part 1 banner is shown only when the session waits and no ask arrives
  within 500 ms. Claude's status file turns `waiting` about 9 ms before the
  hook request arrives, so this avoids a flash.

### Claude Code: the permission hook

Claude routes questions and plan approvals through its `PermissionRequest`
hook, even in bypass mode, so that hook is how chat answers them.

**Launch change.**

- `spawn_claude` adds a `PermissionRequest` hook to the `--settings` JSON it
  already passes:

  ```json
  {"hooks":{"PermissionRequest":[{"hooks":[{"type":"http",
    "url":"http://127.0.0.1:<hook port>/hook/claude?pane=<tmux name>",
    "headers":{"Authorization":"Bearer $CDASH_HOOK_TOKEN"},
    "allowedEnvVars":["CDASH_HOOK_TOKEN"],
    "timeout":604800}]}]}}
  ```

- `--remote-control` and every other argument stay as they are. Claude merges
  hooks across settings levels, so the user's own hooks keep running.
- The token is 32 random hex characters per launch. It is passed as the
  session's environment (`tmux new-session -e`) and stored as the session
  option `@cdash_hook`, so it never appears in the long-running `claude`
  command line, and an agent restart can still check it. It does appear,
  for a few milliseconds, in the arguments of those two `tmux` calls.
- **The hook has its own listener,** on `127.0.0.1` at a fixed port:
  `CDASH_HOOK_PORT`, default 23275. It is not on the dashboard's port,
  because that port does not fit:
  - with `CDASH_BIND` set to a LAN, Tailscale or `::1` address, nothing
    listens on 127.0.0.1;
  - the Linux and macOS desktop apps run the agent on a random port (port 0),
    so panes that outlive the app would post to a dead port, or to whatever
    process took it next;
  - a tunnel that forwards to the dashboard's port would make the hook
    reachable from outside.
- The hook listener carries nothing but the hook route, so the login guard is
  not involved.
- If the hook port is taken (for example two agents on one host), the agent
  logs it and launches Claude without the hook. Those sessions get the
  banner, as before.

**Verified on 2.1.323 (2026-11-13, live TUI).**

- The hook fires for AskUserQuestion and ExitPlanMode in bypass mode, and
  for Write and Bash prompts in the asking modes.
- The request arrives 5–46 ms before the dialog is drawn. The status file
  turns `waiting` about 9 ms before that, with `waitingFor` set to
  `input needed` (questions) or `permission prompt` (permissions and
  plans).
- A hook answer closes the dialog in 65–160 ms. Then:
  - a question answer reaches the model;
  - a multiple-choice answer sent as an array is stored as "Apple, Pear";
  - a plan approval with `setMode` changes the mode;
  - a plan denial with a message makes the model revise the plan and ask
    again.
- **Answered in the TUI first, the hook's HTTP request stays open** until
  its own timeout. A late hook answer is accepted without error and does
  nothing.
- The request carries `prompt_id`, `tool_name`, `tool_input` and
  `permission_mode`. It carries no `tool_use_id`. The `tool_use` line
  reaches the transcript about 130 ms after the hook request.
- **For a plan, the transcript's ExitPlanMode input can be stale:** when the
  model writes the plan file and calls ExitPlanMode in one message, the
  record holds the previous plan, while the hook gets the new one. Cards
  are built from the hook's input only.

**The endpoint.** `POST /hook/claude?pane=<name>` on the hook listener.

- It is refused unless the pane is live and the bearer token equals the
  pane's `@cdash_hook` (compared in constant time).
- The body limit is 1 MB.

**Only questions and plans from real dialogs become cards.**

- A request for any tool other than AskUserQuestion and ExitPlanMode (a
  permission prompt, after the mode was changed in Terminal) is answered
  empty at once. Claude then shows its own dialog, and chat shows the
  banner.
- Claude also calls the hook for a call that cannot show a dialog, such as
  some subagent calls, and otherwise denies them automatically. A
  main-session dialog turns the status file `waiting` about 9 ms before its
  hook request arrives. So a request becomes a card only if the status file
  shows `waiting` (`input needed` or `permission prompt`) within 300 ms of
  its arrival. Otherwise the agent answers it empty at once, and Claude does
  what it does today.
- If the request names a subagent (the builder checks the input for an
  agent field on 2.1.323), that settles it without waiting.

For a card, the agent finds the pane's hub (`hub_for`), adds an ask built
from the request, and holds the HTTP response until one of these happens:

- **Chat answers.** The response is the decision JSON (below).
- **The request is settled elsewhere.** The response is `200` with an empty
  body, which means no decision. Settled means:
  - the ask is matched to its `tool_use` (same `prompt_id` as the record's
    `promptId`, same tool name, no result yet; when several match, the one
    with equal input, else the oldest), and that `tool_use` gets its
    `tool_result`. Input is only a tie-breaker because an ExitPlanMode
    record can hold the previous plan (see above); or
  - the status file has not shown `waiting` for 2 s, as a safety net; or
  - Claude closes the connection. The builder checks whether it does on
    deny or Esc in the TUI (not observed on 2.1.323); the transcript match
    covers both cases either way.
- **The hub ends.** The response is empty.

**Decisions.**

| Card answer | Hook output (`hookSpecificOutput.decision`) |
|---|---|
| Question | `{"behavior":"allow","updatedInput":{"questions":<echo>,"answers":{<question text>: <label, [labels], or free text>}}}` |
| Approve | `{"behavior":"allow","updatedInput":{"plan":<echo>},"updatedPermissions":[{"type":"setMode","mode":"bypassPermissions","destination":"session"}]}` |
| Keep planning | `{"behavior":"deny","message":<text or "Keep planning.">}` |

**Waiting without a card.** When the status file shows `waiting` with
`waitingFor: "dialog open"`, or `waiting` with no ask after 500 ms, chat
shows the Part 1 banner. A pane without `@cdash_hook` (started before this
change) also gets "Restart the session to answer from chat."

### Claude Code: pickers

**Model and effort.**

- The picker sends `/model <alias>` or `/effort <level>` through Part 1's
  paste path, only while the session is idle and not waiting, and only
  values from the lists below. `/model <x>` and `/effort <x>` typed in the
  composer take the same path.
- After Enter, the engine watches for up to 2 s:
  - if the status file shows `waiting` with `dialog open` and the pane
    shows the confirmation that command raises, it presses that dialog's
    "yes" key after checking its text. `/model` can raise "Switch
    model?" when the prompt cache may be warm. `/effort` raised no
    confirmation on 2.1.325;
  - the command's own result also reads `waiting` with `dialog open` for
    about 0.2 s, so only a dialog still open when the watch ends counts:
    it presses Escape and reports failure.
- The builder records both confirmations' exact text on a real session,
  backing up `~/.claude/settings.json` first and restoring it after,
  because `/model` and `/effort` rewrite it.
- **Lists:** model aliases come from the `models` of the cached `initialize`
  reply (see the slash menu below). Effort levels are low, medium, high,
  xhigh, max and auto.
- **Current values:** the latest assistant record's `message.model`, and its
  effort field. Plan is on when the latest `permissionMode` in the
  transcript (prompt records and `permission-mode` lines) is `plan`; a
  hook request and the footer read by the menu update it too.

**Plan.** Claude has no command that leaves plan mode, so the engine presses
shift+tab (`BTab`) and reads the footer after each press. The footer changes
within one 50 ms frame. Measured on 2.1.323, starting from
`--dangerously-skip-permissions`, the cycle is:

| Mode | Footer starts with |
|---|---|
| bypassPermissions | `⏵⏵ bypass permissions on` |
| auto | `⏵⏵ auto mode on` |
| default | `⏸ manual mode on` |
| acceptEdits | `⏵⏵ accept edits on` |
| plan | `⏸ plan mode on` |

The Plan switch's target is plan (on) or bypassPermissions (off). The
engine:

- reads the footer when the sheet opens, to show whether plan mode is on;
- presses until the target's text shows, at most five presses;
- refuses unless the status is `idle` or `shell` (idle with a background
  shell, Part 4), and when the footer is not recognised.

The `permissionMode` field on prompt records lags, and `permission-mode`
records are written only in later batches, so neither is used for the live
mode.

### Claude Code: slash menu

- **Source.** The agent runs, in the session's folder,
  `claude -p --input-format stream-json --output-format stream-json --verbose --settings '{"disableAllHooks":true}'`.
  It writes one `initialize` control request and closes stdin after the
  reply. Hooks are off so that the owner's SessionStart hooks do not run,
  unseen, in the project folder for a menu. Measured on 2.1.323:
  - with hooks on, the reply comes after 2.6 s, because it waits for the
    SessionStart hooks; with hooks off it comes after 0.6 s, with the same
    153 commands (re-checked 2026-11-13);
  - it lists 153 commands `{name, description, argumentHint, aliases?,
    builtin?}` (skills and plugin commands included), plus `models`;
  - it writes no transcript;
  - it does register a session file for about 2 s, which the collector must
    ignore (as for Part 5's side questions).
- **Caching.** The reply is cached per project folder for the agent's life,
  and refreshed in the background when the menu opens and the copy is over
  an hour old.
- **Hidden** (they open a dialog or picker in the TUI): model, effort, config,
  permissions, resume, rewind, usage (cost, stats), status, tasks, mcp,
  plugin, hooks, agents, memory, export, login, logout, theme, and diff (a
  full-screen panel). Typed bare, chat refuses them with a pointer.
  `/model x` and `/effort x` go through the pickers' path. `/btw` is Part
  5's side chat. `/plan` is the Plan switch.

### Codex

- **Launch stays `approvalPolicy: "never"`** with `danger-full-access`, so
  Codex sends no command, file or permission approvals.
- **Plan switch:** `thread/settings/update` (experimental, already enabled
  on the control client) with `collaborationMode {mode: "plan"|"default",
  settings: {model, reasoning_effort, developer_instructions: null}}`.
- **Questions.** The supervisor's control client (`codex-remote.mjs`)
  already receives every server request, and the first response from any
  client wins. It forwards `item/tool/requestUserInput` as a question card:
  - `isOther` adds free text, sent as `user_note: <text>` the way Codex's
    TUI does;
  - `isSecret` uses a password field.

  The ask id is the JSON-RPC request id, so an answer can only reach that
  request. On `serverRequest/resolved` the supervisor drops the ask, and
  Codex's TUI closes its own prompt the same way. Questions from any thread,
  subagents included, become cards: server requests are not filtered by
  thread (Part 4, finding 8). Any other server request keeps the banner.
- **Implement this plan?** After a turn in plan mode ends with a `plan` item,
  chat shows the card:
  - Yes switches `collaborationMode` to default and sends
    "Implement the plan." with `turn/start`, as Codex's TUI does;
  - No dismisses it.
- **Model and effort:** `model/list` gives models with their
  `supportedReasoningEfforts`. A choice is applied with
  `thread/settings/update {model, effort}`.
- **Slash menu:** Codex has no command-list API. The menu offers what the
  app server can do, and the supervisor runs each when a message starts with
  it:
  - `/compact`: `thread/compact/start`;
  - `/review`: `review/start` on uncommitted changes;
  - `/plan`: the Plan switch;
  - `/model` and `/effort`: the pickers;
  - skills from `skills/list`, sent as `{type:"skill", name, path}` input.
  Other `/names` are refused with "use Terminal for this command".

### OpenCode

- **The native TUI keeps `--auto`.** It answers permissions only; question
  forms stay unanswered, so they need cards.
- **Questions** arrive as forms. `form.created` becomes a question card:
  - a `string` field with options shows buttons plus free text;
  - a `multiselect` field shows checkboxes.
  The answer is `POST …/form/:formID/reply {answer: {q0: …, q1: […]}}`. A 409
  means "already answered". `form.replied` and `form.cancelled` drop the
  card.
- **Pickers:**
  - agent: `GET /api/agent`, keeping primary agents that are not hidden,
    then `POST /api/session/:id/agent`;
  - model and variant: `GET /api/model`, then
    `POST /api/session/:id/model {model: {providerID, id, variant}}`.
- **Slash menu:** `GET /api/command`. A sent message that starts with
  `/<name>` matching a listed command goes to
  `POST /api/session/:id/command {name, text}`; any other text goes as a
  prompt, as today.

### Wire summary

- **Client to agent:**
  - `answer` (above);
  - `{"t":"set","what":"model"|"effort"|"plan"|"agent"|"variant","value":…}`
    (`plan` takes `true` or `false`), replied with
    `{"t":"set","ok":bool,"error"?}`;
  - `{"t":"menu"}`, replied with `{"t":"menu","commands":[…],"models":[…],"efforts":[…],"agents":[…]}`.
- **Hub state gains:**
  - `asks`;
  - `settings {model, effort, plan, agent?, variant?}`;
  - `hooks: bool` (false for a Claude pane started before this change).

## Security

- **The hook route.**
  - It is open only to loopback peers.
  - It needs a per-launch token that sits in the session's environment and
    a tmux option, never in a long-running command line. Another local user
    who catches it in the brief `tmux` call arguments can at most add a
    card (below); accepted risk.
  - It only names live panes, and its body is capped at 1 MB.
  - A forged request can at most add a card whose answer goes back to the
    forger. It cannot answer a real request.
- **Answers** are keyed by the engine's own request ids, so a stale or
  foreign id is refused.
- **Pickers** type only values taken from the tool's own lists, as
  `/model <alias>` or `/effort <level>`. Free text never reaches the TUI
  through a picker.
- **The Claude key presses in this part** (shift+tab, the model-switch
  confirmation, Escape) each follow a screen check, keeping Part 1's rule.

## Author's calls

1. **An HTTP hook, not a command hook.** No binary path has to stay valid
   across agent upgrades. If the agent is unreachable, the hook fails open
   to the TUI dialog, which falls back to the banner.
2. **A token in the environment plus a tmux option,** rather than in the URL,
   so `ps` shows it only during the two brief `tmux` calls.
3. **A seven-day hook timeout.** A dialog can wait as long as the owner does.
   An agent restart drops held hooks; those dialogs fall back to the banner.
4. **Claude's command list comes from a headless `initialize`,** cached,
   because it includes skills and plugin commands that a file scan would
   miss. With hooks off it costs 0.6 s once per project folder, in the
   background, and runs none of the owner's hooks.
5. **Every Claude picker acts only while idle,** the Plan switch included.
   Shift+tab and Enter answer Claude's dialogs (in the plan dialog
   shift+tab means "approve with this feedback"), and during a turn a dialog
   can open between the check and the key press. The status file turns
   `waiting` only about 10 ms before a dialog is drawn. While idle, no
   dialog opens unprompted. The cost is that plan mode cannot be switched
   during a turn from chat; Stop first, or use Terminal.
6. **Settling a Claude ask uses the transcript first** and the status file as
   a 2 s safety net, because a TUI answer leaves the hook request open.
7. **Cards are hub state, not log events,** so reconnecting clients always
   see exactly the pending set.
8. **A dedicated hook listener on 127.0.0.1:23275** (`CDASH_HOOK_PORT`), not
   the dashboard's port. It keeps working with a non-loopback bind and with
   the desktop app's random port, and no tunnel reaches it.
9. **Only questions and plans that come with a dialog become cards.** "The
   status turns `waiting` within 300 ms" is the measured mark of a real
   dialog. A request without a dialog (some subagent calls) is answered
   empty at once, so behaviour stays as it is today instead of a card that
   flashes or a subagent that stalls.
10. **Chat confirms "Switch model?" raised by the command it typed.** The
    owner's choice covered the model and effort pickers; `/effort` turned out
    to apply without a confirmation on 2.1.325.

## Left to the builder

- Card, menu and sheet layouts, and the exact strings.
- The model-switch confirmation's text and key (recorded on a real session,
  with the settings file restored).

## Acceptance checks

- **Unit tests:**
  - the hook route refuses a non-loopback peer, a wrong token, an unknown
    pane and an oversized body, and answers a request for any other tool
    empty at once;
  - each decision row above serialises exactly as shown;
  - ask matching by `prompt_id` and tool name, with input as tie-breaker,
    including an ExitPlanMode record that holds the previous plan, and
    settling by result, by status and by disconnect;
  - Codex and OpenCode supervisor tests with fake servers: a question
    becomes an ask, an answer becomes the right reply, and a resolution
    elsewhere drops the ask;
  - the Claude footer parser on the five strings above.
- **Real sessions on this host:**
  - **Claude:**
    - an AskUserQuestion with a multiple-choice question is answered from
      chat;
    - the Plan switch turns plan mode on and off, and the footer agrees;
    - a plan is approved from chat, and the session is back in bypass mode;
    - a request answered in Terminal disappears from chat within a second,
      including a plan written in the same message as its ExitPlanMode;
    - Open in Claude still connects, and a request answered in the Claude
      app also disappears from chat.
  - **Codex:** with the Plan switch on, a plan-mode question is answered
    from chat, answering it closes the TUI's prompt, and Yes on "Implement
    this plan?" starts the implementing turn.
  - **OpenCode:** a question form is answered from chat, and permissions
    are still approved by `--auto`.

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
