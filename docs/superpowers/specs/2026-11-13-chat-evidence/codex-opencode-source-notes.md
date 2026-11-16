# Codex app-server 0.167.0 facts (research agent, 2026-11-13)

Schemas: scratchpad/codex-schema/{stable,experimental,ts-experimental}/. Source: scratchpad/codex-src-0.167. "(exp)" = needs initialize capabilities.experimentalApi:true.

1. Approvals (ServerRequest.ts)
- item/commandExecution/requestApproval {threadId, turnId, itemId, startedAtMs, kind: command|humanCommand|writeStdin, approvalId?, environmentId, reason?, networkApprovalContext?{host,protocol}, command?, cwd?, commandActions?, additionalPermissions?, proposedExecpolicyAmendment?: string[], proposedNetworkPolicyAmendments?, availableDecisions?} → {decision: "accept"|"acceptForSession"|{acceptWithExecpolicyAmendment:{execpolicy_amendment}}|{applyNetworkPolicyAmendment:{network_policy_amendment}}|"decline"|"cancel"}. Offer only availableDecisions; humanCommand: accept/decline/cancel only.
- item/fileChange/requestApproval {threadId, turnId, itemId, startedAtMs, reason?, grantRoot?} → {decision: accept|acceptForSession|decline|cancel}.
- item/permissions/requestApproval {threadId, turnId, itemId, environmentId, startedAtMs, cwd, reason, permissions:{network,fileSystem}} → {permissions (granted subset), scope: turn|session, strictAutoReview?}; decline = permissions:{}.
- Legacy v1: execCommandApproval, applyPatchApproval.
- serverRequest/resolved {threadId, requestId}.
- Multi-client: every connection subscribed to thread gets request, same id. First response wins; later dropped silently. All subscribers get serverRequest/resolved (incl. answerer). Pending requests replayed to late joiner via thread/resume. TUI closes prompt on serverRequest/resolved. humanCommand only to clients with requiredHumanApproval (exp).
- approvalPolicy never: no command/file approvals; MCP elicitations auto-declined.

2. Questions
- item/tool/requestUserInput {threadId, turnId, itemId, questions:[{id, header, question, isOther, isSecret, options:[{label,description}]|null}], isBlocking} → {answers:{[questionId]:{answers: string[]}}}. No multi-select. isOther: client adds free text; TUI sends [label] | [label, "user_note: <text>"] | ["None of the above", "user_note: <text>"]. isBlocking true in Plan mode. 1–3 questions, 2–3 options. Tool exists only in Plan mode unless feature default_mode_request_user_input.
- MCP: mcpServer/elicitation/request {threadId, turnId, serverName, mode: form{message, requestedSchema}|url{url, elicitationId}|…} → {action: accept|decline|cancel, content, _meta}.

3. Plan mode: collaborationMode:{mode:"plan"|"default", settings:{model, reasoning_effort, developer_instructions:null}} on turn/start (exp) or thread/settings/update; persists. collaborationMode/list (exp). Output item {type:"plan", text} streamed by item/plan/delta. turn/plan/updated {explanation, plan:[{step,status}]} = checklist. NO server-side plan approval: TUI "Implement this plan?" just sends turn/start "Implement the plan." in Default mode (or new thread with plan).

4. Settings: turn/start overrides (this + later turns): model, effort, summary, approvalPolicy, sandboxPolicy{type}, approvalsReviewer, cwd, serviceTier, collaborationMode/permissions (exp). thread/settings/update (exp) same fields, no turn, returns {}. turn/settings/update (exp) {threadId, turnId, model?, effort?} running turn only → {status: applied|targetUnavailable}. Settings belong to thread, saved in rollout. thread/settings/updated (exp) to all subscribers {threadId, threadSettings:{cwd, approvalPolicy, approvalsReviewer, sandboxPolicy, model, modelProvider, effort, summary, collaborationMode, serviceTier…}}. --remote TUI applies updates, re-sends full settings each turn it starts.

5. Slash commands (tui/src/slash_command.rs, 62): model, presets, daybreak, ide, permissions, keymap, vim, setup-default-sandbox, experimental, approve, memories, skills, import, hooks, review, rename, new, archive, delete, resume, fork, worktree, app, init, compact, config, recap, plan, voice, goal, agents, side, btw, copy, export, raw, tui, diff, mention, status, daemon, warnings, cd, pwd, usage, debug-config, title, statusline, theme, pets, mcp, apps, plugins, logout, quit, exit, feedback, ps, stop, clear, subagents. Descriptions in description(). App-server: compact=thread/compact/start; review=review/start; rename=thread/name/set; model/permissions/plan=settings; fork/resume=thread/*; side/btw=ephemeral thread/fork + turn/start; goal=thread/goal/*; skills=skills/list; !cmd=thread/shellCommand. No API lists custom prompts/presets.

6. Images: UserInput variants (start & steer): text{text,text_elements}, image{url|fileId, detail?}, localImage{path}, audio, localAudio, skill{name,path}, mention. Text cap 1,048,576 chars. http(s) image URLs rejected; use data URL. localImage read on app-server host. Resized ≤2048px. Live item/* echo input incl full data URL (omit_app_server_notification_media off by default). thread/turns/list rebuilds content as text, then image{url} (data URL kept), then localImage{path}; interleaving lost.

7. Rewind: thread/rollback REMOVED (absent in 0.162.1 too). thread/revert {threadId, beforeTurnId} → {thread, turnsBackwardsCursor, itemsBackwardsCursor}: drops that turn + later from history, files untouched; paginated non-ephemeral threads only; keeps subscriptions; ends active turn, cancels pending requests, broadcasts thread/reverted {threadId}; clients re-read thread/turns/list. TUI double-Esc calls thread/revert; only a turn's first prompt editable; remote mode can't restore prompts with local images. thread/fork {threadId, lastTurnId?|beforeTurnId?, ephemeral?}.

8. Usage: thread/tokenUsage/updated {threadId, turnId, tokenUsage:{total, last:{inputTokens, cachedInputTokens, outputTokens, reasoningOutputTokens, totalTokens}, modelContextWindow}} (replayed on resume). account/rateLimits/read → {rateLimits:{primary/secondary:{usedPercent, resetsAt}}}; account/rateLimits/updated partial.

9. Diffs: turn/diff/updated {threadId, turnId, diff} whole-turn unified diff (replace on update). item/fileChange/patchUpdated {itemId, changes:[{path,kind,diff}]}.

10. Other: review/start {threadId, target: uncommittedChanges|baseBranch|commit|custom}; contextCompaction item; thread/name/updated; thread/status/changed {status: active{activeFlags: waitingOnApproval|waitingOnUserInput}} = clean waiting signal; thread/queue/* (exp); thread/sample (exp, one-shot side question, new since 0.162.1). 0.162.1 has same decisions, thread/revert, thread/settings/update/updated.

# OpenCode 2.2.6 API facts (research agent, 2026-11-13)

OpenAPI: scratchpad/opencode-openapi.json (GET /openapi.json). Source: scratchpad/src/opencode-2.2.6.

- `--auto` is TUI-only: TUI replies {decision:"once"} to every pending permission of the session and subagents. Forms not answered. Dashboard sees permission.asked then permission.replied; loses race.
- Default permissions: `*:*=allow`; reading *.env / *.env.* asks. build/plan add question=allow; plan denies edit outside ~/.opencode/plan/*. Last matching rule wins; no match = ask. Owner's opencode.json has "permission":"allow". `POST /api/session {permissions:[...]}` / `PATCH /api/session/:id {permissions}` can force ask.
- Permissions: item `{id:"per…", sessionID, action, resources[], save?[], metadata?, source?:{type:"tool",messageID,id}, message?}`; list `GET /api/session/:id/permission` → {data}. Event permission.replied `{sessionID, requestID, reply}`. Reply `POST /api/session/:id/permission/:requestID/reply {decision:"once"|"always"|"reject", message?}` → 204. First reply wins; second → 404. reject rejects all pending in session; with message model continues with feedback.
- Forms: events form.created {form}, form.replied {id, sessionID, answer}, form.cancelled {id, sessionID}. Form `{id:"frm_…", sessionID, title, metadata?, fields[]}`; field types string/number/integer/boolean/multiselect/external; key, title?, description?, required?, when?; string & multiselect have options + custom. Question tool: title "Questions", metadata {kind:"question", tool:{messageID,id}}; field key "q<i>", title=header, description=question, type string or multiselect, options [{value:label,label,description}], custom:true, not required. List `GET …/form`; `GET …/form/:formID` adds state. Reply `POST …/form/:formID/reply {answer:{q0:"text", q1:["X","free"]}}` → 204; 400 invalid; 409 already settled. Cancel `DELETE …/form/:formID?message=…` → 204.
- Agents: `GET /api/agent` → {id,name,description?,mode,hidden,model?,permissions}; build(default), plan, general, explore. `POST /api/session/:id/agent {agent}` → 204 (unknown IDs also 204: validate first). Event session.agent.selected; history row agent-switched. No per-prompt agent.
- Model: `POST /api/session/:id/model {model:{providerID,id,variant?}}` → 204, unchecked. Event session.model.selected; history row model-switched. Lists GET /api/model (variants[{id}], capabilities.input[], limit.context, cost[]), GET /api/model/default. No defaultVariant in schema.
- Commands: `GET /api/command` → [{name, description?}] (init, review, goal + custom .md + MCP prompts). Run `POST /api/session/:id/command {name, text, files?, agents?, skills?, delivery?}` → 204. TUI-only: undo, redo, compact, fork, rename, models. Compact `POST …/compact`. Fork `POST …/fork {before?}`. Title `PATCH /api/session/:id {title?}`.
- Files: /prompt `files:[{uri, source?, name?, description?, mention?}]`, uri = data:…;base64 (type sniffed) | file:///abs[?start&end] | media:. 20 MiB/file; images resized. History user row `files:[{data:<base64>, mime, source:{type:"inline"}|{type:"uri",uri}, name?}]` (inline base64 unless X-OpenCode-Capabilities: experimental-media-uri). One big image can exceed opencode-api.mjs 8 MiB response cap.
- Revert: `POST …/revert/stage {messageID, files?}` → {messageID, snapshot?, files:FileDiff[]} restores files after messageID, messages stay until commit (event session.revert.staged). `POST …/revert/commit` deletes messages from messageID inclusive (session.revert.committed {to}); next prompt/compact also commits. `DELETE …/revert` undoes (session.revert.cleared). All 409 while busy. TUI /undo: interrupt, POST /api/experimental/session/:id/wait, stage last user message.
- Usage: session cost, tokens{input,output,reasoning,cache{read,write}}; assistant rows cost?, tokens?, finish?. Events session.step.ended, session.usage.updated. Diff `GET …/diff?from&to&context` → [{file,patch,additions,deletions,status}]; `GET /api/vcs/diff?mode=working|branch|committed`.
- Shell `POST …/shell {id?, command}`; inbox `GET …/inbox`, `PATCH …/inbox/:id {delivery}`, `DELETE …/inbox/:id`; delivery:"queue" exists. `POST …/interrupt?resume=` → {interrupted}. /api/fs/find|list|read. SSE `?types=` filter.
- Side effect: research server left a project record for the scratchpad folder in OpenCode DB.

# P5: how Codex 0.167 app-server and OpenCode 2.2.6 expose RC Dashboard chat data (research agent, 2026-11-13)

This is read-only source research: no live sessions were used and no model was called. It adds to research-codex.md and research-opencode.md without repeating them.

Path prefixes (relative to the scratchpad):
- `cx/` = codex-src-0.167/codex-rs/
- `ts/` = codex-schema/ts-experimental/
- `P/` = src/opencode-2.2.6/packages/

Conventions:
- "(exp)" marks a Codex method that needs `initialize.capabilities.experimentalApi: true`. The dashboard's control client already sets it.
- Some OpenCode routes are location-scoped: `/api/shell`, `/api/vcs/*`, `/api/model`, `/api/agent`, `/api/command`, `/api/skill` and `/api/experimental/usage/limits`. They answer `{location, data}` and accept `?location[directory]=` (P/schema/src/location.ts:34-36). `/api/event` covers all locations.

## Corrections to the earlier notes
- Codex `TokenUsageBreakdown` also has `cacheWriteInputTokens`.
- OpenCode `GET /api/session/:id/diff` returns `{data: FileDiff[]}`, not a bare array.
- The OpenCode TUI has `/btw`, built on `POST /api/session/:id/generate`.
- OpenCode `GET /api/model` entries have no `defaultVariant`, so the repo's `normalizeModels` always gets null (crates/agent/src/collect/opencode-api.mjs:133).

## Most important for the dashboard
1. **Codex subagent events reach the dashboard unfiltered.** Every initialized connection is attached to spawned subagent threads, so their `turn/*` and `item/*` notifications reach the dashboard. codex-remote.mjs does not filter on `params.threadId` (lines 68-69 and 538-548). A child's turn can toggle `working`, replace the `turnId` used for steer and interrupt, and leak child items into the chat.
2. **Running shells:**
   - Codex: `thread/backgroundTerminals/list` (exp), plus `item/started` and `item/completed` for commandExecution items with `source:"unifiedExecStartup"`.
   - OpenCode: `GET /api/shell`, plus the `shell.created`, `shell.exited` and `shell.deleted` events.
3. **Running subagents:**
   - Codex: `thread/list {ancestorThreadId}` or `thread/loaded/list` + `thread/read`, with `thread/status/changed` broadcast live.
   - OpenCode: intersect `GET /api/session?parentID=<sid>` with `GET /api/session/active`; live signals are `session.created` and `session.execution.*`.
4. **Checklist:**
   - Codex: `turn/plan/updated` is live only. It is missing from thread/turns/list and not replayed on resume.
   - OpenCode: `todowrite` is behind the `todo` feature flag, which is off in release builds. There is no todo API or event.
5. **Side chat:**
   - Codex `/side` and `/btw`: `thread/fork {ephemeral:true}`, then `thread/inject_items` with a boundary message, then `turn/start`. Cleanup is `turn/interrupt` + `thread/unsubscribe`.
   - Codex `thread/sample` (exp) is a one-shot, tool-less alternative that needs an idle parent.
   - OpenCode `/btw` is `POST /api/session/:id/generate`: one call with no tools, history untouched, cost not counted.
6. **Rewind:**
   - Codex: the TUI's double-Esc calls `thread/revert` with `beforeTurnId` = the turn that starts with the selected prompt, then puts the prompt back in the composer. The remote TUI ignores reverts made by other clients.
   - OpenCode: while a revert is staged, history still returns every row, so clients hide ids ≥ `session.revert.messageID`. `/undo` puts the prompt back; `/redo` clears the whole staged revert.
7. **Images:**
   - Codex: the `--remote` TUI sends full-resolution data URLs (up to 32 MiB per message). They appear in item events and in `thread/turns/list`.
   - OpenCode: rows keep up to 5 MiB of base64 per image. `session.inbox.enqueued` always carries the full base64, which can trip the repo's 1 MiB SSE cap.
   - Neither server has a thumbnail route, so the agent must strip images and make thumbnails itself.
8. **Usage:**
   - Codex: no per-turn cost, and model/list has no context window; use `tokenUsage.modelContextWindow` instead.
   - OpenCode: cost and tokens per session and per step, `limit.context` per model, and no provider rate-limit data.

## 1. Running shells and agents

### Codex: background terminals (unified exec, TUI /ps)
- List call: `thread/backgroundTerminals/list` (exp) {threadId, cursor?, limit?} → {data:[{itemId, processId, command, cwd, osPid, cpuPercent, rssKb}], nextCursor}. cx/app-server-protocol/src/protocol/common.rs:777-782, v2/thread.rs:1258-1291.
  - The thread must be loaded. osPid, cpuPercent and rssKb are always null in 0.167 (cx/app-server/src/request_processors/thread_processor.rs:2613-2642).
  - It returns only processes that have not exited, sorted by processId; the cursor is the last processId (thread_processor.rs:6865-6889; cx/core/src/unified_exec/process_manager.rs:1838-1855).
  - itemId is the id of the commandExecution item that started the process. A thread holds at most 64, LRU-pruned (cx/core/src/unified_exec/mod.rs:75, process_manager.rs:1730).
- No notification fires when the list changes. Track these signals instead:
  - Start: `item/started` with item {type:"commandExecution", source:"unifiedExecStartup", processId, command, cwd, status:"inProgress"} (ts/v2/ThreadItem.ts, ts/v2/CommandExecutionSource.ts; cx/core/src/tools/events.rs:117,179). Plain agent commands have source "agent"; `!cmd` has "userShell".
  - End: `item/completed` for the same item id (status completed|failed, exitCode, aggregatedOutput, durationMs). An exit watcher sends it when the process exits, so it can arrive after `turn/completed` and still carry the original turnId (cx/core/src/unified_exec/async_watcher.rs:155-240).
  - In between: `item/commandExecution/terminalInteraction` {threadId, turnId, itemId, processId, stdin} for write_stdin (empty stdin means a poll), and `item/commandExecution/outputDelta` {threadId, turnId, itemId, delta}.
  - `process/outputDelta` and `process/exited` belong only to the client's own `process/spawn`, not to agent terminals.
- The TUI's /ps does not call the list RPC. It keeps a local list keyed by processId: an entry is added on item/started with source unifiedExecStartup and removed on item/completed (cx/tui/src/chatwidget/command_lifecycle.rs:21-52, 135-205). /ps prints that list with the last 3 output lines (cx/tui/src/chatwidget.rs:1512-1521). /stop sends `thread/backgroundTerminals/clean` (cx/tui/src/chatwidget/slash_dispatch.rs:589-594; cx/core/src/session/handlers.rs:65-67).
- Lifetime: terminals survive turn end and `turn/interrupt`. They are killed by clean, by thread shutdown or unload, and by `thread/revert` (which restarts the runtime) (cx/core/src/session/handlers.rs:308-333).

### Codex: subagents (collab / spawn)
- A subagent is a separate thread. Relevant Thread fields: `parentThreadId`, `source: {subAgent: {thread_spawn: {parent_thread_id, depth, agent_path, agent_nickname, agent_role}}}`, `agentNickname`, `agentRole`, and `status: {type: notLoaded|idle|systemError|active, activeFlags}` (ts/v2/Thread.ts, ts/SubAgentSource.ts, ts/v2/ThreadStatus.ts).
- Listing running subagents (all stable methods):
  - `thread/list {ancestorThreadId: <root>}` lists descendants at any depth; `{parentThreadId}` lists direct children only. With either filter and no sourceKinds, the default interactive-source filter is switched off (thread_processor.rs:2799-2830, 6027-6031). Loaded threads get their live status filled in (thread_processor.rs:2888). Running means `status.type == "active"`. This lists persisted threads only.
  - The TUI's /subagents picker instead calls `thread/loaded/list` → {data: threadIds}, then `thread/read {threadId, includeTurns:false}` for each, and keeps threads whose `source.subAgent.thread_spawn.parent_thread_id` chains back to the primary thread. It treats active as running and notLoaded as closed (cx/tui/src/app/session_lifecycle.rs:1246-1310; cx/tui/src/app/loaded_threads.rs:52-102).
- Live signals:
  - `thread/status/changed {threadId, status}` is broadcast to every connection for every loaded thread, children included (cx/app-server/src/thread_status.rs:223-249; cx/app-server/src/outgoing_message.rs:847-859, 879-889).
  - Agent-spawned children get no `thread/started`; only thread/start and thread/fork send it (thread_processor.rs:1766, 5937). Instead, every initialized connection is attached to the new child automatically (cx/app-server/src/lib.rs:1327-1345; cx/app-server/src/request_processors/thread_lifecycle.rs:189-243). The child's `turn/*` and `item/*` notifications therefore reach the dashboard with the child's threadId.
  - Parent-side item `collabAgentToolCall` {tool: spawnAgent|sendInput|resumeAgent|wait|closeAgent|sendMessage|followupTask|interruptAgent|listAgents, status: inProgress|completed|failed|interrupted, senderThreadId, receiverThreadIds, prompt, model, reasoningEffort, agentsStates: {[threadId]: {status: pendingInit|running|interrupted|completed|errored|shutdown|notFound, message}}}. The spawnAgent item completes once the spawn itself finishes, with receiverThreadIds=[child] and state pendingInit or running, not when the child finishes (cx/app-server-protocol/src/protocol/event_mapping.rs:79-137). Later `wait` and `closeAgent` items carry the final agentsStates.
  - Multi-agent v2 also sends `subAgentActivity` items {kind: started|interacted|interrupted|completed, agentThreadId, agentPath, model, reasoningEffort}, always as `item/completed` with no started event (event_mapping.rs:187-203). "started" comes at spawn (cx/core/src/tools/handlers/multi_agents_v2/spawn.rs:165-172). "completed" goes to the parent only when the child ends with status Completed; errored or shut-down children produce none (cx/core/src/agent/control/completion.rs:50-85).
  - Whether a model can spawn subagents at all is set per model by `Model.multiAgentVersion` ("disabled"|"v1"|"v2"). The default `MultiAgentMode` is explicitRequestOnly.
- **Problem in the dashboard's Codex code:** crates/agent/src/collect/codex-remote.mjs:68-69 takes `turnId` from any `turn/started` or `turn/completed`. The control handler (codex-remote.mjs:538-548) and `chatMessages` never check `params.threadId`. A subagent's turns would toggle the working state, replace the turnId used for steer and interrupt, and leak the child's items into the chat.

### OpenCode
Shells
- The agent tool is `shell` (v1 `bash`); v1 `task` is now `subagent` (P/core/src/v1/config/migrate.ts:29-30). Input `{command, workdir?, timeout?, background?}` (P/core/src/tool/plugin/shell.ts:46-59). `background:true` returns at once, text "Command moved to the background (shell ID: sh_…)", metadata `{status:"running", shellID}` (shell.ts:94-99, 241-245). A blocking call can be pushed to background with `POST /api/session/:id/background` → 204 (TUI ctrl+b; P/protocol/src/groups/session.ts:691; shell.ts:247-254).
- `GET /api/shell` → `{location, data: Shell.Info[]}`: RUNNING shells only, location-wide: foreground and background tool shells, user `!cmd` shells, `POST /api/shell` shells (P/core/src/shell.ts:187-191; P/protocol/src/groups/shell.ts:10-21). Shell.Info `{id:"sh_…", status: running|exited|timeout|killed, command, cwd, shell, file, pid?, exit?, signal?, metadata, time{started, completed?}}`. Owner is `metadata.sessionID` (tool: shell.ts:196; `!cmd`: `{sessionID, background:true}`, P/core/src/session/shell.ts:17). A subagent's shells carry the child session ID. `GET /api/shell/:id` also serves exited ones (last 25 kept, P/core/src/shell.ts:32); `GET /api/shell/:id/output?cursor&limit` pages output (64 KiB default).
- Events (ephemeral): `shell.created {info}`, `shell.exited {id, exit?, status}`, `shell.deleted {id}` (P/schema/src/shell.ts:58-61; emitted P/core/src/shell.ts:178-179, 423-426, 442). `session.shell.started {sessionID, shell}` and `session.shell.ended {sessionID, shell, output{output, cursor, size, truncated}}` fire ONLY for user `!cmd` (`POST …/shell`) and project a history row `{type:"shell", shellID, command, status, exit?, output?, time{created, completed?}}` (P/core/src/session/session.ts:191-233; P/core/src/session/message-updater.ts:192-216; P/schema/src/session-message.ts:120-133).
- Background job list (experimental): `GET /api/experimental/job?sessionID=&kind=shell|subagent|image&status=running|completed|error|cancelled&descendants=true` → `{data: Job.Info[]}` (P/protocol/src/groups/job.ts:14-44). Holds only BACKGROUNDED jobs (`background:true` or moved by `/background`) that are running or finished with the notification not yet delivered. Foreground tool calls are absent (P/core/src/job/list.ts:76-123; P/core/src/job.ts:185-187, 389-401). Job.Info `{kind, id, sessionID (owner = parent), status, title?, time{started, completed?}}` plus shell `{shellID, command}`, subagent `{childSessionID, agent (agent NAME), description}` (id = childSessionID), image `{path, model}` (P/schema/src/job.ts:7-59). `descendants=true` walks parentID (P/core/src/session/background.ts:53-69). There are no `job.*` events.
- When background work finishes, a synthetic history row is added. Shell: `{type:"synthetic", text:"<shell id=… state=… command=…>…</shell>", description: command, metadata{source:"shell", shellID, jobID?, state: completed|cancelled|error, exit?…}}`. Subagent: text `<subagent sessionID=… state=… description=…>…`, metadata `{source:"subagent", childID, agent, state}` (P/core/src/shell/result.ts:45-63; P/core/src/session/subagent-completion.ts:37-44).
- PTYs are user terminals, not agent shells: `GET /api/pty` (exited ones included; id, title, command, args, cwd, status, pid, exitCode) and `GET /api/experimental/session/:id/terminal` (PersistentPty.Info with sessionID, foregroundProcess). Events `pty.*` and `persistent-pty.*`.

Subagents
- Tool `subagent`, input `{agent, description, prompt, model?, sessionID? (continue a child), background?}` (P/core/src/tool/plugin/subagent.ts:17, 30-48). It creates a child session `{parentID: caller, title: description, agent, model}` (subagent.ts:185-198). Tool progress metadata `{sessionID: child, status:"running"}`; final metadata `{sessionID, status: completed|running}` (subagent.ts:201, 258-265). Nesting depth comes from `experimental.subagent_depth` (default 1, subagent.ts:135-139). The job ID is the child session ID (P/core/src/session/subagent-job.ts:45-63).
- To list running subagents, take the children from `GET /api/session?parentID=<sid>` (paginated, default 50; `parentID=null` gives roots) and intersect them with `GET /api/session/active` → `{data:{"ses_…":{type:"running"}}}`. The active list holds every session, root or child, that this process is executing (P/server/src/handlers/session.ts:158-164).
- Live signals: `session.created {sessionID, parentID, title, agent, model, …}`, then `session.execution.started|succeeded|failed|interrupted {sessionID}` for the child. A child's Session.Info also has `time.busy`, `time.idle` and `outcome`. The mini TUI uses exactly these four sources: tool `metadata.sessionID`, `parentID` list, active map, live events (P/tui/src/mini/stream-v2.subagent.ts:3-17). Background subagents also appear in the job list.

## 2. Thinking

### Codex
- Item `{type:"reasoning", id, summary: string[], content: string[]}` (ts/v2/ThreadItem.ts; conversion at cx/app-server-protocol/src/protocol/v2/item.rs:979-983). `summary` holds the Responses API summary_text parts; `content` holds the raw reasoning_text/text parts (cx/core/src/event_mapping.rs:214-239). `content` is filled only when the provider returns raw reasoning text. OpenAI-hosted models normally return only summaries; that is inferred from API behaviour, not from this source.
- Deltas. The server forwards all of them unconditionally (cx/app-server/src/bespoke_event_handling.rs:1028-1038; mapping at event_mapping.rs:387-412):
  - `item/reasoning/summaryTextDelta` {threadId, turnId, itemId, delta, summaryIndex}
  - `item/reasoning/summaryPartAdded` {threadId, turnId, itemId, summaryIndex} starts a new summary section; sent when summaryIndex > 0 (cx/core/src/session/turn.rs:3485-3494)
  - `item/reasoning/textDelta` {threadId, turnId, itemId, delta, contentIndex} carries raw content (turn.rs:3505-3521)
- Order: `item/started` (empty arrays) → deltas → `item/completed` (final arrays).
- The TUI renders summary deltas. It shows textDelta only when config `show_raw_agent_reasoning` is set; that check is in the client, not the server (cx/tui/src/chatwidget/protocol.rs:148-178).
- Summary verbosity: `summary: auto|concise|detailed|none` on turn/start (ts/ReasoningSummary.ts), or config `model_reasoning_summary`.
- History: reasoning items appear in `thread/turns/list` only with `itemsView:"full"`. The "summary" view keeps just the first user message and the last agent message (cx/app-server-protocol/src/protocol/turn_items_view.rs:9-31).

### OpenCode
- History: an assistant `content[]` part `{type:"reasoning", text, native?, time?{created, completed?}}`, with no part ID (P/schema/src/session-message.ts:192-205). The token count is in assistant `tokens.reasoning`.
- Stream events:
  - `session.reasoning.started {sessionID, assistantMessageID, ordinal, native?}`
  - `session.reasoning.delta {…, ordinal, delta}`
  - `session.reasoning.ended {…, ordinal, text, native?}`; it carries the full text, so replace rather than append.
  `ordinal` numbers the reasoning blocks inside one assistant message, on a counter separate from text blocks. Deltas are batched every 100 ms (P/core/src/session/runner/publish-llm-event.ts:98, 160-167, 230-252, 436-451; projection P/core/src/session/message-updater.ts:390-410).
- There is a single `text` field, with no summary/raw split. It holds what the provider streams:
  - OpenAI Responses: the reasoning summary (`reasoningSummary:"auto"`, encrypted reasoning kept in `native`) (P/core/src/variant.ts:41, 111-113; P/ai/src/providers/openai-options.ts:41-45).
  - Anthropic: adaptive thinking requested with `display:"summarized"` (variant.ts:42); signature and redacted data are kept in `native` (P/ai/src/protocols/anthropic-messages.ts:1188, 1217).
  - Chat-completions providers may stream raw reasoning.

## 3. Todo and plan checklist

### Codex
- `turn/plan/updated` {threadId, turnId, explanation: string|null, plan: [{step, status: "pending"|"inProgress"|"completed"}]}. Each update replaces the whole list (cx/app-server-protocol/src/protocol/v2/turn.rs:630; bespoke_event_handling.rs:1323-1330, 1364-1384). It goes to the thread's subscribers.
- It comes only from the `update_plan` tool, which is rejected in Plan mode ("update_plan is a TODO/checklist tool and is not allowed in Plan mode", cx/core/src/tools/handlers/plan.rs:86-97). In Default mode the checklist and the Plan-mode `plan` item are separate things.
- It is not saved in history: it is absent from thread/turns/list and not replayed on thread/resume (thread_history.rs has no PlanUpdate handling). A client that joins later sees no checklist until the next update. No list API exists.

### OpenCode
- Tool `todowrite` exists ONLY when the feature flag `todo` is on: `experimental.todo: true` in config, or `OPENCODE_FLAGS=todo` (or `all`). Flags default to on only in local/dev builds, so the tool is OFF in release 2.2.6 (P/core/src/tool/plugin/todo.ts:12, 50-56; P/schema/src/config/flag.ts:11-18; P/core/src/flag.ts:26).
- Input `{todos:[{content, status: pending|in_progress|completed|cancelled}]}`. Each call replaces the full list; items have no id and no priority. Output `{todos}`, metadata `{todos}`, text like "Todos updated: 2/5 completed, 1 in progress…" (todo.ts:14-42, 73-77).
- There is no todo API and no todo event. The current list is `state.input.todos` of the latest completed `todowrite` tool part before the revert boundary (P/tui/src/util/todo.ts:32-43). Live sequence: `session.tool.input.started {id, name:"todowrite"}` → `session.tool.called {id, input}` → `session.tool.success {id, content, metadata{todos}}`. Only `input.started` carries the tool name.

## 4. Usage and context

### Codex
- `thread/tokenUsage/updated` {threadId, turnId, tokenUsage: {total, last, modelContextWindow: number|null}}. The breakdown is {totalTokens, inputTokens, cachedInputTokens, **cacheWriteInputTokens**, outputTokens, reasoningOutputTokens}. research-codex.md is missing cacheWriteInputTokens (ts/v2/TokenUsageBreakdown.ts; cx/app-server-protocol/src/protocol/v2/thread.rs:2053-2061). `total` accumulates over the thread; `last` covers the last model request.
- When it is sent: the server turns each core TokenCount event into this notification. TokenCount fires:
  - after each sampling request, once its tools resolve (cx/core/src/session/turn.rs:3572-3579)
  - after compaction
  - on context-window-exceeded (usage set to full)
  - on usage-limit errors
  (cx/core/src/session/mod.rs:5507-5546; turn.rs:1894-1903). On thread/resume and thread/fork it is replayed, but only to the connection that joined (cx/app-server/src/request_processors/token_usage_replay.rs:35-60).
- Context use: the TUI's "% left" compares `last.totalTokens` with `modelContextWindow`, subtracting a 12,000-token baseline from both (cx/protocol/src/protocol.rs:2659-2705). model/list has no context-window field; this notification is the only source.
- Rate limits: `account/rateLimits/updated {rateLimits: RateLimitSnapshot}` rides on the same TokenCount event whenever the session holds a snapshot. The snapshot is updated from response headers on every request (turn.rs:3310-3315). It goes only to that thread's subscribers (bespoke_event_handling.rs:1652-1678). Updates are sparse: merge each one into the last read.
  - RateLimitSnapshot {limitId, limitName, normalModelSlug, primary|secondary: {usedPercent, windowDurationMins, resetsAt}|null, credits: {hasCredits, unlimited, balance}|null, individualLimit, spendControlReached, planType, rateLimitReachedType}.
  - `account/rateLimits/read` → {ordinaryUsageAllowed, rateLimits, rateLimitsByLimitId, rateLimitResetCredits, accountId, rateLimitUpsell}.
- Cost: neither notifications nor Turn carry per-turn cost. `account/usage/read {threadId?}` (stable) → {summary, dailyUsageBuckets, threadUsage?: {threadId, estimatedUsageCreditsMicros, estimatedUsageUsdMicros, nativeUsageUsdMicros?, groups:[{model, reasoningEffort, speed, estimatedUsageCreditsMicros, netNewInputTokens, cachedInputTokens, inputTokens, outputTokens, totalTokens}]}}. It requires ChatGPT auth and calls the backend (cx/app-server/src/request_processors/account_processor.rs:1283-1305).

### OpenCode
- Session.Info has `cost` (USD) and `tokens {input, output, reasoning, cache{read, write}}`, both cumulative for that session only; children keep their own. It also has `outcome?` (succeeded|failed|interrupted) and `time{created, updated, busy?, idle?, viewed?, archived?}` (P/schema/src/session.ts:40-56).
- `session.usage.updated {sessionID, cost, tokens}` carries the new cumulative totals. It is published after each `session.step.ended` or priced `session.step.failed` (P/core/src/session/projector.ts:1163-1173).
- Per-step figures: `session.step.ended {sessionID, assistantMessageID, finish, rawFinish?, native?, cost, tokens, snapshot?, files?}`. Each assistant row (one LLM call) holds `cost?, tokens?, finish?, model{providerID, id, variant?}, retry?, error?`.
- `tokens.input` counts non-cached input only (P/core/src/session/usage.ts:11-19). Cost is the provider-reported amount, else the catalog price (usage.ts:23-52).
- Context fill, as the TUI and desktop compute it: take the last assistant row with `tokens` after the last completed compaction and before the revert boundary. Used = input + output + reasoning + cache.read + cache.write. Percent = used / `limit.context` of the matching `GET /api/model` entry (P/client/src/solid/usage.ts:8-21; P/tui/src/util/session.ts:46-60).
- The root's displayed cost is the sum over the session family (P/client/src/solid/data.ts:1539-1547).
- Model limit: `GET /api/model` → `data[].limit {context, input?, output}` (P/schema/src/model.ts:133-137). `GET /api/session/:id/context` returns the message rows after the last compaction, not numbers.
- Rate limits: there are NO per-provider windows or rate-limit headers.
  - The only quota data is `GET /api/experimental/usage/limits` → `{location, data: null | {workspace{id, name?, usageURL}, go?{rolling, weekly, monthly: {percent, limited, resetsAt}}, budgets[{id, limit, spent, scope, per, coverage, models, vendors, inputPriceAtLeast?, picked, shared, resetsAt}]}}`. It applies to an OpenCode Console sign-in (Zen/Go) and is what the TUI `/usage` shows (P/schema/src/usage-limits.ts:12-76; P/protocol/src/groups/usage.ts:10-23; P/tui/src/component/usage.tsx:37).
  - Throttling appears as `session.retry.scheduled {sessionID, assistantMessageID, attempt, at, error{type:"provider.rate-limit", message, status?, response?{body}, retryAfterMs?}}` and as assistant `retry{attempt, at, error}` (P/core/src/session/to-session-error.ts:40-41; P/schema/src/session-error.ts:7-14).
  - `session.status` (idle|busy|retry) and `session.idle` are in the event union, but nothing in core or server 2.2.6 emits them.
- `GET /api/experimental/session/stats?from&to&project&timezone&tools` aggregates usage across sessions (TUI `/stats`).

## 5. Diff

### Codex
- `turn/diff/updated` {threadId, turnId, diff}: a git-style unified diff ("diff --git a/x b/x", new/deleted file mode, `index <oid>..<oid>`, ---/+++, hunks). It shows the net change since the turn started, across all files; replace it on every update (cx/core/src/turn_diff_tracker.rs:47-60, 114-180, 335-364).
  - It tracks only apply_patch changes. Shell edits (sed, scripts) do not appear.
  - It is sent after each patch (cx/core/src/tools/events.rs:620-650) and at turn end (cx/core/src/session/turn.rs:3585-3594). It is live only and does not appear in thread/turns/list.
- `fileChange` item {id, changes: FileUpdateChange[], status: inProgress|completed|failed|declined}; FileUpdateChange is {path, kind: {type:"add"}|{type:"delete"}|{type:"update", move_path}, diff}.
  - `diff` holds the full file content for add and delete, and a unified diff without file headers for update. Moves append "\n\nMoved to: <path>" (cx/app-server-protocol/src/protocol/item_builders.rs:342-381).
  - It is kept in history (full view). `item/fileChange/patchUpdated` {threadId, turnId, itemId, changes} streams the changes. `item/fileChange/outputDelta` is deprecated and no longer sent.
- Workspace diff: no app-server API exists. The TUI's /diff runs `git diff --no-textconv --no-ext-diff --submodule=short --ignore-submodules=dirty --color`, plus a diff per untracked file, through app-server `command/exec` (cx/tui/src/get_git_diff.rs:51-110; slash_dispatch.rs:498-523). `gitDiffToRemote {cwd}` → {sha, diff} is a legacy v1 method (common.rs:1545).

### OpenCode
- Session diff: `GET /api/session/:id/diff?from=<userMsgID>&to=<userMsgID>&context=<n>` → `{data: FileDiff[]}`. Correction: the array is wrapped in `data`.
  - It diffs OpenCode's own snapshot trees (a shadow git repo per location), not git HEAD. It compares the first `snapshot.start` among the turn's assistant steps with the last `snapshot.end`. If the turn is still running in this process, it compares with a fresh capture of the working copy instead.
  - Default range is the newest user message's turn. A turn runs from the first prompt after the last idle marker to the next idle marker. A range crossing a location switch is an error; without snapshots the result is `[]` (P/core/src/session/diff.ts:23-149; P/protocol/src/groups/session.ts:515-537).
- VCS diff: `GET /api/vcs/diff?mode=working|branch|committed&base?&context?&untracked=files|folders&location[directory]=` → `{location, data: FileDiff[]}`. Modes: working = HEAD → working copy, branch = merge-base(base) → working copy, committed = merge-base → HEAD. It goes through the git (or hg) provider (P/protocol/src/groups/vcs.ts:26-32, 111-123; P/schema/src/vcs.ts:28-33).
- FileDiff format `{file, patch, additions, deletions, status: added|deleted|modified, more?: true}` (P/schema/src/file-diff.ts:6-14).
  - `patch` is unified-diff text.
  - Omitting `context` gives FULL-FILE context (PATCH_CONTEXT_LINES = 2^31-1), so send e.g. `context=3`.
  - Each response has a total patch budget of 10,000,000 bytes. Files past it get a header-only empty patch, but their counts are kept (P/core/src/vcs/patch.ts:5-8; P/core/src/vcs.ts:241-251; P/core/src/git.ts:995-1000).
- Changed-file lists without patches: assistant `snapshot{start?, end?, files?}` and `session.step.ended.files` (relative paths).

## 6. Side chat

### Codex
- /side and /btw are the same command (cx/tui/src/slash_command.rs:146-148; slash_dispatch.rs:403-405). Text after the command (`/btw question`) becomes the first side message (slash_dispatch.rs:1092-1108). The command can run while a task is running.
- The exact TUI calls (cx/tui/src/app/side.rs:716-853; cx/tui/src/app_server_session.rs:1156-1265):
  1. `thread/fork` with these params:
     - `threadId`: the parent
     - `ephemeral: true` and `excludeTurns: true`
     - model, effort and serviceTier copied from the parent's current settings
     - `developerInstructions`: the existing instructions plus SIDE_DEVELOPER_INSTRUCTIONS
     - in remote mode, approvalPolicy, sandbox and permissions are cleared so the saved ones apply
     - no lastTurnId or beforeTurnId, so the full history is copied
     (side.rs:638-652, app_server_session.rs:1206-1258.) The fork fails before the parent's first user message (side.rs:664-674).
  2. `thread/inject_items` {threadId: child, items: [{type:"message", role:"user", content:[{type:"input_text", text: SIDE_BOUNDARY_PROMPT}]}]} (side.rs:28-42, 624-636, 795-806). This is a stable method.
  3. `turn/start` on the child with the question.
  - The TUI also hides the inherited history from view (side.rs:686-695).
- Cleanup when the user leaves the side chat (return, Ctrl+C, switching away):
  - `turn/interrupt {threadId: child, turnId}`, retried with the actual turn id on a race, then `thread/unsubscribe {threadId: child}` (side.rs:435-516).
  - An ephemeral thread is never written to disk. Once it is idle with no subscribers, it is shut down after `thread_unload_delay_secs` (default 60) (cx/core/src/config/mod.rs:4145-4146; thread_lifecycle.rs:866).
  - thread/revert is rejected on ephemeral threads. The TUI's agents overview ignores ephemeral `thread/started` (cx/tui/src/app/agents_overview_threads.rs:123-126).
- Cost: the fork carries the parent's whole model context, so the first side turn re-sends it all. The prefix is identical, so prompt-cache reuse is likely; that is inferred, not verified.
- `thread/sample` (exp) is **not** used by /side; no TUI code calls it. It is a one-shot alternative:
  - Request {threadId, expectedTurnId, prompt (≤4,000 bytes), effort?, threadSource?, turnTrigger?, responsesapiClientMetadata?, disableMcp?, interruptOnCancel?} → {sourceTurnId, responseId, text|null, inputTokens, cachedInputTokens, outputTokens}.
  - The source thread must be loaded, idle and a persistent root thread (not ephemeral, not a subagent). expectedTurnId must be its latest finished turn.
  - It forks ephemerally from the persisted history (built for cache reuse) and returns one model response. Timeout is 30 s. `text` is null if the model tried a tool call, and any approval or question aborts it. Cleanup is automatic.
  - Cancel with `thread/sample/cancel {requestId}`.
  - Sources: cx/app-server-protocol/src/protocol/v2/thread_sample.rs; cx/app-server/src/thread_sample.rs:55-58, 75-330; thread_processor.rs:656-688.
  - Good for a single /btw answer while the parent is idle. It cannot do a multi-turn or tool-using side chat.

### OpenCode
- Native route: `POST /api/session/:id/generate {prompt}` → `{data:{text}}`, described as "Generate transient text from the current session context without mutating session history" (P/protocol/src/groups/session.ts:634-649).
  - It uses the session's current agent and model through the same request pipeline as a primary turn: system prompt, tool definitions, history (the settled prefix after the last compaction) and any instruction update, plus one user message.
  - It makes one non-streaming call with retries. There is no tool loop, so a tool call yields empty text. It emits no events.
  - Its usage is only logged, NOT added to the session's cost or tokens (P/core/src/session/generate.ts:25-68; P/core/src/session/history.ts:304-329; P/core/src/session/model-request.ts:447).
  - There is no busy check, so it can run while the session works; only the settled prefix is sent.
- TUI `/btw` ("Ask a side question", TUI-only; this corrects research-opencode.md's TUI-only list) is this call. Its prompt is "The user is asking a quick side question about the conversation so far. Answer directly and concisely in markdown from what you already know. Do not call any tools and do not take any actions." + "\n\n" + question. The answer appears in a dialog (P/tui/src/feature-plugins/prompt/btw.tsx:15-79).
- Cost of `/btw`: one request with the whole current context as input. The prompt cache may hit because the prefix matches the primary turn's.
- Fork alternative:
  - `POST /api/session/:id/fork {before?}` → `{data: Session.Info}`. It creates a new ROOT session: parentID is unset and `fork{sessionID, boundary{type: before|through, messageID}}` is set.
  - The fork copies the history rows under new IDs and is titled "<title> (fork #n)". It also copies agent, model, permissions and `metadata`, including our `cdash_owner`. Its cost starts at 0. Event `session.forked {sessionID, parentID, boundary, …}` (P/core/src/session.ts:339-381; P/core/src/session/projector.ts:120-200).
  - `POST …/prompt` then runs a FULL agent loop, which can use tools and edit files, and resends the copied context. The cost lands on the fork.
  - Cleanup: `DELETE /api/session/:forkID` → 204. It interrupts, waits for idle, cancels jobs, deletes children, emits `session.deleted {sessionID}`, hard-deletes the row and releases blobs (P/core/src/session.ts:391-401; projector.ts:963-968).
  - Neither create nor fork has an ephemeral or no-persist flag.

## 7. Rewind

### Codex
- `thread/revert` (stable) {threadId, beforeTurnId} → {thread (turns always empty), turnsBackwardsCursor, itemsBackwardsCursor} (v2/thread.rs:1308-1338).
  - It rejects ephemeral threads, non-paginated threads, and threads that do not accept direct input.
  - It shuts down the thread's runtime: the active turn ends as interrupted, pending requests are cancelled and background terminals are killed. It then restarts the runtime on the history before beforeTurnId (thread_processor.rs:2243-2340; cx/core/src/session/handlers.rs:308-333). Files are left as they are.
  - It broadcasts `thread/reverted {threadId}` to all connections (thread_processor.rs:690-711).
- TUI double-Esc (cx/tui/src/app_backtrack.rs:1-22, 144-180, 526-681; cx/tui/src/app/event_dispatch.rs:737-900):
  - With an empty composer, the first Esc arms backtrack. The second opens the transcript with the latest user prompt highlighted. Left/Right pick a prompt; Enter confirms.
  - It re-reads history (`thread/read includeTurns:false`, then `thread/turns/list`) and stops if the latest turn id has changed.
  - **beforeTurnId** is the id of the turn whose **first** visible user message is the selected prompt. Steers (the second or later user message in a turn), turns still in progress, and review-mode prompts are refused (app_backtrack.rs:568-681).
  - **The prompt is put back:** `restore_user_message_to_composer` restores text, text_elements, local images, remote image URLs and mention bindings (event_dispatch.rs:845-874). The prompt is also restored after an error. Queue autosend is suppressed afterwards.
  - The TUI started with `--remote` refuses when the prompt has local images or starts with / or ! (event_dispatch.rs:745-754). Backtrack is unavailable in side conversations.
- Other clients: the TUI chat view ignores a `thread/reverted` it did not start (no-op arm at cx/tui/src/chatwidget/protocol.rs:383); only the agents overview reacts. A revert from the dashboard therefore leaves the remote TUI showing stale turns until it resumes. This comes from reading the code; it was not tested.

### OpenCode
- Events: `session.revert.staged {sessionID, revert{messageID, partID?, snapshot?, files?: FileDiff[]}}`, `session.revert.cleared {sessionID}` and `session.revert.committed {sessionID, to: messageID}` (P/core/src/session/revert.ts:50-80).
- Stage: body `{messageID, files?: boolean}`, response `{data: Session.Revert}`. `files:false` hides messages and leaves files alone. Staging again first restores the files of the previous staged revert, so repeated undo walks further back (revert.ts:33-41). Stage, commit and clear all return 409 while the session executes (P/core/src/session/session.ts:332-357).
- While a revert is staged, `GET /api/session/:id/message` returns ALL rows unchanged: no filter and no per-row flag. `Session.Info.revert.messageID` is the only marker, and clients hide rows with `id >= messageID` (IDs ascend) (P/tui/src/routes/session/rows.ts:56-70; P/client/src/solid/data.ts:1214-1221).
- Commit, explicit or implicit on the next prompt or compact (P/core/src/session/session.ts:172-173, 264):
  - It deletes message rows from the boundary on (boundary included) and inbox rows enqueued at or after it, then releases their blobs.
  - Only `session.revert.committed` is emitted, with no per-message events. Clients splice IDs `>= to` (P/core/src/session/projector.ts:1131-1161; P/client/src/solid/data.ts:1222-1237).
  - Afterwards the history API simply no longer returns those rows.
- Clear restores the files and wakes the session, so pending work resumes (session.ts:344-351). Notifications admitted while a revert is staged do not wake the agent (session.ts:323).
- TUI `/undo` (P/tui/src/routes/session/index.tsx:1120-1145, 181-186, 782-787):
  - The target is the last user row with non-empty text before the current boundary.
  - If that prompt is still queued in the inbox, the TUI cancels the item and puts the prompt back.
  - Otherwise it reads the prompt (text, and images through the media read), calls `POST …/interrupt`, then `POST /api/experimental/session/:id/wait`, then `POST …/revert/stage {messageID}`. It then appends the text and images to the composer, so the prompt IS put back.
  - The message menu's "Revert" stages at that message and restores its prompt if it is a user row (P/tui/src/routes/session/dialog-message.tsx:35-50).
- TUI `/redo` is enabled only when `session.revert` exists. It calls `DELETE /api/session/:id/revert`, which clears the whole staged revert at once; there is no step-wise redo (index.tsx:1147-1162). Clicking the "N messages reverted" banner does the same (index.tsx:2265-2300).
- On prompt submit the TUI calls `POST …/revert/commit` before admission (P/tui/src/component/prompt/index.tsx:1342-1347).

## 8. Attachments in history

### Codex
- Live `item/started`/`item/completed` userMessage: `content` is the UserInput[] as sent: text{text, text_elements}, image{url:"data:<mime>;base64,…" | fileId, detail?}, localImage{path, detail?}. The full data URL is included.
- `thread/turns/list` userMessage: content is rebuilt from the saved UserMessage event in this order: text, then image{url} (inline and file images in their original order), then localImage{path}, then audio (cx/app-server-protocol/src/protocol/thread_history.rs:1710-1785). Image URLs are stored as submitted, not the copy resized for the model (cx/protocol/src/legacy_events.rs:83-117; cx/protocol/src/protocol.rs:2790-2820). The original interleaving of text and images is lost.
- Sizes: the dashboard starts the TUI with `--remote`, which gives AppServerTarget::Remote (cx/tui/src/lib.rs:1090-1099). In that mode the TUI turns attached or pasted local images into inline data URLs at original resolution, within a 32 MiB budget per message, because of "the WebSocket server's 64 MiB message limit" (cx/tui/src/chatwidget/image_submission.rs:130-170; cx/tui/src/app/session_lifecycle.rs:515). One image prompt typed in the TUI can therefore put tens of MB of base64 into item events and into each thread/turns/list page that contains it. A TUI not started with --remote sends localImage{path} instead.
- Ways to strip images:
  - The thread config feature `omit_app_server_notification_media` removes image and audio inputs from userMessage (and tool outputs) in item/started and item/completed. It applies to all subscribers of the thread, not one connection, and does not affect thread/turns/list (cx/app-server/src/bespoke_event_handling.rs:1129-1130, 1157-1158; cx/app-server/src/notification_media.rs:156-170).
  - initialize `capabilities.optOutNotificationMethods` can only drop whole methods for one connection.
- Thumbnails, suggested approach: the agent runs on the same host and already reads every event and history page. It can replace each `image.url` with {mime, bytes, sha256} and serve a small thumbnail (and the original on request) from an agent endpoint keyed by hash.
  - For images sent from the dashboard, save the upload to a file on the agent host and send `localImage{path}`. Events and history then carry only the path; the app-server reads the file and core resizes it for the model.
  - Caveat: the remote TUI will not restore such prompts for editing (section 7).
- `thread/attachment/*` stores generic per-thread metadata {attachmentType, identityKey, payload}. It is not image storage.

### OpenCode
- Row shape: a user row has `files?: [{data: <base64>, mime, source: {type:"inline"} | {type:"uri", uri}, name?, description?, mention?{start, end, text}}]`, plus `agents?` and `skills?` (P/schema/src/prompt.ts:13-38, 94-100; P/schema/src/session-message.ts:76-84).
  - `file://` URIs are read at admission and stored inline with source `uri`. A directory becomes a listing with mime `application/x-directory` (P/core/src/session/prompt.ts:173-222).
  - Tool results can contain `{type:"file", uri:"data:…;base64,…", mime, name?}` (P/schema/src/tool.ts:73-79).
- Sizes:
  - Any attachment may be up to 20 MiB raw, about 26.7 MiB as base64 (prompt.ts:224).
  - png, jpeg, gif, webp and bmp are normalized at admission. An image is stored unchanged if it fits 2000×2000 and 5 MiB of base64; an opaque PNG over 384 KiB is re-encoded as JPEG q80 when that is smaller. Larger images are scaled down in ×0.75 steps and re-encoded as JPEG q80→40, or PNG, until they fit 5 MiB of base64 (P/core/src/image.ts:61-68; P/core/src/image/photon-normalize.ts:28-30, 62-147).
  - The limits are configurable as `media.image.{auto_resize, max_width, max_height, max_base64_bytes}` (P/core/src/config/plugin/image.ts:18-24).
  - PDFs and other non-images are never shrunk.
- Media refs:
  - With the header `X-OpenCode-Capabilities: experimental-media-uri`, `GET …/message`, `GET …/message/:id` and `GET …/inbox` return `files[{uri:"media:<sha256>", mime, source, name?, …, bytes}]` without `data` (P/server/src/media-uri.ts:3-13; P/schema/src/prompt.ts:54-73; P/server/src/handlers/message.ts:63-70; P/server/src/handlers/session.ts:526, 625).
  - This needs flag `media_blob` on for the session's location (`experimental.media_blob: true` or `OPENCODE_FLAGS=media_blob`). The flag is off in release builds and only affects media stored after it was turned on; otherwise rows stay inline (P/core/src/session/blob.ts:262-267, 342-352).
  - `width` and `height` exist in the schema, but nothing in 2.2.6 writes them; the insert stores hash, mime, bytes and data (blob.ts:295-299).
  - Bytes come from `GET /api/experimental/media/:hash`, which serves only referenced blobs with `Cache-Control: private, max-age=31536000, immutable`, ETag `"<hash>"` and 304 on If-None-Match. png, jpeg, gif and webp keep their mime; anything else is served as `application/octet-stream` (P/server/src/handlers/media.ts:8-35).
- Live events always carry the full base64:
  - `session.inbox.enqueued {sessionID, inboxID, item{type:"user", payload{text, files[{data,…}], …}, delivery}}` (P/core/src/session/inbox.ts:198-203). SSE frames are plain JSON (P/server/src/event-feed.ts:29-31).
  - `session.tool.success` content can also carry data URIs.
  - The repo drops the SSE stream on a frame over 1 MiB (crates/agent/src/collect/opencode-api.mjs:63) and fails REST bodies over 8 MiB (opencode-api.mjs:31), so a single image prompt can trip either limit. `GET /api/event?types=…` filters on the server.
- Thumbnails: there is no server thumbnail or resize route. The cheapest path:
  1. The agent fetches the rows, ideally with media refs on, or with `?type=user`, or one row at a time with `GET …/message/:id`.
  2. It strips `data` and sends clients `{name, mime, size ≈ len*3/4}` plus a small thumbnail it makes itself.
  3. It serves the full bytes on demand, from `/api/experimental/media/:hash` when refs are on.

## 9. Commands for a picker

### Codex
- `skills/list` (stable) {cwds?, forceReload?} → {data:[{cwd, skills:[…], errors}]} (ts/v2/SkillsList*.ts, ts/v2/SkillMetadata.ts).
  - Each skill: {name, description, shortDescription?, interface?: {displayName, shortDescription, iconSmall, iconLarge, iconSmallUrl, iconLargeUrl, brandColor, defaultPrompt}, dependencies?, path, scope: user|repo|system|admin, enabled, pluginId}.
  - `skills/changed {}` only signals that the list is stale.
  - To invoke a skill, add UserInput `{type:"skill", name, path}` to the turn input.
- No API lists custom prompts or slash commands. /presets are TUI "quick dials" kept in config.toml under `[tui.quick_dials]` (cx/core/src/config/mod.rs:904, 3372). They may be readable through the raw layer JSON of `config/read {includeLayers:true}`; not verified. MCP prompts are not exposed by any app-server API.
- Other lists that exist: plugin/list, app/list, hooks/list, collaborationMode/list.

### OpenCode
- `GET /api/command` → `{location, data: [{name, description?}]}`. That is ALL: no template, arguments, agent, model, subtask or source (P/schema/src/command.ts:9-13; P/protocol/src/groups/command.ts:9-12). A `command.updated` event (no payload) fires on change.
- Sources:
  - Built-ins `init` and `review` (P/core/src/plugin/command.ts:29-54) and `goal` (P/core/src/plugin/goal.ts:343-346). `schedule` exists only with flag `schedules` (P/core/src/tool/plugin/schedule.ts:156-160).
  - Config: `command` entries in config documents, and `{command,commands}/**/*.md` in config dirs. The name is the relative path without `.md` (P/core/src/config/plugin/command.ts:28-31, 119-149). The definition fields (template, description, agent, model, subagent|subtask) stay on the server (P/schema/src/config/command.ts:7-13).
  - MCP prompts, named `<server>:<prompt>` (sanitized), with arguments split positionally from the text (P/core/src/plugin/command.ts:55-80, 104-106).
- Skills are not commands:
  - `GET /api/skill` → `{location, data:[{id, name, description?, autoinvoke?, path, content}]}`, where `content` is the full SKILL.md text.
  - A skill is attached with prompt `skills:[{id}]` or `POST /api/experimental/session/:id/skill`.
  - The TUI offers skills as `@<id>` mentions and commands under `/` (P/tui/src/component/prompt/autocomplete.tsx:443-457, 491-512).
- The TUI `/` picker merges its own keymap slash names with the server commands. TUI-only names: agents, btw, cd, clear, compact, connect, copy, debug, diff, editor, exit, export, fork, help, highlights, mcps, models, new, open, pair, plugins, redo, reload, rename, restart, sessions, settings, skills, stats, status, terminal, themes, timeline, undo, update, usage, variants (aliases thinking, effort), worktrees.

## 10. Model, effort and mode lists

### Codex
- `model/list` {cursor?, limit?, includeHidden?} → {data: Model[], nextCursor}. Model fields (ts/v2/Model.ts; no context window):
  - identity: id, model, displayName, description, hidden, isDefault
  - effort: supportedReasoningEfforts: [{reasoningEffort, description}], defaultReasoningEffort
  - capabilities: inputModalities, multiAgentVersion ("disabled"|"v1"|"v2"|null)
  - service tiers: serviceTiers: [{id, name, description}], defaultServiceTier, additionalSpeedTiers (deprecated)
  - other: upgrade, upgradeInfo, availabilityNux, modelSpecialty, availableAccessPrograms, supportsPersonality (deprecated, always false)
- ReasoningEffort is an open string: none|minimal|low|medium|high|xhigh|max|ultra|persistent, or a custom value the model defines (cx/protocol/src/openai_models.rs:68-98). Effort levels are per model, from supportedReasoningEfforts.
- `collaborationMode/list` (exp) {} → {data: [{name, mode: "plan"|"default"|null, model: string|null, reasoning_effort: string|null}]}. Note that `reasoning_effort` is snake_case (ts/v2/CollaborationModeMask.ts; cx/app-server/src/request_processors/catalog_processor.rs:344-356). Apply a mode with `collaborationMode {mode, settings:{model, reasoning_effort, developer_instructions}}` on turn/start or thread/settings/update.
- `modelProvider/capabilities/read` → {imageGeneration, webSearch}.

### OpenCode
- Agents: `GET /api/agent` → `{location, data: Agent.Info[]}`, hidden agents included.
  - Agent.Info `{id, name, model?{providerID, id, variant?}, request{settings, headers, body}, system?, description?, mode: subagent|primary|all, hidden, color?: string, steps?, permissions}` (P/schema/src/agent.ts:30-33).
  - Built-ins: build (primary, the default), plan (primary), general and explore (subagent), and compaction, title and summary (primary, hidden) (P/core/src/plugin/agent.ts:56-118; P/core/src/plugin/plan.ts:34-37).
  - TUI Tab cycles, with wraparound, through agents where `mode !== "subagent" && !hidden` (P/tui/src/context/local.tsx:59-60, 117-125).
- Models: `GET /api/model?types=chat|decision|image` → `{location, data: Model.Info[]}`. It lists ENABLED models only; the default type is chat (P/server/src/handlers/model.ts:11-16; P/core/src/model.ts:249).
  - Model.Info `{id, modelID, providerID, canonical?, type?, family?, name, compatibility?, package?, settings?, headers?, body?, capabilities{tools, input[], output[], temperature?}, variants[{id, settings?, headers?, body?}], time{released}, cost[{tier?{type:"context", size}, input, output, cache{read, write}}], status: alpha|beta|deprecated|active, enabled, limit{context, input?, output}}` (P/schema/src/model.ts:71-137).
  - `defaultVariant` does NOT exist, so the repo's `normalizeModels` always gets null (opencode-api.mjs:133). "No variant" is the default.
  - `headers` and `body` are config overlays; don't forward them blindly.
- Variants: variant IDs are per-provider effort presets (P/core/src/variant.ts), e.g.:
  - OpenAI: none, minimal, low, medium, high, xhigh (:108)
  - Anthropic: low, medium, high, xhigh, max (:333)
  - toggle models: none, thinking (:52)
- `GET /api/model/default` → `{location, data: Model.Info | null}`: the configured default if it is enabled, else the first available text model (P/core/src/model.ts:300-305).
- TUI variant cycle: default → variants[0] → … → last → default, where `"default"` means no variant (P/tui/src/model-preference.ts:41-43, 67-75). The TUI command is `/variants`, with aliases `/thinking` and `/effort`.
- Agent, model and variant are TUI-local drafts until submit. Then the TUI calls `POST …/agent` and `POST …/model {model{providerID, id, variant}}`, which emit `session.agent.selected` and `session.model.selected` (P/tui/src/component/prompt/index.tsx:1281-1297). The session's current variant is `Session.Info.model.variant`.
